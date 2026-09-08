//! `xsec fetch`: populate the `data/` cache the backtest reads through.
//!
//! Downloads the Bybit linear-instruments list plus every `--universe` symbol's
//! full daily-bar history, and writes `data/manifest.json` recording — per
//! requested base — whether it resolved to a Bybit linear perp and its bar
//! coverage. That manifest is the tradeability oracle the CoinMarketCap
//! universe work (#32) reads: "is this coin on Bybit, and does it have a candle
//! at week T". The only part of the binary that touches the network.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Args;
use nautilus_model::{enums::BarAggregation, identifiers::InstrumentId, instruments::Instrument};
use serde::Serialize;

use crate::data::{
    exchange::{
        MarketData,
        bybit::{
            BybitMarketData, bar_cache_is_fresh, bar_cache_path, linear_perp_id,
            write_instruments_snapshot,
        },
    },
    universe::read_universe,
};

/// `xsec fetch` flags. The universe comes from the global `--universe`.
#[derive(Args, Debug)]
pub struct FetchArgs {
    /// Re-download every symbol even if its cache is present and still fresh.
    #[arg(long)]
    pub refresh: bool,
}

/// What a fetch run did, for the stdout summary.
#[derive(Debug, Default)]
pub struct FetchReport {
    /// Bases that hit the Bybit network this run.
    pub fetched: Vec<String>,
    /// Bases already present and fresh on disk (skipped unless `--refresh`).
    pub cached: Vec<String>,
    /// Bases with no Bybit linear perp — nothing to fetch.
    pub unlisted: Vec<String>,
    /// Bases whose fetch errored: `(base, message)`. Not fatal.
    pub failed: Vec<(String, String)>,
}

impl FetchReport {
    /// One-line count plus the unlisted / failed detail.
    pub fn print_summary(&self) {
        println!(
            "fetch: {} fetched, {} cached, {} unlisted, {} failed",
            self.fetched.len(),
            self.cached.len(),
            self.unlisted.len(),
            self.failed.len(),
        );
        if !self.unlisted.is_empty() {
            println!("  no Bybit linear perp: {}", self.unlisted.join(" "));
        }
        for (base, err) in &self.failed {
            println!("  failed {base}: {err}");
        }
    }
}

#[derive(Serialize)]
struct SymbolEntry {
    base: String,
    instrument_id: Option<String>,
    status: &'static str,
    bars: usize,
    first_bar: Option<String>,
    last_bar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Manifest {
    generated_at: String,
    universe: String,
    instruments: usize,
    symbols: Vec<SymbolEntry>,
}

/// Fetch instruments + bar history for the universe at `universe_path` into
/// `data_dir`. A network failure for a single symbol is recorded and skipped,
/// not fatal; a failure fetching the instruments list is.
pub fn run(universe_path: &Path, data_dir: &Path, args: &FetchArgs) -> Result<FetchReport> {
    let bases = read_universe(universe_path)?;
    std::fs::create_dir_all(data_dir).ok();

    let market = BybitMarketData::new()?;
    let instruments = market
        .instruments()
        .context("fetch Bybit linear instruments")?;
    write_instruments_snapshot(data_dir, &instruments)?;
    log::info!("cached {} Bybit linear instruments", instruments.len());
    let listed: HashSet<InstrumentId> = instruments.iter().map(|i| i.id()).collect();

    let mut report = FetchReport::default();
    let mut symbols = Vec::with_capacity(bases.len());

    for base in &bases {
        let id = linear_perp_id(base);

        if !listed.contains(&id) {
            log::warn!("{base}: no Bybit linear perp ({id}) — skipping");
            report.unlisted.push(base.clone());
            symbols.push(SymbolEntry {
                base: base.clone(),
                instrument_id: None,
                status: "unlisted",
                bars: 0,
                first_bar: None,
                last_bar: None,
                error: None,
            });
            continue;
        }

        let hit_network = args.refresh || !bar_cache_is_fresh(data_dir, &id);
        if args.refresh {
            std::fs::remove_file(bar_cache_path(data_dir, &id)).ok();
        }

        match market.bars(id, BarAggregation::Day) {
            Ok(bars) => {
                if hit_network {
                    report.fetched.push(base.clone());
                } else {
                    report.cached.push(base.clone());
                }
                symbols.push(SymbolEntry {
                    base: base.clone(),
                    instrument_id: Some(id.to_string()),
                    status: if hit_network { "fetched" } else { "cached" },
                    bars: bars.len(),
                    first_bar: bars.first().and_then(|b| bar_date(b.ts_event.as_u64())),
                    last_bar: bars.last().and_then(|b| bar_date(b.ts_event.as_u64())),
                    error: None,
                });
            }
            Err(e) => {
                log::error!("{base}: fetch failed: {e:#}");
                report.failed.push((base.clone(), format!("{e:#}")));
                symbols.push(SymbolEntry {
                    base: base.clone(),
                    instrument_id: Some(id.to_string()),
                    status: "failed",
                    bars: 0,
                    first_bar: None,
                    last_bar: None,
                    error: Some(format!("{e:#}")),
                });
            }
        }
    }

    let manifest = Manifest {
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        universe: universe_path.display().to_string(),
        instruments: instruments.len(),
        symbols,
    };
    let manifest_path = data_dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).context("serialize manifest")?,
    )
    .with_context(|| format!("write {}", manifest_path.display()))?;

    Ok(report)
}

/// A bar's `ts_event` (UNIX nanoseconds) as a `YYYY-MM-DD` UTC date.
fn bar_date(ts_nanos: u64) -> Option<String> {
    DateTime::<Utc>::from_timestamp((ts_nanos / 1_000_000_000) as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
}
