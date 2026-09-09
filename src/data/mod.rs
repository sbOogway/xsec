//! Market data and per-run I/O.
//!
//! * [`exchange`] — two seams: [`MarketData`](exchange::MarketData) (what the
//!   backtest reads — the offline `data/<venue>/` cache or an in-memory fake)
//!   and [`ExchangeAdapter`](exchange::ExchangeAdapter) (what `xsec fetch`
//!   ([`exchange::fetch`]) pulls from — [`exchange::bybit`] is the first impl).
//!   [`exchange::cache`] owns the on-disk layout both agree on.
//! * [`universe`] — the plain-text trading-universe file reader.
//! * [`backtest`] — per-run artifact capture (`runs/<uuid>/{config,legs,
//!   portfolio,fills}.csv`).

pub mod backtest;
pub mod exchange;
pub mod universe;
