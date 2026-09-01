use std::{fmt::Debug, time::Duration};

use nautilus_common::{actor::DataActor, enums::Environment, timer::TimeEvent};
use nautilus_model::{
    enums::{AccountType, BookType, OmsType},
    identifiers::{StrategyId, Venue},
    types::Money,
};
use nautilus_trading::{Strategy, StrategyConfig, StrategyCore, nautilus_strategy};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_live::node::LiveNode;

const ENVIRONMENT: Environment = Environment::Backtest;
const BASES: [&str; 2] = ["BTC", "ETH"];

#[derive(bon::Builder)]
pub struct XSectionalMomentum {
    #[builder(default = StrategyCore::new(StrategyConfig {
         strategy_id: Some(StrategyId::from("XSECMOM")),
         order_id_tag:Some("001".to_string()),
         ..Default::default()
    }))]
    core: StrategyCore,

    #[builder(default = BASES.into_iter().map(String::from).collect())]
    symbols: Vec<String>,

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
}

fn main() {
    match ENVIRONMENT {
        Environment::Backtest => {
            let mut engine = BacktestEngine::new(BacktestEngineConfig::default()).unwrap();
            let strategy = XSectionalMomentum::builder().build();

            engine
                .add_venue(
                    SimulatedVenueConfig::builder()
                        .venue(Venue::from("BACKTEST"))
                        .oms_type(OmsType::Hedging)
                        .account_type(AccountType::Margin)
                        .book_type(BookType::L1_MBP)
                        .starting_balances(vec![Money::from("1_000_000 USD")])
                        .build()
                        .unwrap(),
                )
                .unwrap();

            engine.add_data(data, None, true, true);
            engine.add_strategy(strategy).unwrap();
            engine.run(None, None, None, false).unwrap();
        }
        Environment::Sandbox => todo!(),
        Environment::Live => todo!(),
    }
}
