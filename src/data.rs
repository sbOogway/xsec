use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, Utc};
use parquet::{
    arrow::{
        ArrowWriter,
        arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder},
    },
};
use serde::{Deserialize, Serialize};

use nautilus_core::nanos::UnixNanos;
use nautilus_model::{
    data::{Bar, BarType, Data},
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::InstrumentId,
    types::{Price, Quantity},
};

const DATA_DIR: &str = "data";
const PAGE: usize = 1000;
const STALE_AFTER_HOURS: i64 = 24;
const LOOKBACK_YEARS: i64 = 5;
const BYBIT_KLINE: &str = "https://api.bybit.com/v5/market/kline";
const MAX_PARALLEL_SYMBOLS: usize = 4;
const BYBIT_CATEGORY: &str = "linear";

#[derive(Serialize, Deserialize, Debug)]
struct CacheMeta {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    fetched_at: DateTime<Utc>,
}

/// Bybit's API expects the bare ticker (e.g. `BTCUSDT`), not the Nautilus
/// `Symbol` which carries class info (`BTCUSDT-LINEAR`).
fn bybit_ticker(instrument_id: &InstrumentId) -> String {
    let sym = instrument_id.symbol.as_str();
    if let Some((base, _rest)) = sym.split_once('-') {
        base.to_string()
    } else {
        sym.to_string()
    }
}

pub async fn ensure_data(instrument_ids: &[InstrumentId]) -> Result<()> {
    fs::create_dir_all(DATA_DIR).with_context(|| format!("create {DATA_DIR} dir"))?;
    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_PARALLEL_SYMBOLS));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut tasks = Vec::with_capacity(instrument_ids.len());
    for id in instrument_ids {
        let sem = sem.clone();
        let client = client.clone();
        let id = *id;
        let ticker = bybit_ticker(&id);
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await?;
            fetch_and_cache(&client, &ticker).await
        }));
    }
    for t in tasks {
        match t.await {
            Ok(Ok(p)) => println!("[data] ready: {}", p.display()),
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(anyhow::anyhow!("join error: {e}")),
        }
    }
    Ok(())
}

async fn fetch_and_cache(client: &reqwest::Client, ticker: &str) -> Result<PathBuf> {
    let parquet_path = PathBuf::from(DATA_DIR).join(format!("{ticker}_1d.parquet"));
    let meta_path = PathBuf::from(DATA_DIR).join(format!("{ticker}_1d.meta.json"));
    if let Some(meta) = read_meta(&meta_path)? {
        if cache_is_fresh(&meta) && parquet_path.exists() {
            println!("[data] cache hit: {ticker}");
            return Ok(parquet_path);
        }
    }
    println!("[data] fetching: {ticker}");
    let rows = paginate_kline(client, ticker).await?;
    let (start_ms, end_ms) = rows
        .first()
        .zip(rows.last())
        .map(|(a, b)| (a.ts_ms, b.ts_ms))
        .unwrap_or((0, 0));
    write_parquet_atomic(&parquet_path, &rows)?;
    let meta = CacheMeta {
        start: DateTime::<Utc>::from_timestamp_millis(start_ms).unwrap_or_else(Utc::now),
        end: DateTime::<Utc>::from_timestamp_millis(end_ms).unwrap_or_else(Utc::now),
        fetched_at: Utc::now(),
    };
    let meta_json = serde_json::to_vec_pretty(&meta)?;
    fs::write(&meta_path, meta_json)
        .with_context(|| format!("write meta {}", meta_path.display()))?;
    Ok(parquet_path)
}

fn read_meta(path: &Path) -> Result<Option<CacheMeta>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    let meta: CacheMeta = serde_json::from_slice(&bytes)?;
    Ok(Some(meta))
}

fn cache_is_fresh(meta: &CacheMeta) -> bool {
    let span = (meta.end - meta.start).num_days();
    let stale = (Utc::now() - meta.end).num_hours() > STALE_AFTER_HOURS;
    span >= 4 * 365 && !stale
}

