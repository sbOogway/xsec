//! Signal-agnostic backtest wiring shared by every strategy in
//! [`crate::strategy`], plus free-standing helpers used by more than one of
//! them.
//!
//! A concrete strategy embeds a [`RuntimeState`] (conventionally the field
//! `runtime`) and writes a three-method `impl` of [`StrategyRuntime`]; that
//! unlocks the rest — the rebalance clock, warm-up requests, the rolling price
//! buffers, artifact capture and notional-sized market orders — as provided
//! methods. What stays in the strategy's own `strategy.rs` is the signal: how
//! it ranks the universe and how it splits the budget across legs.
//!
//! The rebalance clock and the capture join key both derive from the same
//! [`crate::period::RebalancePeriod`] value (`StrategyRuntime::Period`,
//! produced by `StrategyRuntime::current_period`) — a strategy declares its
//! cadence once, rather than keying its clock guard and its
//! `legs.csv`/`portfolio.csv` rows off two independent definitions of "what
//! period is this."

use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    time::Duration,
};

use anyhow::{Result, anyhow};
use nautilus_common::actor::DataActorNative;
use nautilus_common::timer::TimeEvent;
use nautilus_model::{
    data::Bar,
    enums::{BarAggregation, OrderSide},
    events::OrderFilled,
    identifiers::{InstrumentId, Venue},
    instruments::Instrument,
    types::Quantity,
};
use nautilus_trading::{Strategy, StrategyNative};
use rust_decimal::Decimal;

use crate::{
    capture::RunCapture,
    config::RunConfig,
    data::{get_bar_type, structure::BoundedQueue},
    period::RebalancePeriod,
};

/// Per-run state every strategy carries: the resolved universe, the rebalance
/// clock marker, the rolling close-price buffers and the artifact-capture
/// handle. Populated in `on_start`.
///
/// Generic over the strategy's own rebalance period type `P` (a calendar
/// month, a calendar day, an ISO week, ...) — see
/// [`crate::period::RebalancePeriod`].
pub struct RuntimeState<P: RebalancePeriod> {
    /// Instrument ids for the run's universe, resolved in `on_start`.
    pub instruments: Vec<InstrumentId>,
    /// The period of the last rebalance; the `on_time_event` guard.
    pub last_period: Option<P>,
    /// Rolling close-price buffer per instrument (depth = the formation window).
    pub prices: HashMap<InstrumentId, BoundedQueue<Decimal>>,
    /// Per-run artifact capture (`runs/<uuid>/{legs,portfolio,fills}.csv`).
    /// `None` until `on_start` opens the files.
    pub capture: Option<RunCapture<P>>,
}

// Not `#[derive(Default)]`: the derive macro would add an unwanted `P:
// Default` bound (none of these fields need one — `Option<P>` is `None`
// regardless of what `P` is).
impl<P: RebalancePeriod> Default for RuntimeState<P> {
    fn default() -> Self {
        Self {
            instruments: Vec::new(),
            last_period: None,
            prices: HashMap::new(),
            capture: None,
        }
    }
}

