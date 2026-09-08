//! Library surface for the strategy backtests.
//!
//! The binary (`src/main.rs`) owns the top-level CLI parser (it composes
//! [`config::SharedArgs`] with the strategy subcommand) and the live / sandbox
//! bootstrap; everything else lives here:
//!
//! * [`strategy`] — one folder per strategy over the shared
//!   [`strategy::common`] mechanics, which in turn owns the percent-of-equity
//!   [`sizing`](strategy::common::sizing) helpers and the rolling price
//!   [`buffer`](strategy::common::buffer).
//! * [`data`] — the market-data seam ([`data::exchange::MarketData`]): the
//!   offline `data/` cache the backtest reads
//!   ([`data::exchange::CachedMarketData`]), the cache layout
//!   ([`data::exchange::cache`]), the Bybit HTTP surface
//!   ([`data::exchange::bybit`]) and `xsec fetch` ([`data::exchange::fetch`])
//!   that fills the cache, plus the [`universe`](data::universe) file reader and
//!   run-artifact [`capture`](data::backtest).
//! * [`engine`] — the backtest bootstrap: `RunConfig` + `MarketData` + strategy
//!   into a wired [`BacktestEngine`](engine::build_backtest_engine), then run.
//! * [`config`] and [`period`] — the shared run flags / resolved config and the
//!   cadence-agnostic rebalance period. At the root, depending on neither
//!   `strategy` nor `data`, so both can build on them without a cycle.

pub mod config;
pub mod data;
pub mod engine;
pub mod period;
pub mod strategy;
