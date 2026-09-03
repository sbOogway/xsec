use std::{collections::HashMap, fmt::Debug, str::FromStr, time::Duration};

use anyhow::anyhow;
use chrono::{DateTime, Datelike};
use nautilus_common::{actor::DataActor, enums::Environment, timer::TimeEvent};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, Data},
    enums::{AccountType, BarAggregation, BookType, OmsType, OrderSide},
    events::PositionOpened,
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
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::data::{bar_type, structure::BoundedQueue};

mod data;

const ENVIRONMENT: Environment = Environment::Backtest;
const BASES: &[&str] = &[
    "BTC", "ETH", "BNB", "XRP", "SOL", "TRX", "DOGE", "XMR", "LINK", "ADA", "XLM",
    "BCH", "LTC", "UNI", "HBAR", "AVAX", "SUI", "CRO", "TAO", "NEAR", "OKB", "AAVE",
    "MNT", "ONDO", "ENA", "DOT", "ICP", "MORPHO", "WLD", "ETC", "OP", "POL", "ALGO", "QNT", "ATOM",
    "KAS", "ARB", "RENDER", "FIL", "CAKE", "CRV", "INJ", "STX", "TIA", "VET", "JUP", 
    "HYPE", "ASTER", "ZEC","CC", "GRAM" ,
    "PYTH", "PUMPFUN", "1000PEPE", "EIGEN", "FLR", "IMX", "JST", "SEI", "XDC", "JST"
    // "JST", "KITE", "FF", "GRAM", "LIT", "PENDLE", "LDO", "CFX", "XTZ", "JASMY", "TWT", "CVX",
    // "ENS", "JTO", "WIF", "COMP", "STRK", "2Z", "KAIA", "IOTA", "ZBCN", "THETA", "AXS", "NEO",
    // "CHZ", "EGLD", "APE", "AR", "MANA", "SAND", "BAT", "KSM", "DYDX", "GLM", "QTUM", "ZRX", "GMX",
    // "ORCA", "KAITO", "COW", "SNX", "LPT", "NMR", "BR", "SUPER", "AIOZ", "ARC", "WAL", "ETHFI",
    // "SSV",
];
const TIMEFRAME: BarAggregation = BarAggregation::Month;

const HOLDING_MONTHS: u16 = 1;
const LOOKBACK_MONTHS: u16 = 3;

const PERCENTILE: &str = "0.1";

const DOLLAR_POSITION_SIZE: f64 = 50.0;
const STARTING_BALANCE: &str = "1_000 USDT";

const DATE_START: &str = "2025-01-01";
const DATE_END: &str = "2026-08-01";

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

    #[builder(default = HOLDING_MONTHS)]
    holding_months: u16,

    #[builder(default = LOOKBACK_MONTHS)]
    lookback_months: u16,

    #[builder(default = 0)]
    last_month: u32,

    #[builder(default)]
    prices: HashMap<InstrumentId, BoundedQueue<Decimal>>,

    #[builder(default)]
    returns: HashMap<InstrumentId, Decimal>,

    /// The run UUID: keys `runs/<uuid>.*` and matches `logs/<uuid>.log`.
    #[builder(default = Uuid::now_v7().to_string())]
    run_id: String,
}

nautilus_strategy!(XSectionalMomentum, {
    fn on_position_opened(&mut self, event: PositionOpened) {
        log::info!("new position debug {:#?}", event);
    }
});

impl Debug for XSectionalMomentum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XSectionalMomentum")
            .field("run_id", &self.run_id)
            .field("core", &self.core)
            .field("symbols", &self.symbols)
            .field("warmup_bars", &self.lookback_months)
            .finish()
    }
}

