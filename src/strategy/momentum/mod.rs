//! Jegadeesh–Titman cross-sectional momentum over Bybit USDT-margined linear
//! perpetuals.
//!
//! Each calendar month the universe is ranked by trailing return over the
//! formation window; the run goes long the top `percentile` and short the
//! bottom `percentile`, sized to a fraction of account equity, and holds for
//! one month. [`config`] owns the knobs and the market; [`strategy`] owns the
//! ranking and the budget split — everything else comes from
//! [`crate::strategy::common`].

pub mod config;
pub mod strategy;

pub use config::{Args, Config};
pub use strategy::XSectionalMomentum;
