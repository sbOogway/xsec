//! Shared backtest inputs: the global command-line surface and the resolved
//! [`RunConfig`].
//!
//! A run picks its strategy with a clap subcommand ([`crate::strategy::StrategyKind`]);
//! each strategy owns its own flags, validation and market in
//! `src/strategy/<name>/config.rs`. What lives here is the surface every
//! strategy shares — the universe file, the backtest window, the starting
//! balance, the run uuid — plus [`build_config`], which validates those and
//! resolves them into the [`RunConfig`] the strategy holds and the capture
//! layer serialises to `runs/<uuid>/config.csv`.

use std::{path::PathBuf, str::FromStr};

use anyhow::{Result, anyhow, ensure};
use clap::Parser;
use nautilus_core::UnixNanos;
use nautilus_model::types::Money;
use uuid::Uuid;

use crate::strategy::StrategyKind;
use crate::universe::read_universe;

/// Cross-sectional strategy backtests over Bybit USDT-margined linear
/// perpetuals. Pick a strategy with a subcommand; `--help` on the subcommand
/// lists its knobs. The coin universe is read from `--universe` (a plain-text
/// file, one base asset per line).
#[derive(Parser, Debug)]
#[command(name = env!("CARGO_PKG_NAME"), version, about, long_about = None)]
pub struct CliArgs {
    /// Run UUID; keys `runs/<uuid>/` and matches `logs/<uuid>/logs.log`
    /// [default: a fresh UUID-7].
    #[arg(long, global = true)]
    pub uuid: Option<String>,

    /// Universe file: one base asset per line; blank lines and `#` comments
    /// are ignored.
    #[arg(long, global = true, default_value = "universe.txt")]
    pub universe: PathBuf,

    /// Starting balance for the simulated account. Must be USDT.
    #[arg(long, global = true, default_value = "1_000 USDT")]
    pub starting_balance: String,

    /// Backtest window start (inclusive), `YYYY-MM-DD`.
    #[arg(long, global = true, default_value = "2020-01-01")]
    pub date_start: String,

    /// Backtest window end (inclusive), `YYYY-MM-DD`.
    #[arg(long, global = true, default_value = "2026-09-02")]
    pub date_end: String,

    /// The strategy to run.
    #[command(subcommand)]
    pub strategy: StrategyKind,
}

/// The resolved, validated run configuration shared by every strategy. Built
/// once in `main` from a [`Cli`], held by the strategy, and written to
/// `runs/<uuid>/config.csv` alongside the strategy's own rows.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub run_id: String,
    /// The strategy subcommand name (`cross_sectional_momentum`), recorded in
    /// `config.csv` and used by `analysis/` to label a run.
    pub strategy: String,
    pub date_start: String,
    pub date_end: String,
    pub starting_balance: String,
    pub bases: Vec<String>,
    /// The universe file `bases` was read from (provenance for config.csv).
    pub universe_path: String,
    /// The full command line, space-joined (provenance for config.csv).
    pub argv: String,
}

/// Validate the shared flags on a parsed [`Cli`] and resolve them into a
/// [`RunConfig`]. Strategy-specific flags are validated separately by that
/// strategy's `config::build`.
///
/// `strategy` is the subcommand name; `argv` is recorded verbatim in the config
/// sidecar (pass `std::env::args().collect()`).
pub fn build_config(cli: &CliArgs, argv: &[String], strategy: &str) -> Result<RunConfig> {
    let bases = read_universe(&cli.universe)?;

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
        run_id: cli.uuid.clone().unwrap_or_else(|| Uuid::now_v7().to_string()),
        strategy: strategy.to_string(),
        date_start: cli.date_start.trim().to_string(),
        date_end: cli.date_end.trim().to_string(),
        bases,
        starting_balance: cli.starting_balance.trim().to_string(),
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// A universe file with `n` distinct symbols.
    fn universe_file(n: usize) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for i in 0..n {
            writeln!(f, "SYM{i}").unwrap();
        }
        f
    }

    /// Parse `args` (without the program name) into a `CliArgs`, defaulting
    /// `--universe` to `universe` and appending the momentum subcommand so the
    /// parser is satisfied.
    fn cli(universe: &std::path::Path, args: &[&str]) -> CliArgs {
        let mut full = vec!["xsec", "--universe", universe.to_str().unwrap()];
        full.extend_from_slice(args);
        full.push("momentum");
        CliArgs::try_parse_from(full).expect("args parse")
    }

    #[test]
    fn shared_defaults_are_stable() {
        let uni = universe_file(20);
        let cfg = build_config(&cli(uni.path(), &[]), &[], "cross_sectional_momentum").unwrap();

        assert_eq!(cfg.strategy, "cross_sectional_momentum");
        assert_eq!(cfg.starting_balance, "1_000 USDT");
        assert_eq!(cfg.date_start, "2020-01-01");
        assert_eq!(cfg.date_end, "2026-09-02");
        assert_eq!(cfg.bases.len(), 20);
        // A fresh UUID-7 when --uuid is absent.
        assert_eq!(cfg.run_id.len(), 36);
    }

    #[test]
    fn uuid_and_argv_flow_through() {
        let uni = universe_file(20);
        let cfg = build_config(
            &cli(uni.path(), &["--uuid", "run-42"]),
            &["xsectional-rs".into(), "--uuid".into(), "run-42".into()],
            "cross_sectional_momentum",
        )
        .unwrap();

        assert_eq!(cfg.run_id, "run-42");
        assert!(cfg.argv.contains("run-42"));
        assert_eq!(cfg.universe_path, uni.path().display().to_string());
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
            "cross_sectional_momentum",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("must be before"), "{err}");
    }

    #[test]
    fn rejects_non_usdt_starting_balance() {
        let uni = universe_file(20);
        let err = build_config(
            &cli(uni.path(), &["--starting-balance", "1000 USDC"]),
            &[],
            "cross_sectional_momentum",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("must be USDT"), "{err}");
    }

    #[test]
    fn missing_universe_file_is_an_error() {
        let err = build_config(
            &cli(std::path::Path::new("nope.txt"), &[]),
            &[],
            "cross_sectional_momentum",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("read universe file"), "{err}");
    }
}
