use std::{collections::HashMap, fmt::Debug, str::FromStr, time::Duration};

use anyhow::anyhow;
use chrono::{DateTime, Datelike};
use clap::Parser;
use nautilus_common::{actor::DataActor, enums::Environment, timer::TimeEvent};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, Data},
    enums::{AccountType, BookType, OmsType, OrderSide},
    events::{OrderFilled, PositionOpened},
    identifiers::{InstrumentId, StrategyId, Venue},
    instruments::Instrument,
    types::{Money, Quantity},
};

use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_live::node::LiveNode;
use rust_decimal::{Decimal, prelude::ToPrimitive};

use xsectional_rs::{
    capture::{RunCapture, YearMonth}, config::{self, Cli, RunConfig}, data::{self, bar_type, structure::BoundedQueue}, sizing::{self, Conviction}, strategy::XSectionalMomentum,
};

/// Which runtime to boot. The `Backtest` path is the one that is wired end to
/// end; `Live` is a thin sketch and `Sandbox` is unimplemented. Everything
/// else is configured per run through [`Cli`] / [`RunConfig`].
const ENVIRONMENT: Environment = Environment::Backtest;
fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let cli = Cli::parse();
    let argv: Vec<String> = std::env::args().collect();
    let config = config::build_config(&cli, &argv)?;
    // Echoed on stdout so the caller can key `logs/<uuid>/logs.log` and the
    // `runs/<uuid>/` files to the same id.
    println!("run_id={}", config.run_id);

    match ENVIRONMENT {
        Environment::Backtest => {
            let instrument_ids = config::instrument_ids(&config.bases);
            let starting_balance = Money::from(config.starting_balance.as_str());
            let start = Some(UnixNanos::from_str(&config.date_start).unwrap());
            let end = Some(UnixNanos::from_str(&config.date_end).unwrap());

            let mut engine = BacktestEngine::new(BacktestEngineConfig::default()).unwrap();
            engine
                .add_venue(
                    SimulatedVenueConfig::builder()
                        .venue(Venue::from(config::VENUE))
                        .oms_type(OmsType::Hedging)
                        .account_type(AccountType::Margin)
                        .book_type(BookType::L1_MBP)
                        .starting_balances(vec![starting_balance])
                        .build()
                        .unwrap(),
                )
                .unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            let instruments = rt.block_on(data::fetch_linear_instruments()).unwrap();
            data::seed_instruments(&instruments);
            for inst in &instruments {
                if instrument_ids.contains(&inst.id()) {
                    engine.add_instrument(inst).unwrap();
                }
            }
            for id in &instrument_ids {
                let bars = rt
                    .block_on(data::fetch_bars_cached(*id, config::TIMEFRAME))
                    .unwrap();
                log::info!("loaded {} bars for {}", bars.len(), id);
                engine
                    .add_data(bars.into_iter().map(Data::Bar).collect(), None, false, true)
                    .unwrap();
            }

            let strategy = XSectionalMomentum::builder().config(config).build();
            engine.add_strategy(strategy).unwrap();
            engine.run(start, end, None, false).unwrap();
        }
        Environment::Sandbox => todo!(),
        Environment::Live => {
            use nautilus_bybit::{
                common::enums::BybitProductType, config::BybitDataClientConfig,
                factories::BybitDataClientFactory,
            };
            use nautilus_common::factories::ClientConfig;
            use nautilus_model::identifiers::TraderId;

            let strategy = XSectionalMomentum::builder().config(config).build();

            let data_config = BybitDataClientConfig {
                product_types: vec![BybitProductType::Linear],
                ..Default::default()
            };
            let factory = BybitDataClientFactory::new();
            let cfg: Box<dyn ClientConfig> = Box::new(data_config);

            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let mut node = LiveNode::builder(TraderId::from("TRADER-001"), Environment::Live)?
                    .with_name("XSectionalMomentum-Live")
                    .add_data_client(None, Box::new(factory), cfg)?
                    .build()?;

                node.add_strategy(strategy)?;
                node.run().await
            })
            .unwrap();
        }
    }

    Ok(())
}
