//! Inputs for the momentum strategy: its CLI flags ([`Args`]), its
//! resolved-and-validated [`Config`], the rows it contributes to
//! `runs/<uuid>/config.csv` ([`config_rows`]), and the market it trades
//! ([`VENUE`], [`TIMEFRAME`], [`instrument_ids`]).

use std::path::PathBuf;

use anyhow::{Result, ensure};
use clap::{Args as ClapArgs, ValueEnum};
use nautilus_model::{enums::BarAggregation, identifiers::InstrumentId};

use crate::{period::HoldingPeriod, strategy::common::Market};

/// The trading venue. Bybit-only: the data layer talks to the Bybit HTTP API
/// and nothing else.
pub const VENUE: &str = "BYBIT";

/// Bar size the strategy ranks on: always daily bars, whatever the rebalance
/// cadence (`--holding-period`) is.
pub const TIMEFRAME: BarAggregation = BarAggregation::Day;

/// The market this strategy trades, for [`crate::strategy::common::StrategyRuntime`].
pub const MARKET: Market = Market {
    venue: VENUE,
    timeframe: TIMEFRAME,
};

/// Where the strategy's tradeable universe — and, at each rebalance, its
/// eligible-to-hold set — comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, ValueEnum)]
pub enum Source {
    /// The fixed `--universe` file, scored and ranked in full every rebalance.
    #[default]
    Bybit,
    /// CoinMarketCap's historical top-N by market cap *as of each rebalance
    /// date* gates which names are eligible to hold; the score is unchanged
    /// (still the composite over Bybit daily closes). Needs
    /// `make snapshot_cmc_history` + `make cmc_resolution` first.
    Coinmarketcap,
}

impl Source {
    /// The `runs/<uuid>/config.csv` `source` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bybit => "bybit",
            Self::Coinmarketcap => "coinmarketcap",
        }
    }
}

/// Momentum flags. Defaults are a reasonable starting point; every knob here is
/// tunable per run.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Fast-momentum lookback, in daily bars.
    #[arg(long, default_value_t = 7)]
    pub fast_days: u32,

    /// Medium-momentum lookback, in daily bars.
    #[arg(long, default_value_t = 3)]
    pub medium_days: u32,

    /// Slow-momentum lookback, in daily bars.
    #[arg(long, default_value_t = 30)]
    pub slow_days: u32,

    /// Weight on fast momentum in the composite score.
    #[arg(long, default_value_t = 0.3)]
    pub fast_weight: f64,

    /// Weight on medium momentum in the composite score.
    #[arg(long, default_value_t = 0.0)]
    pub medium_weight: f64,

    /// Weight on slow momentum in the composite score.
    #[arg(long, default_value_t = 0.7)]
    pub slow_weight: f64,

    /// Number of names held long at a time (top of the composite score).
    #[arg(long, default_value_t = 5)]
    pub top_n: usize,

    /// Number of names held short at a time (bottom of the composite score).
    /// `0` = long-only; the long side then takes the whole budget regardless
    /// of `--long-w`.
    #[arg(long, default_value_t = 5)]
    pub short_n: usize,

    /// Share of the gross budget on the long side; the short side gets the
    /// remainder. `0.5` = dollar-neutral. Only has an effect when
    /// `--short-n > 0`.
    #[arg(long, default_value_t = 0.5)]
    pub long_short_balance: f64,

    /// BTC trailing-return window for the regime filter, in days.
    #[arg(long, default_value_t = 30)]
    pub regime_lookback_days: u32,

    /// Flatten the whole book to cash whenever BTC's trailing return over
    /// `--regime-lookback-days` is negative. Off by default; when on, the
    /// universe must contain `BTC`.
    #[arg(long, action = clap::ArgAction::Set, default_value_t = true)]
    pub regime_filter: bool,

    /// Gross exposure as a fraction of account equity, per rebalance.
    #[arg(long, default_value_t = 0.8)]
    pub risk_fraction: f64,

    /// Within-side tilt toward higher-conviction names. `0.0` = equal dollars
    /// per leg.
    #[arg(long, default_value_t = 0.0)]
    pub allocation_tilt: f64,

    /// Rebalance clock unit: which `RebalancePeriod` the cadence runs on
    /// (`day`, `iso-week`, `month`).
    #[arg(long, value_enum, default_value_t = HoldingPeriod::Day)]
    pub holding_period: HoldingPeriod,

    /// Number of `--holding-period` units the book is held before it is
    /// re-ranked and its top-`n` / bottom-`n` delta is traded.
    #[arg(long, default_value_t = 7)]
    pub number_holding_periods: u32,

    /// Universe source: `bybit` ranks the fixed `--universe`; `coinmarketcap`
    /// gates the eligible set each rebalance by CoinMarketCap's historical
    /// top-N by market cap as of that date (the score is unchanged). With
    /// `coinmarketcap`, `--universe` is ignored — the traded set is derived
    /// from the snapshots. Run `make snapshot_cmc_history` + `make
    /// cmc_resolution` first.
    #[arg(long, value_enum, default_value_t = Source::Bybit)]
    pub source: Source,

    /// [`--source coinmarketcap`] the `coins/cmc/<YYYYMMDD>.csv` snapshot
    /// directory.
    #[arg(long, default_value = "coins/cmc")]
    pub cmc_snapshots: PathBuf,

    /// [`--source coinmarketcap`] snapshot depth that counts as "in the top-N"
    /// each rebalance (`200` = the whole snapshot). Must be ≥ `--top-n +
    /// --short-n`.
    #[arg(long, default_value_t = 200)]
    pub cmc_top_n: usize,
}

