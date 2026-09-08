//! Schema-contract smoke test for run-artifact capture.
//!
//! The issue spec (#1, #4, #6) is explicit that "the schema is the contract":
//! downstream tooling (`analysis/tearsheet.py`) relies on exact headers and the
//! `portfolio.net_return` arithmetic. These tests drive [`RunCapture`] through
//! small carried-book scenarios and assert that contract, without booting the
//! full backtest engine (which would need network/data fixtures and be
//! non-deterministic).
//!
//! `RunCapture` is generic over the rebalance period ([`RebalancePeriod`], #20):
//! the weekly test instantiates the same lifecycle with [`IsoWeek`] where the
//! others use [`YearMonth`], so a second, independently-implemented period type
//! is proven, not just that the trait compiles.

use std::collections::HashMap;

use rust_decimal::Decimal;
use tempfile::tempdir;

use nautilus_model::{enums::OrderSide, identifiers::InstrumentId};
use xsec::data::backtest::{RunCapture, RunConfig};
use xsec::period::{IsoWeek, YearMonth};

// The schema downstream tooling depends on — pinned here as literals so a
// change to the capture headers has to be a deliberate change to this test too.
const LEGS_HEADER: &str = "run_id,period,period_end_date,instrument_id,side,entry_price,exit_price,per_leg_return,notional_usdt";
const PORTFOLIO_HEADER: &str = "run_id,period,period_end_date,n_long,n_short,gross_return,fee_paid_usdt,net_return,equity_end_of_period_usdt,n_fills,fills_ref";
const FILLS_HEADER: &str =
    "run_id,ts_event,instrument_id,side,order_side,quantity,fill_price,fee_usdt";

