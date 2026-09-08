//! `xsec fetch`: fill the `data/` cache from Bybit.
//!
//! Orchestration only — the network lives in [`super::bybit`], the on-disk
//! layout in [`super::cache`]. Downloads the linear-instruments list plus every
//! `--universe` symbol's full daily-bar history, and writes `manifest.json`
//! recording, per requested base, whether it resolved to a Bybit linear perp
//! and its bar coverage. That manifest is the tradeability oracle the
//! CoinMarketCap universe work (#32) reads. The only part of the binary that
//! touches the network.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use clap::Args;
use nautilus_model::{identifiers::InstrumentId, instruments::Instrument};

use crate::data::{
    exchange::{bybit, cache, cache::ManifestEntry},
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

/// Fetch instruments + bar history for the universe at `universe_path` into
/// `data_dir`. A network failure for a single symbol is recorded and skipped;
/// a failure fetching the instruments list aborts.
pub fn run(universe_path: &Path, data_dir: &Path, args: &FetchArgs) -> Result<FetchReport> {
    let bases = read_universe(universe_path)?;
    let rt = tokio::runtime::Runtime::new().context("tokio runtime for xsec fetch")?;

    let instruments = rt
        .block_on(bybit::fetch_linear_instruments())
        .context("fetch Bybit linear instruments")?;
    cache::write_instruments(data_dir, &instruments)?;
    log::info!("cached {} Bybit linear instruments", instruments.len());
    let listed: HashSet<InstrumentId> = instruments.iter().map(|i| i.id()).collect();

    let mut report = FetchReport::default();
    let mut entries = Vec::with_capacity(bases.len());

    for base in &bases {
        let id = bybit::linear_perp_id(base);

        if !listed.contains(&id) {
            log::warn!("{base}: no Bybit linear perp ({id}) — skipping");
            report.unlisted.push(base.clone());
            entries.push(ManifestEntry {
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

        let networked = args.refresh || !cache::bars_are_fresh(data_dir, &id);
        let outcome = if networked {
            rt.block_on(bybit::fetch_bars(id))
                .and_then(|bars| cache::write_bars(data_dir, &id, &bars).map(|()| bars))
        } else {
            cache::read_bars(data_dir, &id).map(Option::unwrap_or_default)
        };

        match outcome {
            Ok(bars) => {
                if networked {
                    report.fetched.push(base.clone());
                } else {
                    report.cached.push(base.clone());
                }
                let (first_bar, last_bar) = ManifestEntry::bar_dates(&bars);
                entries.push(ManifestEntry {
                    base: base.clone(),
                    instrument_id: Some(id.to_string()),
                    status: if networked { "fetched" } else { "cached" },
                    bars: bars.len(),
                    first_bar,
                    last_bar,
                    error: None,
                });
            }
            Err(e) => {
                log::error!("{base}: fetch failed: {e:#}");
                report.failed.push((base.clone(), format!("{e:#}")));
                entries.push(ManifestEntry {
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

    cache::write_manifest(
        data_dir,
        &universe_path.display().to_string(),
        instruments.len(),
        &entries,
    )?;

    Ok(report)
}
