//! Signal-agnostic backtest wiring shared by every strategy in
//! [`crate::strategy`].
//!
//! A concrete strategy embeds a [`HarnessState`] (conventionally the field
//! `harness`) and writes a three-method `impl` of [`Harness`]; that unlocks the
//! rest — the rebalance clock, warm-up requests, the rolling price buffers,
//! artifact capture and notional-sized market orders — as provided methods.
//! What stays in the strategy's own `strategy.rs` is the signal: how it ranks
//! the universe and how it splits the budget across legs.

use std::{collections::HashMap, fmt::Debug, time::Duration};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike};
use nautilus_common::actor::DataActorNative;
use nautilus_common::timer::TimeEvent;
use nautilus_model::{
    data::Bar,
    enums::{BarAggregation, OrderSide},
    identifiers::{InstrumentId, Venue},
    instruments::Instrument,
    events::OrderFilled,
    types::Quantity,
};
use nautilus_trading::{Strategy, StrategyNative};
use rust_decimal::Decimal;

use crate::{
    capture::RunCapture,
    config::RunConfig,
    data::{get_bar_type, structure::BoundedQueue},
};

/// Per-run state every strategy carries: the resolved universe, the rebalance
/// clock marker, the rolling close-price buffers and the artifact-capture
/// handle. `Default`-constructed by the builder; populated in `on_start`.
#[derive(Default)]
pub struct HarnessState {
    /// Instrument ids for the run's universe, resolved in `on_start`.
    pub instruments: Vec<InstrumentId>,
    /// Calendar month of the last rebalance; the `on_time_event` guard.
    pub last_month: u32,
    /// Rolling close-price buffer per instrument (depth = the formation window).
    pub prices: HashMap<InstrumentId, BoundedQueue<Decimal>>,
    /// Per-run artifact capture (`runs/<uuid>/{legs,portfolio,fills}.csv`).
    /// `None` until `on_start` opens the files.
    pub capture: Option<RunCapture>,
}

impl HarnessState {
    /// The most recent close seen per instrument — the exit mark used to price
    /// out legs one rebalance after entry.
    pub fn latest_closes(&self) -> HashMap<InstrumentId, Decimal> {
        self.prices
            .iter()
            .filter_map(|(id, queue)| queue.inner.back().map(|close| (*id, *close)))
            .collect()
    }

    /// Fold a bar's close into its rolling buffer (no-op for an instrument
    /// outside the universe).
    pub fn record_close(&mut self, bar: &Bar) {
        if let Some(buffer) = self.prices.get_mut(&bar.instrument_id()) {
            buffer.push_back_overwrite(bar.close.as_decimal());
        }
    }
}

/// The market a strategy trades and the bar size it ranks on. Each strategy's
/// `config.rs` owns these constants; the harness needs them to build bar types
/// and to look up the venue account.
#[derive(Clone, Copy, Debug)]
pub struct Market {
    pub venue: &'static str,
    pub timeframe: BarAggregation,
}

impl Market {
    fn venue_id(&self) -> Venue {
        Venue::new(self.venue)
    }
}

/// The calendar month (`1..=12`) a time event falls in, UTC.
pub fn month_of(event: &TimeEvent) -> u32 {
    DateTime::from_timestamp_nanos(event.ts_event.as_u64() as i64).month()
}

