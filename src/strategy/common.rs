//! Helpers shared by more than one strategy in [`crate::strategy`].

use nautilus_model::identifiers::InstrumentId;
use rust_decimal::Decimal;

use crate::data::structure::BoundedQueue;

/// The `N`-day return `(P_now - P_{now-N}) / P_{now-N}`, needing `N + 1`
/// price points in `queue`. `None` if the buffer isn't deep enough yet
/// (still warming up) or the reference price is zero.
pub fn n_day_return(queue: &BoundedQueue<Decimal>, days: usize) -> Option<Decimal> {
    let len = queue.inner.len();
    if len <= days {
        return None;
    }
    let now = *queue.inner.back()?;
    let past = *queue.inner.get(len - 1 - days)?;
    (!past.is_zero()).then(|| (now - past) / past)
}

/// The instrument id for `BTC` within `bases`, matched case-insensitively.
/// Callers should validate `bases` contains it at config-build time.
pub fn btc_instrument_id(bases: &[String], venue: &str) -> InstrumentId {
    let base = bases
        .iter()
        .find(|base| base.eq_ignore_ascii_case("BTC"))
        .expect("BTC must be present in the universe");
    InstrumentId::from(format!("{base}USDT-LINEAR.{venue}").as_str())
}
