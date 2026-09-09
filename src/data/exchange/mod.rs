//! Exchange adapters and the `data/` cache they share.
//!
//! Two seams, kept apart on purpose:
//!
//! * [`MarketData`] — what the backtest bootstrap ([`crate::engine`]) reads
//!   instruments and bar history through. Impls: [`CachedMarketData`] (the
//!   offline `data/<venue>/` cache, in [`cache`]) and [`InMemoryMarketData`] (a
//!   fixture fake). Offline, sync, no venue knowledge.
//! * [`ExchangeAdapter`] — what `xsec fetch` ([`fetch`]) pulls *from*: a venue's
//!   instrument list, symbol convention and bar history. Impl:
//!   [`bybit::BybitAdapter`]. Networked; the only thing that reaches out.
//!
//! Nothing that satisfies `ExchangeAdapter` satisfies `MarketData`, so a fetch
//! adapter can never be handed to the backtest. [`cache`] owns the on-disk file
//! layout both sides agree on; [`bybit`] is the Bybit HTTP surface.

pub mod bybit;
pub mod cache;
pub mod fetch;

pub use cache::CachedMarketData;

use std::collections::HashMap;

use anyhow::Result;
use clap::ValueEnum;
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

/// The venues `xsec fetch` can pull from — the `--exchange` flag. Each keeps its
/// own cache under `data/<exchange>/`.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Exchange {
    #[default]
    Bybit,
}

impl Exchange {
    /// The lowercased tag: the `--exchange` value and the `data/<tag>/` subdir.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Exchange::Bybit => "bybit",
        }
    }
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A venue `xsec fetch` ([`fetch::run`]) pulls instruments and bar history from.
///
/// Sync: an adapter blocks on its own async client internally, so `fetch::run`
/// stays runtime-free and this trait is object-safe. Deliberately *not*
/// [`MarketData`] — that is the offline backtest-read seam, and a networked
/// fetch adapter has no business being handed to the engine.
pub trait ExchangeAdapter {
    /// The venue tag in instrument ids (`"BYBIT"` → `BTCUSDT-LINEAR.BYBIT`).
    fn venue(&self) -> &'static str;

    /// Every tradeable instrument on the venue.
    fn instruments(&self) -> Result<Vec<InstrumentAny>>;

    /// This venue's perp instrument id for a base asset (`"BTC"` →
    /// `BTCUSDT-LINEAR.BYBIT`).
    fn perp_id(&self, base: &str) -> InstrumentId;

    /// Full daily-bar history for `instrument_id`, oldest first.
    fn bars(&self, instrument_id: InstrumentId) -> Result<Vec<Bar>>;
}
