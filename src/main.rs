use std::path::Path;

use clap::{Parser, Subcommand};
use nautilus_common::enums::Environment;
use nautilus_live::node::LiveNode;
use nautilus_model::identifiers::InstrumentId;

use xsec::{
    config::{self, RunConfig, SharedArgs},
    data::{
        exchange::{
            CachedMarketData, Exchange, ExchangeAdapter,
            bybit::BybitAdapter,
            cache::{self, DATA_DIR},
            fetch::{self, FetchArgs},
        },
        snapshot::cmc::{CmcSnapshotData, Resolution, derive_universe},
        universe::read_universe,
    },
    engine,
    strategy::{
        StrategyKind,
        common::StrategyRuntime,
        momentum::{CmcGate, Momentum, Source, config as momentum},
    },
};

/// The committed CMC→Bybit resolution map (`make cmc_resolution`).
const CMC_RESOLUTION_PATH: &str = "coins/cmc_resolution.csv";

/// Cross-sectional strategy backtests over USDT-margined linear perpetuals.
///
/// `xsec fetch` downloads market data into `data/<exchange>/`; `xsec <strategy>`
/// runs a backtest against that cache (never the network). `--help` on a
/// subcommand lists its knobs. The coin universe is read from `--universe` (a
/// plain-text file, one base asset per line).
///
/// The run-level flags live in [`SharedArgs`]; each strategy owns its own flags
/// in `src/strategy/<name>/config.rs`. This struct just composes the two.
#[derive(Parser, Debug)]
#[command(name = env!("CARGO_PKG_NAME"), version, about, long_about = None)]
struct CliArgs {
    #[command(flatten)]
    shared: SharedArgs,

    /// What to do this run: fetch data, or run a strategy backtest.
    #[command(subcommand)]
    command: Command,
}

/// The binary's top-level subcommands: `fetch`, plus one per strategy (folded in
/// from [`StrategyKind`] so `xsec momentum …` sits directly under `xsec`).
#[derive(Subcommand, Debug)]
enum Command {
    /// Download `--exchange` instruments + bar history for `--universe` into
    /// `data/<exchange>/`. The only subcommand that reaches the network; run it
    /// once before a backtest, and again to refresh.
    Fetch(FetchArgs),

    /// Run a strategy backtest against the `data/<exchange>/` cache (run
    /// `xsec fetch` first). One subcommand per strategy.
    #[command(flatten)]
    Strategy(StrategyKind),
}

/// The [`ExchangeAdapter`] for `--exchange`.
fn adapter_for(exchange: Exchange) -> anyhow::Result<Box<dyn ExchangeAdapter>> {
    match exchange {
        Exchange::Bybit => Ok(Box::new(BybitAdapter::new()?)),
    }
}

/// The backtest window as `NaiveDate`s, for the `--source coinmarketcap`
/// universe derivation ([`SharedArgs`] keeps the raw `YYYY-MM-DD` strings).
fn window_dates(shared: &SharedArgs) -> anyhow::Result<(chrono::NaiveDate, chrono::NaiveDate)> {
    let parse = |s: &str| {
        chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .map_err(|e| anyhow::anyhow!("date {s:?}: {e}"))
    };
    Ok((parse(&shared.date_start)?, parse(&shared.date_end)?))
}

/// Which runtime to boot. The `Backtest` path is the one that is wired end to
/// end; `Live` is a thin sketch and `Sandbox` is unimplemented. Everything else
/// is configured per run through [`CliArgs`] and the chosen strategy's config.
const ENVIRONMENT: Environment = Environment::Backtest;

fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let cli = CliArgs::parse();
    let argv: Vec<String> = std::env::args().collect();
    let data_dir = Path::new(DATA_DIR).join(cli.shared.exchange.as_str());

    match &cli.command {
        Command::Fetch(args) => {
            let adapter = adapter_for(cli.shared.exchange)?;
            let report = fetch::run(adapter.as_ref(), &cli.shared.universe, &data_dir, args)?;
            report.log_summary();
        }
        Command::Strategy(strategy) => match strategy {
            StrategyKind::Momentum(args) => {
                // `--source bybit` reads `--universe`; `--source coinmarketcap`
                // derives its traded set from the snapshots and carries a gate
                // that narrows the scored set each rebalance.
                let (bases, cmc) = match args.source {
                    Source::Bybit => (read_universe(&cli.shared.universe)?, None),
                    Source::Coinmarketcap => {
                        let snapshots = CmcSnapshotData::open(&args.cmc_snapshots)?;
                        let resolution = Resolution::open(Path::new(CMC_RESOLUTION_PATH))?;
                        let listed = cache::read_manifest_bases(&data_dir)?;
                        let (start, end) = window_dates(&cli.shared)?;
                        let bases = derive_universe(
                            &snapshots,
                            &resolution,
                            &listed,
                            start,
                            end,
                            args.cmc_top_n,
                        );
                        anyhow::ensure!(
                            !bases.is_empty(),
                            "no CoinMarketCap top-{} coin in [{}, {}] resolved to a Bybit perp \
                             with fetched bars — run \
                             `xsec fetch --universe <coins/cmc_union_*.txt>` first",
                            args.cmc_top_n,
                            cli.shared.date_start,
                            cli.shared.date_end,
                        );
                        (bases, Some(CmcGate::new(Box::new(snapshots), resolution)))
                    }
                };

                let run =
                    config::build_config_with_bases(&cli.shared, &argv, strategy.name(), bases)?;
                let strategy_config = momentum::build(args, &run.bases)?;
                // Echoed on stdout so the caller can key `logs/<uuid>/logs.log`
                // and the `runs/<uuid>/` files to the same id.
                println!("run_id={}", run.run_id);

                let strategy = Momentum::builder()
                    .run(run.clone())
                    .config(strategy_config)
                    .maybe_cmc(cmc)
                    .build();
                let instrument_ids = momentum::instrument_ids(&run.bases);
                run_engine(&run, &data_dir, &instrument_ids, strategy)?;
            }
        },
    }

    Ok(())
}

/// Boot the configured [`ENVIRONMENT`] for `strategy`. The backtest bring-up
/// lives in [`xsec::engine`] and reads market data from the `data_dir` cache
/// ([`CachedMarketData`]); `Live` / `Sandbox` are bootstrapped inline here
/// because they need an entirely different setup (a `LiveNode`, real venue
/// clients).
fn run_engine<S: StrategyRuntime>(
    run: &RunConfig,
    data_dir: &Path,
    instrument_ids: &[InstrumentId],
    strategy: S,
) -> anyhow::Result<()> {
    match ENVIRONMENT {
        Environment::Backtest => {
            let market = CachedMarketData::open(data_dir)?;
            engine::run_backtest(run, &market, instrument_ids, strategy)?;
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
