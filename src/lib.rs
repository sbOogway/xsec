//! Library surface for the cross-sectional momentum backtest.
//!
//! The binary (`src/main.rs`) owns the strategy and engine wiring; the pieces
//! that are worth testing on their own — run-artifact capture and the Bybit
//! data cache — live here.

pub mod capture;
pub mod data;
