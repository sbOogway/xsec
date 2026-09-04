//! Schema-contract smoke test for run-artifact capture.
//!
//! The issue spec (#1, #4, #6) is explicit that "the schema is the contract":
//! downstream tooling (`analysis/tearsheet.py`) relies on exact headers and the
//! `portfolio.net_return` arithmetic. This test drives [`RunCapture`] through a
//! 2-symbol, 6-month scenario and asserts that contract, without booting the
//! full backtest engine (which would need network/data fixtures and be
//! non-deterministic).

use std::collections::HashMap;

use rust_decimal::Decimal;
use tempfile::tempdir;

use nautilus_model::{enums::OrderSide, identifiers::InstrumentId};
use xsectional_rs::capture::{RunCapture, RunConfig, YearMonth};

// The schema downstream tooling depends on — pinned here as literals so a
// change to the capture headers has to be a deliberate change to this test too.
const LEGS_HEADER: &str =
    "run_id,month,instrument_id,side,entry_price,exit_price,per_leg_return,notional_usdt";
const PORTFOLIO_HEADER: &str = "run_id,month,n_long,n_short,gross_return,fee_paid_usdt,net_return,equity_end_of_month_usdt,n_fills,fills_ref";
const FILLS_HEADER: &str =
    "run_id,ts_event,instrument_id,side,order_side,quantity,fill_price,fee_usdt";

const RUN_ID: &str = "test-0000-run";
const MONTHS: [YearMonth; 6] = [
    YearMonth { year: 2025, month: 1 },
    YearMonth { year: 2025, month: 2 },
    YearMonth { year: 2025, month: 3 },
    YearMonth { year: 2025, month: 4 },
    YearMonth { year: 2025, month: 5 },
    YearMonth { year: 2025, month: 6 },
];

fn month_start_nanos(m: YearMonth) -> u64 {
    // 2025-MM-01T00:00:00Z, good enough for month bucketing.
    let days_before_month: i64 = match m.month {
        1 => 0,
        2 => 31,
        3 => 59,
        4 => 90,
        5 => 120,
        6 => 151,
        _ => unreachable!(),
    };
    let days_since_epoch = 20089 + days_before_month; // 2025-01-01 == day 20089
    (days_since_epoch as u64) * 86_400 * 1_000_000_000
}

