use std::{collections::HashMap, fmt::Debug, time::Duration, str::FromStr};

use chrono::{DateTime, Datelike};
use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_model::{
    data::Bar,
    enums::OrderSide,
    events::{OrderFilled, PositionOpened},
    identifiers::{InstrumentId, StrategyId, Venue},
    types::Quantity,
    instruments::Instrument,
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};

use crate::{
    capture::{RunCapture, YearMonth},
    config::{self, RunConfig},
    data::{bar_type, structure::BoundedQueue},
    sizing::{self, Conviction},
};

use anyhow::anyhow;
use rust_decimal::{Decimal, prelude::ToPrimitive};

#[derive(bon::Builder)]
pub struct XSectionalMomentum {
    #[builder(default = StrategyCore::new(StrategyConfig {
         strategy_id: Some(StrategyId::from("X-SEC-MOM")),
         order_id_tag:Some("001".to_string()),
         ..Default::default()
    }))]
    core: StrategyCore,

    /// The resolved run configuration (CLI flags + universe file). Every
    /// strategy knob lives here; the capture layer serialises it to
    /// `runs/<uuid>/config.csv`.
    config: RunConfig,

    /// Bybit instrument ids for `config.bases`, filled in `on_start`.
    #[builder(skip)]
    instruments: Vec<InstrumentId>,

    #[builder(default = 0)]
    last_month: u32,

    #[builder(default)]
    prices: HashMap<InstrumentId, BoundedQueue<Decimal>>,

    #[builder(default)]
    returns: HashMap<InstrumentId, Decimal>,

    /// Per-run artifact capture (`runs/<uuid>/{legs,portfolio,fills}.csv`).
    /// `None` until `on_start` opens the files.
    #[builder(skip)]
    capture: Option<RunCapture>,
}

nautilus_strategy!(XSectionalMomentum, {
    fn on_position_opened(&mut self, event: PositionOpened) {
        log::info!("new position debug {:#?}", event);
    }

    fn on_order_filled(&mut self, event: &OrderFilled) {
        if let Some(capture) = self.capture.as_mut() {
            capture.record_fill(event);
        }
    }
});

impl Debug for XSectionalMomentum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XSectionalMomentum")
            .field("config", &self.config)
            .field("core", &self.core)
            .field("instruments", &self.instruments)
            .finish()
    }
}

impl DataActor for XSectionalMomentum {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.instruments = config::instrument_ids(&self.config.bases);

        log::info!("run_id={}", self.config.run_id);
        log::info!("{:#?}", self);

        if self.config.holding_months != 1 {
            return Err(anyhow!(
                "holding_months={} is not supported: the rebalance path assumes a one-month hold. \
                 Revisit the age-based close in on_time_event before changing this.",
                self.config.holding_months
            ));
        }

        self.capture = Some(RunCapture::open(&self.config)?);

        self.clock().set_timer(
            "DAILY",
            Duration::from_hours(24),
            None,
            None,
            None,
            None,
            None,
        )?;

        let warmup = std::num::NonZeroUsize::new(self.config.lookback_months as usize)
            .ok_or_else(|| anyhow!("lookback_months must be > 0"))?;

        let instruments = self.instruments.clone();
        for instrument in instruments {
            let bar_type = bar_type(instrument, config::TIMEFRAME);
            log::info!("[{}] requesting {warmup} warmup bars", instrument);

            self.request_bars(bar_type, None, None, Some(warmup), None, None)?;
            self.subscribe_bars(bar_type, None, None);

            self.prices.insert(
                instrument,
                BoundedQueue::new(self.config.lookback_months.into()),
            );
        }

        anyhow::Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        let id = bar.instrument_id();

