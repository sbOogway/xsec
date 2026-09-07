//! Long-only top-5 composite-momentum strategy over Bybit USDT-margined linear
//! perpetuals, rebalanced daily with a BTC trend regime filter.
//!
//! [`config`] owns the knobs and the market; [`strategy`] owns the ranking and
//! the budget split — everything else comes from [`crate::strategy::common`].

pub mod config;
pub mod strategy;

pub use config::{Args, Config};
pub use strategy::Top5MomentumFiltered;
