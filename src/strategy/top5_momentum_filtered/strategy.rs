//! The top-5 momentum strategy: rank the universe by a composite fast/medium/
//! slow momentum score, go long the top names, hold a week, repeat —
//! flattening to cash whenever BTC's trend regime filter is negative.
//! Everything that is not the signal — the rebalance clock, the price
//! buffers, artifact capture, notional-sized orders — comes from
//! [`crate::strategy::runtime::StrategyRuntime`].

use std::fmt::Debug;

use nautilus_common::{actor::DataActor, timer::TimeEvent};
use nautilus_model::{
    data::Bar,
    enums::OrderSide,
    events::OrderFilled,
    identifiers::{InstrumentId, StrategyId},
};
use nautilus_trading::{StrategyConfig, StrategyCore, nautilus_strategy};
use rust_decimal::{Decimal, prelude::ToPrimitive};

use crate::{
    config::RunConfig,
    data::structure::BoundedQueue,
    period::IsoWeek,
    sizing::{self, Conviction},
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

nautilus_strategy!(Top5MomentumFiltered, {
    fn on_order_filled(&mut self, event: &OrderFilled) {
        self.record_fill(event);
    }
});

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
        let run = self.run.clone();
        let rows = config::config_rows(&self.config);
        self.open_capture(&run, &rows)?;

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
        log::debug!("bar {} @ {}", bar.instrument_id(), bar.ts_event);
        self.runtime_mut().record_close(bar);
        anyhow::Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.finish_capture();
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        let Some(period) = self.period_rolled(event) else {
            return anyhow::Ok(());
        };

        let equity = self.usdt_equity();

        let btc = btc_instrument_id(&self.run.bases);
        let btc_return = self
            .runtime()
            .prices
            .get(&btc)
            .and_then(|queue| n_day_return(queue, self.config.regime_lookback_days as usize));
        let regime_is_positive = btc_return.is_some_and(|r| r >= Decimal::ZERO);

        if !regime_is_positive {
            self.close_all();
            let latest_close = self.runtime().latest_closes();
            if let Some(capture) = self.runtime_mut().capture.as_mut() {
                capture.record_rebalance(period, equity, Vec::new());
                capture.finalise_completed(period, &latest_close, equity);
            }
            self.mark_rebalanced(period);
            return anyhow::Ok(());
        }

        let holding_days = 7_u64 * self.config.holding_weeks as u64;
        self.close_expired(event, holding_days);

        // --- signal: composite fast/medium/slow momentum score, per name ---
        let instruments = self.runtime().instruments.clone();
        let mut scores: Vec<(InstrumentId, f64)> = Vec::with_capacity(instruments.len());
        for instrument in &instruments {
            let Some(queue) = self.runtime().prices.get(instrument) else {
                continue;
            };
            let fast = n_day_return(queue, self.config.fast_days as usize);
            let medium = n_day_return(queue, self.config.medium_days as usize);
            let slow = n_day_return(queue, self.config.slow_days as usize);
            if let (Some(fast), Some(medium), Some(slow)) = (fast, medium, slow) {
                let score = self.config.fast_weight * fast.to_f64().unwrap_or(0.0)
                    + self.config.medium_weight * medium.to_f64().unwrap_or(0.0)
                    + self.config.slow_weight * slow.to_f64().unwrap_or(0.0);
                scores.push((*instrument, score));
            }
        }

        // --- rank and take the top `top_n`, unconditionally ---
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(self.config.top_n);

        log::info!("scores this week {scores:#?}");

        let budget = self.config.risk_fraction * equity.to_f64().unwrap_or(0.0);
        let allocation = sizing::allocate(
            budget,
            &scores,
            self.config.allocation_tilt,
            Conviction::High,
        );

        let mut legs: Vec<(InstrumentId, OrderSide, Decimal, f64)> = Vec::new();
        for (instrument, notional) in allocation {
            if !self.submit_notional_market(instrument, OrderSide::Buy, notional) {
                continue;
            }
            // Entry mark: the close of the last daily bar before this
            // rebalance. Paired with the same instrument's close one
            // rebalance later, this is a clean close-to-close holding-period
            // return.
            if let Some(entry_price) = self
                .runtime()
                .prices
                .get(&instrument)
                .and_then(|q| q.inner.back().copied())
            {
                legs.push((instrument, OrderSide::Buy, entry_price, notional));
            }
        }

        let latest_close = self.runtime().latest_closes();
        if let Some(capture) = self.runtime_mut().capture.as_mut() {
            capture.record_rebalance(period, equity, legs);
            capture.finalise_completed(period, &latest_close, equity);
        }

        self.mark_rebalanced(period);
        anyhow::Ok(())
    }
}

/// The `N`-day return `(P_now - P_{now-N}) / P_{now-N}`, needing `N + 1`
/// price points in `queue`. `None` if the buffer isn't deep enough yet
/// (still warming up) or the reference price is zero.
fn n_day_return(queue: &BoundedQueue<Decimal>, days: usize) -> Option<Decimal> {
    let len = queue.inner.len();
    if len <= days {
        return None;
    }
    let now = *queue.inner.back()?;
    let past = *queue.inner.get(len - 1 - days)?;
    (!past.is_zero()).then(|| (now - past) / past)
}

/// The instrument id for `BTC` within `bases`, matched case-insensitively.
/// `config::build` guarantees `bases` contains it.
fn btc_instrument_id(bases: &[String]) -> InstrumentId {
    let base = bases
        .iter()
        .find(|base| base.eq_ignore_ascii_case("BTC"))
        .expect("config::build requires BTC in the universe");
    InstrumentId::from(format!("{base}USDT-LINEAR.{}", config::VENUE).as_str())
}