impl DataActor for XSectionalMomentum {
    fn on_start(&mut self) -> anyhow::Result<()> {
        log::info!("run_id={}", self.run_id);
        log::info!("{:#?}", self);

        self.clock().set_timer(
            "DAILY",
            Duration::from_hours(24),
            None,
            None,
            None,
            None,
            None,
        )?;

        let warmup = std::num::NonZeroUsize::new(self.lookback_months as usize)
            .ok_or_else(|| anyhow!("warmup_bars must be > 0"))?;

        let symbols = self.symbols.clone();
        for symbol in symbols {
            let bar_type = bar_type(symbol, TIMEFRAME);
            log::info!("[{}] requesting {warmup} warmup bars", symbol);

            // let start = Some(UnixNanos::from_str("2024-01-01").unwrap());
            self.request_bars(bar_type, None, None, Some(warmup), None, None)?;
            self.subscribe_bars(bar_type, None, None);

            self.prices
                .insert(symbol, BoundedQueue::new(self.lookback_months.into()));
        }

        anyhow::Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        let id = bar.instrument_id();

        log::debug!("bar {} @ {}", id, bar.ts_event);
        if let Some(symbol_returns) = self.prices.get_mut(&id) {
            symbol_returns.push_back_overwrite(bar.close.as_decimal());
        }
        anyhow::Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        log::info!("XSectionalMomentum stopped");
        anyhow::Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        let timestamp_integer = event.ts_event.as_u64() as i64;
        let current_month = DateTime::from_timestamp_nanos(timestamp_integer).month();
        if self.last_month == current_month {
            return anyhow::Ok(());
        }

        let open_positions = self.cache().positions_open(None, None, None, None, None);

        for position in open_positions {
            let holding_ns = 27_u64 * self.holding_months as u64 * 86_400 * 1_000_000_000;
            let age_ns = event
                .ts_event
                .as_u64()
                .saturating_sub(position.ts_opened.as_u64());
            if age_ns > holding_ns {
                let _ = self.close_position(&position, None, None, None, None, None, None);
            }
        }

        log::info!("hello from on_time_event {}", event);
        for symbol in &self.symbols {
            let bar_type = bar_type(*symbol, TIMEFRAME);
            log::debug!("{} -> {:?}", symbol, self.cache().bar(&bar_type));
            log::debug!(
                "{} -> {:#?}",
                symbol,
                self.prices.get(symbol).unwrap().inner
            );

            let price_lookback_months = self.prices.get(symbol).unwrap().inner.get(0);
            let price_current = self
                .prices
                .get(symbol)
                .unwrap()
                .inner
                .get((self.lookback_months - 1).into());
            if price_current == None {
                self.returns.remove(symbol);
                continue;
            }

            let returns_lookback = (price_current.unwrap() - price_lookback_months.unwrap())
                / price_lookback_months.unwrap();

            log::debug!("{} return lookback -> {}", symbol, returns_lookback);
            self.returns.insert(*symbol, returns_lookback);
        }

        let binding = self.returns.clone();
        let mut sorted_returns: Vec<(&InstrumentId, &Decimal)> = binding.iter().collect();
        sorted_returns.sort_by_key(|&(_, v)| v);

        let percentile_size = (Decimal::from_str(PERCENTILE).unwrap()
            * Decimal::from(sorted_returns.len()))
        .as_i128() as usize;

        let percentile_bottom = &sorted_returns[..percentile_size];
        let percentile_top = &sorted_returns[sorted_returns.len() - percentile_size..];

        log::info!("returns sorted {:#?}", sorted_returns);
        log::info!("returns bottom {:#?}", percentile_bottom);
        log::info!("returns top {:#?}", percentile_top);

        let account = self.cache().account_for_venue(&Venue::new("BYBIT"));
        let binding = account.unwrap();
        let balances = binding.balances();

        log::info!("balance {:#?}", balances);

        for (instrument, _) in percentile_bottom {
            self.submit_notional_market(**instrument, OrderSide::Sell, DOLLAR_POSITION_SIZE);
        }

        for (instrument, _) in percentile_top {
            self.submit_notional_market(**instrument, OrderSide::Buy, DOLLAR_POSITION_SIZE);
        }

        self.last_month = current_month;
        anyhow::Ok(())
    }
}

