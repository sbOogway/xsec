//! Bybit data adapter: bar-type construction, the on-disk cache (`data/`) and
//! the shared HTTP client that fetches linear-perp instruments and daily bar
//! history.
//!
//! [`BybitMarketData`] is the networked [`MarketData`] that `xsec fetch` drives
//! to populate the cache; [`get_bar_type`] is also used by the rebalance
//! runtime in [`crate::strategy::common`]. The cache layout — [`bar_cache_path`],
//! [`instruments_snapshot_path`] — and its readers/writers are shared with the
//! offline [`CachedMarketData`](super::cached::CachedMarketData) the backtest
//! reads through.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;
use nautilus_bybit::{common::enums::BybitProductType, http::client::BybitHttpClient};
use nautilus_model::{
    data::{Bar, BarSpecification, BarType},
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::InstrumentId,
    instruments::InstrumentAny,
    types::Currency,
};

use crate::data::exchange::MarketData;

/// The on-disk cache directory. `xsec fetch` writes it, `CachedMarketData` reads
/// it; nothing here honours a different root except through the explicit
/// `data_dir` parameter on the path helpers below.
pub const DATA_DIR: &str = "data";
const STALE_AFTER_HOURS: i64 = 24;

pub fn get_bar_type(instrument_id: InstrumentId, aggregation: BarAggregation) -> BarType {
    BarType::new(
        instrument_id,
        BarSpecification::new(1, aggregation, PriceType::Last),
        AggregationSource::External,
    )
}

/// The on-disk daily-bar cache path for `instrument_id`, under `data_dir`:
/// `<data_dir>/BTCUSDT_1M.msgpack` for `BTCUSDT-LINEAR.BYBIT`. The `_1M` suffix
/// is historical — the key encodes neither the venue nor the timeframe, and
/// only daily Bybit-linear bars are ever fetched.
pub fn bar_cache_path(data_dir: &Path, instrument_id: &InstrumentId) -> PathBuf {
    let sym = instrument_id.symbol.as_str();
    let bare = sym.split_once('-').map(|(b, _)| b).unwrap_or(sym);
    data_dir.join(format!("{bare}_1M.msgpack"))
}

fn cache_path(instrument_id: &InstrumentId) -> PathBuf {
    bar_cache_path(Path::new(DATA_DIR), instrument_id)
}

/// The Bybit linear-perp instrument id for a base asset
/// (`BTC` → `BTCUSDT-LINEAR.BYBIT`).
pub fn linear_perp_id(base: &str) -> InstrumentId {
    InstrumentId::from(format!("{base}USDT-LINEAR.BYBIT").as_str())
}

/// Path to the linear-instruments snapshot under `data_dir`.
pub fn instruments_snapshot_path(data_dir: &Path) -> PathBuf {
    data_dir.join("instruments.json")
}

fn cache_is_fresh(bars: &[Bar]) -> bool {
    let Some(last) = bars.last() else {
        return false;
    };
    let last_ms = last.ts_event.as_u64() / 1_000_000;
    let now_ms = Utc::now().timestamp_millis() as u64;
    let age_hours = now_ms.saturating_sub(last_ms) / 3_600_000;
    age_hours <= STALE_AFTER_HOURS as u64
}

/// Fetch all Bybit linear instruments, once, and cache them on the client.
/// Subsequent per-symbol calls reuse this seeded cache so we don't burn a
/// full instruments request for every ticker.
pub async fn fetch_linear_instruments() -> Result<Vec<InstrumentAny>> {
    let client = BybitHttpClient::default();
    let instruments = client
        .request_instruments(BybitProductType::Linear, None, None)
        .await
        .context("bybit request_instruments")?;
    client.cache_instruments(&instruments);
    Ok(instruments)
}

/// Fetch the monthly bar history for `instrument_id`. Uses a shared client
/// that has had instruments seeded by `fetch_linear_instruments`, so we
/// don't refetch the instruments list per symbol.
/// Results are cached on disk in Nautilus msgpack; subsequent calls within
/// `STALE_AFTER_HOURS` of the last bar skip the network.
pub async fn fetch_bars_cached(
    instrument_id: InstrumentId,
    aggregation: BarAggregation,
) -> Result<Vec<Bar>> {
    fs::create_dir_all(DATA_DIR).ok();

    let bar_type = get_bar_type(instrument_id, aggregation);
    let path = cache_path(&instrument_id);
    if let Ok(bytes) = fs::read(&path)
        && let Ok(bars) = rmp_serde::from_slice::<Vec<Bar>>(&bytes)
        && cache_is_fresh(&bars)
    {
        println!("[data] cache hit: {instrument_id} ({} bars)", bars.len());
        return Ok(bars);
    }

    println!("[data] fetching: {instrument_id}");
    let client = shared_client();
    let bars = client
        .request_bars(BybitProductType::Linear, bar_type, None, None, None, true)
        .await
        .context("bybit request_bars")?;

    // Pace cold fetches to stay under Bybit's per-second rate caps.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    write_bar_cache(Path::new(DATA_DIR), &instrument_id, &bars)?;

    println!("[data] cached {} bars for {instrument_id}", bars.len());
    Ok(bars)
}

