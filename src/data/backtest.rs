//! Per-run artifact capture for a backtest.
//!
//! A single backtest run writes four files under `runs/<UUID>/`, all keyed by
//! the run UUID the user already uses for `logs/<UUID>/logs.log`:
//!
//! * `runs/<UUID>/config.csv`    — the strategy configuration (key,value).
//! * `runs/<UUID>/legs.csv`      — one row per (entry period, instrument) leg.
//! * `runs/<UUID>/portfolio.csv` — one row per rebalance period, the aggregate.
//! * `runs/<UUID>/fills.csv`     — one row per `OrderFilled` event.
//!
//! The portfolio file is the source of truth for the tearsheet's headline
//! return series; the legs and fills files are substrate for future
//! per-leg / per-trade diagnostics.
//!
//! `portfolio.gross_return` / `net_return` are **account-level** per-period
//! returns: the period's summed leg PnL (in USDT) divided by the period's
//! opening equity, so compounding the series tracks
//! `equity_end_of_period_usdt`. They are *not* the mean per-leg return — that
//! would ignore how much of the account is actually deployed.
//!
//! Capture follows the **carried-book** flow
//! ([`record_book_turnover`](RunCapture::record_book_turnover),
//! [`finish_book`](RunCapture::finish_book)): a leg can span many turnovers,
//! each period's PnL is the whole open book marked close-to-close over that
//! period, a leg's `legs.csv` row is written once (when it finally closes,
//! spanning its full hold), and a portfolio row's `n_long` / `n_short` is the
//! book size over the period rather than the count entered.
//!
//! `RunCapture` is generic over [`crate::period::RebalancePeriod`]: the
//! `period` / `period_end_date` columns and the finalisation logic below work
//! the same way whatever cadence a strategy rebalances on — a calendar month,
//! an ISO week and a run-time-selected `CalendarPeriod` are all just
//! implementations of that trait.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use nautilus_model::{enums::OrderSide, events::OrderFilled, identifiers::InstrumentId};
use rust_decimal::Decimal;

use crate::period::RebalancePeriod;

pub const RUN_DIR: &str = "runs";

pub const LEGS_HEADER: &str = "run_id,period,period_end_date,instrument_id,side,entry_price,exit_price,per_leg_return,notional_usdt";
pub const PORTFOLIO_HEADER: &str = "run_id,period,period_end_date,n_long,n_short,gross_return,fee_paid_usdt,net_return,equity_end_of_period_usdt,n_fills,fills_ref";
pub const FILLS_HEADER: &str =
    "run_id,ts_event,instrument_id,side,order_side,quantity,fill_price,fee_usdt";
pub const SELECTION_HEADER: &str =
    "run_id,period,period_end_date,cmc_rank,cmc_symbol,cmc_id,instrument_id,score,side";

/// One candidate in a `--source coinmarketcap` rebalance: an eligible name (in
/// that date's CoinMarketCap snapshot top-N and resolved to a Bybit perp) that
/// produced a score, and where it landed in the book. Rows for one rebalance
/// are written together by [`RunCapture::record_selection`].
pub struct SelectionRow {
    pub cmc_rank: u32,
    pub cmc_symbol: String,
    pub cmc_id: u64,
    pub instrument_id: InstrumentId,
    pub score: f64,
    /// `"long"` | `"short"` | `"none"`.
    pub side: &'static str,
}

/// The shared run configuration, written to `<UUID>/config.csv` (with the
/// strategy's own rows appended) so a tearsheet — or a human — can label a run
/// without re-reading the source.
///
/// Defined in [`crate::config`] (it is also the strategy's runtime input) and
/// re-exported here for the capture API.
pub use crate::config::RunConfig;

