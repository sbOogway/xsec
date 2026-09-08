//! The top-5 momentum strategy: rank the universe by a composite fast/medium/
//! slow momentum score and hold the top names. Every
//! `--number-holding-periods` of the `--holding-period` clock unit it re-ranks
//! and trades only the delta — closing names that fell out of the top `top_n`,
//! opening the ones that just entered, leaving the rest to ride — and it
//! flattens to cash whenever BTC's trend regime filter is negative. Everything
//! that is not the signal — the rebalance clock, the price buffers, artifact
//! capture, notional-sized orders — comes from
//! [`crate::strategy::common::StrategyRuntime`].

use std::{collections::HashSet, fmt::Debug};

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
    sizing::{self, Conviction},
    strategy::common::{Market, RuntimeState, StrategyRuntime, btc_instrument_id, n_day_return},
};

use super::config::{self, Config};

#[derive(bon::Builder)]
pub struct Top5MomentumFiltered {
    #[builder(default = StrategyCore::new(StrategyConfig {
         strategy_id: Some(StrategyId::from("TOP5-MOM-FILTERED")),
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

    /// `BTC`'s instrument id, resolved once in `on_start` for the regime
    /// filter.
    #[builder(skip)]
    btc_instrument: Option<InstrumentId>,

    /// The names the strategy currently intends to hold — its own book of
    /// record, kept in lock-step with `RunCapture`'s. It drives the
    /// close/open diff at each turnover, rather than `cache().positions_open()`
    /// (whose fills can lag a turnover and drop a name from the diff).
    #[builder(skip)]
    book: HashSet<InstrumentId>,
}

nautilus_strategy!(Top5MomentumFiltered, {
    fn on_order_filled(&mut self, event: &OrderFilled) {
        self.record_fill(event);
    }
});

impl StrategyRuntime for Top5MomentumFiltered {
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

impl Debug for Top5MomentumFiltered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Top5MomentumFiltered")
            .field("run", &self.run)
            .field("config", &self.config)
            .field("core", &self.core)
            .field("instruments", &self.runtime.instruments)
            .field("book", &self.book)
            .finish()
    }
}

impl DataActor for Top5MomentumFiltered {
    fn on_start(&mut self) -> anyhow::Result<()> {
        let run = self.run.clone();
        let rows = config::config_rows(&self.config);
        self.open_capture(&run, &rows)?;

        self.btc_instrument = Some(btc_instrument_id(&self.run.bases, config::VENUE));

        let instruments = config::instrument_ids(&self.run.bases);
        let window = self
            .config
            .fast_days
            .max(self.config.medium_days)
            .max(self.config.slow_days)
            .max(self.config.regime_lookback_days) as usize
            + 1;
        self.start_universe(instruments, window)?;
        anyhow::Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        log::debug!("bar {} @ {}", bar.instrument_id(), bar.ts_event);
        self.runtime_mut().record_close(bar);
        anyhow::Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.finish_book_capture();
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        let Some(period) = self.period_rolled(event) else {
            return anyhow::Ok(());
        };

        let equity = self.usdt_equity();

        let btc = self.btc_instrument.expect("set in on_start");
        let btc_return = self
            .runtime()
            .prices
            .get(&btc)
            .and_then(|queue| n_day_return(queue, self.config.regime_lookback_days as usize));
        let regime_is_positive = btc_return.is_some_and(|r| r >= Decimal::ZERO);

        // The regime filter runs every period, whatever the rebalance cadence:
        // the book flattens to cash the moment BTC's trend turns negative.
        if !regime_is_positive {
            let closed: Vec<InstrumentId> = self.book.drain().collect();
            self.close_all();
            let marks = self.runtime().latest_closes();
            if let Some(capture) = self.runtime_mut().capture.as_mut() {
                capture.record_book_turnover(period, equity, &marks, &[], &closed);
            }
            self.mark_rebalanced(period);
            return anyhow::Ok(());
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

        // --- rank and take the top `top_n` ---
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(self.config.top_n);

        log::info!("rebalance scores {scores:#?}");

        // Trade only the delta against the book of record: close the names that
        // fell out of the top `top_n`, open the ones that just entered, leave
        // the rest to ride.
        let target: HashSet<InstrumentId> = scores.iter().map(|(id, _)| *id).collect();
        let dropped: Vec<InstrumentId> = self.book.difference(&target).copied().collect();
        self.close_positions(&dropped);

        // Size a fresh full book off current equity, but submit orders only for
        // the names that just joined — a survivor keeps the size it was opened
        // at.
        let budget = self.config.risk_fraction * equity.to_f64().unwrap_or(0.0);
        let allocation = sizing::allocate(
            budget,
            &scores,
            self.config.allocation_tilt,
            Conviction::High,
        );

        let mut opened: Vec<(InstrumentId, OrderSide, Decimal, f64)> = Vec::new();
        for (instrument, notional) in allocation {
            if self.book.contains(&instrument) {
                continue; // survivor — rides untouched
            }
            if !self.submit_notional_market(instrument, OrderSide::Buy, notional) {
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
                opened.push((instrument, OrderSide::Buy, entry_price, notional));
            }
        }

        // Keep the book of record — and `RunCapture`'s — in lock-step with what
        // was actually traded.
        for instrument in &dropped {
            self.book.remove(instrument);
        }
        for (instrument, ..) in &opened {
            self.book.insert(*instrument);
        }

        let marks = self.runtime().latest_closes();
        if let Some(capture) = self.runtime_mut().capture.as_mut() {
            capture.record_book_turnover(period, equity, &marks, &opened, &dropped);
        }

        self.mark_rebalanced(period);
        anyhow::Ok(())
    }
}
