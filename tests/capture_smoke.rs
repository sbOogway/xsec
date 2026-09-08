//! Schema-contract smoke test for run-artifact capture.
//!
//! The issue spec (#1, #4, #6) is explicit that "the schema is the contract":
//! downstream tooling (`analysis/tearsheet.py`) relies on exact headers and the
//! `portfolio.net_return` arithmetic. This test drives [`RunCapture`] through a
//! 2-symbol, 6-month scenario and asserts that contract, without booting the
//! full backtest engine (which would need network/data fixtures and be
//! non-deterministic).
//!
//! `RunCapture` is generic over the rebalance period ([`RebalancePeriod`], #20)
//! — a second, smaller test below exercises the same lifecycle instantiated
//! with [`IsoWeek`] instead of [`YearMonth`], so both cadences are proven, not
//! just that the trait compiles.

use std::collections::HashMap;

use rust_decimal::Decimal;
use tempfile::tempdir;

use nautilus_model::{enums::OrderSide, identifiers::InstrumentId};
use xsec::capture::{RunCapture, RunConfig};
use xsec::period::{IsoWeek, YearMonth};

// The schema downstream tooling depends on — pinned here as literals so a
// change to the capture headers has to be a deliberate change to this test too.
const LEGS_HEADER: &str = "run_id,period,period_end_date,instrument_id,side,entry_price,exit_price,per_leg_return,notional_usdt";
const PORTFOLIO_HEADER: &str = "run_id,period,period_end_date,n_long,n_short,gross_return,fee_paid_usdt,net_return,equity_end_of_period_usdt,n_fills,fills_ref";
const FILLS_HEADER: &str =
    "run_id,ts_event,instrument_id,side,order_side,quantity,fill_price,fee_usdt";

const RUN_ID: &str = "test-0000-run";
const MONTHS: [YearMonth; 6] = [
    YearMonth {
        year: 2025,
        month: 1,
    },
    YearMonth {
        year: 2025,
        month: 2,
    },
    YearMonth {
        year: 2025,
        month: 3,
    },
    YearMonth {
        year: 2025,
        month: 4,
    },
    YearMonth {
        year: 2025,
        month: 5,
    },
    YearMonth {
        year: 2025,
        month: 6,
    },
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
        strategy: "cross_sectional_momentum".to_string(),
        date_start: "2025-01-01".to_string(),
        date_end: "2025-07-01".to_string(),
        bases: vec!["BTC".to_string(), "ETH".to_string()],
        starting_balance: "1000 USDT".to_string(),
        universe_path: "universe.txt".to_string(),
        argv: "xsectional-rs --uuid test-0000-run".to_string(),
    };
    // The rows the momentum strategy contributes (see
    // `strategy::momentum::config::config_rows`).
    let strategy_rows = [
        ("lookback_months".to_string(), "3".to_string()),
        ("holding_months".to_string(), "1".to_string()),
        ("percentile".to_string(), "0.1".to_string()),
        ("risk_pct".to_string(), "1".to_string()),
        ("long_w".to_string(), "0.5".to_string()),
        ("signal_tilt".to_string(), "0".to_string()),
    ];

    let mut capture: RunCapture<YearMonth> =
        RunCapture::open_in(dir.path(), &cfg, &strategy_rows).unwrap();

    // Exit mark for every leg: long +10%, short instrument -10% (=> short leg +10%).
    let mut exits: HashMap<InstrumentId, Decimal> = HashMap::new();
    exits.insert(long, Decimal::from(110));
    exits.insert(short, Decimal::from(90));

    let equity = Decimal::from(1000);
    let fee_per_month = Decimal::from(1); // 1 USDT total fees / month

    for m in MONTHS {
        let ts = month_start_nanos(m);
        capture.record_fill_row(
            m,
            ts,
            long,
            OrderSide::Buy,
            Decimal::from(1),
            Decimal::from(100),
            fee_per_month / Decimal::from(2),
        );
        capture.record_fill_row(
            m,
            ts,
            short,
            OrderSide::Sell,
            Decimal::from(1),
            Decimal::from(100),
            fee_per_month / Decimal::from(2),
        );

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
    assert_eq!(cfg_map["strategy"], "cross_sectional_momentum");
    assert_eq!(cfg_map["lookback_months"], "3");
    assert_eq!(cfg_map["holding_months"], "1");
    assert_eq!(cfg_map["bases"], "BTC ETH");
    assert_eq!(cfg_map["universe_path"], "universe.txt");
    assert_eq!(cfg_map["argv"], "xsectional-rs --uuid test-0000-run");

    // --- legs schema: period label + end date, side vocabulary, 6dp returns ---
    for row in &legs.rows {
        assert_eq!(row.len(), 9);
        assert_eq!(&row[0], RUN_ID);
        assert!(row[1].starts_with("2025-"), "period is YYYY-MM: {}", row[1]);
        assert!(
            row[2].starts_with("2025-"),
            "period_end_date is YYYY-MM-DD: {}",
            row[2]
        );
        assert!(
            matches!(row[4].as_str(), "long" | "short"),
            "side: {}",
            row[4]
        );
        assert_eq!(
            row[7].split('.').nth(1).map(str::len),
            Some(6),
            "6dp return: {}",
            row[7]
        );
        assert_eq!(&row[7], "0.100000", "both legs designed to return +10%");
    }

    // --- portfolio arithmetic: account-level returns off period-start equity ---
    // Each month: two legs at +10% on 50 USDT notional => 10 USDT leg PnL,
    // against 1000 USDT opening equity => gross_return == 0.01.
    // net_return == gross_return - fee_paid / equity_start.
    let expected_gross = (Decimal::from(10) / equity).round_dp(6);
    for row in &portfolio.rows {
        assert_eq!(row.len(), 11);
        assert!(
            row[2].starts_with("2025-"),
            "period_end_date is YYYY-MM-DD: {}",
            row[2]
        );
        assert_eq!(&row[3], "1", "n_long");
        assert_eq!(&row[4], "1", "n_short");
        let gross: Decimal = row[5].parse().unwrap();
        let fee: Decimal = row[6].parse().unwrap();
        let net: Decimal = row[7].parse().unwrap();
        assert_eq!(
            gross, expected_gross,
            "gross_return = leg PnL / equity_start"
        );
        let expected_net = (gross - fee / equity).round_dp(6);
        assert_eq!(
            net, expected_net,
            "net_return identity for period {}",
            row[1]
        );
        assert_eq!(&row[9], "2", "n_fills");
        assert!(row[10].ends_with("/fills.csv"), "fills_ref: {}", row[10]);
    }

    // n_fills in portfolio matches fills.csv rows for that period
    assert_eq!(
        portfolio
            .rows
            .iter()
            .map(|r| r[9].parse::<usize>().unwrap())
            .sum::<usize>(),
        fills.rows.len(),
    );
}

