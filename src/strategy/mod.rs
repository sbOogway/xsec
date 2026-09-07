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
pub mod top5_momentum_filtered;

use clap::Subcommand;

/// Which strategy a run drives. One subcommand per strategy, each carrying that
/// strategy's flags (`xsec momentum --lookback-months 6`).
#[derive(Subcommand, Debug)]
pub enum StrategyKind {
    /// Jegadeesh–Titman cross-sectional momentum: each month go long the top
    /// decile and short the bottom decile of the universe by trailing return.
    Momentum(momentum::config::Args),
    /// Long-only top-5 composite momentum, rebalanced weekly, with a BTC
    /// regime filter that flattens the book to cash on a negative trend.
    Top5MomentumFiltered(top5_momentum_filtered::config::Args),
}

impl StrategyKind {
    /// The canonical name recorded as `strategy` in `runs/<uuid>/config.csv`.
    pub fn name(&self) -> &'static str {
        match self {
            StrategyKind::Momentum(_) => "cross_sectional_momentum",
            StrategyKind::Top5MomentumFiltered(_) => "top5_momentum_filtered",
        }
    }
}
