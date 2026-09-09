//! Shared backtest inputs: the run-level command-line flags every strategy
//! shares ([`SharedArgs`]) and the resolved [`RunConfig`].
//!
//! Each strategy owns its own flags, validation and market in
//! `src/strategy/<name>/config.rs`, and the binary composes [`SharedArgs`] with
//! the chosen strategy's subcommand into its top-level parser. What lives here
//! is only the shared surface — the universe file, the backtest window, the
//! starting balance, the run uuid — plus [`build_config`], which validates
//! those and resolves them into the [`RunConfig`] the strategy holds and the
//! capture layer serialises to `runs/<uuid>/config.csv`.
//!
//! This module deliberately knows nothing about the strategy enum: the binary
//! owns that composition, so `config` stays a leaf that `strategy` and `data`
//! can both depend on.

use std::{path::PathBuf, str::FromStr};

use anyhow::{Result, anyhow, ensure};
use clap::Args;
use nautilus_core::UnixNanos;
use nautilus_model::types::Money;
use uuid::Uuid;

use crate::data::{exchange::Exchange, universe::read_universe};

/// The run-level flags shared by every strategy. Flattened into the binary's
/// top-level parser alongside the strategy subcommand; every field is `global`
/// so it can appear before or after the subcommand on the command line.
#[derive(Args, Debug)]
pub struct SharedArgs {
    /// Run UUID; keys `runs/<uuid>/` and matches `logs/<uuid>/logs.log`
    /// [default: a fresh UUID-7].
    #[arg(long, global = true)]
    pub uuid: Option<String>,

    /// Universe file: one base asset per line; blank lines and `#` comments
    /// are ignored.
    #[arg(long, global = true, default_value = "universe.txt")]
    pub universe: PathBuf,

    /// Exchange to fetch from / backtest against; its cache lives under
    /// `data/<exchange>/`.
    #[arg(long, global = true, value_enum, default_value_t = Exchange::Bybit)]
    pub exchange: Exchange,

    /// Starting balance for the simulated account. Must be USDT.
    #[arg(long, global = true, default_value = "1_000 USDT")]
    pub starting_balance: String,

    /// Backtest window start (inclusive), `YYYY-MM-DD`.
    #[arg(long, global = true, default_value = "2020-01-01")]
    pub date_start: String,

    /// Backtest window end (inclusive), `YYYY-MM-DD`.
    #[arg(long, global = true, default_value = "2026-09-02")]
    pub date_end: String,
}

/// The resolved, validated run configuration shared by every strategy. Built
/// once in `main` from a [`SharedArgs`], held by the strategy, and written to
/// `runs/<uuid>/config.csv` alongside the strategy's own rows.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub run_id: String,
    /// The strategy subcommand name (`momentum`), recorded in
    /// `config.csv` and used by `analysis/` to label a run.
    pub strategy: String,
    /// The exchange this run's data comes from (`bybit`).
    pub exchange: String,
    pub date_start: String,
    pub date_end: String,
    pub starting_balance: String,
    pub bases: Vec<String>,
    /// The universe file `bases` was read from (provenance for config.csv).
    pub universe_path: String,
    /// The full command line, space-joined (provenance for config.csv).
    pub argv: String,
}

/// Validate the [`SharedArgs`] and resolve them into a [`RunConfig`], reading
/// the traded universe from `--universe`. Strategy-specific flags are validated
/// separately by that strategy's `config::build`.
///
/// `strategy` is the subcommand name; `argv` is recorded verbatim in the config
/// sidecar (pass `std::env::args().collect()`).
pub fn build_config(args: &SharedArgs, argv: &[String], strategy: &str) -> Result<RunConfig> {
    let bases = read_universe(&args.universe)?;
    build_config_with_bases(args, argv, strategy, bases, args.universe.display().to_string())
}

