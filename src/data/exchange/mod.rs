//! Exchange data adapters and the `data/` cache they share.
//!
//! [`MarketData`] is the seam the backtest bootstrap ([`crate::engine`]) pulls
//! its instruments and bar history through:
//!
//! * [`CachedMarketData`] — the offline `data/` cache, and the only adapter the
//!   backtest reads through. Lives in [`cache`] with the file layout it reads.
//! * [`InMemoryMarketData`] — a fixture-backed fake for tests that drive the
//!   bootstrap without a network or disk.
//!
//! The other modules fill that cache: [`bybit`] is the Bybit HTTP surface,
//! [`cache`] owns the on-disk file layout, and [`fetch`] (`xsec fetch`)
//! orchestrates the two. Only [`fetch`] touches the network.

pub mod bybit;
pub mod cache;
pub mod fetch;

pub use cache::CachedMarketData;

use std::collections::HashMap;

use anyhow::Result;
use nautilus_model::{
    data::Bar, enums::BarAggregation, identifiers::InstrumentId, instruments::InstrumentAny,
};

/// The market data a backtest needs at bootstrap: the venue's tradeable
/// instruments, and per-instrument bar history.
///
/// Consumed once, at start-up, by [`crate::engine::build_backtest_engine`].
/// [`instruments`](Self::instruments) is always called before
/// [`bars`](Self::bars), so an adapter may rely on that ordering to prime
/// symbol resolution.
pub trait MarketData {
    /// Every tradeable instrument on the venue.
    fn instruments(&self) -> Result<Vec<InstrumentAny>>;

    /// Bar history for `instrument_id` at `aggregation`, oldest first. An
    /// instrument with no history yields an empty vec, not an error.
    fn bars(&self, instrument_id: InstrumentId, aggregation: BarAggregation) -> Result<Vec<Bar>>;
}

/// A [`MarketData`] backed entirely by in-memory fixtures — no network, no
/// disk. Lets a test drive the backtest bootstrap end to end.
pub struct InMemoryMarketData {
    instruments: Vec<InstrumentAny>,
    bars: HashMap<InstrumentId, Vec<Bar>>,
}

impl InMemoryMarketData {
    #[must_use]
    pub fn new(instruments: Vec<InstrumentAny>, bars: HashMap<InstrumentId, Vec<Bar>>) -> Self {
        Self { instruments, bars }
    }
}

impl MarketData for InMemoryMarketData {
    fn instruments(&self) -> Result<Vec<InstrumentAny>> {
        Ok(self.instruments.clone())
    }

    fn bars(&self, instrument_id: InstrumentId, _aggregation: BarAggregation) -> Result<Vec<Bar>> {
        Ok(self.bars.get(&instrument_id).cloned().unwrap_or_default())
    }
}
