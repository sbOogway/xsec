//! Offline market data: the `data/` cache `xsec fetch` populated, and nothing
//! else. No HTTP client, no Tokio runtime — a backtest reads through this so a
//! run is reproducible and never silently reaches for the network.
//!
//! A missing snapshot or a missing per-symbol bar file is a hard error naming
//! `xsec fetch`, never an empty series — the [`MarketData`] contract reads empty
//! as "this instrument has no history", which would let a backtest quietly run
//! on a degenerate universe.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nautilus_model::{
    data::Bar, enums::BarAggregation, identifiers::InstrumentId, instruments::InstrumentAny,
};

use crate::data::exchange::{MarketData, bybit};

/// A [`MarketData`] served entirely from the on-disk cache under `data_dir`.
#[derive(Debug)]
pub struct CachedMarketData {
    data_dir: PathBuf,
}

impl CachedMarketData {
    /// Point the adapter at a cache directory (conventionally `data/`). Errors
    /// if the directory does not exist — `xsec fetch` has not run.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        if !data_dir.is_dir() {
            bail!(
                "no data cache at {} — run `xsec fetch --universe <file>` first",
                data_dir.display()
            );
        }
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
        })
    }
}

impl MarketData for CachedMarketData {
    fn instruments(&self) -> Result<Vec<InstrumentAny>> {
        bybit::read_instruments_snapshot(&self.data_dir)
    }

    fn bars(&self, instrument_id: InstrumentId, _aggregation: BarAggregation) -> Result<Vec<Bar>> {
        let path = bybit::bar_cache_path(&self.data_dir, &instrument_id);
        let bytes = std::fs::read(&path).with_context(|| {
            format!(
                "no cached bars for {instrument_id} ({}) — run `xsec fetch` for a universe that includes it",
                path.display()
            )
        })?;
        rmp_serde::from_slice(&bytes).with_context(|| format!("decode cached bars {}", path.display()))
    }
}
