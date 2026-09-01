use std::{fmt::Debug, str::FromStr, time::Duration};

use nautilus_common::{actor::DataActor, enums::Environment, timer::TimeEvent};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{BarSpecification, BarType, Data},
    enums::{AccountType, AggregationSource, BookType, OmsType, PriceType},
    identifiers::{InstrumentId, StrategyId, Venue},
    instruments::Instrument,
    types::Money,
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_live::node::LiveNode;

mod data;

const ENVIRONMENT: Environment = Environment::Backtest;
const BASES: [&str; 2] = ["BTC", "ETH"];

#[derive(bon::Builder)]
pub struct XSectionalMomentum {
    #[builder(default = StrategyCore::new(StrategyConfig {
         strategy_id: Some(StrategyId::from("X-SEC-MOM")),
         order_id_tag:Some("001".to_string()),
         ..Default::default()
    }))]
    core: StrategyCore,

    #[builder(default = BASES.into_iter().map(|base|{format!("{}USDT-LINEAR.BYBIT", base)}).map(InstrumentId::from).collect())]
    symbols: Vec<InstrumentId>,

    #[builder(default = 12)]
    months: u16,
}

impl XSectionalMomentum {}

nautilus_strategy!(XSectionalMomentum, {});

impl Debug for XSectionalMomentum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XSectionalMomentum")
            .field("core", &self.core)
            .finish()
    }
}

impl DataActor for XSectionalMomentum {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.clock().set_timer(
            "DAILY",
            Duration::from_hours(24),
            None,
            None,
            None,
            None,
            None,
        )?;
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        log::info!("hello from on_time_event {}", event);
        for symbol in &self.symbols {
            let spec = BarSpecification::new(
                1,
                nautilus_model::enums::BarAggregation::Day,
                PriceType::Last,
            );
            let bar_type = BarType::new(*symbol, spec, AggregationSource::External);
            let bar = self.cache().bar(&bar_type);
            log::info!("{} -> {:?}", symbol, bar);
        }
        anyhow::Ok(())
    }
}

fn main() {
    nautilus_common::logging::ensure_logging_initialized();

    match ENVIRONMENT {
        Environment::Backtest => {
            let mut engine = BacktestEngine::new(BacktestEngineConfig::default()).unwrap();
            let strategy = XSectionalMomentum::builder().build();

            engine
                .add_venue(
                    SimulatedVenueConfig::builder()
                        .venue(Venue::from("BYBIT"))
                        .oms_type(OmsType::Hedging)
                        .account_type(AccountType::Margin)
                        .book_type(BookType::L1_MBP)
                        .starting_balances(vec![Money::from("1_000_000 USD")])
                        .build()
                        .unwrap(),
                )
                .unwrap();

            let symbols: Vec<InstrumentId> = BASES
                .iter()
                .map(|b| InstrumentId::from(format!("{b}USDT-LINEAR.BYBIT").as_str()))
                .collect();

            let rt = tokio::runtime::Runtime::new().unwrap();
            for id in &symbols {
                let (instruments, bars) = rt.block_on(data::fetch_linear_with_bars(*id)).unwrap();
                for inst in instruments {
                    if symbols.contains(&inst.id()) {
                        engine.add_instrument(&inst).unwrap();
                    }
                }
                log::info!("loaded {} bars for {}", bars.len(), id);
                engine
                    .add_data(bars.into_iter().map(Data::Bar).collect(), None, false, true)
                    .unwrap();
            }
            engine.add_strategy(strategy).unwrap();

            let start = Some(UnixNanos::from_str("2024-01-01").unwrap());
            let end = Some(UnixNanos::from_str("2026-01-01").unwrap());
            engine.run(start, end, None, false).unwrap();
        }
        Environment::Sandbox => todo!(),
        Environment::Live => todo!(),
    }
}
