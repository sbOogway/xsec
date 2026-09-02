use std::{collections::{HashMap, VecDeque}, fmt::Debug, str::FromStr, time::Duration};

use anyhow::anyhow;
use chrono::{DateTime, Datelike};
use nautilus_common::{actor::DataActor, enums::Environment, timer::TimeEvent};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarSpecification, BarType, Data},
    enums::{AccountType, AggregationSource, BarAggregation, BookType, OmsType, PriceType},
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
use rust_decimal::Decimal;

use crate::data::bar_type;

mod data;

const ENVIRONMENT: Environment = Environment::Backtest;
const BASES: [&str; 2] = ["BTC", "ETH"];
const TIMEFRAME: BarAggregation = BarAggregation::Month;
// fn monthly_bar_type(symbol: InstrumentId) -> BarType {
//     BarType::new(
//         symbol,
//         BarSpecification::new(1, BarAggregation::Month, PriceType::Last),
//         AggregationSource::External,
//     )
// }

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

    #[builder(default = 12)]
    warmup_bars: u32,

    #[builder(default = 0)]
    last_month: u32,

    #[builder(default)]
    returns: HashMap<InstrumentId, VecDeque<Decimal>>
    // #[builder(default)]
    // last_seen_ts: HashMap<InstrumentId, UnixNanos>,
}

// impl XSectionalMomentum {
//     fn remember_ts(&mut self, id: InstrumentId, ts: UnixNanos) {
//         let entry = self.last_seen_ts.entry(id).or_insert(UnixNanos::default());
//         if ts > *entry {
//             *entry = ts;
//         }
//     }

//     fn is_fresh(&self, id: &InstrumentId, ts: UnixNanos) -> bool {
//         match self.last_seen_ts.get(id) {
//             Some(last) => ts > *last,
//             None => true,
//         }
//     }
// }

nautilus_strategy!(XSectionalMomentum, {});

impl Debug for XSectionalMomentum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XSectionalMomentum")
            .field("core", &self.core)
            .field("symbols", &self.symbols)
            .field("warmup_bars", &self.warmup_bars)
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

        let warmup = std::num::NonZeroUsize::new(self.warmup_bars as usize)
            .ok_or_else(|| anyhow!("warmup_bars must be > 0"))?;

        let symbols = self.symbols.clone();
        for symbol in symbols {
            let bar_type = bar_type(symbol, TIMEFRAME);
            log::info!("[{}] requesting {warmup} warmup bars", symbol);

            // let start = Some(UnixNanos::from_str("2024-01-01").unwrap());
            self.request_bars(bar_type, None, None, Some(warmup), None, None)?;
            self.subscribe_bars(bar_type, None, None);

            self.returns.insert(symbol, VecDeque::with_capacity(self.months.into()));
        }

        anyhow::Ok(())
    }

    // fn on_historical_bars(&mut self, bars: &[Bar]) -> anyhow::Result<()> {
    //     log::info!("received {} historical bars (live warmup)", bars.len());
    //     // for bar in bars {
    //     //     self.remember_ts(bar.instrument_id(), bar.ts_event);
    //     // }
    //     anyhow::Ok(())
    // }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        let timestamp_integer = bar.ts_event.as_u64() as i64;
        let current_month = DateTime::from_timestamp_nanos(timestamp_integer).month();
        if self.last_month == current_month {
            return anyhow::Ok(());
        }
        let id = bar.instrument_id();
        // if !self.is_fresh(&id, bar.ts_event) {
        //     return anyhow::Ok(());
        // }
        // self.remember_ts(id, bar.ts_event);
        log::info!("bar {} @ {}", id, bar.ts_event);
        // self.returns.insert(bar.instrument_id(), );
        if let Some(symbol_returns) = self.returns.get_mut(&id) {
            symbol_returns.push_back(bar.close.as_decimal());
        }
        self.last_month = current_month;
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
            log::info!("{} -> {:?}", symbol, self.cache().bar(&bar_type));
            // let price = self.cache().price(symbol, PriceType::Mark).unwrap();
            // log::info!("{}", price);
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
                let (instruments, bars) = rt.block_on(data::fetch_linear_with_bars(*id, TIMEFRAME)).unwrap();
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