impl<P: RebalancePeriod> RuntimeState<P> {
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
/// `config.rs` owns these constants; the runtime needs them to build bar types
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

/// Backtest plumbing shared by every strategy, unlocked once a strategy exposes
/// its [`RuntimeState`], its [`Market`] and its rebalance [`RebalancePeriod`].
///
/// Implement the `Period` associated type and the three accessors; everything
/// else is provided. The trait bounds are exactly what
/// [`nautilus_backtest`](nautilus_backtest)'s `add_strategy` asks for, so any
/// type that can be added to the engine can implement this.
pub trait StrategyRuntime:
    Strategy + StrategyNative + DataActorNative + Debug + Sized + 'static
{
    /// This strategy's rebalance cadence (a calendar month, a calendar day, an
    /// ISO week, ...).
    /// Also the join key `RunCapture` groups `legs.csv` / `portfolio.csv` rows
    /// by — one declaration drives both the clock and the capture schema.
    type Period: RebalancePeriod;

    fn runtime(&self) -> &RuntimeState<Self::Period>;
    fn runtime_mut(&mut self) -> &mut RuntimeState<Self::Period>;
    fn market(&self) -> Market;

    /// The [`Self::Period`] a clock tick at `ts_nanos` falls in. A fixed-cadence
    /// strategy returns `SomeConcretePeriod::from_nanos(ts_nanos)`; one with a
    /// run-time `--holding-period` flag dispatches on it. Drives both
    /// [`period_rolled`](Self::period_rolled) and the fill-fee accrual in
    /// [`record_fill`](Self::record_fill).
    fn current_period(&self, ts_nanos: u64) -> Self::Period;

    /// Open the run's capture files: the shared config rows plus whatever rows
    /// this strategy contributes. Call once from `on_start`.
    fn open_capture(&mut self, run: &RunConfig, strategy_rows: &[(String, String)]) -> Result<()> {
        self.runtime_mut().capture = Some(RunCapture::open(run, strategy_rows)?);
        Ok(())
    }

    /// Install the daily timer that drives `on_time_event`, then for every
    /// instrument in `instruments`: request `window` bars of warm-up history,
    /// subscribe to its bars, and allocate its `window`-deep price buffer.
    fn start_universe(&mut self, instruments: Vec<InstrumentId>, window: usize) -> Result<()> {
        let warmup = std::num::NonZeroUsize::new(window)
            .ok_or_else(|| anyhow!("formation window must be > 0"))?;
        let timeframe = self.market().timeframe;

        self.clock().set_timer(
            "DAILY",
            Duration::from_hours(24),
            None,
            None,
            None,
            None,
            None,
        )?;

        for instrument in &instruments {
            let bar_type = get_bar_type(*instrument, timeframe);
            log::info!("[{instrument}] requesting {warmup} warm-up bars");
            self.request_bars(bar_type, None, None, Some(warmup), None, None)?;
            self.subscribe_bars(bar_type, None, None);
            self.runtime_mut()
                .prices
                .insert(*instrument, BoundedQueue::new(window));
        }

        self.runtime_mut().instruments = instruments;
        Ok(())
    }

    /// The rebalance period `event` falls in, if it differs from the last
    /// rebalance, else `None`. On a roll the caller does its work and then
    /// calls [`mark_rebalanced`](Self::mark_rebalanced) with the same period.
    fn period_rolled(&self, event: &TimeEvent) -> Option<Self::Period> {
        let period = self.current_period(event.ts_event.as_u64());
        (self.runtime().last_period != Some(period)).then_some(period)
    }

    /// Record that a rebalance for `period` has completed.
    fn mark_rebalanced(&mut self, period: Self::Period) {
        self.runtime_mut().last_period = Some(period);
    }

    /// Close every open position older than `holding_days`. With the hold
    /// pinned to one rebalance period this turns the whole book over each
    /// rebalance; a longer hold would keep younger tranches open.
    fn close_expired(&mut self, event: &TimeEvent, holding_days: u64) {
        let holding_ns = holding_days * 86_400 * 1_000_000_000;
        let now = event.ts_event.as_u64();
        let open = self.cache().positions_open(None, None, None, None, None);
        for position in open {
            let age_ns = now.saturating_sub(position.ts_opened.as_u64());
            if age_ns > holding_ns {
                let _ = self.close_position(&position, None, None, None, None, None, None);
            }
        }
    }

    /// Close every open position unconditionally, regardless of age — for a
    /// strategy that needs to flatten to cash outright (e.g. a regime filter
    /// going red) rather than let positions age out.
    fn close_all(&mut self) {
        let open = self.cache().positions_open(None, None, None, None, None);
        for position in open {
            let _ = self.close_position(&position, None, None, None, None, None, None);
        }
    }

    /// Close every open position whose instrument is in `instruments` — for a
    /// carried-book strategy that turns over only the names that dropped out of
    /// its target set, leaving the rest to ride.
    fn close_positions(&mut self, instruments: &[InstrumentId]) {
        if instruments.is_empty() {
            return;
        }
        let wanted: HashSet<InstrumentId> = instruments.iter().copied().collect();
        let open = self.cache().positions_open(None, None, None, None, None);
        for position in open {
            if wanted.contains(&position.instrument_id) {
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

    /// Forward an `OrderFilled` to the capture layer, tagged with the rebalance
    /// period its timestamp falls in.
    fn record_fill(&mut self, event: &OrderFilled) {
        let period = self.current_period(event.ts_event.as_u64());
        if let Some(capture) = self.runtime_mut().capture.as_mut() {
            capture.record_fill(period, event);
        }
    }

    /// Finalise capture at `on_stop` for the full-turnover flow: price out every
    /// leg still open against the latest close and flush.
    fn finish_capture(&mut self) {
        let equity = self.usdt_equity();
        let latest_close = self.runtime().latest_closes();
        if let Some(capture) = self.runtime_mut().capture.as_mut() {
            capture.finish(&latest_close, equity);
        }
    }

    /// Finalise capture at `on_stop` for the carried-book flow: mark the open
    /// book to the latest close, write its last portfolio row and a `legs.csv`
    /// row per still-open leg, and flush.
    fn finish_book_capture(&mut self) {
        let equity = self.usdt_equity();
        let marks = self.runtime().latest_closes();
        if let Some(capture) = self.runtime_mut().capture.as_mut() {
            capture.finish_book(&marks, equity);
        }
    }
}

/// The `N`-day return `(P_now - P_{now-N}) / P_{now-N}`, needing `N + 1`
/// price points in `queue`. `None` if the buffer isn't deep enough yet
/// (still warming up) or the reference price is zero.
pub fn n_day_return(queue: &BoundedQueue<Decimal>, days: usize) -> Option<Decimal> {
    let len = queue.inner.len();
    if len <= days {
        return None;
    }
    let now = *queue.inner.back()?;
    let past = *queue.inner.get(len - 1 - days)?;
    (!past.is_zero()).then(|| (now - past) / past)
}

/// The instrument id for `BTC` within `bases`, matched case-insensitively.
/// Callers should validate `bases` contains it at config-build time.
pub fn btc_instrument_id(bases: &[String], venue: &str) -> InstrumentId {
    let base = bases
        .iter()
        .find(|base| base.eq_ignore_ascii_case("BTC"))
        .expect("BTC must be present in the universe");
    InstrumentId::from(format!("{base}USDT-LINEAR.{venue}").as_str())
}
