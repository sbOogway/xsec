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
    config::{self, RunConfig, SharedArgs},
    data::exchange::bybit as market_data,
    strategy::{
        StrategyKind,
        common::StrategyRuntime,
        momentum::{Momentum, config as momentum},
    },
};

/// Cross-sectional strategy backtests over Bybit USDT-margined linear
/// perpetuals. Pick a strategy with a subcommand; `--help` on the subcommand
/// lists its knobs. The coin universe is read from `--universe` (a plain-text
/// file, one base asset per line).
///
/// The run-level flags live in [`SharedArgs`]; each strategy owns its own flags
/// in `src/strategy/<name>/config.rs`. This struct just composes the two.
#[derive(Parser, Debug)]
#[command(name = env!("CARGO_PKG_NAME"), version, about, long_about = None)]
struct CliArgs {
    #[command(flatten)]
    shared: SharedArgs,

    /// The strategy to run.
    #[command(subcommand)]
    strategy: StrategyKind,
}

/// Which runtime to boot. The `Backtest` path is the one that is wired end to
/// end; `Live` is a thin sketch and `Sandbox` is unimplemented. Everything else
/// is configured per run through [`CliArgs`] and the chosen strategy's config.
const ENVIRONMENT: Environment = Environment::Backtest;

fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let cli = CliArgs::parse();
    let argv: Vec<String> = std::env::args().collect();

    match &cli.strategy {
        StrategyKind::Momentum(args) => {
            let run = config::build_config(&cli.shared, &argv, cli.strategy.name())?;
            let strategy_config = momentum::build(args, &run.bases)?;
            // Echoed on stdout so the caller can key `logs/<uuid>/logs.log` and
            // the `runs/<uuid>/` files to the same id.
            println!("run_id={}", run.run_id);

            let strategy = Momentum::builder()
                .run(run.clone())
                .config(strategy_config)
                .build();
            let instrument_ids = momentum::instrument_ids(&run.bases);
            run_engine(
                &run,
                momentum::VENUE,
                momentum::TIMEFRAME,
                &instrument_ids,
                strategy,
            )?;
        }
    }

    Ok(())
}

/// Boot the configured [`ENVIRONMENT`] for `strategy`, loading the venue,
/// instruments and bars it needs. `venue` / `timeframe` / `instrument_ids` are
/// the strategy's market surface, read from its `config.rs`.
fn run_engine<S: StrategyRuntime>(
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
            let instruments = rt.block_on(market_data::fetch_linear_instruments()).unwrap();
            market_data::seed_instruments(&instruments);
            for inst in &instruments {
                if instrument_ids.contains(&inst.id()) {
                    engine.add_instrument(inst).unwrap();
                }
            }
            for id in instrument_ids {
                let bars = rt
                    .block_on(market_data::fetch_bars_cached(*id, timeframe))
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
                    .with_name("Momentum-Live")
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
