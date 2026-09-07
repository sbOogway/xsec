//! Inputs for the top-5 momentum strategy: its CLI flags ([`Args`]), its
//! resolved-and-validated [`Config`], the rows it contributes to
//! `runs/<uuid>/config.csv` ([`config_rows`]), and the market it trades
//! ([`VENUE`], [`TIMEFRAME`], [`instrument_ids`]).

use anyhow::{Result, ensure};
use clap::Args as ClapArgs;
use nautilus_model::{enums::BarAggregation, identifiers::InstrumentId};

use crate::strategy::common::Market;

/// The trading venue. Bybit-only: the data layer talks to the Bybit HTTP API
/// and nothing else.
pub const VENUE: &str = "BYBIT";

/// Bar size the strategy ranks on: daily bars, rebalanced weekly.
pub const TIMEFRAME: BarAggregation = BarAggregation::Day;

/// The market this strategy trades, for [`crate::strategy::common::StrategyRuntime`].
pub const MARKET: Market = Market {
    venue: VENUE,
    timeframe: TIMEFRAME,
};

/// Top-5 momentum flags. Defaults are a reasonable starting point; every knob
/// here is tunable per run.
#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Fast-momentum lookback, in daily bars.
    #[arg(long, default_value_t = 1)]
    pub fast_days: u32,

    /// Medium-momentum lookback, in daily bars.
    #[arg(long, default_value_t = 3)]
    pub medium_days: u32,

    /// Slow-momentum lookback, in daily bars.
    #[arg(long, default_value_t = 7)]
    pub slow_days: u32,

    /// Weight on fast momentum in the composite score.
    #[arg(long, default_value_t = 0.3)]
    pub fast_weight: f64,

    /// Weight on medium momentum in the composite score.
    #[arg(long, default_value_t = 0.2)]
    pub medium_weight: f64,

    /// Weight on slow momentum in the composite score.
    #[arg(long, default_value_t = 0.5)]
    pub slow_weight: f64,

    /// Number of names held long each week.
    #[arg(long, default_value_t = 5)]
    pub top_n: usize,

    /// BTC trailing-return window for the regime filter, in days.
    #[arg(long, default_value_t = 20)]
    pub regime_lookback_days: u32,

    /// Gross exposure as a fraction of equity, per rebalance (long-only, so
    /// this is net exposure too).
    #[arg(long, default_value_t = 0.8)]
    pub risk_fraction: f64,

    /// Within-book tilt toward higher-conviction names. `0.0` = equal dollars
    /// per leg.
    #[arg(long, default_value_t = 0.0)]
    pub allocation_tilt: f64,

    /// Holding period, in weeks. Only `1` is currently supported.
    #[arg(long, default_value_t = 1)]
    pub holding_weeks: u16,
}

/// The resolved, validated top-5 momentum configuration the strategy holds.
#[derive(Clone, Debug)]
pub struct Config {
    pub fast_days: u32,
    pub medium_days: u32,
    pub slow_days: u32,
    pub fast_weight: f64,
    pub medium_weight: f64,
    pub slow_weight: f64,
    pub top_n: usize,
    pub regime_lookback_days: u32,
    /// Gross exposure as a fraction of equity, per rebalance.
    pub risk_fraction: f64,
    /// Within-book allocation tilt toward higher-conviction names (0 = equal).
    pub allocation_tilt: f64,
    pub holding_weeks: u16,
}

/// Validate a parsed [`Args`] against the traded universe and resolve it into
/// a [`Config`]. `bases` is the universe this run will trade (from
/// [`crate::config::build_config`]); its size gates `--top-n`, and it must
/// contain `BTC` for the regime filter to read from.
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
        args.top_n <= bases.len(),
        "--top-n={} exceeds the universe size ({})",
        args.top_n,
        bases.len()
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
        args.holding_weeks == 1,
        "--holding-weeks={} is not supported: the rebalance path assumes a one-week hold",
        args.holding_weeks
    );

    ensure!(
        bases.iter().any(|base| base.eq_ignore_ascii_case("BTC")),
        "universe must include BTC: the regime filter reads BTC's trailing return"
    );

    Ok(Config {
        fast_days: args.fast_days,
        medium_days: args.medium_days,
        slow_days: args.slow_days,
        fast_weight: args.fast_weight,
        medium_weight: args.medium_weight,
        slow_weight: args.slow_weight,
        top_n: args.top_n,
        regime_lookback_days: args.regime_lookback_days,
        risk_fraction: args.risk_fraction,
        allocation_tilt: args.allocation_tilt,
        holding_weeks: args.holding_weeks,
    })
}