/// Write `bars` to the on-disk cache for `instrument_id` under `data_dir`, via a
/// `.tmp` + rename so a crash can't leave a half-written file.
pub fn write_bar_cache(data_dir: &Path, instrument_id: &InstrumentId, bars: &[Bar]) -> Result<()> {
    fs::create_dir_all(data_dir).ok();
    let path = bar_cache_path(data_dir, instrument_id);
    let bytes = rmp_serde::to_vec_named(bars)?;
    let tmp = path.with_extension("msgpack.tmp");
    fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Whether the on-disk bar cache for `instrument_id` under `data_dir` exists and
/// its last bar is within [`STALE_AFTER_HOURS`] — the check `xsec fetch` uses to
/// count a symbol as already cached rather than freshly downloaded.
pub fn bar_cache_is_fresh(data_dir: &Path, instrument_id: &InstrumentId) -> bool {
    let path = bar_cache_path(data_dir, instrument_id);
    let Ok(bytes) = fs::read(&path) else {
        return false;
    };
    let Ok(bars) = rmp_serde::from_slice::<Vec<Bar>>(&bytes) else {
        return false;
    };
    cache_is_fresh(&bars)
}

/// Write the linear-instruments snapshot `xsec fetch` produces. JSON, not
/// msgpack: `InstrumentAny`'s `Currency` fields deserialize through a strict
/// registry lookup, and [`read_instruments_snapshot`] walks the JSON to
/// pre-register the base coins Bybit lists outside nautilus's built-in table
/// before the real decode.
pub fn write_instruments_snapshot(data_dir: &Path, instruments: &[InstrumentAny]) -> Result<()> {
    fs::create_dir_all(data_dir).ok();
    let path = instruments_snapshot_path(data_dir);
    let bytes = serde_json::to_vec(instruments).context("serialize instruments snapshot")?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Read the snapshot written by [`write_instruments_snapshot`], registering
/// every currency code it mentions as a crypto currency first so the strict
/// `Currency` deserialize inside `InstrumentAny` can't miss on a freshly-listed
/// Bybit base coin.
pub fn read_instruments_snapshot(data_dir: &Path) -> Result<Vec<InstrumentAny>> {
    let path = instruments_snapshot_path(data_dir);
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "no instruments snapshot at {} — run `xsec fetch --universe <file>` first",
            path.display()
        )
    })?;
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    prime_currencies(&raw);
    serde_json::from_value(raw)
        .with_context(|| format!("decode instruments from {}", path.display()))
}

/// Register every currency code `value` mentions (recursively) as a crypto
/// currency, so the strict `Currency` deserialize inside `InstrumentAny` can't
/// miss. `InstrumentAny` carries currencies two ways: bare codes under
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

fn shared_client() -> BybitHttpClient {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<BybitHttpClient> = OnceLock::new();
    CLIENT.get_or_init(BybitHttpClient::default).clone()
}

/// Seed the shared bybit client with the instruments list from
/// `fetch_linear_instruments`. Must be called once before the first
/// `fetch_bars_cached` so `request_bars` can resolve symbols.
pub fn seed_instruments(instruments: &[InstrumentAny]) {
    shared_client().cache_instruments(instruments);
}

/// The production [`MarketData`]: Bybit's HTTP API with the on-disk bar cache.
/// Owns a Tokio runtime so the backtest bootstrap can stay synchronous —
/// `instruments` and `bars` block on the async fetch functions above.
pub struct BybitMarketData {
    rt: tokio::runtime::Runtime,
}

impl BybitMarketData {
    /// Create the adapter and its Tokio runtime.
    pub fn new() -> Result<Self> {
        Ok(Self {
            rt: tokio::runtime::Runtime::new().context("tokio runtime for BybitMarketData")?,
        })
    }
}

impl MarketData for BybitMarketData {
    fn instruments(&self) -> Result<Vec<InstrumentAny>> {
        let instruments = self.rt.block_on(fetch_linear_instruments())?;
        // Prime the shared client so `bars` can resolve symbols.
        seed_instruments(&instruments);
        Ok(instruments)
    }

    fn bars(&self, instrument_id: InstrumentId, aggregation: BarAggregation) -> Result<Vec<Bar>> {
        self.rt.block_on(fetch_bars_cached(instrument_id, aggregation))
    }
}
