//! The momentum strategy: rank the universe by a composite fast/medium/slow
//! momentum score, hold the top `top_n` names long and the bottom `short_n`
//! short. Every `--number-holding-periods` of the `--holding-period` clock unit
//! it re-ranks and trades only the delta — closing names that left their slice,
//! opening the ones that just entered (a name that flipped from the top slice
//! to the bottom is closed and reopened on the other side), leaving the rest to
//! ride. With `--regime-filter` on it also flattens the whole book to cash
//! whenever BTC's trend regime is negative. Everything that is not the signal —
//! the rebalance clock, the price buffers, artifact capture, notional-sized
//! orders — comes from [`crate::strategy::common::StrategyRuntime`].

use std::{collections::HashMap, fmt::Debug};

use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_model::{
    data::Bar,
    enums::OrderSide,
    events::OrderFilled,
    identifiers::{InstrumentId, StrategyId},
};
use nautilus_trading::{StrategyConfig, StrategyCore, nautilus_strategy};
use rust_decimal::{Decimal, prelude::ToPrimitive};

use crate::{
    config::RunConfig,
    period::{CalendarPeriod, RebalancePeriod},
    strategy::common::{
        Market, RuntimeState, StrategyRuntime, btc_instrument_id, n_day_return,
        sizing::{self, Conviction},
    },
};

use super::config::{self, Config};

#[derive(bon::Builder)]
pub struct Momentum {
    #[builder(default = StrategyCore::new(StrategyConfig {
         strategy_id: Some(StrategyId::from("MOMENTUM-001")),
         order_id_tag: Some("001".to_string()),
         ..Default::default()
    }))]
    core: StrategyCore,

    /// Shared run configuration: universe, dates, starting balance, uuid.
    run: RunConfig,

    /// This strategy's resolved knobs.
    config: Config,

    /// Signal-agnostic backtest state: universe ids, rebalance clock, rolling
    /// price buffers, capture handle. Filled in `on_start`.
    #[builder(skip)]
    runtime: RuntimeState<CalendarPeriod>,

    /// `BTC`'s instrument id, resolved once in `on_start` — only when
    /// `--regime-filter` is on.
    #[builder(skip)]
    btc_instrument: Option<InstrumentId>,

    /// The names the strategy currently intends to hold and on which side — its
    /// own book of record, kept in lock-step with `RunCapture`'s. It drives the
    /// close/open diff at each turnover, rather than `cache().positions_open()`
    /// (whose fills can lag a turnover and drop a name from the diff).
    #[builder(skip)]
    book: HashMap<InstrumentId, OrderSide>,
}

nautilus_strategy!(Momentum, {
    fn on_order_filled(&mut self, event: &OrderFilled) {
        self.record_fill(event);
    }
});

impl StrategyRuntime for Momentum {
    type Period = CalendarPeriod;

    fn runtime(&self) -> &RuntimeState<CalendarPeriod> {
        &self.runtime
    }
    fn runtime_mut(&mut self) -> &mut RuntimeState<CalendarPeriod> {
        &mut self.runtime
    }
    fn market(&self) -> Market {
        config::MARKET
    }
    fn current_period(&self, ts_nanos: u64) -> CalendarPeriod {
        self.config.holding_period.period_at(ts_nanos)
    }
}

impl Debug for Momentum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Momentum")
            .field("run", &self.run)
            .field("config", &self.config)
            .field("core", &self.core)
            .field("instruments", &self.runtime.instruments)
            .field("book", &self.book)
            .finish()
    }
}