/// The rows this strategy appends to `runs/<uuid>/config.csv`, after the
/// shared run rows.
pub fn config_rows(cfg: &Config) -> Vec<(String, String)> {
    vec![
        ("fast_days".to_string(), cfg.fast_days.to_string()),
        ("medium_days".to_string(), cfg.medium_days.to_string()),
        ("slow_days".to_string(), cfg.slow_days.to_string()),
        ("fast_weight".to_string(), cfg.fast_weight.to_string()),
        ("medium_weight".to_string(), cfg.medium_weight.to_string()),
        ("slow_weight".to_string(), cfg.slow_weight.to_string()),
        ("top_n".to_string(), cfg.top_n.to_string()),
        (
            "regime_lookback_days".to_string(),
            cfg.regime_lookback_days.to_string(),
        ),
        ("risk_fraction".to_string(), cfg.risk_fraction.to_string()),
        ("allocation_tilt".to_string(), cfg.allocation_tilt.to_string()),
        ("holding_weeks".to_string(), cfg.holding_weeks.to_string()),
    ]
}

/// Bybit linear-perp instrument ids for `bases` (`BTC` → `BTCUSDT-LINEAR.BYBIT`).
pub fn instrument_ids(bases: &[String]) -> Vec<InstrumentId> {
    bases
        .iter()
        .map(|base| InstrumentId::from(format!("{base}USDT-LINEAR.{VENUE}").as_str()))
        .collect()
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

    /// Parse top5-momentum-filtered flags (no program name, no subcommand) into [`Args`].
    fn args(extra: &[&str]) -> Args {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: Args,
        }

        let mut full = vec!["top5-momentum-filtered"];
        full.extend_from_slice(extra);
        Wrap::try_parse_from(full).expect("args parse").args
    }

    #[test]
    fn defaults_are_stable() {
        let cfg = build(&args(&[]), &bases(20)).unwrap();
        assert_eq!(cfg.fast_days, 1);
        assert_eq!(cfg.medium_days, 3);
        assert_eq!(cfg.slow_days, 7);
        assert_eq!(cfg.fast_weight, 0.3);
        assert_eq!(cfg.medium_weight, 0.2);
        assert_eq!(cfg.slow_weight, 0.5);
        assert_eq!(cfg.top_n, 5);
        assert_eq!(cfg.regime_lookback_days, 20);
        assert_eq!(cfg.risk_fraction, 0.8);
        assert_eq!(cfg.allocation_tilt, 0.0);
        assert_eq!(cfg.holding_weeks, 1);
    }

    #[test]
    fn overrides_flow_through() {
        let cfg = build(
            &args(&["--fast-days", "2", "--top-n", "3"]),
            &bases(20),
        )
        .unwrap();
        assert_eq!(cfg.fast_days, 2);
        assert_eq!(cfg.top_n, 3);
    }

    #[test]
    fn rejects_top_n_above_universe_size() {
        let err = build(&args(&["--top-n", "25"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("exceeds the universe size"), "{err}");
    }

    #[test]
    fn rejects_top_n_of_zero() {
        let err = build(&args(&["--top-n", "0"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--top-n must be >= 1"), "{err}");
    }

    #[test]
    fn rejects_universe_missing_btc() {
        let bases_without_btc: Vec<String> = (0..20).map(|i| format!("SYM{i}")).collect();
        let err = build(&args(&[]), &bases_without_btc)
            .unwrap_err()
            .to_string();
        assert!(err.contains("must include BTC"), "{err}");
    }

    #[test]
    fn rejects_negative_weight() {
        let err = build(&args(&["--fast-weight=-0.1"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--fast-weight must be finite and >= 0"), "{err}");
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
    fn rejects_multi_week_hold() {
        let err = build(&args(&["--holding-weeks", "2"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("one-week hold"), "{err}");
    }

    #[test]
    fn instrument_ids_are_bybit_linear_perps() {
        let ids = instrument_ids(&["BTC".to_string(), "ETH".to_string()]);
        assert_eq!(ids[0], InstrumentId::from("BTCUSDT-LINEAR.BYBIT"));
        assert_eq!(ids[1], InstrumentId::from("ETHUSDT-LINEAR.BYBIT"));
    }
}
