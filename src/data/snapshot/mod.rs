//! Point-in-time market-cap snapshots: the read seam a strategy uses to gate
//! its universe by an external ranking, plus a fixture fake.
//!
//! Sibling of [`super::exchange`] and shaped like [`super::exchange::MarketData`]
//! — offline, sync, venue-agnostic. The one real impl,
//! [`cmc::CmcSnapshotData`], reads the `coins/cmc/<YYYYMMDD>.csv` files
//! `scripts/coinmarketcap_snapshot_scrape.sh` writes; [`cmc::Resolution`] maps a
//! raw CoinMarketCap symbol to its Bybit base coin (via the committed
//! `coins/cmc_resolution.csv`), and [`cmc::derive_universe`] turns a snapshot
//! range into the Bybit instrument set a `momentum --source coinmarketcap` run
//! loads bars for.

pub mod cmc;

use std::collections::BTreeMap;

use chrono::NaiveDate;

/// One ranked coin in a daily snapshot. The signal reads `rank` (for the
/// top-N cut) and `cmc_symbol` (for resolution); `cmc_id` and `rank` are also
/// carried into `runs/<uuid>/cmc_selection.csv`.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotRow {
    pub rank: u32,
    pub cmc_id: u64,
    pub cmc_symbol: String,
}

/// A source of point-in-time top-N-by-market-cap snapshots.
///
/// Consumed once, at `on_start`, by the momentum strategy's `--source
/// coinmarketcap` path. [`snapshot_asof`](Self::snapshot_asof) forward-fills so
/// a rebalance on a date with no published snapshot (a scrape gap, or a weekly
/// cadence landing mid-week) still resolves.
pub trait SnapshotData {
    /// The snapshot dates available, ascending.
    fn dates(&self) -> &[NaiveDate];

    /// The ranked rows of the most recent snapshot on or before `date`, with
    /// that snapshot's own date. `None` if `date` precedes the first snapshot.
    fn snapshot_asof(&self, date: NaiveDate) -> Option<(NaiveDate, &[SnapshotRow])>;
}

/// A [`SnapshotData`] backed by an in-memory map — no disk. Lets a test drive
/// the `--source coinmarketcap` path over fixtures, alongside
/// [`InMemoryMarketData`](super::exchange::InMemoryMarketData).
#[derive(Debug)]
pub struct InMemorySnapshotData {
    dates: Vec<NaiveDate>,
    by_date: BTreeMap<NaiveDate, Vec<SnapshotRow>>,
}

impl InMemorySnapshotData {
    #[must_use]
    pub fn new(by_date: BTreeMap<NaiveDate, Vec<SnapshotRow>>) -> Self {
        Self {
            dates: by_date.keys().copied().collect(),
            by_date,
        }
    }
}

impl SnapshotData for InMemorySnapshotData {
    fn dates(&self) -> &[NaiveDate] {
        &self.dates
    }

    fn snapshot_asof(&self, date: NaiveDate) -> Option<(NaiveDate, &[SnapshotRow])> {
        self.by_date
            .range(..=date)
            .next_back()
            .map(|(d, rows)| (*d, rows.as_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn row(rank: u32, sym: &str) -> SnapshotRow {
        SnapshotRow {
            rank,
            cmc_id: rank as u64,
            cmc_symbol: sym.to_string(),
        }
    }

    #[test]
    fn snapshot_asof_forward_fills_and_bounds_below() {
        let data = InMemorySnapshotData::new(BTreeMap::from([
            (d("2024-01-07"), vec![row(1, "BTC"), row(2, "ETH")]),
            (d("2024-01-14"), vec![row(1, "BTC"), row(2, "SOL")]),
        ]));

        assert!(data.snapshot_asof(d("2024-01-06")).is_none(), "before the first");
        assert_eq!(data.snapshot_asof(d("2024-01-07")).unwrap().0, d("2024-01-07"));
        // a mid-week date forward-fills to the prior snapshot
        assert_eq!(data.snapshot_asof(d("2024-01-10")).unwrap().0, d("2024-01-07"));
        assert_eq!(data.snapshot_asof(d("2024-01-14")).unwrap().0, d("2024-01-14"));
        assert_eq!(data.snapshot_asof(d("2025-06-01")).unwrap().0, d("2024-01-14"));
        assert_eq!(data.dates(), [d("2024-01-07"), d("2024-01-14")]);
    }
}
