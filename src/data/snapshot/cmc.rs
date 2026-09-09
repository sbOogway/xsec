//! CoinMarketCap snapshots on disk: [`CmcSnapshotData`] reads the
//! `coins/cmc/<YYYYMMDD>.csv` files the scrape writes, [`Resolution`] loads the
//! committed `coins/cmc_resolution.csv` symbol map, and [`derive_universe`]
//! turns a snapshot range into the Bybit base set a `--source coinmarketcap`
//! run trades.
//!
//! No network — the scrape (`scripts/coinmarketcap_snapshot_scrape.sh`) and the
//! resolution map (`make cmc_resolution`) produce these files; this module only
//! reads them.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use chrono::NaiveDate;

use super::{SnapshotData, SnapshotRow};

/// The `coins/cmc/<YYYYMMDD>.csv` snapshots, loaded once into memory.
#[derive(Debug)]
pub struct CmcSnapshotData {
    dates: Vec<NaiveDate>,
    by_date: BTreeMap<NaiveDate, Vec<SnapshotRow>>,
}

impl CmcSnapshotData {
    /// Load every `<YYYYMMDD>.csv` under `dir`. Errors if the directory is
    /// absent or holds no snapshot, naming `make snapshot_cmc_history`.
    pub fn open(dir: &Path) -> Result<Self> {
        if !dir.is_dir() {
            bail!(
                "no CoinMarketCap snapshots at {} — run `make snapshot_cmc_history` first",
                dir.display()
            );
        }

        let mut by_date = BTreeMap::new();
        for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("csv") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(date) = NaiveDate::parse_from_str(stem, "%Y%m%d") else {
                continue; // e.g. coins/cmc_resolution.csv is not a dated snapshot
            };
            let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let rows = parse_snapshot(&text).with_context(|| format!("parse {}", path.display()))?;
            by_date.insert(date, rows);
        }

        ensure!(
            !by_date.is_empty(),
            "no <YYYYMMDD>.csv snapshots under {} — run `make snapshot_cmc_history` first",
            dir.display()
        );

        Ok(Self {
            dates: by_date.keys().copied().collect(),
            by_date,
        })
    }
}

impl SnapshotData for CmcSnapshotData {
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

/// Parse one snapshot CSV
/// (`rank,cmc_id,symbol,name,market_cap_usd,price_usd,volume_24h_usd,pct_change_1h,pct_change_24h,pct_change_7d`).
/// Only `rank` / `cmc_id` / `symbol` are kept.
fn parse_snapshot(text: &str) -> Result<Vec<SnapshotRow>> {
    let mut lines = text.lines();
    let header = lines.next().context("empty snapshot file")?;
    ensure!(
        header.starts_with("rank,cmc_id,symbol,"),
        "unexpected snapshot header: {header}"
    );

    let mut rows = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_csv_line(line);
        ensure!(fields.len() >= 3, "snapshot row has < 3 fields: {line}");
        rows.push(SnapshotRow {
            rank: fields[0]
                .parse()
                .with_context(|| format!("rank in row: {line}"))?,
            cmc_id: fields[1]
                .parse()
                .with_context(|| format!("cmc_id in row: {line}"))?,
            cmc_symbol: fields[2].clone(),
        });
    }
    Ok(rows)
}

/// Split one CSV record, honouring `"…"` quoting and `""` escapes (the scrape
/// emits `@csv`, which quotes the `name` column — the only one that can hold a
/// comma).
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

// --- resolution map --------------------------------------------------------

/// The committed `coins/cmc_resolution.csv`: raw CoinMarketCap symbol → Bybit
/// base coin (or `None` for a stablecoin / a name with no Bybit perp).
#[derive(Debug)]
pub struct Resolution {
    map: HashMap<String, Option<String>>,
}