        log::debug!("bar {} @ {}", id, bar.ts_event);
        if let Some(symbol_returns) = self.prices.get_mut(&id) {
            symbol_returns.push_back_overwrite(bar.close.as_decimal());
        }
        anyhow::Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        log::info!("XSectionalMomentum stopped");
        let equity = self.usdt_equity();
        let latest_close = self.latest_closes();
        if let Some(capture) = self.capture.as_mut() {
            capture.finish(&latest_close, equity);
        }
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        let timestamp_integer = event.ts_event.as_u64() as i64;
        let current_month = DateTime::from_timestamp_nanos(timestamp_integer).month();
        if self.last_month == current_month {
            return anyhow::Ok(());
        }

        // Close the book, then reopen it below at fresh target notionals off the
        // current equity. The age gate keeps this general: with `holding_months`
        // pinned to 1 (enforced in `on_start`) every position is >27 days old at
        // the next rebalance, so the whole book turns over; a future multi-month
        // hold would keep younger tranches open here.
        let open_positions = self.cache().positions_open(None, None, None, None, None);
        for position in open_positions {
            let holding_ns = 27_u64 * self.config.holding_months as u64 * 86_400 * 1_000_000_000;
            let age_ns = event
                .ts_event
                .as_u64()
                .saturating_sub(position.ts_opened.as_u64());
            if age_ns > holding_ns {
                let _ = self.close_position(&position, None, None, None, None, None, None);
            }
        }

        log::info!("hello from on_time_event {}", event);
        for instrument in &self.instruments {
            let bar_type = bar_type(*instrument, config::TIMEFRAME);
            log::debug!("{} -> {:?}", instrument, self.cache().bar(&bar_type));
            log::debug!(
                "{} -> {:#?}",
                instrument,
                self.prices.get(instrument).unwrap().inner
            );

            let price_lookback_months = self.prices.get(instrument).unwrap().inner.front();
            let price_current = self
                .prices
                .get(instrument)
                .unwrap()
                .inner
                .get((self.config.lookback_months - 1).into());
            if price_current.is_none() {
                self.returns.remove(instrument);
                continue;
            }

            let returns_lookback = (price_current.unwrap() - price_lookback_months.unwrap())
                / price_lookback_months.unwrap();

            log::debug!("{} return lookback -> {}", instrument, returns_lookback);
            self.returns.insert(*instrument, returns_lookback);
        }

        let returns_clone = self.returns.clone();
        let mut sorted_returns: Vec<(&InstrumentId, &Decimal)> = returns_clone.iter().collect();
        sorted_returns.sort_by_key(|&(_, v)| v);

        let percentile_size = (Decimal::from_str(&self.config.percentile)
            .expect("percentile validated in build_config")
            * Decimal::from(sorted_returns.len()))
        .as_i128() as usize;

        let percentile_bottom = &sorted_returns[..percentile_size];
        let percentile_top = &sorted_returns[sorted_returns.len() - percentile_size..];

        log::info!("returns sorted {:#?}", sorted_returns);
        log::info!("returns bottom {:#?}", percentile_bottom);
        log::info!("returns top {:#?}", percentile_top);

        let account = self
            .cache()
            .account_for_venue(&Venue::new(config::VENUE))
            .unwrap();
        let balances = account.balances();

        log::info!("balance {:#?}", balances);

        let entry_month = YearMonth::from_nanos(event.ts_event.as_u64());
        let mut legs: Vec<(InstrumentId, OrderSide, Decimal, f64)> = Vec::new();

        // Gross budget for this rebalance, split between the two sides. Sizing is
        // off the equity the account reports *now*, so the book compounds.
        let equity = self.usdt_equity();
        let budget = self.config.risk_pct * equity.to_f64().unwrap_or(0.0);
        let (long_budget, short_budget) = sizing::split_sides(budget, self.config.long_w);

        let short_signals: Vec<(InstrumentId, f64)> = percentile_bottom
            .iter()
            .map(|(i, r)| (**i, r.to_f64().unwrap_or(0.0)))
            .collect();
        let long_signals: Vec<(InstrumentId, f64)> = percentile_top
            .iter()
            .map(|(i, r)| (**i, r.to_f64().unwrap_or(0.0)))
            .collect();

