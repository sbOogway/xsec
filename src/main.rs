use clap::Parser;
use nautilus_common::enums::Environment;
use nautilus_live::node::LiveNode;
use nautilus_model::identifiers::InstrumentId;

use xsec::{
    config::{self, RunConfig, SharedArgs},
    data::exchange::bybit::BybitMarketData,
    engine,
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
            run_engine(&run, &instrument_ids, strategy)?;
        }
    }

    Ok(())
}

/// Boot the configured [`ENVIRONMENT`] for `strategy`. The backtest bring-up
/// lives in [`xsec::engine`]; `Live` / `Sandbox` are bootstrapped inline here
/// because they need an entirely different setup (a `LiveNode`, real venue
/// clients).
fn run_engine<S: StrategyRuntime>(
    run: &RunConfig,
    instrument_ids: &[InstrumentId],
    strategy: S,
) -> anyhow::Result<()> {
    match ENVIRONMENT {
        Environment::Backtest => {
            engine::run_backtest(run, &BybitMarketData::new()?, instrument_ids, strategy)?;
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