#[test]
fn capture_writes_the_contract() {
    let dir = tempdir().unwrap();
    let long = InstrumentId::from("BTCUSDT-LINEAR.BYBIT");
    let short = InstrumentId::from("ETHUSDT-LINEAR.BYBIT");

    let cfg = RunConfig {
        run_id: RUN_ID.to_string(),
        lookback_months: 3,
        holding_months: 1,
        percentile: "0.1".to_string(),
        date_start: "2025-01-01".to_string(),
        date_end: "2025-07-01".to_string(),
        bases: vec!["BTC".to_string(), "ETH".to_string()],
        starting_balance: "1000 USDT".to_string(),
        risk_pct: 1.0,
        long_w: 0.5,
        signal_tilt: 0.0,
        universe_path: "universe.txt".to_string(),
        argv: "xsectional-rs --uuid test-0000-run".to_string(),
    };

    let mut capture = RunCapture::open_in(dir.path(), &cfg).unwrap();

    // Exit mark for every leg: long +10%, short instrument -10% (=> short leg +10%).
    let mut exits: HashMap<InstrumentId, Decimal> = HashMap::new();
    exits.insert(long, Decimal::from(110));
    exits.insert(short, Decimal::from(90));

    let equity = Decimal::from(1000);
    let fee_per_month = Decimal::from(1); // 1 USDT total fees / month

    for m in MONTHS {
        let ts = month_start_nanos(m);
        capture.record_fill_row(ts, long, OrderSide::Buy, Decimal::from(1), Decimal::from(100), fee_per_month / Decimal::from(2));
        capture.record_fill_row(ts, short, OrderSide::Sell, Decimal::from(1), Decimal::from(100), fee_per_month / Decimal::from(2));

        capture.record_rebalance(
            m,
            equity,
            vec![
                (long, OrderSide::Buy, Decimal::from(100), 50.0),
                (short, OrderSide::Sell, Decimal::from(100), 50.0),
            ],
        );
        capture.finalise_completed(m, &exits, equity);
    }
    capture.finish(&exits, equity);
    drop(capture);

    let run_dir = dir.path().join(RUN_ID);
    let legs = read_csv(run_dir.join("legs.csv"));
    let portfolio = read_csv(run_dir.join("portfolio.csv"));
    let fills = read_csv(run_dir.join("fills.csv"));
    let config = read_csv(run_dir.join("config.csv"));

    // --- headers are the contract ---
    assert_eq!(legs.header, LEGS_HEADER);
    assert_eq!(portfolio.header, PORTFOLIO_HEADER);
    assert_eq!(fills.header, FILLS_HEADER);
    assert_eq!(config.header, "key,value");

    // --- row counts within expected bounds ---
    assert_eq!(portfolio.rows.len(), 6, "one portfolio row per entry month");
    assert_eq!(legs.rows.len(), 12, "two legs per month for six months");
    assert_eq!(fills.rows.len(), 12, "two fills per month for six months");

    // --- config sidecar carries the run id and params ---
    let cfg_map: HashMap<&str, &str> = config
        .rows
        .iter()
        .map(|r| (r[0].as_str(), r[1].as_str()))
        .collect();
    assert_eq!(cfg_map["run_id"], RUN_ID);
    assert_eq!(cfg_map["lookback_months"], "3");
    assert_eq!(cfg_map["holding_months"], "1");
    assert_eq!(cfg_map["bases"], "BTC ETH");
    assert_eq!(cfg_map["universe_path"], "universe.txt");
    assert_eq!(cfg_map["argv"], "xsectional-rs --uuid test-0000-run");

    // --- legs schema: month label, side vocabulary, 6dp returns ---
    for row in &legs.rows {
        assert_eq!(row.len(), 8);
        assert_eq!(&row[0], RUN_ID);
        assert!(row[1].starts_with("2025-"), "month is YYYY-MM: {}", row[1]);
        assert!(matches!(row[3].as_str(), "long" | "short"), "side: {}", row[3]);
        assert_eq!(row[6].split('.').nth(1).map(str::len), Some(6), "6dp return: {}", row[6]);
        assert_eq!(&row[6], "0.100000", "both legs designed to return +10%");
    }

    // --- portfolio arithmetic: account-level returns off month-start equity ---
    // Each month: two legs at +10% on 50 USDT notional => 10 USDT leg PnL,
    // against 1000 USDT opening equity => gross_return == 0.01.
    // net_return == gross_return - fee_paid / equity_start.
    let expected_gross = (Decimal::from(10) / equity).round_dp(6);
    for row in &portfolio.rows {
        assert_eq!(row.len(), 10);
        assert_eq!(&row[2], "1", "n_long");
        assert_eq!(&row[3], "1", "n_short");
        let gross: Decimal = row[4].parse().unwrap();
        let fee: Decimal = row[5].parse().unwrap();
        let net: Decimal = row[6].parse().unwrap();
        assert_eq!(gross, expected_gross, "gross_return = leg PnL / equity_start");
        let expected_net = (gross - fee / equity).round_dp(6);
        assert_eq!(net, expected_net, "net_return identity for month {}", row[1]);
        assert_eq!(&row[8], "2", "n_fills");
        assert!(row[9].ends_with("/fills.csv"), "fills_ref: {}", row[9]);
    }

    // n_fills in portfolio matches fills.csv rows for that month
    assert_eq!(
        portfolio.rows.iter().map(|r| r[8].parse::<usize>().unwrap()).sum::<usize>(),
        fills.rows.len(),
    );
}

struct Csv {
    header: String,
    rows: Vec<Vec<String>>,
}

fn read_csv(path: std::path::PathBuf) -> Csv {
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default().to_string();
    let rows = lines
        .filter(|l| !l.is_empty())
        .map(|l| l.split(',').map(str::to_string).collect())
        .collect();
    Csv { header, rows }
}
