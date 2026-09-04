//! The cross-sectional momentum strategy: rank the universe by trailing return,
//! go long the top `percentile` and short the bottom `percentile`, hold a
//! month, repeat. Everything that is not the signal — the rebalance clock, the
//! price buffers, artifact capture, notional-sized orders — comes from
//! [`crate::strategy::common::Harness`].

use std::{collections::HashMap, fmt::Debug, str::FromStr};

use anyhow::anyhow;
use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_model::{
    data::Bar,
    enums::OrderSide,
    events::{OrderFilled, PositionOpened},
    identifiers::{InstrumentId, StrategyId, Venue},
};
use nautilus_trading::{StrategyConfig, StrategyCore, nautilus_strategy};
use rust_decimal::{Decimal, prelude::ToPrimitive};

use crate::{
    capture::YearMonth,
    config::RunConfig,
    sizing::{self, Conviction},
    strategy::common::{Harness, HarnessState, Market},
};

use super::config::{self, Config};

#[derive(bon::Builder)]
pub struct XSectionalMomentum {
    #[builder(default = StrategyCore::new(StrategyConfig {
         strategy_id: Some(StrategyId::from("X-SEC-MOM")),
         order_id_tag: Some("001".to_string()),
         ..Default::default()
    }))]
    core: StrategyCore,

    /// Shared run configuration: universe, dates, starting balance, uuid.
    run: RunConfig,

    /// This strategy's resolved knobs (lookback, percentile, risk, tilt).
    config: Config,

    /// Signal-agnostic backtest state: universe ids, rebalance clock, rolling
    /// price buffers, capture handle. Filled in `on_start`.
    #[builder(skip)]
    harness: HarnessState,

    /// Trailing return per instrument, recomputed each rebalance.
    #[builder(default)]
    returns: HashMap<InstrumentId, Decimal>,
}

nautilus_strategy!(XSectionalMomentum, {
    fn on_position_opened(&mut self, event: PositionOpened) {
        log::info!("new position debug {:#?}", event);
    }

    fn on_order_filled(&mut self, event: &OrderFilled) {
        self.record_fill(event);
    }
});

impl Harness for XSectionalMomentum {
    fn harness(&self) -> &HarnessState {
        &self.harness
    }
    fn harness_mut(&mut self) -> &mut HarnessState {
        &mut self.harness
    }
    fn market(&self) -> Market {
        config::MARKET
    }
}

impl Debug for XSectionalMomentum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XSectionalMomentum")
            .field("run", &self.run)
            .field("config", &self.config)
            .field("core", &self.core)
            .field("instruments", &self.harness.instruments)
            .finish()
    }
}

impl DataActor for XSectionalMomentum {
    fn on_start(&mut self) -> anyhow::Result<()> {
        log::info!("run_id={}", self.run.run_id);

        // Defense in depth: `config::build` already rejects this, but the
        // rebalance path below assumes a one-month hold.
        if self.config.holding_months != 1 {
            return Err(anyhow!(
                "holding_months={} is not supported: the rebalance path assumes a one-month hold. \
                 Revisit the age-based close in on_time_event before changing this.",
                self.config.holding_months
            ));
        }

        let run = self.run.clone();
        let rows = config::config_rows(&self.config);
        self.open_capture(&run, &rows)?;

        let instruments = config::instrument_ids(&self.run.bases);
        let window = self.config.lookback_months as usize;
        self.start_universe(instruments, window)?;

        log::info!("{:#?}", self);
        anyhow::Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        log::debug!("bar {} @ {}", bar.instrument_id(), bar.ts_event);
        self.harness_mut().record_close(bar);
        anyhow::Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        log::info!("XSectionalMomentum stopped");
        self.finish_capture();
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        let Some(current_month) = self.month_rolled(event) else {
            return anyhow::Ok(());
        };

        let holding_months = self.config.holding_months;
        self.close_expired(event, holding_months);

        // --- signal: trailing return over the formation window, per name ---
        let lookback = self.config.lookback_months;
        let instruments = self.harness().instruments.clone();
        let mut computed: Vec<(InstrumentId, Option<Decimal>)> =
            Vec::with_capacity(instruments.len());
        for instrument in &instruments {
            let queue = self.harness().prices.get(instrument);
            let past = queue.and_then(|q| q.inner.front().copied());
            let now = queue.and_then(|q| q.inner.get((lookback - 1) as usize).copied());
            let ret = match (now, past) {
                (Some(now), Some(past)) => Some((now - past) / past),
                _ => None,
            };
            log::debug!("{instrument} return lookback -> {ret:?}");
            computed.push((*instrument, ret));
        }
        for (instrument, ret) in computed {
            match ret {
                Some(ret) => {
                    self.returns.insert(instrument, ret);
                }
                None => {
                    self.returns.remove(&instrument);
                }
            }
        }

        // --- rank and cut the top / bottom `percentile` ---
        let returns_clone = self.returns.clone();
        let mut sorted_returns: Vec<(&InstrumentId, &Decimal)> = returns_clone.iter().collect();
        sorted_returns.sort_by_key(|&(_, v)| v);

        let percentile_size = (Decimal::from_str(&self.config.percentile)
            .expect("percentile validated in config::build")
            * Decimal::from(sorted_returns.len()))
        .as_i128() as usize;

        let percentile_bottom = &sorted_returns[..percentile_size];
        let percentile_top = &sorted_returns[sorted_returns.len() - percentile_size..];

        log::info!("returns sorted {sorted_returns:#?}");
        log::info!("returns bottom {percentile_bottom:#?}");
        log::info!("returns top {percentile_top:#?}");

        {
            let account = self
                .cache()
                .account_for_venue(&Venue::new(config::VENUE))
                .unwrap();
            log::info!("balance {:#?}", account.balances());
        }

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
                .harness()
                .prices
                .get(&instrument)
                .and_then(|q| q.inner.back().copied())
            {
                legs.push((instrument, side, entry_price, notional));
            }
        }

        let latest_close = self.harness().latest_closes();
        if let Some(capture) = self.harness_mut().capture.as_mut() {
            capture.record_rebalance(entry_month, equity, legs);
            capture.finalise_completed(entry_month, &latest_close, equity);
        }

        self.mark_rebalanced(current_month);
        anyhow::Ok(())
    }
}
