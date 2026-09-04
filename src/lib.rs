//! Library surface for the strategy backtests.
//!
//! The binary (`src/main.rs`) is arg-parsing plus engine/live bootstrap;
//! everything else lives here: the [`strategy`] module (one folder per strategy,
//! over the shared [`strategy::common`] harness), the shared run [`config`], the
//! [`universe`] file reader, run-artifact [`capture`], the [`sizing`] helpers
//! and the Bybit [`data`] cache.

pub mod capture;
pub mod config;
pub mod data;
pub mod sizing;
pub mod strategy;
pub mod universe;