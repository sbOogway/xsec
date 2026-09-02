use std::{
    collections::{HashMap, VecDeque},
    fmt::Debug,
    str::FromStr,
    time::Duration,
};
use nautilus_model::types::Currency;

use anyhow::anyhow;
use chrono::{DateTime, Datelike};
use nautilus_common::{actor::DataActor, enums::Environment, timer::TimeEvent};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarSpecification, BarType, Data},
    enums::{
        AccountType, AggregationSource, BarAggregation, BookType, OmsType, OrderSide, PriceType,
    },
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

use crate::data::{bar_type, structure::BoundedQueue};

mod data;

const ENVIRONMENT: Environment = Environment::Backtest;
const BASES: [&str; 45] = [
    "BTC", "ETH", "BNB", "XRP", "SOL", "TRX", "HYPE", "ZEC", "DOGE", "XMR", "LINK", "ADA", "XLM",
    "BCH", "LTC", "UNI", "HBAR", "AVAX", "SUI", "CRO", "TAO", "NEAR", "OKB", "AAVE", "ASTER",
    "MNT", "ONDO", "ENA", "DOT", "ICP", "MORPHO", "WLD", "ETC", "OP", "POL", "ALGO", "QNT", "ATOM",
    "KAS", "ARB", "RENDER", "FIL", "TRUMP", "CAKE", "CRV",
];
const TIMEFRAME: BarAggregation = BarAggregation::Month;

const HOLDING_MONTHS: u16 = 3;
const LOOKBACK_MONTHS: u16 = 12;

const PERCENTILE: &str = "0.1";

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
}

nautilus_strategy!(XSectionalMomentum, {});

impl Debug for XSectionalMomentum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XSectionalMomentum")
            .field("core", &self.core)
            .field("symbols", &self.symbols)
            .field("warmup_bars", &self.lookback_months)
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

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        let timestamp_integer = event.ts_event.as_u64() as i64;
        let current_month = DateTime::from_timestamp_nanos(timestamp_integer).month();
        if self.last_month == current_month {
            return anyhow::Ok(());
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
            let order = self.order().market(
                **instrument,
                OrderSide::Sell,
                Quantity::from_str("5.00").unwrap(),
                None,
                None,
                Some(true),
                None,
                None,
                None,
                None,
            );
            let _ = self.submit_order(order, None, None, None);
        }

        self.last_month = current_month;
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
                        .starting_balances(vec![Money::from("1_000 USDT")])
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
                let (instruments, bars) = rt
                    .block_on(data::fetch_linear_with_bars(*id, TIMEFRAME))
                    .unwrap();
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

            let start = Some(UnixNanos::from_str("2020-01-01").unwrap());
            let end = Some(UnixNanos::from_str("2026-01-01").unwrap());
            engine.run(start, end, None, false).unwrap();
        }
        Environment::Sandbox => todo!(),
        Environment::Live => {
            use nautilus_bybit::{
                common::enums::BybitProductType, config::BybitDataClientConfig,
                factories::BybitDataClientFactory,
            };
            use nautilus_common::factories::{ClientConfig, DataClientFactory};
            use nautilus_model::identifiers::TraderId;

            let strategy = XSectionalMomentum::builder().build();

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
