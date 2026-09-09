//! The backtest bootstrap ([`xsec::engine`]) over the [`MarketData`] seam.
//!
//! `build_backtest_engine` is the whole bring-up — venue, starting balance,
//! instruments, bar history, strategy — up to but not including the sim run.
//! Driving it with an [`InMemoryMarketData`] fixture proves the wiring works
//! with no Bybit HTTP call, which `run_engine` in `src/main.rs` could never do.

use std::collections::HashMap;

use nautilus_core::UnixNanos;
use nautilus_model::{
    data::Bar,
    enums::BarAggregation,
    identifiers::{InstrumentId, Symbol, Venue},
    instruments::{CryptoPerpetual, Instrument, InstrumentAny},
    types::{Currency, Price, Quantity},
};

use xsec::{
    config::RunConfig,
    data::exchange::{CachedMarketData, InMemoryMarketData, MarketData, bybit::get_bar_type, cache},
    engine,
    strategy::momentum::{Momentum, config as momentum},
};

/// A minimal Bybit linear-perp instrument for `base` (`BTC` →
/// `BTCUSDT-LINEAR.BYBIT`). Only the fields the engine reads at bring-up are
/// populated; the rest are `None`.
fn perp(base: &str) -> InstrumentAny {
    let id = InstrumentId::from(format!("{base}USDT-LINEAR.BYBIT").as_str());
    InstrumentAny::CryptoPerpetual(CryptoPerpetual::new(
        id,
        Symbol::from(format!("{base}USDT").as_str()),
        Currency::from(base),
        Currency::from("USDT"),
        Currency::from("USDT"),
        false,
        2,
        3,
        Price::from("0.01"),
        Quantity::from("0.001"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        UnixNanos::default(),
        UnixNanos::default(),
    ))
}

/// `n` daily bars for `id`, a gentle uptrend so warm-up and ranking have
/// something to chew on.
fn daily_bars(id: InstrumentId, n: u64) -> Vec<Bar> {
    let bar_type = get_bar_type(id, BarAggregation::Day);
    (0..n)
        .map(|day| {
            let base = 100.0 + day as f64;
            let ts = UnixNanos::from(day * 86_400 * 1_000_000_000);
            Bar::new(
                bar_type,
                Price::new(base, 2),
                Price::new(base + 1.0, 2),
                Price::new(base - 1.0, 2),
                Price::new(base + 0.5, 2),
                Quantity::new(10.0, 1),
                ts,
                ts,
            )
        })
        .collect()
}

fn momentum_config(bases: &[String]) -> momentum::Config {
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: momentum::Args,
    }

    // `--top-n 1 --short-n 1` fits the 3-name fixture universe (the defaults,
    // 5 + 5, would overlap it).
    let args = Wrap::try_parse_from(["momentum", "--top-n", "1", "--short-n", "1"])
        .expect("args parse")
        .args;
    momentum::build(&args, bases).expect("config build")
}

#[test]
fn build_backtest_engine_wires_a_fixture_market_without_network() {
    let bases: Vec<String> = ["BTC", "ETH", "SOL"].iter().map(|s| s.to_string()).collect();
    let instrument_ids = momentum::instrument_ids(&bases);

    let instruments: Vec<InstrumentAny> = bases.iter().map(|b| perp(b)).collect();
    let bars: HashMap<InstrumentId, Vec<Bar>> = instrument_ids
        .iter()
        .map(|id| (*id, daily_bars(*id, 40)))
        .collect();
    let market = InMemoryMarketData::new(instruments, bars);

    let run = RunConfig {
        run_id: "test-engine-boot".to_string(),
        strategy: "momentum".to_string(),
        exchange: "bybit".to_string(),
        date_start: "2024-01-01".to_string(),
        date_end: "2024-03-01".to_string(),
        bases: bases.clone(),
        starting_balance: "1000 USDT".to_string(),
        universe_path: "universe.txt".to_string(),
        argv: "xsec momentum".to_string(),
    };

    let strategy = Momentum::builder()
        .run(run.clone())
        .config(momentum_config(&bases))
        .build();

    let engine = engine::build_backtest_engine(&run, &market, &instrument_ids, strategy)
        .expect("engine wires over in-memory market data, no network");

    assert_eq!(
        engine.list_venues(),
        vec![Venue::from("BYBIT")],
        "the strategy's own venue is the one added to the engine",
    );
}