#[derive(Clone)]
struct BarRow {
    ts_ms: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

async fn paginate_kline(client: &reqwest::Client, symbol: &str) -> Result<Vec<BarRow>> {
    let mut all: Vec<BarRow> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut page_count = 0u32;
    loop {
    let mut req = client
        .get(BYBIT_KLINE)
        .query(&[
            ("category", BYBIT_CATEGORY),
            ("symbol", symbol),
            ("interval", "D"),
            ("limit", &PAGE.to_string()),
        ]);
        if let Some(c) = &cursor {
            req = req.query(&[("cursor", c)]);
        }
        let resp = with_backoff(|| req.try_clone().expect("cloneable").send()).await?;
        let body: serde_json::Value = resp.json().await?;
        let ret_code = body
            .get("retCode")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if ret_code != 0 {
            anyhow::bail!("bybit retCode={ret_code} body={body}");
        }
        let list = body
            .pointer("/result/list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let next = body
            .pointer("/result/nextPageCursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        for entry in &list {
            let arr = entry.as_array();
            let arr = match arr {
                Some(a) => a,
                None => continue,
            };
            if arr.len() < 6 {
                continue;
            }
            let ts_ms = arr[0].as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let open = arr[1].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            let high = arr[2].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            let low = arr[3].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            let close = arr[4].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            let volume = arr[5].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            all.push(BarRow {
                ts_ms,
                open,
                high,
                low,
                close,
                volume,
            });
        }
        page_count += 1;
        let full_page = list.len() >= PAGE;
        let have_cursor = next.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        if !(full_page && have_cursor) {
            break;
        }
        cursor = next;
    }
    all.sort_by_key(|r| r.ts_ms);
    if all.len() < 100 {
        anyhow::bail!("only fetched {} rows for {symbol}", all.len());
    }
    println!("[data]   {symbol}: {} bars in {page_count} pages", all.len());
    Ok(all)
}

async fn with_backoff<F, Fut>(mut op: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = reqwest::Result<reqwest::Response>>,
{
    let mut delay = Duration::from_millis(250);
    for _ in 0..4 {
        match op().await {
            Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
            Ok(r) => return Ok(r),
            Err(_) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
        }
    }
    op().await.context("reqwest failed after retries")
}

fn write_parquet_atomic(final_path: &Path, rows: &[BarRow]) -> Result<()> {
    let tmp = final_path.with_extension("parquet.tmp");
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts_ms", DataType::Int64, false),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
    ]));
    let ts: ArrayRef = Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.ts_ms)));
    let open: ArrayRef = Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.open)));
    let high: ArrayRef = Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.high)));
    let low: ArrayRef = Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.low)));
    let close: ArrayRef = Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.close)));
    let volume: ArrayRef = Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.volume)));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![ts, open, high, low, close, volume],
    )?;
    let file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    fs::rename(&tmp, final_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), final_path.display()))?;
    Ok(())
}

fn read_parquet(path: &Path) -> Result<Vec<BarRow>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let batch_reader: ParquetRecordBatchReader = builder.build()?;
    let mut out: Vec<BarRow> = Vec::new();
    for batch_res in batch_reader {
        let batch = batch_res?;
        let ts = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("ts_ms col")?;
        let open = batch
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("open col")?;
        let high = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("high col")?;
        let low = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("low col")?;
        let close = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("close col")?;
        let volume = batch
            .column(5)
            .as_any()
            .downcast_ref::<Float64Array>()
            .context("volume col")?;
        for i in 0..batch.num_rows() {
            out.push(BarRow {
                ts_ms: ts.value(i),
                open: open.value(i),
                high: high.value(i),
                low: low.value(i),
                close: close.value(i),
                volume: volume.value(i),
            });
        }
    }
    Ok(out)
}

pub fn load_data(instrument_id: &InstrumentId) -> Result<Vec<Data>> {
    let ticker = bybit_ticker(instrument_id);
    let parquet_path = PathBuf::from(DATA_DIR).join(format!("{ticker}_1d.parquet"));
    let rows = read_parquet(&parquet_path)?;
    let spec = nautilus_model::data::BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(*instrument_id, spec, AggregationSource::External);
    let price_prec = 2u8;
    let size_prec = 4u8;
    let mut out: Vec<Data> = Vec::with_capacity(rows.len());
    for r in &rows {
        let ts_ns: u64 = (r.ts_ms.max(0) as u64).saturating_mul(1_000_000);
        let ts = UnixNanos::from(ts_ns);
        let open = Price::new(r.open, price_prec);
        let high = Price::new(r.high, price_prec);
        let low = Price::new(r.low, price_prec);
        let close = Price::new(r.close, price_prec);
        let volume = Quantity::new(r.volume, size_prec);
        let bar = Bar::new(bar_type, open, high, low, close, volume, ts, ts);
        out.push(Data::Bar(bar));
    }
    Ok(out)
}

#[allow(dead_code)]
pub fn lookback_years() -> i64 {
    LOOKBACK_YEARS
}
