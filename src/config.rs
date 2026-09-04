//! Backtest inputs: the command-line surface and the resolved [`RunConfig`].
//!
//! Every knob that used to be a `const` at the top of `src/main.rs` is now a
//! field on [`Cli`] (a `clap` parser) with a default equal to the old constant.
//! [`build_config`] validates a parsed `Cli` and turns it into a [`RunConfig`] —
//! the single configuration value the strategy holds and the capture layer
//! serialises to `runs/<uuid>/config.csv`.
//!
//! [`VENUE`] and [`TIMEFRAME`] stay constants: the data layer (`src/data.rs`) is
//! Bybit-only, and the ranking, rebalance cadence and capture schema all assume
//! monthly bars.

use std::{path::PathBuf, str::FromStr};

use anyhow::{Context, Result, anyhow, ensure};
use clap::Parser;
use nautilus_core::UnixNanos;
use nautilus_model::{enums::BarAggregation, identifiers::InstrumentId, types::Money};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use uuid::Uuid;

use crate::universe::read_universe;

/// The trading venue. Bybit-only: the data layer talks to the Bybit HTTP API
/// and nothing else.
pub const VENUE: &str = "BYBIT";

/// Bar size the strategy ranks on. Monthly-only: the rebalance cadence, the
/// holding-period return and the capture schema all assume calendar months.
pub const TIMEFRAME: BarAggregation = BarAggregation::Month;

/// The largest `--percentile` that still yields two non-overlapping deciles.
const MAX_PERCENTILE: &str = "0.5";

/// Cross-sectional momentum backtest over Bybit USDT-margined linear perpetuals.
///
/// Defaults reproduce the constants this binary used to carry; override any of
/// them per run. The coin universe is read from `--universe` (a plain-text file,
/// one base asset per line).
#[derive(Parser, Debug)]
#[command(name = "xsectional-rs", version, about, long_about = None)]
pub struct Cli {
    /// Run UUID; keys `runs/<uuid>/` and matches `logs/<uuid>/logs.log`
    /// [default: a fresh UUID-7].
    #[arg(long)]
    pub uuid: Option<String>,

    /// Universe file: one base asset per line; blank lines and `#` comments
    /// are ignored.
    #[arg(long, default_value = "universe.txt")]
    pub universe: PathBuf,

    /// Trailing-return formation window, in months.
    #[arg(long, default_value_t = 3)]
    pub lookback_months: u16,

    /// Holding period, in months. Only `1` is currently supported.
    #[arg(long, default_value_t = 1)]
    pub holding_months: u16,

    /// Long/short cut as a fraction of the ranked universe
    /// (`0.1` = top and bottom decile).
    #[arg(long, default_value = "0.1")]
    pub percentile: String,

    /// Gross exposure target as a fraction of account equity, per rebalance.
    #[arg(long, default_value_t = 0.8)]
    pub risk_pct: f64,

    /// Share of the gross budget on the long side; the short side gets the
    /// remainder. `0.5` = dollar-neutral.
    #[arg(long, default_value_t = 0.5)]
    pub long_w: f64,

    /// Within-side allocation tilt toward higher-conviction names.
    /// `0.0` = equal dollars per leg.
    #[arg(long, default_value_t = 0.0)]
    pub signal_tilt: f64,

    /// Starting balance for the simulated account. Must be USDT.
    #[arg(long, default_value = "1_000 USDT")]
    pub starting_balance: String,

    /// Backtest window start (inclusive), `YYYY-MM-DD`.
    #[arg(long, default_value = "2020-01-01")]
    pub date_start: String,

    /// Backtest window end (inclusive), `YYYY-MM-DD`.
    #[arg(long, default_value = "2026-09-02")]
    pub date_end: String,
}

/// The resolved, validated run configuration. Built once in `main` from a
/// [`Cli`], held by the strategy, and written verbatim to
/// `runs/<uuid>/config.csv`.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub run_id: String,
    pub lookback_months: u16,
    pub holding_months: u16,
    pub percentile: String,
    pub date_start: String,
    pub date_end: String,
    pub bases: Vec<String>,
    pub starting_balance: String,
    /// Gross exposure target as a fraction of account equity, per rebalance.
    pub risk_pct: f64,
    /// Share of the gross budget on the long side (0.5 = dollar-neutral).
    pub long_w: f64,
    /// Within-side allocation tilt toward higher-conviction names (0 = equal).
    pub signal_tilt: f64,
    /// The universe file `bases` was read from (provenance for config.csv).
    pub universe_path: String,
    /// The full command line, space-joined (provenance for config.csv).
    pub argv: String,
}

