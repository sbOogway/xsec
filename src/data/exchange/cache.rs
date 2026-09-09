//! The `data/<venue>/` cache: the on-disk file layout `xsec fetch`
//! ([`super::fetch`]) writes, and [`CachedMarketData`] — the offline
//! [`MarketData`] a backtest reads through. No network here. All paths take an
//! explicit `data_dir` (the per-venue root), so nothing here knows the venue.
//!
//! | file | content |
//! | --- | --- |
//! | `<base>_1d.msgpack` | one symbol's full daily-bar history (Nautilus msgpack) |
//! | `instruments.json` | the venue's linear-instruments snapshot |
//! | `manifest.json` | per requested base: did it resolve to a perp on the venue, and its bar coverage |
//!
//! `CachedMarketData` turns a missing snapshot or bar file into a hard error
//! naming `xsec fetch`, never an empty series — the [`MarketData`] contract
//! reads empty as "this instrument has no history", which would let a backtest
//! quietly run on a degenerate universe.
//!
//! The instruments snapshot is JSON, not msgpack: `InstrumentAny`'s `Currency`
//! fields deserialize through a strict registry lookup, and Bybit lists coins
//! outside nautilus's built-in table, so [`read_instruments`] walks the JSON and
//! pre-registers every currency code before the real decode.

use std::{
    collections::HashSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use nautilus_model::{
    data::Bar, enums::BarAggregation, identifiers::InstrumentId, instruments::InstrumentAny,
    types::Currency,
};
use serde::Serialize;

use super::MarketData;

/// The cache root. `xsec fetch` writes it; the backtest reads it.
pub const DATA_DIR: &str = "data";

const STALE_AFTER_HOURS: u64 = 24;

// --- the offline adapter -------------------------------------------------

/// A [`MarketData`] served entirely from the on-disk cache under `data_dir` — no
/// HTTP client, no Tokio runtime. The only adapter a backtest reads through.
#[derive(Debug)]
pub struct CachedMarketData {
    data_dir: PathBuf,
}

impl CachedMarketData {
    /// Point the adapter at a cache directory (conventionally [`DATA_DIR`]).
    /// Errors if the directory does not exist — `xsec fetch` has not run.
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
        read_instruments(&self.data_dir)
    }

    fn bars(&self, instrument_id: InstrumentId, _aggregation: BarAggregation) -> Result<Vec<Bar>> {
        read_bars(&self.data_dir, &instrument_id)?.ok_or_else(|| {
            anyhow!(
                "no cached bars for {instrument_id} — run `xsec fetch` for a universe that includes it"
            )
        })
    }
}

// --- paths -----------------------------------------------------------------

/// The daily-bar cache path for `instrument_id` under `data_dir`:
/// `<data_dir>/BTCUSDT_1d.msgpack` for `BTCUSDT-LINEAR.BYBIT`. The key drops the
/// venue and keeps a `_1d` timeframe tag; only daily bars are ever cached.
pub fn bar_path(data_dir: &Path, instrument_id: &InstrumentId) -> PathBuf {
    let sym = instrument_id.symbol.as_str();
    let bare = sym.split_once('-').map(|(b, _)| b).unwrap_or(sym);
    data_dir.join(format!("{bare}_1d.msgpack"))
}

/// Path to the linear-instruments snapshot under `data_dir`.
pub fn instruments_path(data_dir: &Path) -> PathBuf {
    data_dir.join("instruments.json")
}

/// Path to the fetch manifest under `data_dir`.
pub fn manifest_path(data_dir: &Path) -> PathBuf {
    data_dir.join("manifest.json")
}

// --- bars ----------------------------------------------------------------

/// Write `bars` to the cache for `instrument_id`, via a tmp + rename so a crash
/// can't leave a half-written file.
pub fn write_bars(data_dir: &Path, instrument_id: &InstrumentId, bars: &[Bar]) -> Result<()> {
    let bytes = rmp_serde::to_vec_named(bars).context("serialize bars")?;
    write_atomic(&bar_path(data_dir, instrument_id), &bytes)
}

/// Read the cached bars for `instrument_id`, or `Ok(None)` if the file is
/// absent. A present-but-unreadable file is an error.
pub fn read_bars(data_dir: &Path, instrument_id: &InstrumentId) -> Result<Option<Vec<Bar>>> {
    let path = bar_path(data_dir, instrument_id);
    match fs::read(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        Ok(bytes) => rmp_serde::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("decode {}", path.display())),
    }
}

/// Whether the cache for `instrument_id` exists and its last bar is within
/// [`STALE_AFTER_HOURS`] — how `xsec fetch` decides a symbol is already cached
/// rather than in need of a download.
pub fn bars_are_fresh(data_dir: &Path, instrument_id: &InstrumentId) -> bool {
    let Ok(Some(bars)) = read_bars(data_dir, instrument_id) else {
        return false;
    };
    let Some(last) = bars.last() else {
        return false;
    };
    let last_ms = last.ts_event.as_u64() / 1_000_000;
    let now_ms = Utc::now().timestamp_millis() as u64;
    now_ms.saturating_sub(last_ms) / 3_600_000 <= STALE_AFTER_HOURS
}