impl Resolution {
    /// Load `coins/cmc_resolution.csv` (`cmc_symbol,bybit_base,status`). Errors
    /// naming `make cmc_resolution` if the file is absent.
    pub fn open(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| {
            format!(
                "no CMC resolution map at {} — run `make cmc_resolution` first",
                path.display()
            )
        })?;

        let mut lines = text.lines();
        let header = lines.next().unwrap_or_default();
        ensure!(
            header == "cmc_symbol,bybit_base,status",
            "unexpected resolution header in {}: {header}",
            path.display()
        );

        let mut map = HashMap::new();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.splitn(3, ',').collect();
            ensure!(
                fields.len() == 3,
                "resolution row has != 3 fields in {}: {line}",
                path.display()
            );
            let bybit_base = (!fields[1].is_empty()).then(|| fields[1].to_string());
            map.insert(fields[0].to_string(), bybit_base);
        }
        Ok(Self { map })
    }

    /// The Bybit base coin for a raw CMC symbol, or `None` if it is unknown,
    /// a stablecoin, or has no Bybit perp.
    #[must_use]
    pub fn bybit_base(&self, cmc_symbol: &str) -> Option<&str> {
        self.map.get(cmc_symbol).and_then(|b| b.as_deref())
    }
}

// --- universe derivation -------------------------------------------------

