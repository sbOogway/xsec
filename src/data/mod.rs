//! Market data and per-run I/O.
//!
//! * [`exchange`] — the [`MarketData`](exchange::MarketData) seam and its
//!   adapters: Bybit ([`exchange::bybit`], the only venue today) and an
//!   in-memory fake ([`exchange::InMemoryMarketData`]).
//! * [`universe`] — the plain-text trading-universe file reader.
//! * [`backtest`] — per-run artifact capture (`runs/<uuid>/{config,legs,
//!   portfolio,fills}.csv`).

pub mod backtest;
pub mod exchange;
pub mod universe;
