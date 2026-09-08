//! Backtest bootstrap: turn a resolved [`RunConfig`], a [`MarketData`] source
//! and a strategy into a wired [`BacktestEngine`], then run it.
//!
//! The instruments and bars come through the [`MarketData`] seam, so a test can
//! drive the whole bring-up over in-memory fixtures
//! ([`InMemoryMarketData`](crate::data::exchange::InMemoryMarketData)) with no
//! network. The venue, starting balance and bar timeframe come from the
//! strategy's own [`Market`](crate::strategy::common::Market) and the
//! [`RunConfig`]; nothing here is strategy-specific.
//!
//! Only the `Backtest` environment lives here. The `Live` / `Sandbox`
//! bootstraps are different enough (a `LiveNode`, real venue clients) that
//! `src/main.rs` keeps them.

use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::Data,
    enums::{AccountType, BookType, OmsType},
    identifiers::{InstrumentId, Venue},
    instruments::Instrument,
    types::Money,
};

use crate::{config::RunConfig, data::exchange::MarketData, strategy::common::StrategyRuntime};

/// Wire a [`BacktestEngine`] for `strategy`: its venue and starting balance,
/// the `instrument_ids` subset of the venue's instruments, and each of their
/// bar histories — every instrument and bar pulled from `market`.
///
/// Does not run the engine, so no `runs/<uuid>/` artifacts are written yet;
/// [`run_backtest`] does that. Split out so a test can assert the bring-up over
/// an [`InMemoryMarketData`](crate::data::exchange::InMemoryMarketData) fixture.
pub fn build_backtest_engine<S: StrategyRuntime>(
    run: &RunConfig,
    market: &dyn MarketData,
    instrument_ids: &[InstrumentId],
    strategy: S,
) -> Result<BacktestEngine> {
    let market_surface = strategy.market();

    // Already validated in `config::build_config`; re-parsed (not `unwrap`ed)
    // because `RunConfig` keeps the raw strings for the config sidecar.
    let starting_balance = Money::from_str(run.starting_balance.trim())
        .map_err(|e| anyhow!("--starting-balance {:?}: {e}", run.starting_balance))?;

    let mut engine = BacktestEngine::new(BacktestEngineConfig::default())?;
    engine.add_venue(
        SimulatedVenueConfig::builder()
            .venue(Venue::from(market_surface.venue))
            .oms_type(OmsType::Hedging)
            .account_type(AccountType::Margin)
            .book_type(BookType::L1_MBP)
            .starting_balances(vec![starting_balance])
            .build()?,
    )?;

    let instruments = market.instruments().context("fetch instruments")?;
    for inst in &instruments {
        if instrument_ids.contains(&inst.id()) {
            engine.add_instrument(inst)?;
        }
    }
    for id in instrument_ids {
        let bars = market
            .bars(*id, market_surface.timeframe)
            .with_context(|| format!("fetch bars for {id}"))?;
        log::info!("loaded {} bars for {id}", bars.len());
        engine.add_data(bars.into_iter().map(Data::Bar).collect(), None, false, true)?;
    }

    engine.add_strategy(strategy)?;
    Ok(engine)
}

/// [`build_backtest_engine`], then run it over the [`RunConfig`]'s date window.
/// The run triggers the strategy's `on_start`, which opens the run's capture
/// files under `runs/<uuid>/`.
pub fn run_backtest<S: StrategyRuntime>(
    run: &RunConfig,
    market: &dyn MarketData,
    instrument_ids: &[InstrumentId],
    strategy: S,
) -> Result<()> {
    let start = UnixNanos::from_str(run.date_start.trim())
        .map_err(|e| anyhow!("--date-start {:?}: {e}", run.date_start))?;
    let end = UnixNanos::from_str(run.date_end.trim())
        .map_err(|e| anyhow!("--date-end {:?}: {e}", run.date_end))?;

    let mut engine = build_backtest_engine(run, market, instrument_ids, strategy)?;
    engine.run(Some(start), Some(end), None, false)?;
    Ok(())
}
