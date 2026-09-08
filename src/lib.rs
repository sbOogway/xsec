//! Library surface for the strategy backtests.
//!
//! The binary (`src/main.rs`) owns the top-level CLI parser (it composes
//! [`config::SharedArgs`] with the strategy subcommand) plus the engine/live
//! bootstrap; everything else lives here:
//!
//! * [`strategy`] — one folder per strategy over the shared
//!   [`strategy::common`] mechanics, which in turn owns the percent-of-equity
//!   [`sizing`](strategy::common::sizing) helpers and the rolling price
//!   [`buffer`](strategy::common::buffer).
//! * [`data`] — the Bybit fetch/cache layer
//!   ([`data::exchange::bybit`]), the [`universe`](data::universe) file reader
//!   and run-artifact [`capture`](data::backtest).
//! * [`config`] and [`period`] — the shared run flags / resolved config and the
//!   cadence-agnostic rebalance period. At the root, depending on neither
//!   `strategy` nor `data`, so both can build on them without a cycle.

pub mod config;
pub mod data;
pub mod period;
pub mod strategy;
