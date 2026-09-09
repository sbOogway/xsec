//! Bybit: the [`BybitAdapter`] `xsec fetch` pulls from, over a shared HTTP
//! client. [`get_bar_type`] is public because the rebalance runtime in
//! [`crate::strategy::common`] also builds bar types; everything else here is
//! the adapter's own plumbing. No disk — the cache layout lives in
//! [`super::cache`].

use std::sync::OnceLock;

use anyhow::{Context, Result};
use nautilus_bybit::{common::enums::BybitProductType, http::client::BybitHttpClient};
use nautilus_model::{
    data::{Bar, BarSpecification, BarType},
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::InstrumentId,
    instruments::InstrumentAny,
};

use crate::data::exchange::ExchangeAdapter;

/// The external daily [`BarType`] for `instrument_id` at `aggregation`.
pub fn get_bar_type(instrument_id: InstrumentId, aggregation: BarAggregation) -> BarType {
    BarType::new(
        instrument_id,
        BarSpecification::new(1, aggregation, PriceType::Last),
        AggregationSource::External,
    )
}

/// The Bybit [`ExchangeAdapter`]: the HTTP calls below, plus a Tokio runtime so
/// [`fetch::run`](super::fetch::run) can stay synchronous.
pub struct BybitAdapter {
    rt: tokio::runtime::Runtime,
}

impl BybitAdapter {
    /// Create the adapter and its Tokio runtime.
    pub fn new() -> Result<Self> {
        Ok(Self {
            rt: tokio::runtime::Runtime::new().context("tokio runtime for BybitAdapter")?,
        })
    }
}

impl ExchangeAdapter for BybitAdapter {
    fn venue(&self) -> &'static str {
        "BYBIT"
    }

    fn instruments(&self) -> Result<Vec<InstrumentAny>> {
        self.rt.block_on(fetch_linear_instruments())
    }

    fn perp_id(&self, base: &str) -> InstrumentId {
        InstrumentId::from(format!("{base}USDT-LINEAR.BYBIT").as_str())
    }

    fn bars(&self, instrument_id: InstrumentId) -> Result<Vec<Bar>> {
        self.rt.block_on(fetch_bars(instrument_id))
    }
}

/// Fetch every Bybit linear instrument and seed the shared client's cache, so
/// the per-symbol [`fetch_bars`] calls resolve symbols without re-requesting the
/// list.
async fn fetch_linear_instruments() -> Result<Vec<InstrumentAny>> {
    let client = shared_client();
    let instruments = client
        .request_instruments(BybitProductType::Linear, None, None)
        .await
        .context("bybit request_instruments")?;
    client.cache_instruments(&instruments);
    Ok(instruments)
}

/// Fetch the full daily-bar history for `instrument_id`. Requires
/// [`fetch_linear_instruments`] to have run first so the shared client can
/// resolve the symbol.
async fn fetch_bars(instrument_id: InstrumentId) -> Result<Vec<Bar>> {
    let bar_type = get_bar_type(instrument_id, BarAggregation::Day);
    let bars = shared_client()
        .request_bars(BybitProductType::Linear, bar_type, None, None, None, true)
        .await
        .context("bybit request_bars")?;

    // Pace cold fetches to stay under Bybit's per-second rate caps.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    Ok(bars)
}

/// The process-wide Bybit HTTP client. Clones share one instrument cache, so a
/// list seeded by [`fetch_linear_instruments`] is visible to every later
/// [`fetch_bars`].
fn shared_client() -> BybitHttpClient {
    static CLIENT: OnceLock<BybitHttpClient> = OnceLock::new();
    CLIENT.get_or_init(BybitHttpClient::default).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_venue_and_perp_id() {
        let adapter = BybitAdapter::new().unwrap();
        assert_eq!(adapter.venue(), "BYBIT");
        assert_eq!(
            adapter.perp_id("BTC"),
            InstrumentId::from("BTCUSDT-LINEAR.BYBIT"),
        );
    }

    #[test]
    fn get_bar_type_is_one_day_last_external() {
        let bar_type = get_bar_type(InstrumentId::from("BTCUSDT-LINEAR.BYBIT"), BarAggregation::Day);
        assert_eq!(bar_type.to_string(), "BTCUSDT-LINEAR.BYBIT-1-DAY-LAST-EXTERNAL");
    }
}