/// A leg the carried-book flow is holding: entered at `entry_period`, marked
/// forward to `mark` at the last turnover, still open. Priced out only when it
/// leaves the target set (or at `on_stop`).
struct OpenLeg<P> {
    side: OrderSide,
    entry_price: Decimal,
    notional_usdt: f64,
    entry_period: P,
    /// Close the leg was last marked to — the reference for the *next*
    /// period's contribution, so the per-period moves telescope to the full
    /// `(exit - entry) / entry`.
    mark: Decimal,
}

/// Everything accrued for a turnover period until its portfolio row is
/// written: the equity snapshot taken at the turnover, and the fills' fee
/// total and count.
#[derive(Default)]
struct PeriodAccrual {
    equity_start: Option<Decimal>,
    fee_paid: Decimal,
    n_fills: u32,
}

pub struct RunCapture<P: RebalancePeriod> {
    run_id: String,
    fills_ref: String,
    /// The run's directory (`runs/<run_id>/` or a test's tmp equivalent), kept
    /// so `cmc_selection.csv` can be opened lazily.
    run_dir: PathBuf,
    legs: BufWriter<File>,
    portfolio: BufWriter<File>,
    fills: BufWriter<File>,
    /// `runs/<run_id>/cmc_selection.csv`, opened on the first
    /// [`record_selection`](Self::record_selection) — so a `--source bybit` run
    /// never creates the file.
    selection: Option<BufWriter<File>>,
    /// Turnover periods carrying fee accrual until their portfolio row is
    /// written, oldest first.
    periods: BTreeMap<P, PeriodAccrual>,
    /// The carried book: one entry per instrument currently held.
    open_book: BTreeMap<InstrumentId, OpenLeg<P>>,
    /// The last turnover period recorded — the period whose portfolio row the
    /// next turnover finalises.
    last_turnover: Option<P>,
}

impl<P: RebalancePeriod> RunCapture<P> {
    /// Open (append mode) the four run files under `runs/<run_id>/`, writing
    /// headers to any that are new, and (re)write the config sidecar. Creates
    /// the run directory on demand. `strategy_rows` are the running strategy's
    /// own `key,value` pairs, appended to the shared rows in `config.csv`.
    pub fn open(cfg: &RunConfig, strategy_rows: &[(String, String)]) -> Result<Self> {
        Self::open_in(Path::new(RUN_DIR), cfg, strategy_rows)
    }