/// The resolved, validated momentum configuration the strategy holds.
#[derive(Clone, Debug)]
pub struct Config {
    pub fast_days: u32,
    pub medium_days: u32,
    pub slow_days: u32,
    pub fast_weight: f64,
    pub medium_weight: f64,
    pub slow_weight: f64,
    pub top_n: usize,
    pub short_n: usize,
    /// Share of the gross budget on the long side (0.5 = dollar-neutral).
    pub long_short_balance: f64,
    pub regime_lookback_days: u32,
    /// Flatten to cash on a negative BTC trend.
    pub regime_filter: bool,
    /// Gross exposure as a fraction of equity, per rebalance.
    pub risk_fraction: f64,
    /// Within-side allocation tilt toward higher-conviction names (0 = equal).
    pub allocation_tilt: f64,
    /// Rebalance clock unit.
    pub holding_period: HoldingPeriod,
    /// Number of `holding_period` units between full re-ranks.
    pub number_holding_periods: u32,
    /// Where the universe (and the per-rebalance eligible set) comes from.
    pub source: Source,
    /// `--source coinmarketcap`: the snapshot directory.
    pub cmc_snapshots: PathBuf,
    /// `--source coinmarketcap`: snapshot depth treated as the top-N.
    pub cmc_top_n: usize,
}