impl DataActor for Momentum {
    fn on_start(&mut self) -> anyhow::Result<()> {
        log::info!("run_id={}", self.run.run_id);

        let run = self.run.clone();
        let rows = config::config_rows(&self.config);
        self.open_capture(&run, &rows)?;

        if self.config.regime_filter {
            self.btc_instrument = Some(btc_instrument_id(&self.run.bases, config::VENUE));
        }

        let instruments = config::instrument_ids(&self.run.bases);
        let window = self
            .config
            .fast_days
            .max(self.config.medium_days)
            .max(self.config.slow_days)
            .max(self.config.regime_lookback_days) as usize
            + 1;
        self.start_universe(instruments, window)?;

        log::info!("{:#?}", self);
        anyhow::Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        log::debug!("bar {} @ {}", bar.instrument_id(), bar.ts_event);
        self.runtime_mut().record_close(bar);
        anyhow::Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        log::info!("Momentum stopped");
        self.finish_book_capture();
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        let Some(period) = self.period_rolled(event) else {
            return anyhow::Ok(());
        };

        let equity = self.usdt_equity();

        // --- regime filter (opt-in) ---
        // Runs every period, whatever the rebalance cadence: the book flattens
        // to cash — longs *and* shorts — the moment BTC's trend turns negative.
        if self.config.regime_filter {
            let btc = self
                .btc_instrument
                .expect("set in on_start when regime_filter is on");
            let btc_return =
                self.runtime().prices.get(&btc).and_then(|queue| {
                    n_day_return(queue, self.config.regime_lookback_days as usize)
                });
            let regime_is_positive = btc_return.is_some_and(|r| r >= Decimal::ZERO);

            if !regime_is_positive {
                let closed: Vec<InstrumentId> = self.book.drain().map(|(id, _)| id).collect();
                self.close_all();
                let marks = self.runtime().latest_closes();
                if let Some(capture) = self.runtime_mut().capture.as_mut() {
                    capture.record_book_turnover(period, equity, &marks, &[], &closed);
                }
                self.mark_rebalanced(period);
                return anyhow::Ok(());
            }
        }

        // Re-rank and turn the book over only once per `number_holding_periods`
        // clock units. Between turnovers the book rides untouched.
        if let Some(last) = self.runtime().last_period {
            let mut due = last;
            for _ in 0..self.config.number_holding_periods {
                due = due.next();
            }
            if period < due {
                return anyhow::Ok(());
            }
        }

        // --- signal: composite fast/medium/slow momentum score, per name ---
        let instruments = self.runtime().instruments.clone();
        let mut scores: Vec<(InstrumentId, f64)> = Vec::with_capacity(instruments.len());
        for instrument in &instruments {
            let Some(queue) = self.runtime().prices.get(instrument) else {
                continue;
            };
            let fast = n_day_return(queue, self.config.fast_days as usize);
            let medium = n_day_return(queue, self.config.medium_days as usize);
            let slow = n_day_return(queue, self.config.slow_days as usize);
            if let (Some(fast), Some(medium), Some(slow)) = (fast, medium, slow) {
                let score = self.config.fast_weight * fast.to_f64().unwrap_or(0.0)
                    + self.config.medium_weight * medium.to_f64().unwrap_or(0.0)
                    + self.config.slow_weight * slow.to_f64().unwrap_or(0.0);
                scores.push((*instrument, score));
            }
        }

        // --- rank: best score first ---
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Long the top `top_n`, short the bottom `short_n`. `config::build`
        // gates `top_n + short_n` against the full universe; clamp here too so a
        // still-warming-up scored set (fewer names) can never make the slices
        // overlap.
        let top_n = self.config.top_n.min(scores.len());
        let short_n = self.config.short_n.min(scores.len().saturating_sub(top_n));

        let long_signals: Vec<(InstrumentId, f64)> = scores[..top_n].to_vec();
        let short_signals: Vec<(InstrumentId, f64)> = scores[scores.len() - short_n..].to_vec();

        log::info!("rebalance: long {long_signals:#?} short {short_signals:#?}");

        // Target book: the side we want each name on this turnover.
        let mut target: HashMap<InstrumentId, OrderSide> = HashMap::new();
        for (id, _) in &long_signals {
            target.insert(*id, OrderSide::Buy);
        }
        for (id, _) in &short_signals {
            target.insert(*id, OrderSide::Sell);
        }

        // Trade only the delta against the book of record. A held name is
        // "dropped" when the target no longer wants it *on the side it is held*
        // — which also covers a flip (top slice -> bottom slice): it is closed
        // here and reopened on the other side below.
        let dropped: Vec<InstrumentId> = self
            .book
            .iter()
            .filter(|(id, side)| target.get(id) != Some(side))
            .map(|(id, _)| *id)
            .collect();
        self.close_positions(&dropped);

        // Size a fresh full book off current equity. With no short side the long
        // book takes the whole budget regardless of `long_w`.
        let budget = self.config.risk_fraction * equity.to_f64().unwrap_or(0.0);
        let (long_budget, short_budget) = if short_n == 0 {
            (budget, 0.0)
        } else {
            sizing::split_sides(budget, self.config.long_w)
        };
        let tilt = self.config.allocation_tilt;
        let allocation = sizing::allocate(long_budget, &long_signals, tilt, Conviction::High)
            .into_iter()
            .map(|(id, n)| (id, OrderSide::Buy, n))
            .chain(
                sizing::allocate(short_budget, &short_signals, tilt, Conviction::Low)
                    .into_iter()
                    .map(|(id, n)| (id, OrderSide::Sell, n)),
            )
            .collect::<Vec<_>>();

        let net_notional: f64 = allocation
            .iter()
            .map(|&(_, side, n)| if side == OrderSide::Sell { -n } else { n })
            .sum();
        log::info!(
            "rebalance {}: equity={equity} budget={budget:.2} long={long_budget:.2} short={short_budget:.2} net_notional={net_notional:.2}",
            period.label()
        );

        // Submit orders only for names that just joined (or flipped side) — a
        // same-side survivor keeps the size it was opened at.
        let mut opened: Vec<(InstrumentId, OrderSide, Decimal, f64)> = Vec::new();
        for (instrument, side, notional) in allocation {
            if self.book.get(&instrument) == Some(&side) {
                continue; // same-side survivor — rides untouched
            }
            if !self.submit_notional_market(instrument, side, notional) {
                continue;
            }
            // Entry mark: the close of the last daily bar before this turnover.
            // Marked forward at each subsequent turnover, the per-period moves
            // telescope to a clean close-to-close hold return when the leg
            // finally closes.
            if let Some(entry_price) = self
                .runtime()
                .prices
                .get(&instrument)
                .and_then(|q| q.inner.back().copied())
            {
                opened.push((instrument, side, entry_price, notional));
            }
        }

        // Keep the book of record — and `RunCapture`'s — in lock-step with what
        // was actually traded.
        for instrument in &dropped {
            self.book.remove(instrument);
        }
        for (instrument, side, ..) in &opened {
            self.book.insert(*instrument, *side);
        }

        let marks = self.runtime().latest_closes();
        if let Some(capture) = self.runtime_mut().capture.as_mut() {
            capture.record_book_turnover(period, equity, &marks, &opened, &dropped);
        }

        self.mark_rebalanced(period);
        anyhow::Ok(())
    }
}