/// Same lifecycle as [`capture_writes_the_contract`], instantiated with
/// [`IsoWeek`] instead of [`YearMonth`] — proves `RunCapture<P>` works for a
/// second, independently-implemented `RebalancePeriod`, not just that it
/// compiles against one.
#[test]
fn capture_writes_the_contract_for_a_weekly_cadence() {
    let dir = tempdir().unwrap();
    let long = InstrumentId::from("BTCUSDT-LINEAR.BYBIT");
    let run_id = "test-0001-weekly";

    let cfg = RunConfig {
        run_id: run_id.to_string(),
        strategy: "top5_momentum".to_string(),
        date_start: "2026-01-01".to_string(),
        date_end: "2026-02-01".to_string(),
        bases: vec!["BTC".to_string()],
        starting_balance: "1000 USDT".to_string(),
        universe_path: "universe.txt".to_string(),
        argv: "xsec --uuid test-0001-weekly".to_string(),
    };

    let mut capture: RunCapture<IsoWeek> = RunCapture::open_in(dir.path(), &cfg, &[]).unwrap();

    let weeks = [
        IsoWeek {
            year: 2026,
            week: 2,
        },
        IsoWeek {
            year: 2026,
            week: 3,
        },
        IsoWeek {
            year: 2026,
            week: 4,
        },
    ];
    let equity = Decimal::from(1000);
    let mut exits: HashMap<InstrumentId, Decimal> = HashMap::new();
    exits.insert(long, Decimal::from(110));

    for w in weeks {
        capture.record_rebalance(
            w,
            equity,
            vec![(long, OrderSide::Buy, Decimal::from(100), 100.0)],
        );
        capture.finalise_completed(w, &exits, equity);
    }
    capture.finish(&exits, equity);
    drop(capture);

    let run_dir = dir.path().join(run_id);
    let legs = read_csv(run_dir.join("legs.csv"));
    let portfolio = read_csv(run_dir.join("portfolio.csv"));

    assert_eq!(legs.header, LEGS_HEADER);
    assert_eq!(portfolio.header, PORTFOLIO_HEADER);
    assert_eq!(portfolio.rows.len(), 3, "one portfolio row per entry week");
    assert_eq!(legs.rows.len(), 3, "one leg per week for three weeks");

    for row in &legs.rows {
        assert!(
            row[1].starts_with("2026-W"),
            "period is an ISO week label: {}",
            row[1]
        );
        assert_eq!(
            &row[7], "0.100000",
            "the one leg is designed to return +10%"
        );
    }
    for row in &portfolio.rows {
        assert!(
            row[1].starts_with("2026-W"),
            "period is an ISO week label: {}",
            row[1]
        );
        assert_eq!(&row[9], "0", "no fills recorded in this smoke test");
    }
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
