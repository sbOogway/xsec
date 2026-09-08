//! Composite-momentum strategy over Bybit USDT-margined linear perpetuals,
//! rebalanced on a configurable cadence (`--holding-period`, default daily).
//!
//! Each re-rank scores the universe by a composite fast/medium/slow momentum
//! score, holds the top `--top-n` names long and the bottom `--short-n` short
//! (`--short-n 0` = long-only), trades only the change against the carried
//! book, and — with `--regime-filter` on — flattens to cash on a negative BTC
//! trend.
//!
//! [`config`] owns the knobs and the market; [`strategy`] owns the ranking and
//! the budget split — everything else comes from [`crate::strategy::common`].

pub mod config;
pub mod strategy;

pub use config::{Args, Config};
pub use strategy::Momentum;