/// [`build_config`] with the traded universe supplied by the caller rather than
/// read from `--universe` — the `momentum --source coinmarketcap` path derives
/// its `bases` from the CoinMarketCap snapshots instead of a universe file, and
/// passes a `universe_path` sentinel (e.g. `"<derived: coinmarketcap>"`) for the
/// config sidecar's provenance row.
pub fn build_config_with_bases(
    args: &SharedArgs,
    argv: &[String],
    strategy: &str,
    bases: Vec<String>,
    universe_path: String,
) -> Result<RunConfig> {
    ensure!(!bases.is_empty(), "the resolved universe is empty");

    let balance = Money::from_str(args.starting_balance.trim())
        .map_err(|e| anyhow!("--starting-balance {:?}: {e}", args.starting_balance))?;
    ensure!(
        balance.currency.code.as_str() == "USDT",
        "--starting-balance must be USDT, got {}",
        balance.currency.code
    );

    let start = UnixNanos::from_str(args.date_start.trim())
        .map_err(|e| anyhow!("--date-start {:?}: {e}", args.date_start))?;
    let end = UnixNanos::from_str(args.date_end.trim())
        .map_err(|e| anyhow!("--date-end {:?}: {e}", args.date_end))?;
    ensure!(
        start < end,
        "--date-start ({}) must be before --date-end ({})",
        args.date_start,
        args.date_end
    );

    Ok(RunConfig {
        run_id: args.uuid.clone().unwrap_or_else(|| Uuid::now_v7().to_string()),
        strategy: strategy.to_string(),
        exchange: args.exchange.as_str().to_string(),
        date_start: args.date_start.trim().to_string(),
        date_end: args.date_end.trim().to_string(),
        bases,
        starting_balance: args.starting_balance.trim().to_string(),
        universe_path,
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

    use clap::Parser;

    use super::*;

    /// A universe file with `n` distinct symbols.
    fn universe_file(n: usize) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for i in 0..n {
            writeln!(f, "SYM{i}").unwrap();
        }
        f
    }

    /// Parse `args` (without the program name) into a [`SharedArgs`], forcing
    /// `--universe` to `universe`.
    fn shared(universe: &std::path::Path, args: &[&str]) -> SharedArgs {
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            shared: SharedArgs,
        }

        let mut full = vec!["xsec", "--universe", universe.to_str().unwrap()];
        full.extend_from_slice(args);
        Wrap::try_parse_from(full).expect("args parse").shared
    }

    #[test]
    fn shared_defaults_are_stable() {
        let uni = universe_file(20);
        let cfg = build_config(&shared(uni.path(), &[]), &[], "momentum").unwrap();

        assert_eq!(cfg.strategy, "momentum");
        assert_eq!(cfg.exchange, "bybit");
        assert_eq!(cfg.starting_balance, "1_000 USDT");
        assert_eq!(cfg.date_start, "2020-01-01");
        assert_eq!(cfg.date_end, "2026-09-02");
        assert_eq!(cfg.bases.len(), 20);
        // A fresh UUID-7 when --uuid is absent.
        assert_eq!(cfg.run_id.len(), 36);
    }

    #[test]
    fn exchange_flag_flows_through() {
        let uni = universe_file(20);
        let cfg = build_config(&shared(uni.path(), &["--exchange", "bybit"]), &[], "momentum").unwrap();
        assert_eq!(cfg.exchange, "bybit");
    }

    #[test]
    fn unknown_exchange_is_rejected() {
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            shared: SharedArgs,
        }
        match Wrap::try_parse_from(["xsec", "--exchange", "kraken"]) {
            Err(e) => assert_eq!(e.kind(), clap::error::ErrorKind::InvalidValue),
            Ok(_) => panic!("unknown --exchange should be rejected"),
        }
    }

    #[test]
    fn uuid_and_argv_flow_through() {
        let uni = universe_file(20);
        let cfg = build_config(
            &shared(uni.path(), &["--uuid", "run-42"]),
            &["xsectional-rs".into(), "--uuid".into(), "run-42".into()],
            "momentum",
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
            &shared(
                uni.path(),
                &["--date-start", "2025-01-01", "--date-end", "2024-01-01"],
            ),
            &[],
            "momentum",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("must be before"), "{err}");
    }

    #[test]
    fn rejects_non_usdt_starting_balance() {
        let uni = universe_file(20);
        let err = build_config(
            &shared(uni.path(), &["--starting-balance", "1000 USDC"]),
            &[],
            "momentum",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("must be USDT"), "{err}");
    }

    #[test]
    fn missing_universe_file_is_an_error() {
        let err = build_config(
            &shared(std::path::Path::new("nope.txt"), &[]),
            &[],
            "momentum",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("read universe file"), "{err}");
    }
}