impl XSectionalMomentum {
    fn submit_notional_market(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        notional_usdt: f64,
    ) {
        let Some(cached) = self.cache().instrument(&instrument_id) else {
            log::warn!("no instrument cached for {instrument_id}, skipping");
            return;
        };
        let bt = bar_type(instrument_id, TIMEFRAME);
        let Some(bar) = self
            .cache()
            .bar_at_index(&bt, 1)
            .or_else(|| self.cache().bar(&bt))
        else {
            log::warn!("no bar cached for {instrument_id}, skipping");
            return;
        };
        let close = bar.close.as_f64();
        if !close.is_finite() || close <= 0.0 {
            log::warn!("invalid close {close} for {instrument_id}, skipping");
            return;
        }
        let precision = cached.size_precision();
        let units = notional_usdt / close;
        if !units.is_finite() || units <= 0.0 {
            log::warn!("computed quantity {units} for {instrument_id}, skipping");
            return;
        }
        let min_lot = 10f64.powi(-(precision as i32));
        if units + f64::EPSILON < min_lot {
            log::warn!(
                "notional {notional_usdt} USDT rounds to 0 for {instrument_id} at precision {precision} (min lot ~{min_lot}); skipping"
            );
            return;
        }
        let order = self.order().market(
            instrument_id,
            side,
            Quantity::new(units, precision),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        // log::info!("{}", order);
        // self.orders.push(order.clone());
        let _ = self.submit_order(order.clone(), None, None, None);

        // Some(order)
    }
}

/// Resolve the run UUID from `--uuid <X>` (or `--uuid=<X>`), falling back to a
/// fresh UUID-7. Echoed on stdout so the caller can key `logs/<UUID>.log` and
/// the `runs/<UUID>.*` files to the same id.
fn resolve_run_id() -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--uuid=") {
            return value.to_string();
        }
        if arg == "--uuid"
            && let Some(value) = args.next()
        {
            return value;
        }
    }
    Uuid::now_v7().to_string()
}

fn main() {
    nautilus_common::logging::ensure_logging_initialized();

    let run_id = resolve_run_id();
    println!("run_id={run_id}");

    match ENVIRONMENT {
        Environment::Backtest => {
            let mut engine = BacktestEngine::new(BacktestEngineConfig::default()).unwrap();
            let strategy = XSectionalMomentum::builder().run_id(run_id).build();

            engine
                .add_venue(
                    SimulatedVenueConfig::builder()
                        .venue(Venue::from("BYBIT"))
                        .oms_type(OmsType::Hedging)
                        .account_type(AccountType::Margin)
                        .book_type(BookType::L1_MBP)
                        .starting_balances(vec![Money::from(STARTING_BALANCE)])
                        .build()
                        .unwrap(),
                )
                .unwrap();

            let symbols: Vec<InstrumentId> = BASES
                .iter()
                .map(|b| InstrumentId::from(format!("{b}USDT-LINEAR.BYBIT").as_str()))
                .collect();

            let rt = tokio::runtime::Runtime::new().unwrap();
            let instruments = rt
                .block_on(data::fetch_linear_instruments())
                .unwrap();
            data::seed_instruments(&instruments);
            for inst in &instruments {
                if symbols.contains(&inst.id()) {
                    engine.add_instrument(inst).unwrap();
                }
            }
            for id in &symbols {
                let bars = rt
                    .block_on(data::fetch_bars_cached(*id, TIMEFRAME))
                    .unwrap();
                log::info!("loaded {} bars for {}", bars.len(), id);
                engine
                    .add_data(bars.into_iter().map(Data::Bar).collect(), None, false, true)
                    .unwrap();
            }
            engine.add_strategy(strategy).unwrap();

            let start = Some(UnixNanos::from_str(DATE_START).unwrap());
            let end = Some(UnixNanos::from_str(DATE_END).unwrap());
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

            let strategy = XSectionalMomentum::builder().run_id(run_id).build();

            let config = BybitDataClientConfig {
                product_types: vec![BybitProductType::Linear],
                ..Default::default()
            };
            let factory = BybitDataClientFactory::new();
            let cfg: Box<dyn ClientConfig> = Box::new(config);

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
}