/// `CachedMarketData` reads back exactly what `xsec fetch` writes: the
/// instruments snapshot (whose `Currency` fields decode through a strict
/// registry lookup) and the per-symbol bar cache.
#[test]
fn cached_market_data_round_trips_a_fetched_universe() {
    let dir = tempfile::tempdir().unwrap();
    let id = InstrumentId::from("BTCUSDT-LINEAR.BYBIT");

    cache::write_instruments(dir.path(), &[perp("BTC")]).unwrap();
    cache::write_bars(dir.path(), &id, &daily_bars(id, 30)).unwrap();

    let market = CachedMarketData::open(dir.path()).expect("cache dir opens");

    let instruments = market.instruments().expect("instruments snapshot decodes");
    assert_eq!(instruments.len(), 1);
    assert_eq!(instruments[0].id(), id);

    assert_eq!(market.bars(id, BarAggregation::Day).unwrap().len(), 30);
}

/// The full bring-up over the disk cache: `build_backtest_engine` wires a
/// strategy from a `data/` directory seeded the way `xsec fetch` would, no
/// network — the composition `cached_market_data_round_trips_a_fetched_universe`
/// and `build_backtest_engine_wires_a_fixture_market_without_network` each prove
/// half of.
#[test]
fn build_backtest_engine_wires_over_the_disk_cache() {
    let dir = tempfile::tempdir().unwrap();
    let bases: Vec<String> = ["BTC", "ETH", "SOL"].iter().map(|s| s.to_string()).collect();
    let instrument_ids = momentum::instrument_ids(&bases);

    cache::write_instruments(dir.path(), &bases.iter().map(|b| perp(b)).collect::<Vec<_>>()).unwrap();
    for id in &instrument_ids {
        cache::write_bars(dir.path(), id, &daily_bars(*id, 40)).unwrap();
    }

    let run = RunConfig {
        run_id: "test-engine-boot-cached".to_string(),
        strategy: "momentum".to_string(),
        exchange: "bybit".to_string(),
        date_start: "2024-01-01".to_string(),
        date_end: "2024-03-01".to_string(),
        bases: bases.clone(),
        starting_balance: "1000 USDT".to_string(),
        universe_path: "universe.txt".to_string(),
        argv: "xsec momentum".to_string(),
    };
    let strategy = Momentum::builder()
        .run(run.clone())
        .config(momentum_config(&bases))
        .build();

    let market = CachedMarketData::open(dir.path()).unwrap();
    let engine = engine::build_backtest_engine(&run, &market, &instrument_ids, strategy)
        .expect("engine wires from the disk cache, no network");

    assert_eq!(engine.list_venues(), vec![Venue::from("BYBIT")]);
}

/// A symbol the fetch never ran for is a hard error, not an empty series — an
/// empty series would let the backtest quietly run on a degenerate universe.
#[test]
fn cached_market_data_errors_on_an_uncached_symbol() {
    let dir = tempfile::tempdir().unwrap();
    cache::write_instruments(dir.path(), &[perp("BTC")]).unwrap();

    let market = CachedMarketData::open(dir.path()).unwrap();
    let err = market
        .bars(InstrumentId::from("ETHUSDT-LINEAR.BYBIT"), BarAggregation::Day)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ETHUSDT-LINEAR.BYBIT"), "{err}");
}

/// Opening a cache that was never populated points at `xsec fetch`.
#[test]
fn cached_market_data_without_a_cache_points_at_xsec_fetch() {
    let err = CachedMarketData::open("does/not/exist-xsec")
        .unwrap_err()
        .to_string();
    assert!(err.contains("xsec fetch"), "{err}");
}
