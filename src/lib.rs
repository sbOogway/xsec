//! Library surface for the strategy backtests.
//!
//! The binary (`src/main.rs`) is arg-parsing plus engine/live bootstrap;
//! everything else lives here:
//!
//! * [`strategy`] — one folder per strategy over the shared
//!   [`strategy::common`] mechanics, which in turn owns the percent-of-equity
//!   [`sizing`](strategy::common::sizing) helpers and the rolling price
//!   [`buffer`](strategy::common::buffer).
//! * [`data`] — the Bybit fetch/cache layer
//!   ([`data::exchange::bybit`]), the [`universe`](data::universe) file reader
//!   and run-artifact [`capture`](data::backtest).
//! * [`config`] and [`period`] — the shared run configuration and the
//!   cadence-agnostic rebalance period, at the root because both `strategy` and
//!   `data` build on them.

pub mod config;
pub mod data;
pub mod period;
pub mod strategy;
