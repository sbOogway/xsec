//! Bybit HTTP: bar-type construction and the shared client that fetches
//! linear-perp instruments and daily bar history.
//!
//! No disk — the on-disk cache layout lives in [`super::cache`], and `xsec fetch`
//! ([`super::fetch`]) is what drives this module to fill it. [`get_bar_type`] is
//! also used by the rebalance runtime in [`crate::strategy::common`].

use std::sync::OnceLock;

use anyhow::{Context, Result};
use nautilus_bybit::{common::enums::BybitProductType, http::client::BybitHttpClient};
use nautilus_model::{
    data::{Bar, BarSpecification, BarType},
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::InstrumentId,
    instruments::InstrumentAny,
};

/// The external daily [`BarType`] for `instrument_id` at `aggregation`.
pub fn get_bar_type(instrument_id: InstrumentId, aggregation: BarAggregation) -> BarType {
    BarType::new(
        instrument_id,
        BarSpecification::new(1, aggregation, PriceType::Last),
        AggregationSource::External,
    )
}

/// The Bybit linear-perp instrument id for a base asset
/// (`BTC` → `BTCUSDT-LINEAR.BYBIT`).
pub fn linear_perp_id(base: &str) -> InstrumentId {
    InstrumentId::from(format!("{base}USDT-LINEAR.BYBIT").as_str())
}

/// Fetch every Bybit linear instrument and seed the shared client's cache, so
/// the per-symbol [`fetch_bars`] calls can resolve symbols without re-requesting
/// the list.
pub async fn fetch_linear_instruments() -> Result<Vec<InstrumentAny>> {
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
pub async fn fetch_bars(instrument_id: InstrumentId) -> Result<Vec<Bar>> {
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
    fn linear_perp_id_builds_the_bybit_usdt_perp() {
        assert_eq!(
            linear_perp_id("BTC"),
            InstrumentId::from("BTCUSDT-LINEAR.BYBIT"),
        );
    }

    #[test]
    fn get_bar_type_is_one_day_last_external() {
        let bar_type = get_bar_type(InstrumentId::from("BTCUSDT-LINEAR.BYBIT"), BarAggregation::Day);
        assert_eq!(
            bar_type.to_string(),
            "BTCUSDT-LINEAR.BYBIT-1-DAY-LAST-EXTERNAL",
        );
    }
}
