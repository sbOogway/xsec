use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use nautilus_bybit::{common::enums::BybitProductType, http::client::BybitHttpClient};
use nautilus_model::{
    data::{Bar, BarSpecification, BarType},
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::InstrumentId,
    instruments::InstrumentAny,
};

const DATA_DIR: &str = "data";
const STALE_AFTER_HOURS: i64 = 24;

pub fn get_bar_type(instrument_id: InstrumentId, aggregation: BarAggregation) -> BarType {
    BarType::new(
        instrument_id,
        BarSpecification::new(1, aggregation, PriceType::Last),
        AggregationSource::External,
    )
}
// cons

fn cache_path(instrument_id: &InstrumentId) -> PathBuf {
    let sym = instrument_id.symbol.as_str();
    let bare = sym.split_once('-').map(|(b, _)| b).unwrap_or(sym);
    PathBuf::from(DATA_DIR).join(format!("{bare}_1M.msgpack"))
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

    let bytes = rmp_serde::to_vec_named(&bars)?;
    let tmp = path.with_extension("msgpack.tmp");
    fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;

    println!("[data] cached {} bars for {instrument_id}", bars.len());
    Ok(bars)
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

#[allow(dead_code)]
pub fn parse_date(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .with_context(|| format!("parse date {s}"))
}

pub mod structure {
    use std::collections::VecDeque;

    // #[derive(Clone)]
    pub struct BoundedQueue<T> {
        pub inner: VecDeque<T>,
        capacity: usize,
    }

    impl<T> BoundedQueue<T> {
        pub fn new(capacity: usize) -> Self {
            Self {
                inner: VecDeque::with_capacity(capacity),
                capacity,
            }
        }

        // pub fn get(self) -> VecDeque<T> {
        //     // self.inner.clone()
        // }

        // VARIANT A: Reject the item if full
        pub fn try_push_back(&mut self, item: T) -> Result<(), T> {
            if self.inner.len() >= self.capacity {
                return Err(item); // Return item back to caller
            }
            self.inner.push_back(item);
            Ok(())
        }

        // VARIANT B: Evict the oldest item (ring buffer behavior)
        pub fn push_back_overwrite(&mut self, item: T) -> Option<T> {
            let mut evicted = None;
            if self.inner.len() >= self.capacity {
                evicted = self.inner.pop_front(); // Evict oldest
            }
            self.inner.push_back(item);
            evicted
        }

        pub fn pop_front(&mut self) -> Option<T> {
            self.inner.pop_front()
        }
    }
}