/// The carried-book flow with fills and a config sidecar, on an [`IsoWeek`]
/// cadence: proves `RunCapture<P>` works for a second, independently-implemented
/// `RebalancePeriod`, and pins the fills schema, the `n_fills` count and the
/// `net_return = gross_return - fee_paid / equity_start` identity.
#[test]
fn capture_writes_the_contract_for_a_weekly_cadence() {
    let dir = tempdir().unwrap();
    let long = InstrumentId::from("BTCUSDT-LINEAR.BYBIT");
    let run_id = "test-0001-weekly";

    let cfg = RunConfig {
        run_id: run_id.to_string(),
        strategy: "momentum".to_string(),
        date_start: "2026-01-01".to_string(),
        date_end: "2026-02-01".to_string(),
        bases: vec!["BTC".to_string()],
        starting_balance: "1000 USDT".to_string(),
        universe_path: "universe.txt".to_string(),
        argv: "xsec --uuid test-0001-weekly".to_string(),
    };
    // A representative slice of the rows the momentum strategy contributes (see
    // `strategy::momentum::config::config_rows`).
    let strategy_rows = [
        ("fast_days".to_string(), "1".to_string()),
        ("slow_days".to_string(), "7".to_string()),
        ("top_n".to_string(), "5".to_string()),
        ("regime_filter".to_string(), "false".to_string()),
    ];

    let mut capture: RunCapture<IsoWeek> =
        RunCapture::open_in(dir.path(), &cfg, &strategy_rows).unwrap();

    let wk = |week| IsoWeek { year: 2026, week };
    let equity = Decimal::from(1000);

    // Week 2: open BTC long @ 100, notional 500; two fills, 0.25 USDT fee each.
    let ts = 1_768_608_000_000_000_000; // ~2026-W02, good enough for bucketing
    capture.record_fill_row(
        wk(2),
        ts,
        long,
        OrderSide::Buy,
        Decimal::from(3),
        Decimal::from(100),
        Decimal::new(25, 2),
    );
    capture.record_fill_row(
        wk(2),
        ts,
        long,
        OrderSide::Buy,
        Decimal::from(2),
        Decimal::from(100),
        Decimal::new(25, 2),
    );
    capture.record_book_turnover(
        wk(2),
        equity,
        &HashMap::from([(long, Decimal::from(100))]),
        &[(long, OrderSide::Buy, Decimal::from(100), 500.0)],
        &[],
    );

    // Week 3: BTC marks to 110 and rides on — this finalises week 2's row
    // (110 - 100 on the 100 entry * 500 notional = +50 PnL).
    capture.record_book_turnover(
        wk(3),
        equity,
        &HashMap::from([(long, Decimal::from(110))]),
        &[],
        &[],
    );

    // Week 4: close the book at 121.
    capture.finish_book(&HashMap::from([(long, Decimal::from(121))]), equity);
    drop(capture);

    let run_dir = dir.path().join(run_id);
    let legs = read_csv(run_dir.join("legs.csv"));
    let portfolio = read_csv(run_dir.join("portfolio.csv"));
    let fills = read_csv(run_dir.join("fills.csv"));
    let config = read_csv(run_dir.join("config.csv"));

    // --- headers are the contract ---
    assert_eq!(legs.header, LEGS_HEADER);
    assert_eq!(portfolio.header, PORTFOLIO_HEADER);
    assert_eq!(fills.header, FILLS_HEADER);
    assert_eq!(config.header, "key,value");

    // --- config sidecar carries the run id, shared rows and strategy rows ---
    let cfg_map: HashMap<&str, &str> = config
        .rows
        .iter()
        .map(|r| (r[0].as_str(), r[1].as_str()))
        .collect();
    assert_eq!(cfg_map["run_id"], run_id);
    assert_eq!(cfg_map["strategy"], "momentum");
    assert_eq!(cfg_map["bases"], "BTC");
    assert_eq!(cfg_map["universe_path"], "universe.txt");
    assert_eq!(cfg_map["argv"], "xsec --uuid test-0001-weekly");
    assert_eq!(cfg_map["fast_days"], "1");
    assert_eq!(cfg_map["regime_filter"], "false");

    // --- fills schema ---
    assert_eq!(fills.rows.len(), 2, "two fills, both in week 2");
    for row in &fills.rows {
        assert_eq!(row.len(), 8);
        assert_eq!(&row[0], run_id);
        assert_eq!(&row[3], "long");
        assert_eq!(&row[4], "BUY");
    }

    // --- portfolio: week 2 (finalised by week 3's turnover), then week 3 ---
    assert_eq!(portfolio.rows.len(), 2, "week 2 and week 3");
    let by_period = |p: &str| {
        portfolio
            .rows
            .iter()
            .find(|r| r[1] == p)
            .cloned()
            .unwrap_or_else(|| panic!("no portfolio row for {p}"))
    };
    let w2 = by_period("2026-W02");
    assert_eq!(w2.len(), 11);
    assert!(w2[2].starts_with("2026-"), "period_end_date: {}", w2[2]);
    assert_eq!(w2[3], "1", "n_long: BTC held over week 2");
    assert_eq!(w2[5], "0.050000", "gross_return = 50 / 1000");
    assert_eq!(w2[6], "0.5", "fee_paid = 0.25 + 0.25");
    assert_eq!(
        w2[7], "0.049500",
        "net_return = 0.05 - 0.5 / 1000"
    );
    assert_eq!(w2[9], "2", "n_fills");
    assert!(w2[10].ends_with("/fills.csv"), "fills_ref: {}", w2[10]);

    // --- one leg row for BTC, written when the book closed, +21% telescoped ---
    assert_eq!(legs.rows.len(), 1);
    let btc = &legs.rows[0];
    assert!(btc[1].starts_with("2026-W02"), "leg keyed to entry week: {}", btc[1]);
    assert_eq!(btc[4], "long");
    assert_eq!(btc[7], "0.210000", "100 -> 121 telescopes to +21%");
}