// --- instruments -------------------------------------------------------

/// Write the linear-instruments snapshot.
pub fn write_instruments(data_dir: &Path, instruments: &[InstrumentAny]) -> Result<()> {
    let bytes = serde_json::to_vec(instruments).context("serialize instruments snapshot")?;
    write_atomic(&instruments_path(data_dir), &bytes)
}

/// Read the snapshot written by [`write_instruments`], registering every
/// currency code it mentions as a crypto currency first so the strict
/// `Currency` deserialize inside `InstrumentAny` can't miss on a freshly-listed
/// Bybit base coin.
pub fn read_instruments(data_dir: &Path) -> Result<Vec<InstrumentAny>> {
    let path = instruments_path(data_dir);
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "no instruments snapshot at {} — run `xsec fetch --universe <file>` first",
            path.display()
        )
    })?;
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    prime_currencies(&raw);
    serde_json::from_value(raw).with_context(|| format!("decode instruments from {}", path.display()))
}

/// Register every currency code `value` mentions (recursively) as a crypto
/// currency. `InstrumentAny` carries currencies two ways: bare codes under
/// `*_currency` keys, and `Money`-shaped `"<amount> <CODE>"` strings under
/// `*notional` keys. `Currency::get_or_create_crypto` is a no-op for a code that
/// is already known (Bybit lists single-character tickers like `4`, hence no
/// length or charset floor).
fn prime_currencies(value: &serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => items.iter().for_each(prime_currencies),
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                if let serde_json::Value::String(s) = val {
                    if key.ends_with("_currency") {
                        register_currency_code(s);
                    } else if key.ends_with("notional")
                        && let Some((_, code)) = s.rsplit_once(' ')
                    {
                        register_currency_code(code);
                    }
                }
                prime_currencies(val);
            }
        }
        _ => {}
    }
}

fn register_currency_code(code: &str) {
    let code = code.trim();
    if !code.is_empty() && code.len() <= 24 && !code.contains(char::is_whitespace) {
        let _ = Currency::get_or_create_crypto(code);
    }
}

// --- manifest ----------------------------------------------------------