/// The Bybit base coins a `--source coinmarketcap` run loads bars for: every
/// coin in the top-`top_n` of a snapshot the run could see over
/// `[start, end]` (every snapshot in the window, plus the one the first
/// rebalance forward-fills to), that resolves to a Bybit base, and that
/// `xsec fetch` has usable bars for (`listed_bases`, from the manifest).
///
/// Sorted and de-duplicated.
#[must_use]
pub fn derive_universe(
    snapshots: &dyn SnapshotData,
    resolution: &Resolution,
    listed_bases: &HashSet<String>,
    start: NaiveDate,
    end: NaiveDate,
    top_n: usize,
) -> Vec<String> {
    // Every snapshot date that could be `snapshot_asof(T)` for some
    // `T ∈ [start, end]`: those inside the window, plus the as-of date at
    // `start` (which a rebalance on `start` forward-fills to).
    let mut dates: BTreeSet<NaiveDate> = snapshots
        .dates()
        .iter()
        .copied()
        .filter(|d| *d >= start && *d <= end)
        .collect();
    if let Some((asof, _)) = snapshots.snapshot_asof(start) {
        dates.insert(asof);
    }

    let mut bases = BTreeSet::new();
    for date in dates {
        let Some((_, rows)) = snapshots.snapshot_asof(date) else {
            continue;
        };
        for row in rows.iter().take(top_n) {
            if let Some(base) = resolution.bybit_base(&row.cmc_symbol)
                && listed_bases.contains(base)
            {
                bases.insert(base.to_string());
            }
        }
    }
    bases.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;
    use crate::data::snapshot::InMemorySnapshotData;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn parse_snapshot_keeps_rank_id_symbol_and_tolerates_quoted_names() {
        let text = "rank,cmc_id,symbol,name,market_cap_usd,price_usd,volume_24h_usd,pct_change_1h,pct_change_24h,pct_change_7d\n\
                    1,1,\"BTC\",\"Bitcoin\",8e11,43943.09,1.9e10,0.09,-0.10,3.97\n\
                    2,7083,\"UNI\",\"Uniswap, Inc\",4e9,6.4,1e8,,,\n";
        let rows = parse_snapshot(text).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], SnapshotRow { rank: 1, cmc_id: 1, cmc_symbol: "BTC".into() });
        // the comma inside the quoted name does not shift the symbol column
        assert_eq!(rows[1].cmc_symbol, "UNI");
        assert_eq!(rows[1].cmc_id, 7083);
    }

    #[test]
    fn cmc_snapshot_data_loads_a_dir_and_skips_non_dated_csvs() {
        let dir = tempdir().unwrap();
        for (name, sym) in [("20240107.csv", "ETH"), ("20240114.csv", "SOL")] {
            let mut f = fs::File::create(dir.path().join(name)).unwrap();
            writeln!(f, "rank,cmc_id,symbol,name,market_cap_usd,price_usd,volume_24h_usd,pct_change_1h,pct_change_24h,pct_change_7d").unwrap();
            writeln!(f, "1,1,\"BTC\",\"Bitcoin\",1,1,1,,,").unwrap();
            writeln!(f, "2,2,\"{sym}\",\"x\",1,1,1,,,").unwrap();
        }
        // a non-dated CSV in the same dir is ignored
        fs::write(dir.path().join("cmc_resolution.csv"), "cmc_symbol,bybit_base,status\n").unwrap();

        let data = CmcSnapshotData::open(dir.path()).unwrap();
        assert_eq!(data.dates(), [d("2024-01-07"), d("2024-01-14")]);
        assert_eq!(data.snapshot_asof(d("2024-01-10")).unwrap().1[1].cmc_symbol, "ETH");
    }

    #[test]
    fn cmc_snapshot_data_errors_without_snapshots() {
        let dir = tempdir().unwrap();
        let err = CmcSnapshotData::open(dir.path()).unwrap_err().to_string();
        assert!(err.contains("snapshot_cmc_history"), "{err}");
    }

    #[test]
    fn resolution_maps_symbols_and_marks_the_rest_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cmc_resolution.csv");
        fs::write(
            &path,
            "cmc_symbol,bybit_base,status\nBTC,BTC,listed\nMATIC,POL,listed\nUSDT,,stablecoin\nLEO,,unlisted\n",
        )
        .unwrap();

        let res = Resolution::open(&path).unwrap();
        assert_eq!(res.bybit_base("BTC"), Some("BTC"));
        assert_eq!(res.bybit_base("MATIC"), Some("POL"));
        assert_eq!(res.bybit_base("USDT"), None);
        assert_eq!(res.bybit_base("LEO"), None);
        assert_eq!(res.bybit_base("NOPE"), None);
    }

    #[test]
    fn derive_universe_respects_top_n_window_and_manifest() {
        let snapshots = InMemorySnapshotData::new(BTreeMap::from([
            (
                d("2024-01-07"),
                vec![
                    SnapshotRow { rank: 1, cmc_id: 1, cmc_symbol: "BTC".into() },
                    SnapshotRow { rank: 2, cmc_id: 2, cmc_symbol: "MATIC".into() },
                    SnapshotRow { rank: 3, cmc_id: 3, cmc_symbol: "LEO".into() }, // unresolved
                ],
            ),
            (
                d("2024-02-04"),
                vec![
                    SnapshotRow { rank: 1, cmc_id: 1, cmc_symbol: "BTC".into() },
                    SnapshotRow { rank: 2, cmc_id: 9, cmc_symbol: "SOL".into() }, // out of window below
                ],
            ),
        ]));
        let dir = tempdir().unwrap();
        let path = dir.path().join("r.csv");
        fs::write(
            &path,
            "cmc_symbol,bybit_base,status\nBTC,BTC,listed\nMATIC,POL,listed\nSOL,SOL,listed\n",
        )
        .unwrap();
        let res = Resolution::open(&path).unwrap();
        let listed = HashSet::from(["BTC".to_string(), "POL".to_string()]); // SOL fetched-failed

        // window excludes the Feb snapshot; top_n = 2 excludes LEO anyway
        let uni = derive_universe(&snapshots, &res, &listed, d("2024-01-01"), d("2024-01-31"), 2);
        assert_eq!(uni, ["BTC", "POL"]);

        // widen the window: SOL is now in range but not in `listed` -> still excluded
        let uni = derive_universe(&snapshots, &res, &listed, d("2024-01-01"), d("2024-03-01"), 2);
        assert_eq!(uni, ["BTC", "POL"]);
    }
}
