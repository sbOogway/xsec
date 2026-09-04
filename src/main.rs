use std::str::FromStr;

use clap::Parser;
use nautilus_common::enums::Environment;
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::Data,
    enums::{AccountType, BarAggregation, BookType, OmsType},
    identifiers::{InstrumentId, Venue},
    instruments::Instrument,
    types::Money,
};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_live::node::LiveNode;

use xsec::{
    config::{self, CliArgs, RunConfig},
    data,
    strategy::{
        StrategyKind,
        common::Harness,
        momentum::{XSectionalMomentum, config as momentum},
    },
};

/// Which runtime to boot. The `Backtest` path is the one that is wired end to
/// end; `Live` is a thin sketch and `Sandbox` is unimplemented. Everything else
/// is configured per run through [`Cli`] and the chosen strategy's config.
const ENVIRONMENT: Environment = Environment::Backtest;

fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let cli = CliArgs::parse();
    let argv: Vec<String> = std::env::args().collect();

    match &cli.strategy {
        StrategyKind::Momentum(args) => {
            let run = config::build_config(&cli, &argv, cli.strategy.name())?;
            let strategy_config = momentum::build(args, &run.bases)?;
            // Echoed on stdout so the caller can key `logs/<uuid>/logs.log` and
            // the `runs/<uuid>/` files to the same id.
            println!("run_id={}", run.run_id);

            let strategy = XSectionalMomentum::builder()
                .run(run.clone())
                .config(strategy_config)
                .build();
            let instrument_ids = momentum::instrument_ids(&run.bases);
            run_engine(&run, momentum::VENUE, momentum::TIMEFRAME, &instrument_ids, strategy)?;
        }
    }

    Ok(())
}

/// Boot the configured [`ENVIRONMENT`] for `strategy`, loading the venue,
/// instruments and bars it needs. `venue` / `timeframe` / `instrument_ids` are
/// the strategy's market surface, read from its `config.rs`.
fn run_engine<S: Harness>(
    run: &RunConfig,
    venue: &str,
    timeframe: BarAggregation,
    instrument_ids: &[InstrumentId],
    strategy: S,
) -> anyhow::Result<()> {
    match ENVIRONMENT {
        Environment::Backtest => {
            let starting_balance = Money::from(run.starting_balance.as_str());
            let start = Some(UnixNanos::from_str(&run.date_start).unwrap());
            let end = Some(UnixNanos::from_str(&run.date_end).unwrap());

            let mut engine = BacktestEngine::new(BacktestEngineConfig::default()).unwrap();
            engine
                .add_venue(
                    SimulatedVenueConfig::builder()
                        .venue(Venue::from(venue))
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
            for id in instrument_ids {
                let bars = rt
                    .block_on(data::fetch_bars_cached(*id, timeframe))
                    .unwrap();
                log::info!("loaded {} bars for {}", bars.len(), id);
                engine
                    .add_data(bars.into_iter().map(Data::Bar).collect(), None, false, true)
                    .unwrap();
            }

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