/// One row of [`Manifest::symbols`]: a requested base and how the fetch went.
#[derive(Serialize)]
pub struct ManifestEntry {
    pub base: String,
    /// `None` when the base has no Bybit linear perp.
    pub instrument_id: Option<String>,
    /// `"fetched"` | `"cached"` | `"unlisted"` | `"failed"`.
    pub status: &'static str,
    pub bars: usize,
    pub first_bar: Option<String>,
    pub last_bar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ManifestEntry {
    /// `first_bar` / `last_bar` for a bar slice, as `YYYY-MM-DD` UTC dates.
    pub fn bar_dates(bars: &[Bar]) -> (Option<String>, Option<String>) {
        let date = |b: &Bar| {
            DateTime::<Utc>::from_timestamp((b.ts_event.as_u64() / 1_000_000_000) as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
        };
        (bars.first().and_then(date), bars.last().and_then(date))
    }
}

#[derive(Serialize)]
struct Manifest<'a> {
    generated_at: String,
    universe: &'a str,
    instruments: usize,
    symbols: &'a [ManifestEntry],
}

/// The Bybit base coins the manifest at `data_dir` has usable bars for
/// (`status` `fetched` or `cached`) — the tradeability filter
/// `momentum --source coinmarketcap` applies to a CoinMarketCap ranking.
/// Errors naming `xsec fetch` if there is no manifest.
pub fn read_manifest_bases(data_dir: &Path) -> Result<HashSet<String>> {
    let path = manifest_path(data_dir);
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "no fetch manifest at {} — run `xsec fetch --universe <file>` first",
            path.display()
        )
    })?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    let bases = manifest["symbols"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| matches!(row["status"].as_str(), Some("fetched" | "cached")))
                .filter_map(|row| row["base"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(bases)
}

/// Write `manifest.json`.
pub fn write_manifest(
    data_dir: &Path,
    universe: &str,
    instruments: usize,
    symbols: &[ManifestEntry],
) -> Result<()> {
    let manifest = Manifest {
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        universe,
        instruments,
        symbols,
    };
    let bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest")?;
    write_atomic(&manifest_path(data_dir), &bytes)
}

// --- shared -----------------------------------------------------------

/// Write `bytes` to `path` via a sibling `.tmp` file + rename, creating the
/// parent directory on demand.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("tmp");
    let tmp = path.with_extension(format!("{ext}.tmp"));
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use nautilus_core::UnixNanos;
    use nautilus_model::{
        data::BarType,
        enums::CurrencyType,
        identifiers::Symbol,
        instruments::{CryptoPerpetual, Instrument},
        types::{Price, Quantity},
    };
    use tempfile::tempdir;

    use super::*;

    fn btc_id() -> InstrumentId {
        InstrumentId::from("BTCUSDT-LINEAR.BYBIT")
    }

    /// A daily bar whose `ts_event` is `secs` past the epoch (all prices flat).
    fn bar_at(secs: i64) -> Bar {
        let bt = BarType::from_str("BTCUSDT-LINEAR.BYBIT-1-DAY-LAST-EXTERNAL").unwrap();
        let px = Price::new(100.0, 2);
        let ts = UnixNanos::from(secs as u64 * 1_000_000_000);
        Bar::new(bt, px, px, px, px, Quantity::new(1.0, 1), ts, ts)
    }

    /// A minimal Bybit linear perp for `base` — every optional field `None`, and
    /// the base currency built (not registered) so a snapshot round-trip has to
    /// go through [`prime_currencies`] to decode it.
    fn perp_with_base(base: &str) -> InstrumentAny {
        InstrumentAny::CryptoPerpetual(CryptoPerpetual::new(
            InstrumentId::from(format!("{base}USDT-LINEAR.BYBIT").as_str()),
            Symbol::from(format!("{base}USDT").as_str()),
            Currency::new(base, 8, 0, base, CurrencyType::Crypto),
            Currency::from("USDT"),
            Currency::from("USDT"),
            false,
            2,
            3,
            Price::from("0.01"),
            Quantity::from("0.001"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            UnixNanos::default(),
            UnixNanos::default(),
        ))
    }

    #[test]
    fn bar_path_strips_the_venue_and_tags_the_timeframe() {
        assert_eq!(
            bar_path(Path::new("data"), &btc_id()),
            Path::new("data/BTCUSDT_1d.msgpack"),
        );
        // A symbol with no venue segment falls back to the whole symbol.
        assert_eq!(
            bar_path(Path::new("data"), &InstrumentId::from("FOO.BYBIT")),
            Path::new("data/FOO_1d.msgpack"),
        );
    }

    #[test]
    fn bars_are_fresh_tracks_the_last_bar_age() {
        let dir = tempdir().unwrap();
        let id = btc_id();
        let now = Utc::now().timestamp();

        assert!(!bars_are_fresh(dir.path(), &id), "no file");

        write_bars(dir.path(), &id, &[]).unwrap();
        assert!(!bars_are_fresh(dir.path(), &id), "empty series");

        write_bars(dir.path(), &id, &[bar_at(now - 3600)]).unwrap();
        assert!(bars_are_fresh(dir.path(), &id), "last bar 1h old");

        write_bars(dir.path(), &id, &[bar_at(now - 48 * 3600)]).unwrap();
        assert!(!bars_are_fresh(dir.path(), &id), "last bar 48h old");
    }

    #[test]
    fn read_instruments_primes_an_unregistered_base_currency() {
        let dir = tempdir().unwrap();
        write_instruments(dir.path(), &[perp_with_base("XQZ777")]).unwrap();

        let got = read_instruments(dir.path())
            .expect("prime_currencies registers XQZ777 before the strict decode");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].base_currency().unwrap().code.as_str(), "XQZ777");
    }

    #[test]
    fn write_manifest_round_trips_the_schema() {
        let dir = tempdir().unwrap();
        let entries = vec![
            ManifestEntry {
                base: "BTC".to_string(),
                instrument_id: Some("BTCUSDT-LINEAR.BYBIT".to_string()),
                status: "fetched",
                bars: 100,
                first_bar: Some("2020-01-01".to_string()),
                last_bar: Some("2026-01-01".to_string()),
                error: None,
            },
            ManifestEntry {
                base: "FOO".to_string(),
                instrument_id: None,
                status: "unlisted",
                bars: 0,
                first_bar: None,
                last_bar: None,
                error: None,
            },
        ];
        write_manifest(dir.path(), "universe.txt", 862, &entries).unwrap();

        let text = fs::read_to_string(manifest_path(dir.path())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["universe"], "universe.txt");
        assert_eq!(v["instruments"], 862);
        assert!(v["generated_at"].as_str().unwrap().ends_with('Z'));
        assert_eq!(v["symbols"][0]["base"], "BTC");
        assert_eq!(v["symbols"][0]["status"], "fetched");
        assert_eq!(v["symbols"][0]["bars"], 100);
        assert!(
            v["symbols"][0].get("error").is_none(),
            "error is skipped when None"
        );
        assert_eq!(v["symbols"][1]["instrument_id"], serde_json::Value::Null);
    }

    #[test]
    fn bar_dates_formats_utc_days() {
        // 2021-03-16T00:00:00Z, then +1 day.
        let bars = [bar_at(1_615_852_800), bar_at(1_615_939_200)];
        assert_eq!(
            ManifestEntry::bar_dates(&bars),
            (Some("2021-03-16".to_string()), Some("2021-03-17".to_string())),
        );
        assert_eq!(ManifestEntry::bar_dates(&[]), (None, None));
    }
}
