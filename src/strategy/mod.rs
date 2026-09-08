//! Trading strategies.
//!
//! One folder per strategy, each with a `strategy.rs` (the Nautilus strategy)
//! and a `config.rs` (its CLI flags, its resolved config, its `config.csv`
//! rows, and the market it trades). Signal-agnostic backtest wiring — the
//! rebalance clock, the price buffers, artifact capture, notional sizing —
//! plus small cross-strategy helpers, lives once in [`common`].
//!
//! The binary picks a strategy with a clap subcommand ([`StrategyKind`]); each
//! variant carries that strategy's [`clap::Args`].

pub mod common;
pub mod momentum;

use clap::Subcommand;

/// Which strategy a run drives. One subcommand per strategy, each carrying that
/// strategy's flags (`xsec momentum --top-n 3`).
#[derive(Subcommand, Debug)]
pub enum StrategyKind {
    /// Composite fast/medium/slow momentum on a configurable rebalance cadence
    /// (`--holding-period`, default daily): long the top `--top-n` names, short
    /// the bottom `--short-n` (`--short-n 0` = long-only), trading only the
    /// carried-book delta each re-rank, with an optional BTC regime filter
    /// (`--regime-filter`) that flattens to cash on a negative trend.
    Momentum(momentum::config::Args),
}

impl StrategyKind {
    /// The canonical name recorded as `strategy` in `runs/<uuid>/config.csv`.
    pub fn name(&self) -> &'static str {
        match self {
            StrategyKind::Momentum(_) => "momentum",
        }
    }
}
