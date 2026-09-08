//! Market data and per-run I/O.
//!
//! * [`exchange`] — the [`MarketData`](exchange::MarketData) seam and its
//!   adapters: the offline `data/` cache ([`exchange::CachedMarketData`], what
//!   the backtest reads), the Bybit HTTP fetcher ([`exchange::bybit`]) and an
//!   in-memory fake ([`exchange::InMemoryMarketData`]).
//! * [`fetch`] — `xsec fetch`: fill the `data/` cache from Bybit.
//! * [`universe`] — the plain-text trading-universe file reader.
//! * [`backtest`] — per-run artifact capture (`runs/<uuid>/{config,legs,
//!   portfolio,fills}.csv`).

pub mod backtest;
pub mod exchange;
pub mod fetch;
pub mod universe;