/// Validate a parsed [`Args`] against the traded universe and resolve it into
/// a [`Config`]. `bases` is the universe this run will trade (from
/// [`crate::config::build_config`]); its size gates `--top-n` + `--short-n`,
/// and it must contain `BTC` when `--regime-filter` is on.
pub fn build(args: &Args, bases: &[String]) -> Result<Config> {
    ensure!(args.fast_days >= 1, "--fast-days must be >= 1");
    ensure!(args.medium_days >= 1, "--medium-days must be >= 1");
    ensure!(args.slow_days >= 1, "--slow-days must be >= 1");

    ensure!(
        args.fast_weight.is_finite() && args.fast_weight >= 0.0,
        "--fast-weight must be finite and >= 0, got {}",
        args.fast_weight
    );
    ensure!(
        args.medium_weight.is_finite() && args.medium_weight >= 0.0,
        "--medium-weight must be finite and >= 0, got {}",
        args.medium_weight
    );
    ensure!(
        args.slow_weight.is_finite() && args.slow_weight >= 0.0,
        "--slow-weight must be finite and >= 0, got {}",
        args.slow_weight
    );
    ensure!(
        args.fast_weight > 0.0 || args.medium_weight > 0.0 || args.slow_weight > 0.0,
        "--fast-weight, --medium-weight and --slow-weight cannot all be 0 \
         (the score would rank nothing)"
    );

    ensure!(args.top_n >= 1, "--top-n must be >= 1");
    ensure!(
        args.top_n + args.short_n <= bases.len(),
        "--top-n ({}) + --short-n ({}) = {} exceeds the universe size ({}); \
         the long and short slices would overlap",
        args.top_n,
        args.short_n,
        args.top_n + args.short_n,
        bases.len()
    );

    ensure!(
        (0.0..=1.0).contains(&args.long_short_balance),
        "--long-w must be in [0.0, 1.0], got {}",
        args.long_short_balance
    );

    ensure!(
        args.regime_lookback_days >= 1,
        "--regime-lookback-days must be >= 1"
    );

    ensure!(
        args.risk_fraction.is_finite() && args.risk_fraction > 0.0,
        "--risk-fraction must be finite and > 0, got {}",
        args.risk_fraction
    );
    ensure!(
        args.allocation_tilt.is_finite() && args.allocation_tilt >= 0.0,
        "--allocation-tilt must be finite and >= 0, got {}",
        args.allocation_tilt
    );

    ensure!(
        args.number_holding_periods >= 1,
        "--number-holding-periods must be >= 1"
    );

    ensure!(
        !args.regime_filter || bases.iter().any(|base| base.eq_ignore_ascii_case("BTC")),
        "--regime-filter is on but the universe has no BTC: the regime filter \
         reads BTC's trailing return"
    );

    if args.source == Source::Coinmarketcap {
        ensure!(
            args.cmc_top_n >= args.top_n + args.short_n,
            "--cmc-top-n ({}) must be >= --top-n + --short-n ({}); the eligible \
             pool would be smaller than the book",
            args.cmc_top_n,
            args.top_n + args.short_n
        );
    }

    Ok(Config {
        fast_days: args.fast_days,
        medium_days: args.medium_days,
        slow_days: args.slow_days,
        fast_weight: args.fast_weight,
        medium_weight: args.medium_weight,
        slow_weight: args.slow_weight,
        top_n: args.top_n,
        short_n: args.short_n,
        long_short_balance: args.long_short_balance,
        regime_lookback_days: args.regime_lookback_days,
        regime_filter: args.regime_filter,
        risk_fraction: args.risk_fraction,
        allocation_tilt: args.allocation_tilt,
        holding_period: args.holding_period,
        number_holding_periods: args.number_holding_periods,
        source: args.source,
        cmc_snapshots: args.cmc_snapshots.clone(),
        cmc_top_n: args.cmc_top_n,
    })
}

