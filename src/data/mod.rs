//! Market data and per-run I/O.
//!
//! * [`exchange`] — the venue adapters that fetch and cache bar history and
//!   instruments. Bybit ([`exchange::bybit`]) is the only venue today.
//! * [`universe`] — the plain-text trading-universe file reader.
//! * [`backtest`] — per-run artifact capture (`runs/<uuid>/{config,legs,
//!   portfolio,fills}.csv`).

pub mod backtest;
pub mod exchange;
pub mod universe;