/// Validate a parsed [`Cli`] and resolve it into a [`RunConfig`].
///
/// `argv` is recorded verbatim in the config sidecar; pass
/// `std::env::args().collect()`.
pub fn build_config(cli: &Cli, argv: &[String]) -> Result<RunConfig> {
    let bases = read_universe(&cli.universe)?;
    let n = bases.len();

    let percentile = Decimal::from_str(cli.percentile.trim())
        .with_context(|| format!("--percentile {:?} is not a number", cli.percentile))?;
    // The 0.5 ceiling is also what keeps the long and short slices in
    // `on_time_event` from overlapping: `floor(percentile * n) <= n / 2`.
    let max = Decimal::from_str(MAX_PERCENTILE).unwrap();
    ensure!(
        percentile > Decimal::ZERO && percentile <= max,
        "--percentile must be in (0, {max}], got {percentile}"
    );

    // Mirror the runtime cut in `on_time_event`: `floor(percentile * n)` names
    // per side. Below 1 there are no trades at all.
    let per_side = (percentile * Decimal::from(n))
        .floor()
        .to_usize()
        .unwrap_or(0);
    let need = (Decimal::ONE / percentile).ceil();
    ensure!(
        per_side >= 1,
        "universe has {n} names but --percentile {percentile} selects none per side \
         (need at least {need}); add names or raise --percentile"
    );

    ensure!(cli.lookback_months >= 1, "--lookback-months must be >= 1");
    ensure!(
        cli.holding_months == 1,
        "--holding-months={} is not supported: the rebalance path assumes a one-month hold. \
         Revisit the age-based close in on_time_event before changing this.",
        cli.holding_months
    );

    ensure!(
        (0.0..=1.0).contains(&cli.long_w),
        "--long-w must be in [0.0, 1.0], got {}",
        cli.long_w
    );
    ensure!(
        cli.risk_pct.is_finite() && cli.risk_pct > 0.0,
        "--risk-pct must be finite and > 0, got {}",
        cli.risk_pct
    );
    ensure!(
        cli.signal_tilt.is_finite() && cli.signal_tilt >= 0.0,
        "--signal-tilt must be finite and >= 0, got {}",
        cli.signal_tilt
    );

    let balance = Money::from_str(cli.starting_balance.trim())
        .map_err(|e| anyhow!("--starting-balance {:?}: {e}", cli.starting_balance))?;
    ensure!(
        balance.currency.code.as_str() == "USDT",
        "--starting-balance must be USDT, got {}",
        balance.currency.code
    );

    let start = UnixNanos::from_str(cli.date_start.trim())
        .map_err(|e| anyhow!("--date-start {:?}: {e}", cli.date_start))?;
    let end = UnixNanos::from_str(cli.date_end.trim())
        .map_err(|e| anyhow!("--date-end {:?}: {e}", cli.date_end))?;
    ensure!(
        start < end,
        "--date-start ({}) must be before --date-end ({})",
        cli.date_start,
        cli.date_end
    );

    Ok(RunConfig {
        run_id: cli
            .uuid
            .clone()
            .unwrap_or_else(|| Uuid::now_v7().to_string()),
        lookback_months: cli.lookback_months,
        holding_months: cli.holding_months,
        percentile: cli.percentile.trim().to_string(),
        date_start: cli.date_start.trim().to_string(),
        date_end: cli.date_end.trim().to_string(),
        bases,
        starting_balance: cli.starting_balance.trim().to_string(),
        risk_pct: cli.risk_pct,
        long_w: cli.long_w,
        signal_tilt: cli.signal_tilt,
        universe_path: cli.universe.display().to_string(),
        argv: sanitise_argv(argv),
    })
}