/// The rows this strategy appends to `runs/<uuid>/config.csv`, after the
/// shared run rows.
pub fn config_rows(cfg: &Config) -> Vec<(String, String)> {
    let mut rows = vec![
        ("fast_days".to_string(), cfg.fast_days.to_string()),
        ("medium_days".to_string(), cfg.medium_days.to_string()),
        ("slow_days".to_string(), cfg.slow_days.to_string()),
        ("fast_weight".to_string(), cfg.fast_weight.to_string()),
        ("medium_weight".to_string(), cfg.medium_weight.to_string()),
        ("slow_weight".to_string(), cfg.slow_weight.to_string()),
        ("top_n".to_string(), cfg.top_n.to_string()),
        ("short_n".to_string(), cfg.short_n.to_string()),
        ("long_short_balance".to_string(), cfg.long_short_balance.to_string()),
        (
            "regime_lookback_days".to_string(),
            cfg.regime_lookback_days.to_string(),
        ),
        ("regime_filter".to_string(), cfg.regime_filter.to_string()),
        ("risk_fraction".to_string(), cfg.risk_fraction.to_string()),
        (
            "allocation_tilt".to_string(),
            cfg.allocation_tilt.to_string(),
        ),
        (
            "holding_period".to_string(),
            cfg.holding_period.as_str().to_string(),
        ),
        (
            "number_holding_periods".to_string(),
            cfg.number_holding_periods.to_string(),
        ),
        ("source".to_string(), cfg.source.as_str().to_string()),
    ];
    if cfg.source == Source::Coinmarketcap {
        rows.push((
            "cmc_snapshots_dir".to_string(),
            cfg.cmc_snapshots.display().to_string(),
        ));
        rows.push(("cmc_top_n".to_string(), cfg.cmc_top_n.to_string()));
    }
    rows
}

/// The Bybit linear-perp instrument id for a base coin
/// (`BTC` → `BTCUSDT-LINEAR.BYBIT`).
#[must_use]
pub fn instrument_id(base: &str) -> InstrumentId {
    InstrumentId::from(format!("{base}USDT-LINEAR.{VENUE}").as_str())
}

