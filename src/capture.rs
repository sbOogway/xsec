//! Per-run artifact capture for the cross-sectional momentum backtest.
//!
//! A single backtest run writes four files under `runs/<UUID>/`, all keyed by
//! the run UUID the user already uses for `logs/<UUID>/logs.log`:
//!
//! * `runs/<UUID>/config.csv`    — the strategy configuration (key,value).
//! * `runs/<UUID>/legs.csv`      — one row per (entry month, instrument) leg.
//! * `runs/<UUID>/portfolio.csv` — one row per rebalance month, the aggregate.
//! * `runs/<UUID>/fills.csv`     — one row per `OrderFilled` event.
//!
//! The portfolio file is the source of truth for the tearsheet's headline
//! return series; the legs and fills files are substrate for future
//! per-leg / per-trade diagnostics.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Utc};
use nautilus_model::{enums::OrderSide, events::OrderFilled, identifiers::InstrumentId};
use rust_decimal::Decimal;

pub const RUN_DIR: &str = "runs";

pub const LEGS_HEADER: &str =
    "run_id,month,instrument_id,side,entry_price,exit_price,per_leg_return,notional_usdt";
pub const PORTFOLIO_HEADER: &str = "run_id,month,n_long,n_short,gross_return,fee_paid_usdt,net_return,equity_end_of_month_usdt,n_fills,fills_ref";
pub const FILLS_HEADER: &str =
    "run_id,ts_event,instrument_id,side,order_side,quantity,fill_price,fee_usdt";

/// A calendar month in UTC, the join key between legs and portfolio rows.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct YearMonth {
    pub year: i32,
    pub month: u32,
}

impl YearMonth {
    pub fn from_nanos(ts_event: u64) -> Self {
        let dt = DateTime::<Utc>::from_timestamp_nanos(ts_event as i64);
        Self {
            year: dt.year(),
            month: dt.month(),
        }
    }