/// Flatten `argv` to one line for the config sidecar, neutralising the
/// characters that would break the `key,value` CSV or split it across rows.
fn sanitise_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| a.replace(['\n', '\r', ','], " "))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Bybit linear-perp instrument ids for `bases`
/// (`BTC` → `BTCUSDT-LINEAR.BYBIT`).
pub fn instrument_ids(bases: &[String]) -> Vec<InstrumentId> {
    bases
        .iter()
        .map(|base| InstrumentId::from(format!("{base}USDT-LINEAR.{VENUE}").as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// A universe file with `n` distinct symbols, enough for the default decile.
    fn universe_file(n: usize) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for i in 0..n {
            writeln!(f, "SYM{i}").unwrap();
        }
        f
    }

    /// Parse `args` (without the argv[0] program name) into a `Cli`, defaulting
    /// `--universe` to `universe` unless the caller overrides it.
    fn cli(universe: &std::path::Path, args: &[&str]) -> Cli {
        let mut full = vec!["xsectional-rs", "--universe", universe.to_str().unwrap()];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).expect("args parse")
    }

    #[test]
    fn defaults_match_the_legacy_constants() {
        let uni = universe_file(20);
        let cfg = build_config(&cli(uni.path(), &[]), &[]).unwrap();

        assert_eq!(cfg.lookback_months, 3);
        assert_eq!(cfg.holding_months, 1);
        assert_eq!(cfg.percentile, "0.1");
        assert_eq!(cfg.risk_pct, 0.8);
        assert_eq!(cfg.long_w, 0.5);
        assert_eq!(cfg.signal_tilt, 0.0);
        assert_eq!(cfg.starting_balance, "1_000 USDT");
        assert_eq!(cfg.date_start, "2020-01-01");
        assert_eq!(cfg.date_end, "2026-09-02");
        assert_eq!(cfg.bases.len(), 20);
        // A fresh UUID-7 when --uuid is absent.
        assert_eq!(cfg.run_id.len(), 36);
    }

    #[test]
    fn uuid_and_overrides_flow_through() {
        let uni = universe_file(40);
        let cfg = build_config(
            &cli(
                uni.path(),
                &[
                    "--uuid",
                    "run-42",
                    "--lookback-months",
                    "6",
                    "--long-w",
                    "0.7",
                ],
            ),
            &["xsectional-rs".into(), "--uuid".into(), "run-42".into()],
        )
        .unwrap();

        assert_eq!(cfg.run_id, "run-42");
        assert_eq!(cfg.lookback_months, 6);
        assert_eq!(cfg.long_w, 0.7);
        assert!(cfg.argv.contains("run-42"));
        assert_eq!(cfg.universe_path, uni.path().display().to_string());
    }

    #[test]
    fn rejects_percentile_out_of_range() {
        let uni = universe_file(20);
        let err = build_config(&cli(uni.path(), &["--percentile", "0.9"]), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("--percentile must be in"), "{err}");
    }

    #[test]
    fn rejects_universe_too_small_for_percentile() {
        let uni = universe_file(6); // floor(0.1 * 6) == 0 names per side
        let err = build_config(&cli(uni.path(), &[]), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("selects none per side"), "{err}");
    }

    #[test]
    fn rejects_long_w_above_one() {
        let uni = universe_file(20);
        let err = build_config(&cli(uni.path(), &["--long-w", "1.5"]), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("--long-w must be in"), "{err}");
    }

    #[test]
    fn rejects_multi_month_hold() {
        let uni = universe_file(20);
        let err = build_config(&cli(uni.path(), &["--holding-months", "2"]), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("one-month hold"), "{err}");
    }

    #[test]
    fn rejects_reversed_dates() {
        let uni = universe_file(20);
        let err = build_config(
            &cli(
                uni.path(),
                &["--date-start", "2025-01-01", "--date-end", "2024-01-01"],
            ),
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("must be before"), "{err}");
    }

    #[test]
    fn rejects_non_usdt_starting_balance() {
        let uni = universe_file(20);
        let err = build_config(&cli(uni.path(), &["--starting-balance", "1000 USDC"]), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be USDT"), "{err}");
    }

    #[test]
    fn missing_universe_file_is_an_error() {
        let err = build_config(&cli(std::path::Path::new("nope.txt"), &[]), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("read universe file"), "{err}");
    }

    #[test]
    fn instrument_ids_are_bybit_linear_perps() {
        let ids = instrument_ids(&["BTC".to_string(), "ETH".to_string()]);
        assert_eq!(ids[0], InstrumentId::from("BTCUSDT-LINEAR.BYBIT"));
        assert_eq!(ids[1], InstrumentId::from("ETHUSDT-LINEAR.BYBIT"));
    }
}
