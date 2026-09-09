//! `xsec fetch`: fill the `data/<venue>/` cache from an [`ExchangeAdapter`].
//!
//! Orchestration only — the network lives behind the adapter, the on-disk
//! layout in [`super::cache`]. Downloads the venue's instrument list plus every
//! `--universe` symbol's full daily-bar history, and writes `manifest.json`
//! recording, per requested base, whether it resolved to a perp on the venue
//! and its bar coverage. That manifest is the tradeability oracle the
//! CoinMarketCap universe work (#32) reads.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use clap::Args;
use nautilus_model::{identifiers::InstrumentId, instruments::Instrument};

use crate::data::{
    exchange::{ExchangeAdapter, cache, cache::ManifestEntry},
    universe::read_universe,
};

/// `xsec fetch` flags. The universe comes from the global `--universe`.
#[derive(Args, Debug)]
pub struct FetchArgs {
    /// Re-download every symbol even if its cache is present and still fresh.
    #[arg(long)]
    pub refresh: bool,
}

/// What a fetch run did — returned for the caller and for tests; the summary is
/// logged via [`log_summary`](Self::log_summary).
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
    /// Log the one-line outcome at `info`. The per-base detail (which coins were
    /// unlisted, which failed and why) is already logged as it happens in
    /// [`run`].
    pub fn log_summary(&self) {
        log::info!(
            "fetch: {} fetched, {} cached, {} unlisted, {} failed",
            self.fetched.len(),
            self.cached.len(),
            self.unlisted.len(),
            self.failed.len(),
        );
    }
}

/// Fetch instruments + bar history for the universe at `universe_path` from
/// `adapter` into `data_dir`. A failure for a single symbol is recorded and
/// skipped; a failure fetching the instruments list aborts.
pub fn run(
    adapter: &dyn ExchangeAdapter,
    universe_path: &Path,
    data_dir: &Path,
    args: &FetchArgs,
) -> Result<FetchReport> {
    let bases = read_universe(universe_path)?;

    let instruments = adapter
        .instruments()
        .with_context(|| format!("fetch {} instruments", adapter.venue()))?;
    cache::write_instruments(data_dir, &instruments)?;
    log::info!("cached {} {} instruments", instruments.len(), adapter.venue());
    let listed: HashSet<InstrumentId> = instruments.iter().map(|i| i.id()).collect();

    let mut report = FetchReport::default();
    let mut entries = Vec::with_capacity(bases.len());

    for base in &bases {
        let id = adapter.perp_id(base);

        if !listed.contains(&id) {
            log::warn!("{base}: no {} perp ({id}) — skipping", adapter.venue());
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
            adapter
                .bars(id)
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, io::Write, str::FromStr};

    use nautilus_core::UnixNanos;
    use nautilus_model::{
        data::{Bar, BarType},
        enums::CurrencyType,
        identifiers::Symbol,
        instruments::{CryptoPerpetual, InstrumentAny},
        types::{Currency, Price, Quantity},
    };
    use tempfile::{NamedTempFile, tempdir};

    use super::*;

    /// An [`ExchangeAdapter`] with a fixed instrument list and bar history, no
    /// network — `bars` errors for an id it has no history for.
    struct FakeExchange {
        instruments: Vec<InstrumentAny>,
        bars: HashMap<InstrumentId, Vec<Bar>>,
    }

    impl ExchangeAdapter for FakeExchange {
        fn venue(&self) -> &'static str {
            "FAKE"
        }
        fn instruments(&self) -> Result<Vec<InstrumentAny>> {
            Ok(self.instruments.clone())
        }
        fn perp_id(&self, base: &str) -> InstrumentId {
            InstrumentId::from(format!("{base}USDT-LINEAR.FAKE").as_str())
        }
        fn bars(&self, id: InstrumentId) -> Result<Vec<Bar>> {
            self.bars
                .get(&id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no history for {id}"))
        }
    }

    fn perp(base: &str) -> InstrumentAny {
        InstrumentAny::CryptoPerpetual(CryptoPerpetual::new(
            InstrumentId::from(format!("{base}USDT-LINEAR.FAKE").as_str()),
            Symbol::from(format!("{base}USDT").as_str()),
            Currency::new(base, 8, 0, base, CurrencyType::Crypto),
            Currency::from("USDT"),
            Currency::from("USDT"),
            false,
            2,
            3,
            Price::from("0.01"),
            Quantity::from("0.001"),
            None, None, None, None, None, None, None, None, None, None, None, None, None, None,
            UnixNanos::default(),
            UnixNanos::default(),
        ))
    }

    fn bar_at(id: InstrumentId, ts_secs: u64) -> Bar {
        let bt = BarType::from_str(&format!("{id}-1-DAY-LAST-EXTERNAL")).unwrap();
        let px = Price::new(100.0, 2);
        let ts = UnixNanos::from(ts_secs * 1_000_000_000);
        Bar::new(bt, px, px, px, px, Quantity::new(1.0, 1), ts, ts)
    }

    fn universe(bases: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{bases}").unwrap();
        f
    }

    fn manifest_status(data_dir: &Path, base: &str) -> String {
        let m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cache::manifest_path(data_dir)).unwrap()).unwrap();
        m["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["base"] == base)
            .unwrap_or_else(|| panic!("no manifest row for {base}"))["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn run_classifies_listed_unlisted_and_failed_bases() {
        let dir = tempdir().unwrap();
        let uni = universe("BTC\nETH\nFOO");
        let btc = InstrumentId::from("BTCUSDT-LINEAR.FAKE");

        let adapter = FakeExchange {
            instruments: vec![perp("BTC"), perp("ETH")], // FOO not listed
            bars: HashMap::from([(btc, vec![bar_at(btc, 0), bar_at(btc, 86_400)])]), // ETH: no history
        };

        let report = run(&adapter, uni.path(), dir.path(), &FetchArgs { refresh: false }).unwrap();

        assert_eq!(report.fetched, ["BTC"]);
        assert_eq!(report.unlisted, ["FOO"]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "ETH");
        assert!(report.cached.is_empty());

        assert_eq!(manifest_status(dir.path(), "BTC"), "fetched");
        assert_eq!(manifest_status(dir.path(), "ETH"), "failed");
        assert_eq!(manifest_status(dir.path(), "FOO"), "unlisted");
    }

    #[test]
    fn run_reuses_a_fresh_cache_and_refresh_forces_a_refetch() {
        let dir = tempdir().unwrap();
        let uni = universe("BTC");
        let btc = InstrumentId::from("BTCUSDT-LINEAR.FAKE");

        // Seed a cache whose last bar is ~now — fresh.
        let now = chrono::Utc::now().timestamp() as u64;
        cache::write_bars(dir.path(), &btc, &[bar_at(btc, now)]).unwrap();

        // The adapter has no bar history, so it errors if asked — proving it isn't.
        let adapter = FakeExchange {
            instruments: vec![perp("BTC")],
            bars: HashMap::new(),
        };

        let report = run(&adapter, uni.path(), dir.path(), &FetchArgs { refresh: false }).unwrap();
        assert_eq!(report.cached, ["BTC"]);
        assert!(report.fetched.is_empty() && report.failed.is_empty());

        // `--refresh` bypasses the freshness check and hits the (failing) adapter.
        let report = run(&adapter, uni.path(), dir.path(), &FetchArgs { refresh: true }).unwrap();
        assert_eq!(report.failed.len(), 1);
    }
}