/// Backtest plumbing shared by every strategy, unlocked once a strategy exposes
/// its [`HarnessState`] and [`Market`].
///
/// Implement the three accessors; everything else is provided. The trait bounds
/// are exactly what [`nautilus_backtest`](nautilus_backtest)'s `add_strategy`
/// asks for, so any type that can be added to the engine can implement this.
pub trait Harness: Strategy + StrategyNative + DataActorNative + Debug + Sized + 'static {
    fn harness(&self) -> &HarnessState;
    fn harness_mut(&mut self) -> &mut HarnessState;
    fn market(&self) -> Market;

    /// Open the run's capture files: the shared config rows plus whatever rows
    /// this strategy contributes. Call once from `on_start`.
    fn open_capture(&mut self, run: &RunConfig, strategy_rows: &[(String, String)]) -> Result<()> {
        self.harness_mut().capture = Some(RunCapture::open(run, strategy_rows)?);
        Ok(())
    }

    /// Install the daily timer that drives `on_time_event`, then for every
    /// instrument in `instruments`: request `window` bars of warm-up history,
    /// subscribe to its bars, and allocate its `window`-deep price buffer.
    fn start_universe(&mut self, instruments: Vec<InstrumentId>, window: usize) -> Result<()> {
        let warmup = std::num::NonZeroUsize::new(window)
            .ok_or_else(|| anyhow!("formation window must be > 0"))?;
        let timeframe = self.market().timeframe;

        self.clock()
            .set_timer("DAILY", Duration::from_hours(24), None, None, None, None, None)?;

        for instrument in &instruments {
            let bar_type = get_bar_type(*instrument, timeframe);
            log::info!("[{instrument}] requesting {warmup} warm-up bars");
            self.request_bars(bar_type, None, None, Some(warmup), None, None)?;
            self.subscribe_bars(bar_type, None, None);
            self.harness_mut()
                .prices
                .insert(*instrument, BoundedQueue::new(window));
        }

        self.harness_mut().instruments = instruments;
        Ok(())
    }

    /// The calendar month of `event` if it differs from the last rebalance,
    /// else `None`. On a roll the caller does its work and then calls
    /// [`mark_rebalanced`](Self::mark_rebalanced).
    fn month_rolled(&self, event: &TimeEvent) -> Option<u32> {
        let month = month_of(event);
        (self.harness().last_month != month).then_some(month)
    }

    /// Record that a rebalance for `month` has completed.
    fn mark_rebalanced(&mut self, month: u32) {
        self.harness_mut().last_month = month;
    }

    /// Close every open position older than `holding_months`. With the hold
    /// pinned to one month this turns the whole book over each rebalance; a
    /// longer hold would keep younger tranches open.
    fn close_expired(&mut self, event: &TimeEvent, holding_months: u16) {
        let holding_ns = 27_u64 * holding_months as u64 * 86_400 * 1_000_000_000;
        let now = event.ts_event.as_u64();
        let open = self.cache().positions_open(None, None, None, None, None);
        for position in open {
            let age_ns = now.saturating_sub(position.ts_opened.as_u64());
            if age_ns > holding_ns {
                let _ = self.close_position(&position, None, None, None, None, None, None);
            }
        }
    }

    /// Total USDT equity (cash + position mark-to-market) reported by the
    /// venue account, or zero if the account is not yet known.
    fn usdt_equity(&self) -> Decimal {
        self.portfolio()
            .equity(&self.market().venue_id(), None)
            .iter()
            .find(|(currency, _)| currency.code.as_str() == "USDT")
            .map(|(_, money)| money.as_decimal())
            .unwrap_or(Decimal::ZERO)
    }

    /// Submit a market order sized to `notional_usdt`. Returns `true` if the
    /// order was submitted, `false` if it was skipped (no instrument/bar, a
    /// non-finite price, or a notional that rounds below the minimum lot).
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
        let bar_type = get_bar_type(instrument_id, self.market().timeframe);
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

    /// Forward an `OrderFilled` to the capture layer.
    fn record_fill(&mut self, event: &OrderFilled) {
        if let Some(capture) = self.harness_mut().capture.as_mut() {
            capture.record_fill(event);
        }
    }

    /// Finalise capture at `on_stop`: price out every leg still open against the
    /// latest close and flush.
    fn finish_capture(&mut self) {
        let equity = self.usdt_equity();
        let latest_close = self.harness().latest_closes();
        if let Some(capture) = self.harness_mut().capture.as_mut() {
            capture.finish(&latest_close, equity);
        }
    }
}