    /// The month immediately after this one.
    pub fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    pub fn label(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

/// The strategy configuration, written verbatim to `<UUID>/config.csv` so a
/// tearsheet (or a human) can label a run without re-reading the source.
pub struct RunConfig {
    pub run_id: String,
    pub lookback_months: u16,
    pub holding_months: u16,
    pub percentile: String,
    pub date_start: String,
    pub date_end: String,
    pub bases: Vec<String>,
    pub starting_balance: String,
    pub dollar_position_size: f64,
}

/// A leg the strategy has entered but not yet priced out (the exit mark is
/// only known one rebalance later).
struct PendingLeg {
    instrument: InstrumentId,
    side: OrderSide,
    entry_price: Decimal,
    notional_usdt: f64,
}

/// Everything captured for one rebalance month until it can be finalised.
#[derive(Default)]
struct MonthAccrual {
    legs: Vec<PendingLeg>,
    equity_start: Option<Decimal>,
    fee_paid: Decimal,
    n_fills: u32,
}

pub struct RunCapture {
    run_id: String,
    fills_ref: String,
    legs: BufWriter<File>,
    portfolio: BufWriter<File>,
    fills: BufWriter<File>,
    /// Rebalance months awaiting finalisation, oldest first.
    months: BTreeMap<YearMonth, MonthAccrual>,
}

impl RunCapture {
    /// Open (append mode) the four run files under `runs/<run_id>/`, writing
    /// headers to any that are new, and (re)write the config sidecar. Creates
    /// the run directory on demand.
    pub fn open(cfg: &RunConfig) -> Result<Self> {
        Self::open_in(Path::new(RUN_DIR), cfg)
    }

    /// As [`open`](Self::open), but rooted at `base_dir` instead of `runs/`.
    /// The per-run files land in `base_dir/<run_id>/`.
    pub fn open_in(base_dir: &Path, cfg: &RunConfig) -> Result<Self> {
        let dir = base_dir.join(&cfg.run_id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        let legs_path = dir.join("legs.csv");
        let portfolio_path = dir.join("portfolio.csv");
        let fills_path = dir.join("fills.csv");

        let legs = open_appending(&legs_path, LEGS_HEADER)?;
        let portfolio = open_appending(&portfolio_path, PORTFOLIO_HEADER)?;
        let fills = open_appending(&fills_path, FILLS_HEADER)?;

        write_config(&dir.join("config.csv"), cfg)?;

        Ok(Self {
            run_id: cfg.run_id.clone(),
            fills_ref: fills_path.to_string_lossy().into_owned(),
            legs,
            portfolio,
            fills,
            months: BTreeMap::new(),
        })
    }

    /// Record an `OrderFilled` event: one fills row now, plus the fee and fill
    /// count folded into that calendar month's accrual.
    pub fn record_fill(&mut self, event: &OrderFilled) {
        let fee = event
            .commission
            .map(|m| m.as_decimal())
            .unwrap_or(Decimal::ZERO);
        self.record_fill_row(
            event.ts_event.as_u64(),
            event.instrument_id,
            event.order_side,
            event.last_qty.as_decimal(),
            event.last_px.as_decimal(),
            fee,
        );
    }

    /// The primitive behind [`record_fill`](Self::record_fill), split out so it
    /// can be exercised without constructing a full `OrderFilled`.
    pub fn record_fill_row(
        &mut self,
        ts_event: u64,
        instrument: InstrumentId,
        order_side: OrderSide,
        quantity: Decimal,
        fill_price: Decimal,
        fee_usdt: Decimal,
    ) {
        let month = YearMonth::from_nanos(ts_event);
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

        let accrual = self.months.entry(month).or_default();
        accrual.fee_paid += fee_usdt;
        accrual.n_fills += 1;
    }

    /// Record a rebalance: snapshot the month's opening equity and stash the
    /// legs entered this month. `legs` is `(instrument, side, entry_price,
    /// notional_usdt)` for each order submitted.
    pub fn record_rebalance(
        &mut self,
        month: YearMonth,
        equity_start: Decimal,
        legs: Vec<(InstrumentId, OrderSide, Decimal, f64)>,
    ) {
        let accrual = self.months.entry(month).or_default();
        accrual.equity_start.get_or_insert(equity_start);
        for (instrument, side, entry_price, notional_usdt) in legs {
            accrual.legs.push(PendingLeg {
                instrument,
                side,
                entry_price,
                notional_usdt,
            });
        }
    }

    /// Finalise every month strictly older than `up_to` for which an exit
    /// price is available, using `latest_close` as the exit mark. Called each
    /// rebalance (with the current month) and once more at `on_stop`.
    pub fn finalise_completed(
        &mut self,
        up_to: YearMonth,
        latest_close: &HashMap<InstrumentId, Decimal>,
        equity_now: Decimal,
    ) {
        let due: Vec<YearMonth> = self
            .months
            .keys()
            .copied()
            .filter(|m| m.next() <= up_to)
            .collect();
        for entry_month in due {
            self.finalise_month(entry_month, latest_close, equity_now);
        }
    }

    /// Finalise all remaining months, treating `latest_close` as the exit mark
    /// for legs that never saw a following rebalance.
    pub fn finish(&mut self, latest_close: &HashMap<InstrumentId, Decimal>, equity_now: Decimal) {
        let remaining: Vec<YearMonth> = self.months.keys().copied().collect();
        for entry_month in remaining {
            self.finalise_month(entry_month, latest_close, equity_now);
        }
        let _ = self.legs.flush();
        let _ = self.portfolio.flush();
        let _ = self.fills.flush();
    }

    fn finalise_month(
        &mut self,
        entry_month: YearMonth,
        latest_close: &HashMap<InstrumentId, Decimal>,
        equity_end: Decimal,
    ) {
        let Some(accrual) = self.months.remove(&entry_month) else {
            return;
        };
        let month_label = entry_month.label();

        let mut n_long = 0u32;
        let mut n_short = 0u32;
        let mut weighted_return = Decimal::ZERO;
        let mut total_notional = Decimal::ZERO;

        for leg in &accrual.legs {
            let Some(exit_price) = latest_close.get(&leg.instrument).copied() else {
                continue;
            };
            if leg.entry_price.is_zero() {
                continue;
            }
            let raw = (exit_price - leg.entry_price) / leg.entry_price;
            let signed = match leg.side {
                OrderSide::Sell => -raw,
                _ => raw,
            };
            match leg.side {
                OrderSide::Sell => n_short += 1,
                _ => n_long += 1,
            }
            let notional = Decimal::try_from(leg.notional_usdt).unwrap_or(Decimal::ZERO);
            weighted_return += signed * notional;
            total_notional += notional;

            let _ = writeln!(
                self.legs,
                "{},{},{},{},{},{},{},{}",
                self.run_id,
                month_label,
                leg.instrument,
                side_label_from_order(leg.side),
                leg.entry_price.normalize(),
                exit_price.normalize(),
                round_6dp(signed),
                leg.notional_usdt,
            );
        }
        let _ = self.legs.flush();

        let gross_return = if total_notional.is_zero() {
            Decimal::ZERO
        } else {
            weighted_return / total_notional
        };
        let equity_start = accrual.equity_start.unwrap_or(equity_end);
        let fee_drag = if equity_start.is_zero() {
            Decimal::ZERO
        } else {
            accrual.fee_paid / equity_start
        };
        let net_return = gross_return - fee_drag;

        let _ = writeln!(
            self.portfolio,
            "{},{},{},{},{},{},{},{},{},{}",
            self.run_id,
            month_label,
            n_long,
            n_short,
            round_6dp(gross_return),
            accrual.fee_paid.normalize(),
            round_6dp(net_return),
            equity_end.normalize(),
            accrual.n_fills,
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

fn write_config(path: &Path, cfg: &RunConfig) -> Result<()> {
    let generated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut w = BufWriter::new(File::create(path).with_context(|| format!("create {}", path.display()))?);
    writeln!(w, "key,value")?;
    writeln!(w, "run_id,{}", cfg.run_id)?;
    writeln!(w, "generated_at,{generated_at}")?;
    writeln!(w, "lookback_months,{}", cfg.lookback_months)?;
    writeln!(w, "holding_months,{}", cfg.holding_months)?;
    writeln!(w, "percentile,{}", cfg.percentile)?;
    writeln!(w, "date_start,{}", cfg.date_start)?;
    writeln!(w, "date_end,{}", cfg.date_end)?;
    writeln!(w, "dollar_position_size,{}", cfg.dollar_position_size)?;
    writeln!(w, "starting_balance,{}", cfg.starting_balance)?;
    writeln!(w, "bases,{}", cfg.bases.join(" "))?;
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
    fn year_month_next_wraps_december() {
        let dec = YearMonth {
            year: 2025,
            month: 12,
        };
        assert_eq!(dec.next(), YearMonth { year: 2026, month: 1 });
    }

    #[test]
    fn round_6dp_pads_and_truncates() {
        assert_eq!(round_6dp(Decimal::new(1, 1)), "0.100000");
        assert_eq!(round_6dp(Decimal::new(1234567, 7)), "0.123457");
    }
}
