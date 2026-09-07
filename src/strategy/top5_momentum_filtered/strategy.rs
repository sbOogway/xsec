//! The top-5 momentum strategy: rank the universe by a composite fast/medium/
//! slow momentum score, go long the top names, hold a week, repeat —
//! flattening to cash whenever BTC's trend regime filter is negative.
//! Everything that is not the signal — the rebalance clock, the price
//! buffers, artifact capture, notional-sized orders — comes from
//! [`crate::strategy::runtime::StrategyRuntime`].

use std::fmt::Debug;

use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_model::{data::Bar, identifiers::StrategyId};
use nautilus_trading::{StrategyConfig, StrategyCore, nautilus_strategy};

use crate::{
    config::RunConfig,
    period::IsoWeek,
    strategy::runtime::{Market, RuntimeState, StrategyRuntime},
};

use super::config::{self, Config};

#[derive(bon::Builder)]
pub struct Top5MomentumFiltered {
    #[builder(default = StrategyCore::new(StrategyConfig {
         strategy_id: Some(StrategyId::from("TOP5-MOM-FILTERED")),
         order_id_tag: Some("001".to_string()),
         ..Default::default()
    }))]
    core: StrategyCore,

    /// Shared run configuration: universe, dates, starting balance, uuid.
    run: RunConfig,

    /// This strategy's resolved knobs.
    config: Config,

    /// Signal-agnostic backtest state: universe ids, rebalance clock, rolling
    /// price buffers, capture handle. Filled in `on_start`.
    #[builder(skip)]
    runtime: RuntimeState<IsoWeek>,
}

nautilus_strategy!(Top5MomentumFiltered);

impl StrategyRuntime for Top5MomentumFiltered {
    type Period = IsoWeek;

    fn runtime(&self) -> &RuntimeState<IsoWeek> {
        &self.runtime
    }
    fn runtime_mut(&mut self) -> &mut RuntimeState<IsoWeek> {
        &mut self.runtime
    }
    fn market(&self) -> Market {
        config::MARKET
    }
}

impl Debug for Top5MomentumFiltered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Top5MomentumFiltered")
            .field("run", &self.run)
            .field("config", &self.config)
            .field("core", &self.core)
            .field("instruments", &self.runtime.instruments)
            .finish()
    }
}

impl DataActor for Top5MomentumFiltered {
    fn on_start(&mut self) -> anyhow::Result<()> {
        let instruments = config::instrument_ids(&self.run.bases);
        let window = self
            .config
            .fast_days
            .max(self.config.medium_days)
            .max(self.config.slow_days)
            .max(self.config.regime_lookback_days) as usize
            + 1;
        self.start_universe(instruments, window)?;
        anyhow::Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        anyhow::Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        anyhow::Ok(())
    }
}
