//! Market data and per-run I/O.
//!
//! * [`exchange`] — the [`MarketData`](exchange::MarketData) seam and its
//!   adapters (the offline `data/` cache the backtest reads, an in-memory fake),
//!   plus the `data/` cache layout ([`exchange::cache`]), the Bybit HTTP surface
//!   ([`exchange::bybit`]) and `xsec fetch` ([`exchange::fetch`]) that fills it.
//! * [`universe`] — the plain-text trading-universe file reader.
//! * [`backtest`] — per-run artifact capture (`runs/<uuid>/{config,legs,
//!   portfolio,fills}.csv`).

pub mod backtest;
pub mod exchange;
pub mod universe;
