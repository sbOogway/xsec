//! Library surface for the strategy backtests.
//!
//! The binary (`src/main.rs`) is arg-parsing plus engine/live bootstrap;
//! everything else lives here:
//!
//! * [`strategy`] — one folder per strategy over the shared
//!   [`strategy::common`] mechanics, which in turn owns the cadence-agnostic
//!   rebalance [`period`](strategy::common::period), the percent-of-equity
//!   [`sizing`](strategy::common::sizing) helpers and the rolling price
//!   [`buffer`](strategy::common::buffer).
//! * [`config`] — the shared run configuration.
//! * [`data`] — the Bybit fetch/cache layer
//!   ([`data::exchange::bybit`]), the [`universe`](data::universe) file reader
//!   and run-artifact [`capture`](data::backtest).

pub mod config;
pub mod data;
pub mod strategy;