/// [`instrument_id`] for every base in `bases`.
pub fn instrument_ids(bases: &[String]) -> Vec<InstrumentId> {
    bases.iter().map(|base| instrument_id(base)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` distinct base symbols including `BTC`, satisfying the regime-filter
    /// eligibility check.
    fn bases(n: usize) -> Vec<String> {
        let mut bases = vec!["BTC".to_string()];
        bases.extend((1..n).map(|i| format!("SYM{i}")));
        bases
    }

    /// Parse momentum flags (no program name, no subcommand) into [`Args`].
    fn args(extra: &[&str]) -> Args {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: Args,
        }

        let mut full = vec!["momentum"];
        full.extend_from_slice(extra);
        Wrap::try_parse_from(full).expect("args parse").args
    }

    #[test]
    fn defaults_are_stable() {
        let cfg = build(&args(&[]), &bases(20)).unwrap();
        assert_eq!(cfg.fast_days, 7);
        assert_eq!(cfg.medium_days, 3);
        assert_eq!(cfg.slow_days, 30);
        assert_eq!(cfg.fast_weight, 0.3);
        assert_eq!(cfg.medium_weight, 0.0);
        assert_eq!(cfg.slow_weight, 0.7);
        assert_eq!(cfg.top_n, 5);
        assert_eq!(cfg.short_n, 5);
        assert_eq!(cfg.long_short_balance, 0.5);
        assert_eq!(cfg.regime_lookback_days, 30);
        assert!(cfg.regime_filter);
        assert_eq!(cfg.risk_fraction, 0.8);
        assert_eq!(cfg.allocation_tilt, 0.0);
        assert_eq!(cfg.holding_period, HoldingPeriod::Day);
        assert_eq!(cfg.number_holding_periods, 7);
        assert_eq!(cfg.source, Source::Bybit);
        assert_eq!(cfg.cmc_snapshots.as_os_str(), "coins/cmc");
        assert_eq!(cfg.cmc_top_n, 200);
    }

    #[test]
    fn cmc_source_flows_through_and_gates_cmc_top_n() {
        let cfg = build(
            &args(&["--source", "coinmarketcap", "--cmc-top-n", "50"]),
            &bases(20),
        )
        .unwrap();
        assert_eq!(cfg.source, Source::Coinmarketcap);
        assert_eq!(cfg.cmc_top_n, 50);

        // default top-n 5 + short-n 5 = 10 <= 50: fine. Now make it fail.
        let err = build(
            &args(&["--source", "coinmarketcap", "--cmc-top-n", "8"]),
            &bases(20),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--cmc-top-n"), "{err}");
    }

    #[test]
    fn cmc_top_n_is_inert_for_the_bybit_source() {
        // absurd --cmc-top-n is fine when --source is bybit (the default)
        let cfg = build(&args(&["--cmc-top-n", "1"]), &bases(20)).unwrap();
        assert_eq!(cfg.cmc_top_n, 1);
        assert_eq!(cfg.source, Source::Bybit);
    }

    #[test]
    fn overrides_flow_through() {
        let cfg = build(
            &args(&["--fast-days", "2", "--top-n", "3", "--short-n", "0"]),
            &bases(20),
        )
        .unwrap();
        assert_eq!(cfg.fast_days, 2);
        assert_eq!(cfg.top_n, 3);
        assert_eq!(cfg.short_n, 0);
    }

    #[test]
    fn long_only_needs_no_btc() {
        // regime filter off and short-n 0: a BTC-less universe is fine.
        let no_btc: Vec<String> = (0..20).map(|i| format!("SYM{i}")).collect();
        let cfg = build(
            &args(&["--short-n", "0", "--regime-filter", "false"]),
            &no_btc,
        )
        .unwrap();
        assert_eq!(cfg.short_n, 0);
        assert!(!cfg.regime_filter);
    }

    #[test]
    fn regime_filter_requires_btc() {
        let no_btc: Vec<String> = (0..20).map(|i| format!("SYM{i}")).collect();
        let err = build(&args(&["--regime-filter", "true"]), &no_btc)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no BTC"), "{err}");
    }

    #[test]
    fn regime_filter_parses_true_and_false() {
        assert!(
            build(&args(&["--regime-filter", "true"]), &bases(20))
                .unwrap()
                .regime_filter
        );
        assert!(
            !build(&args(&["--regime-filter", "false"]), &bases(20))
                .unwrap()
                .regime_filter
        );
    }

    #[test]
    fn rejects_long_and_short_slices_overlapping() {
        // top_n 12 + short_n 12 = 24 > universe 20
        let err = build(&args(&["--top-n", "12", "--short-n", "12"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("would overlap"), "{err}");
    }

    #[test]
    fn rejects_top_n_of_zero() {
        let err = build(&args(&["--top-n", "0"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--top-n must be >= 1"), "{err}");
    }

    #[test]
    fn rejects_long_short_balance_out_of_range() {
        let err = build(&args(&["--long-short-balance", "1.5"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be in [0.0, 1.0]"), "{err}");
    }

    #[test]
    fn rejects_negative_weight() {
        let err = build(&args(&["--fast-weight=-0.1"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--fast-weight must be finite and >= 0"),
            "{err}"
        );
    }

    #[test]
    fn rejects_all_weights_zero() {
        let err = build(
            &args(&[
                "--fast-weight",
                "0",
                "--medium-weight",
                "0",
                "--slow-weight",
                "0",
            ]),
            &bases(20),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("cannot all be 0"), "{err}");
    }

    #[test]
    fn cadence_flags_flow_through() {
        let cfg = build(
            &args(&[
                "--holding-period",
                "iso-week",
                "--number-holding-periods",
                "3",
            ]),
            &bases(20),
        )
        .unwrap();
        assert_eq!(cfg.holding_period, HoldingPeriod::IsoWeek);
        assert_eq!(cfg.number_holding_periods, 3);
    }

    #[test]
    fn rejects_zero_holding_periods() {
        let err = build(&args(&["--number-holding-periods", "0"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--number-holding-periods must be >= 1"),
            "{err}"
        );
    }

    #[test]
    fn instrument_ids_are_bybit_linear_perps() {
        let ids = instrument_ids(&["BTC".to_string(), "ETH".to_string()]);
        assert_eq!(ids[0], InstrumentId::from("BTCUSDT-LINEAR.BYBIT"));
        assert_eq!(ids[1], InstrumentId::from("ETHUSDT-LINEAR.BYBIT"));
    }
}