    /// As [`open`](Self::open), but rooted at `base_dir` instead of `runs/`.
    /// The per-run files land in `base_dir/<run_id>/`.
    pub fn open_in(
        base_dir: &Path,
        cfg: &RunConfig,
        strategy_rows: &[(String, String)],
    ) -> Result<Self> {
        let dir = base_dir.join(&cfg.run_id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        let legs_path = dir.join("legs.csv");
        let portfolio_path = dir.join("portfolio.csv");
        let fills_path = dir.join("fills.csv");

        let legs = open_appending(&legs_path, LEGS_HEADER)?;
        let portfolio = open_appending(&portfolio_path, PORTFOLIO_HEADER)?;
        let fills = open_appending(&fills_path, FILLS_HEADER)?;

        write_config(&dir.join("config.csv"), cfg, strategy_rows)?;

        Ok(Self {
            run_id: cfg.run_id.clone(),
            fills_ref: fills_path.to_string_lossy().into_owned(),
            run_dir: dir,
            legs,
            portfolio,
            fills,
            selection: None,
            periods: BTreeMap::new(),
            open_book: BTreeMap::new(),
            last_turnover: None,
        })
    }

    /// Record an `OrderFilled` event: one fills row now, plus the fee and fill
    /// count folded into `period`'s accrual. `period` is the rebalance period
    /// the fill's timestamp falls in (the caller resolves it via
    /// `StrategyRuntime::current_period`).
    pub fn record_fill(&mut self, period: P, event: &OrderFilled) {
        let fee = event
            .commission
            .map(|m| m.as_decimal())
            .unwrap_or(Decimal::ZERO);
        self.record_fill_row(
            period,
            event.ts_event.as_u64(),
            event.instrument_id,
            event.order_side,
            event.last_qty.as_decimal(),
            event.last_px.as_decimal(),
            fee,
        );
    }

    /// The primitive behind [`record_fill`](Self::record_fill), split out so it
    /// can be exercised without constructing a full `OrderFilled`. The columns
    /// it writes are the `fills.csv` contract, hence the wide signature.
    #[allow(clippy::too_many_arguments)]
    pub fn record_fill_row(
        &mut self,
        period: P,
        ts_event: u64,
        instrument: InstrumentId,
        order_side: OrderSide,
        quantity: Decimal,
        fill_price: Decimal,
        fee_usdt: Decimal,
    ) {
        let _ = writeln!(
            self.fills,
            "{},{},{},{},{},{},{},{}",
            self.run_id,
            ts_event,
            instrument,
            side_label_from_order(order_side),
            order_side_label(order_side),
            quantity,
            fill_price,
            fee_usdt,
        );
        let _ = self.fills.flush();

        let accrual = self.periods.entry(period).or_default();
        accrual.fee_paid += fee_usdt;
        accrual.n_fills += 1;
    }

    /// Write one `cmc_selection.csv` block for `period` — the point-in-time
    /// resolved universe of a `--source coinmarketcap` rebalance. The file is
    /// created on the first call, so `--source bybit` runs never produce it.
    /// `rows` should already be in CMC-rank order.
    pub fn record_selection(&mut self, period: P, rows: &[SelectionRow]) {
        if self.selection.is_none() {
            match open_appending(&self.run_dir.join("cmc_selection.csv"), SELECTION_HEADER) {
                Ok(writer) => self.selection = Some(writer),
                Err(e) => {
                    log::error!("cmc_selection.csv: {e:#}");
                    return;
                }
            }
        }
        let writer = self.selection.as_mut().expect("just opened");
        let end_date = period.end_date();
        for row in rows {
            let _ = writeln!(
                writer,
                "{},{},{},{},{},{},{},{:.6},{}",
                self.run_id,
                period.label(),
                end_date,
                row.cmc_rank,
                row.cmc_symbol,
                row.cmc_id,
                row.instrument_id,
                row.score,
                row.side,
            );
        }
        let _ = writer.flush();
    }

    /// Record a turnover of the carried book at `turnover_period`:
    ///
    /// 1. mark every leg still open to `marks`, book its close-to-close move
    ///    since the last turnover into the *previous* turnover period, and
    ///    write that period's portfolio row;
    /// 2. price out and write a `legs.csv` row for each instrument in `closed`;
    /// 3. start tracking each leg in `opened`
    ///    (`(instrument, side, entry_price, notional_usdt)`).
    ///
    /// `marks` must cover every held and every just-closed instrument.
    pub fn record_book_turnover(
        &mut self,
        turnover_period: P,
        equity: Decimal,
        marks: &HashMap<InstrumentId, Decimal>,
        opened: &[(InstrumentId, OrderSide, Decimal, f64)],
        closed: &[InstrumentId],
    ) {
        // 1. mark the book forward and finalise the period that just ended.
        if let Some(prev) = self.last_turnover {
            self.finalise_turnover_period(prev, marks, equity);
        }

        // 2. price out the legs that left the book.
        for instrument in closed {
            let Some(leg) = self.open_book.remove(instrument) else {
                continue;
            };
            self.write_closed_leg(&leg, *instrument, marks.get(instrument).copied());
        }

        // 3. start tracking the new legs.
        for &(instrument, side, entry_price, notional_usdt) in opened {
            self.open_book.insert(
                instrument,
                OpenLeg {
                    side,
                    entry_price,
                    notional_usdt,
                    entry_period: turnover_period,
                    mark: entry_price,
                },
            );
        }

        self.periods
            .entry(turnover_period)
            .or_default()
            .equity_start
            .get_or_insert(equity);
        self.last_turnover = Some(turnover_period);
    }

    /// Close the carried book at `on_stop`: mark every still-open leg to
    /// `marks`, write the final portfolio row and a `legs.csv` row per leg,
    /// then flush.
    pub fn finish_book(&mut self, marks: &HashMap<InstrumentId, Decimal>, equity: Decimal) {
        if let Some(prev) = self.last_turnover.take() {
            self.finalise_turnover_period(prev, marks, equity);
        }

        let book = std::mem::take(&mut self.open_book);
        for (instrument, leg) in book {
            self.write_closed_leg(&leg, instrument, marks.get(&instrument).copied());
        }

        let _ = self.legs.flush();
        let _ = self.portfolio.flush();
        let _ = self.fills.flush();
        if let Some(selection) = self.selection.as_mut() {
            let _ = selection.flush();
        }
    }

    /// Finalise the turnover period `prev`: mark every still-open leg forward
    /// to `marks`, booking each leg's close-to-close move since the last
    /// turnover, then write `prev`'s portfolio row. `n_long` / `n_short` count
    /// the whole book held over the period; a leg with no mark rides on
    /// (its `mark` unchanged) and still counts toward the book size.
    ///
    /// The single place a carried-book period's portfolio row is written —
    /// shared by [`record_book_turnover`](Self::record_book_turnover) and
    /// [`finish_book`](Self::finish_book).
    fn finalise_turnover_period(
        &mut self,
        prev: P,
        marks: &HashMap<InstrumentId, Decimal>,
        equity: Decimal,
    ) {
        let mut n_long = 0u32;
        let mut n_short = 0u32;
        let mut leg_pnl_usdt = Decimal::ZERO;
        for (instrument, leg) in self.open_book.iter_mut() {
            match leg.side {
                OrderSide::Sell => n_short += 1,
                _ => n_long += 1,
            }
            let Some(&mark_now) = marks.get(instrument) else {
                continue;
            };
            if leg.entry_price.is_zero() {
                continue;
            }
            let raw = (mark_now - leg.mark) / leg.entry_price;
            let signed = match leg.side {
                OrderSide::Sell => -raw,
                _ => raw,
            };
            let notional = Decimal::try_from(leg.notional_usdt).unwrap_or(Decimal::ZERO);
            leg_pnl_usdt += signed * notional;
            leg.mark = mark_now;
        }
        let accrual = self.periods.remove(&prev).unwrap_or_default();
        self.write_portfolio_row(
            &prev.label(),
            prev.end_date(),
            n_long,
            n_short,
            leg_pnl_usdt,
            accrual.fee_paid,
            accrual.n_fills,
            accrual.equity_start.unwrap_or(equity),
            equity,
        );
    }

    /// Write the `legs.csv` row for a leg leaving the carried book, priced out
    /// at `exit_price` (skipped if there is no mark for the instrument).
    fn write_closed_leg(
        &mut self,
        leg: &OpenLeg<P>,
        instrument: InstrumentId,
        exit_price: Option<Decimal>,
    ) {
        let Some(exit_price) = exit_price else {
            return;
        };
        if leg.entry_price.is_zero() {
            return;
        }
        let raw = (exit_price - leg.entry_price) / leg.entry_price;
        let signed = match leg.side {
            OrderSide::Sell => -raw,
            _ => raw,
        };
        self.write_leg_row(
            &leg.entry_period.label(),
            leg.entry_period.end_date(),
            instrument,
            leg.side,
            leg.entry_price,
            exit_price,
            signed,
            leg.notional_usdt,
        );
    }

    // --- shared row writers ---------------------------------------------

    /// One `legs.csv` row. `signed_return` is the leg's close-to-close return
    /// with the position's direction already applied.
    #[allow(clippy::too_many_arguments)]
    fn write_leg_row(
        &mut self,
        period_label: &str,
        period_end_date: NaiveDate,
        instrument: InstrumentId,
        side: OrderSide,
        entry_price: Decimal,
        exit_price: Decimal,
        signed_return: Decimal,
        notional_usdt: f64,
    ) {
        let _ = writeln!(
            self.legs,
            "{},{},{},{},{},{},{},{},{}",
            self.run_id,
            period_label,
            period_end_date,
            instrument,
            side_label_from_order(side),
            entry_price.normalize(),
            exit_price.normalize(),
            round_6dp(signed_return),
            notional_usdt,
        );
        let _ = self.legs.flush();
    }

    /// One `portfolio.csv` row. `gross_return` and the fee drag are both a
    /// fraction of the period's *opening* equity, so the series compounds as an
    /// account-level return that lines up with `equity_end_of_period_usdt`
    /// (modulo the bar-math vs simulated-account differences the README spells
    /// out). Dividing by notional instead would overstate the return by
    /// roughly equity / notional_deployed.
    #[allow(clippy::too_many_arguments)]
    fn write_portfolio_row(
        &mut self,
        period_label: &str,
        period_end_date: NaiveDate,
        n_long: u32,
        n_short: u32,
        leg_pnl_usdt: Decimal,
        fee_paid: Decimal,
        n_fills: u32,
        equity_start: Decimal,
        equity_end: Decimal,
    ) {
        let (gross_return, fee_drag) = if equity_start.is_zero() {
            (Decimal::ZERO, Decimal::ZERO)
        } else {
            (leg_pnl_usdt / equity_start, fee_paid / equity_start)
        };
        let net_return = gross_return - fee_drag;

        let _ = writeln!(
            self.portfolio,
            "{},{},{},{},{},{},{},{},{},{},{}",
            self.run_id,
            period_label,
            period_end_date,
            n_long,
            n_short,
            round_6dp(gross_return),
            fee_paid.normalize(),
            round_6dp(net_return),
            equity_end.normalize(),
            n_fills,
            self.fills_ref,
        );
        let _ = self.portfolio.flush();
    }
}

fn open_appending(path: &Path, header: &str) -> Result<BufWriter<File>> {
    let is_new = !path.exists();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    if is_new {
        writeln!(writer, "{header}")?;
        writer.flush()?;
    }
    Ok(writer)
}

fn write_config(path: &Path, cfg: &RunConfig, strategy_rows: &[(String, String)]) -> Result<()> {
    let generated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut w =
        BufWriter::new(File::create(path).with_context(|| format!("create {}", path.display()))?);
    writeln!(w, "key,value")?;
    writeln!(w, "run_id,{}", cfg.run_id)?;
    writeln!(w, "generated_at,{generated_at}")?;
    writeln!(w, "strategy,{}", cfg.strategy)?;
    writeln!(w, "exchange,{}", cfg.exchange)?;
    writeln!(w, "date_start,{}", cfg.date_start)?;
    writeln!(w, "date_end,{}", cfg.date_end)?;
    writeln!(w, "starting_balance,{}", cfg.starting_balance)?;
    writeln!(w, "bases,{}", cfg.bases.join(" "))?;
    writeln!(w, "universe_path,{}", cfg.universe_path)?;
    writeln!(w, "argv,{}", cfg.argv)?;
    for (key, value) in strategy_rows {
        writeln!(w, "{key},{value}")?;
    }
    w.flush()?;
    Ok(())
}

/// Round a decimal to 6 fractional digits, always emitting all six so
/// downstream plotly scales don't drop significant digits.
fn round_6dp(d: Decimal) -> String {
    format!("{:.6}", d.round_dp(6))
}

fn order_side_label(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
        _ => "NONE",
    }
}

/// `side` in legs/fills is the *position* direction the order expresses.
fn side_label_from_order(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Sell => "short",
        _ => "long",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_6dp_pads_and_truncates() {
        assert_eq!(round_6dp(Decimal::new(1, 1)), "0.100000");
        assert_eq!(round_6dp(Decimal::new(1234567, 7)), "0.123457");
    }
}