/// The carried-book flow ([`RunCapture::record_book_turnover`] +
/// [`RunCapture::finish_book`]): a leg that survives a turnover rides on
/// untouched, its `legs.csv` row is written once when it finally closes, and
/// each period's portfolio PnL is the whole open book marked close-to-close
/// over that period — so the per-period contributions of a multi-turnover leg
/// telescope to its full `(exit - entry) / entry`.
#[test]
fn capture_writes_the_contract_for_a_carried_book() {
    let dir = tempdir().unwrap();
    let a = InstrumentId::from("AAAUSDT-LINEAR.BYBIT");
    let b = InstrumentId::from("BBBUSDT-LINEAR.BYBIT");
    let c = InstrumentId::from("CCCUSDT-LINEAR.BYBIT");
    let run_id = "test-0002-carried";

    let cfg = RunConfig {
        run_id: run_id.to_string(),
        strategy: "top5_momentum_filtered".to_string(),
        date_start: "2026-01-01".to_string(),
        date_end: "2026-04-01".to_string(),
        bases: vec!["AAA".to_string(), "BBB".to_string(), "CCC".to_string()],
        starting_balance: "1000 USDT".to_string(),
        universe_path: "universe.txt".to_string(),
        argv: "xsec --uuid test-0002-carried".to_string(),
    };

    let mut capture: RunCapture<YearMonth> = RunCapture::open_in(dir.path(), &cfg, &[]).unwrap();
    let ym = |month| YearMonth { year: 2026, month };
    let equity = Decimal::from(1000);

    // Jan: open A and B @ 100, notional 400 each.
    let jan_marks = HashMap::from([(a, Decimal::from(100)), (b, Decimal::from(100))]);
    capture.record_book_turnover(
        ym(1),
        equity,
        &jan_marks,
        &[
            (a, OrderSide::Buy, Decimal::from(100), 400.0),
            (b, OrderSide::Buy, Decimal::from(100), 400.0),
        ],
        &[],
    );

    // Feb: A +10% and rides, B -10% and drops out, C joins @ 100 notional 300.
    let feb_marks = HashMap::from([
        (a, Decimal::from(110)),
        (b, Decimal::from(90)),
        (c, Decimal::from(100)),
    ]);
    capture.record_book_turnover(
        ym(2),
        equity,
        &feb_marks,
        &[(c, OrderSide::Buy, Decimal::from(100), 300.0)],
        &[b],
    );

    // Close out in Mar: A now 121 (another +10% on the 110 mark), C flat.
    let mar_marks = HashMap::from([
        (a, Decimal::from(121)),
        (b, Decimal::from(99)),
        (c, Decimal::from(100)),
    ]);
    capture.finish_book(&mar_marks, Decimal::from(1044));
    drop(capture);

    let run_dir = dir.path().join(run_id);
    let legs = read_csv(run_dir.join("legs.csv"));
    let portfolio = read_csv(run_dir.join("portfolio.csv"));

    assert_eq!(legs.header, LEGS_HEADER);
    assert_eq!(portfolio.header, PORTFOLIO_HEADER);

    // One portfolio row per completed period (Jan, Feb); Mar's book is closed
    // by `finish_book`.
    assert_eq!(portfolio.rows.len(), 2, "Jan and Feb");
    let by_period = |rows: &[Vec<String>], p: &str| {
        rows.iter()
            .find(|r| r[1] == p)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {p}"))
    };

    // Jan: A +40, B -40 on 1000 opening equity => 0.
    let jan = by_period(&portfolio.rows, "2026-01");
    assert_eq!(jan[3], "2", "n_long counts the whole book held over Jan");
    assert_eq!(jan[5], "0.000000", "gross_return Jan");
    // Feb: only A is still marked-to-market (C flat), +11 on the 110->121 move
    // against 100 entry * 400 notional = 44, on 1000 => 0.044.
    let feb = by_period(&portfolio.rows, "2026-02");
    assert_eq!(feb[3], "2", "n_long: A carried + C opened");
    assert_eq!(feb[5], "0.044000", "gross_return Feb");

    // One leg row per leg, written when it closed, spanning its whole hold.
    assert_eq!(legs.rows.len(), 3);
    let leg = |rows: &[Vec<String>], inst: &str| {
        rows.iter()
            .find(|r| r[3] == inst)
            .cloned()
            .unwrap_or_else(|| panic!("no leg for {inst}"))
    };
    let a_leg = leg(&legs.rows, a.to_string().as_str());
    assert_eq!(
        a_leg[1], "2026-01",
        "A's leg is keyed to its Jan entry period"
    );
    assert_eq!(
        (&a_leg[5], &a_leg[6]),
        (&"100".to_string(), &"121".to_string())
    );
    assert_eq!(
        a_leg[7], "0.210000",
        "A's full-hold return telescopes to +21%"
    );
    let b_leg = leg(&legs.rows, b.to_string().as_str());
    assert_eq!(
        b_leg[7], "-0.100000",
        "B closed at -10% when it dropped out"
    );
    let c_leg = leg(&legs.rows, c.to_string().as_str());
    assert_eq!(c_leg[1], "2026-02");
    assert_eq!(c_leg[7], "0.000000");
}

