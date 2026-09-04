//! Inputs for the cross-sectional momentum strategy: its CLI flags ([`Args`]),
//! its resolved-and-validated [`Config`], the rows it contributes to
//! `runs/<uuid>/config.csv` ([`config_rows`]), and the market it trades
//! ([`VENUE`], [`TIMEFRAME`], [`instrument_ids`]).
//!
//! Every flag defaults to the value this strategy used to carry as a `const`;
//! [`build`] turns a parsed [`Args`] into a [`Config`] or a clear `anyhow`
//! error. Shared run inputs (universe file, dates, starting balance, uuid) are
//! global flags handled in [`crate::config`].

use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use clap::Args as ClapArgs;
use nautilus_model::{enums::BarAggregation, identifiers::InstrumentId};
use rust_decimal::{Decimal, prelude::ToPrimitive};

use crate::strategy::common::Market;

/// The trading venue. Bybit-only: the data layer talks to the Bybit HTTP API
/// and nothing else.
pub const VENUE: &str = "BYBIT";

/// Bar size the strategy ranks on. Monthly-only: the rebalance cadence, the
/// holding-period return and the capture schema all assume calendar months.
pub const TIMEFRAME: BarAggregation = BarAggregation::Month;

/// The market this strategy trades, for [`crate::strategy::common::Harness`].
pub const MARKET: Market = Market {
    venue: VENUE,
    timeframe: TIMEFRAME,
};

/// The largest `--percentile` that still yields two non-overlapping deciles.
const MAX_PERCENTILE: &str = "0.5";

/// Cross-sectional momentum flags. Defaults reproduce the constants this
/// strategy used to carry; override any of them per run.
#[derive(ClapArgs, Debug)]
pub struct Args {
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
}

/// The resolved, validated momentum configuration the strategy holds.
#[derive(Clone, Debug)]
pub struct Config {
    pub lookback_months: u16,
    pub holding_months: u16,
    pub percentile: String,
    /// Gross exposure target as a fraction of account equity, per rebalance.
    pub risk_pct: f64,
    /// Share of the gross budget on the long side (0.5 = dollar-neutral).
    pub long_w: f64,
    /// Within-side allocation tilt toward higher-conviction names (0 = equal).
    pub signal_tilt: f64,
}

/// Validate a parsed [`Args`] against the traded universe and resolve it into a
/// [`Config`]. `bases` is the universe this run will trade (from
/// [`crate::config::build_config`]); its size gates `--percentile`.
pub fn build(args: &Args, bases: &[String]) -> Result<Config> {
    let n = bases.len();

    let percentile = Decimal::from_str(args.percentile.trim())
        .with_context(|| format!("--percentile {:?} is not a number", args.percentile))?;
    // The 0.5 ceiling is also what keeps the long and short slices in
    // `on_time_event` from overlapping: `floor(percentile * n) <= n / 2`.
    let max = Decimal::from_str(MAX_PERCENTILE).unwrap();
    ensure!(
        percentile > Decimal::ZERO && percentile <= max,
        "--percentile must be in (0, {max}], got {percentile}"
    );

    // Mirror the runtime cut in `on_time_event`: `floor(percentile * n)` names
    // per side. Below 1 there are no trades at all.
    let per_side = (percentile * Decimal::from(n)).floor().to_usize().unwrap_or(0);
    let need = (Decimal::ONE / percentile).ceil();
    ensure!(
        per_side >= 1,
        "universe has {n} names but --percentile {percentile} selects none per side \
         (need at least {need}); add names or raise --percentile"
    );

    ensure!(args.lookback_months >= 1, "--lookback-months must be >= 1");
    ensure!(
        args.holding_months == 1,
        "--holding-months={} is not supported: the rebalance path assumes a one-month hold. \
         Revisit the age-based close in on_time_event before changing this.",
        args.holding_months
    );

    ensure!(
        (0.0..=1.0).contains(&args.long_w),
        "--long-w must be in [0.0, 1.0], got {}",
        args.long_w
    );
    ensure!(
        args.risk_pct.is_finite() && args.risk_pct > 0.0,
        "--risk-pct must be finite and > 0, got {}",
        args.risk_pct
    );
    ensure!(
        args.signal_tilt.is_finite() && args.signal_tilt >= 0.0,
        "--signal-tilt must be finite and >= 0, got {}",
        args.signal_tilt
    );

    Ok(Config {
        lookback_months: args.lookback_months,
        holding_months: args.holding_months,
        percentile: args.percentile.trim().to_string(),
        risk_pct: args.risk_pct,
        long_w: args.long_w,
        signal_tilt: args.signal_tilt,
    })
}

/// The rows this strategy appends to `runs/<uuid>/config.csv`, after the shared
/// run rows.
pub fn config_rows(cfg: &Config) -> Vec<(String, String)> {
    vec![
        ("lookback_months".to_string(), cfg.lookback_months.to_string()),
        ("holding_months".to_string(), cfg.holding_months.to_string()),
        ("percentile".to_string(), cfg.percentile.clone()),
        ("risk_pct".to_string(), cfg.risk_pct.to_string()),
        ("long_w".to_string(), cfg.long_w.to_string()),
        ("signal_tilt".to_string(), cfg.signal_tilt.to_string()),
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

    /// `n` distinct base symbols, enough for the default decile at `n >= 10`.
    fn bases(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("SYM{i}")).collect()
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
    fn defaults_match_the_legacy_constants() {
        let cfg = build(&args(&[]), &bases(20)).unwrap();
        assert_eq!(cfg.lookback_months, 3);
        assert_eq!(cfg.holding_months, 1);
        assert_eq!(cfg.percentile, "0.1");
        assert_eq!(cfg.risk_pct, 0.8);
        assert_eq!(cfg.long_w, 0.5);
        assert_eq!(cfg.signal_tilt, 0.0);
    }

    #[test]
    fn overrides_flow_through() {
        let cfg = build(
            &args(&["--lookback-months", "6", "--long-w", "0.7"]),
            &bases(40),
        )
        .unwrap();
        assert_eq!(cfg.lookback_months, 6);
        assert_eq!(cfg.long_w, 0.7);
    }

    #[test]
    fn rejects_percentile_out_of_range() {
        let err = build(&args(&["--percentile", "0.9"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--percentile must be in"), "{err}");
    }

    #[test]
    fn rejects_universe_too_small_for_percentile() {
        // floor(0.1 * 6) == 0 names per side
        let err = build(&args(&[]), &bases(6)).unwrap_err().to_string();
        assert!(err.contains("selects none per side"), "{err}");
    }

    #[test]
    fn rejects_long_w_above_one() {
        let err = build(&args(&["--long-w", "1.5"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--long-w must be in"), "{err}");
    }

    #[test]
    fn rejects_multi_month_hold() {
        let err = build(&args(&["--holding-months", "2"]), &bases(20))
            .unwrap_err()
            .to_string();
        assert!(err.contains("one-month hold"), "{err}");
    }

    #[test]
    fn instrument_ids_are_bybit_linear_perps() {
        let ids = instrument_ids(&["BTC".to_string(), "ETH".to_string()]);
        assert_eq!(ids[0], InstrumentId::from("BTCUSDT-LINEAR.BYBIT"));
        assert_eq!(ids[1], InstrumentId::from("ETHUSDT-LINEAR.BYBIT"));
    }
}