        let tilt = self.config.signal_tilt;
        let allocation = sizing::allocate(short_budget, &short_signals, tilt, Conviction::Low)
            .into_iter()
            .map(|(id, n)| (id, OrderSide::Sell, n))
            .chain(
                sizing::allocate(long_budget, &long_signals, tilt, Conviction::High)
                    .into_iter()
                    .map(|(id, n)| (id, OrderSide::Buy, n)),
            )
            .collect::<Vec<_>>();

        let net_notional: f64 = allocation
            .iter()
            .map(|&(_, side, n)| if side == OrderSide::Sell { -n } else { n })
            .sum();
        log::info!(
            "rebalance {}: equity={equity} budget={budget:.2} long={long_budget:.2} short={short_budget:.2} net_notional={net_notional:.2}",
            entry_month.label()
        );

        for (instrument, side, notional) in allocation {
            if !self.submit_notional_market(instrument, side, notional) {
                continue;
            }
            // Entry mark: the close of the last monthly bar before this
            // rebalance. Paired with the same instrument's close one rebalance
            // later, this is a clean close-to-close holding-period return.
            if let Some(entry_price) = self
                .prices
                .get(&instrument)
                .and_then(|q| q.inner.back().copied())
            {
                legs.push((instrument, side, entry_price, notional));
            }
        }

        let latest_close = self.latest_closes();
        if let Some(capture) = self.capture.as_mut() {
            capture.record_rebalance(entry_month, equity, legs);
            capture.finalise_completed(entry_month, &latest_close, equity);
        }

        self.last_month = current_month;
        anyhow::Ok(())
    }
}

impl XSectionalMomentum {
    /// Total USDT equity (cash + position mark-to-market) reported by the
    /// simulated venue, or zero if the account is not yet known.
    fn usdt_equity(&self) -> Decimal {
        self.portfolio()
            .equity(&Venue::new(config::VENUE), None)
            .iter()
            .find(|(currency, _)| currency.code.as_str() == "USDT")
            .map(|(_, money)| money.as_decimal())
            .unwrap_or(Decimal::ZERO)
    }

    /// The most recent close seen per instrument — the exit mark used to price
    /// out legs one rebalance after entry.
    fn latest_closes(&self) -> HashMap<InstrumentId, Decimal> {
        self.prices
            .iter()
            .filter_map(|(id, queue)| queue.inner.back().map(|close| (*id, *close)))
            .collect()
    }

    /// Submit a market order sized to `notional_usdt`. Returns `true` if the
    /// order was submitted, `false` if it was skipped (no instrument/bar, or
    /// the notional rounds below the minimum lot).
    fn submit_notional_market(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        notional_usdt: f64,
    ) -> bool {
        let Some(cached) = self.cache().instrument(&instrument_id) else {
            log::warn!("no instrument cached for {instrument_id}, skipping");
            return false;
        };
        let bar_type = bar_type(instrument_id, config::TIMEFRAME);
        let Some(bar) = self
            .cache()
            .bar_at_index(&bar_type, 1)
            .or_else(|| self.cache().bar(&bar_type))
        else {
            log::warn!("no bar cached for {instrument_id}, skipping");
            return false;
        };
        let close = bar.close.as_f64();
        if !close.is_finite() || close <= 0.0 {
            log::warn!("invalid close {close} for {instrument_id}, skipping");
            return false;
        }
        let precision = cached.size_precision();
        let units = notional_usdt / close;
        if !units.is_finite() || units <= 0.0 {
            log::warn!("computed quantity {units} for {instrument_id}, skipping");
            return false;
        }
        let min_lot = 10f64.powi(-(precision as i32));
        if units + f64::EPSILON < min_lot {
            log::warn!(
                "notional {notional_usdt} USDT rounds to 0 for {instrument_id} at precision {precision} (min lot ~{min_lot}); skipping"
            );
            return false;
        }
        let order = self.order().market(
            instrument_id,
            side,
            Quantity::new(units, precision),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let _ = self.submit_order(order.clone(), None, None, None);

        true
    }
}