/// A name that flips side across a turnover — long the top slice one period,
/// short the bottom slice the next — is passed to `record_book_turnover` in
/// *both* `closed` and `opened` in the same call. The long segment must get its
/// own `legs.csv` row (priced out at the turnover mark) and the fresh short
/// leg must start tracking from that same mark.
#[test]
fn capture_writes_the_contract_for_a_side_flip() {
    let dir = tempdir().unwrap();
    let x = InstrumentId::from("XXXUSDT-LINEAR.BYBIT");
    let run_id = "test-0003-flip";

    let cfg = RunConfig {
        run_id: run_id.to_string(),
        strategy: "momentum".to_string(),
        date_start: "2026-01-01".to_string(),
        date_end: "2026-04-01".to_string(),
        bases: vec!["XXX".to_string()],
        starting_balance: "1000 USDT".to_string(),
        universe_path: "universe.txt".to_string(),
        argv: "xsec --uuid test-0003-flip".to_string(),
    };

    let mut capture: RunCapture<YearMonth> = RunCapture::open_in(dir.path(), &cfg, &[]).unwrap();
    let ym = |month| YearMonth { year: 2026, month };
    let equity = Decimal::from(1000);

    // Jan: X opens long @ 100, notional 500.
    capture.record_book_turnover(
        ym(1),
        equity,
        &HashMap::from([(x, Decimal::from(100))]),
        &[(x, OrderSide::Buy, Decimal::from(100), 500.0)],
        &[],
    );

    // Feb: X has run to 120 and flips to the short slice — closed as a long,
    // reopened as a short at the same 120 mark.
    capture.record_book_turnover(
        ym(2),
        equity,
        &HashMap::from([(x, Decimal::from(120))]),
        &[(x, OrderSide::Sell, Decimal::from(120), 500.0)],
        &[x],
    );

    // Mar: the short has worked, X down to 108. Close the book.
    capture.finish_book(&HashMap::from([(x, Decimal::from(108))]), equity);
    drop(capture);

    let run_dir = dir.path().join(run_id);
    let legs = read_csv(run_dir.join("legs.csv"));
    let portfolio = read_csv(run_dir.join("portfolio.csv"));

    // Two leg rows for the one instrument: the long segment, then the short.
    assert_eq!(legs.rows.len(), 2);
    let long_leg = legs
        .rows
        .iter()
        .find(|r| r[4] == "long")
        .expect("a long leg row");
    assert_eq!(long_leg[1], "2026-01", "long leg keyed to its Jan entry");
    assert_eq!(
        (&long_leg[5], &long_leg[6]),
        (&"100".to_string(), &"120".to_string())
    );
    assert_eq!(long_leg[7], "0.200000", "long leg +20% (100 -> 120)");

    let short_leg = legs
        .rows
        .iter()
        .find(|r| r[4] == "short")
        .expect("a short leg row");
    assert_eq!(short_leg[1], "2026-02", "short leg keyed to its Feb entry");
    assert_eq!(
        (&short_leg[5], &short_leg[6]),
        (&"120".to_string(), &"108".to_string())
    );
    assert_eq!(
        short_leg[7], "0.100000",
        "short leg +10% (120 -> 108, signed)"
    );

    // Jan portfolio row: the long marked 100 -> 120 over 500 notional = +100 on
    // 1000 equity. Feb: the short marked 120 -> 108 = +50 on 1000.
    let by_period = |p: &str| {
        portfolio
            .rows
            .iter()
            .find(|r| r[1] == p)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {p}"))
    };
    assert_eq!(by_period("2026-01")[5], "0.100000", "gross_return Jan");
    assert_eq!(by_period("2026-01")[3], "1", "n_long Jan");
    assert_eq!(by_period("2026-02")[5], "0.050000", "gross_return Feb");
    assert_eq!(by_period("2026-02")[4], "1", "n_short Feb");
}

struct Csv {
    header: String,
    rows: Vec<Vec<String>>,
}

fn read_csv(path: std::path::PathBuf) -> Csv {
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default().to_string();
    let rows = lines
        .filter(|l| !l.is_empty())
        .map(|l| l.split(',').map(str::to_string).collect())
        .collect();
    Csv { header, rows }
}
