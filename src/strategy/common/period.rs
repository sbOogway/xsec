//! The cadence-agnostic "what rebalance period is this" abstraction shared by
//! the strategy runtime's rebalance clock ([`super`]) and the run-artifact
//! capture layer ([`crate::data::backtest`]).
//!
//! A period is whatever a strategy rebalances on — a single calendar day
//! ([`CalendarDay`]), an ISO week ([`IsoWeek`]) or a calendar month
//! ([`YearMonth`]). Both the rebalance-clock guard and the `legs.csv` /
//! `portfolio.csv` join key derive from the same value via [`RebalancePeriod`],
//! so there is exactly one definition of "what period is this" per strategy,
//! not two independent ones.
//!
//! A strategy with a fixed cadence uses a concrete type directly. One whose
//! cadence is a run-time flag uses [`CalendarPeriod`], the tagged union of all
//! three, with [`HoldingPeriod`] selecting which arm a `--holding-period` flag
//! picks — `momentum` keys on this.

use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};

/// A rebalance period: the unit a strategy's clock rolls over on, and the join
/// key [`crate::data::backtest::RunCapture`] groups legs and portfolio rows by.
///
/// Implementors also carry an inherent `from_nanos(ts_event: u64) -> Self`
/// bucketing constructor; it is not on the trait because [`CalendarPeriod`]
/// can only bucket once its arm is known. Turning a clock tick into a period is
/// [`StrategyRuntime::current_period`](crate::strategy::common::StrategyRuntime::current_period).
pub trait RebalancePeriod: Ord + Copy + std::fmt::Debug + std::hash::Hash {
    /// The `runs/<uuid>/{legs,portfolio}.csv` `period` column value.
    fn label(&self) -> String;

    /// The last calendar day this period covers — the `period_end_date` column.
    fn end_date(&self) -> NaiveDate;

    /// The period immediately after this one.
    fn next(&self) -> Self;
}

/// A calendar month in UTC. `momentum`'s rebalance period.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct YearMonth {
    pub year: i32,
    pub month: u32,
}

impl YearMonth {
    /// The calendar month `ts_event` (a Nautilus event's nanosecond timestamp)
    /// falls in, in UTC.
    pub fn from_nanos(ts_event: u64) -> Self {
        let dt = DateTime::<Utc>::from_timestamp_nanos(ts_event as i64);
        Self {
            year: dt.year(),
            month: dt.month(),
        }
    }

    fn start_date(&self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("valid year/month")
    }
}

impl RebalancePeriod for YearMonth {
    fn label(&self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    fn end_date(&self) -> NaiveDate {
        // The day before the 1st of the following month.
        self.next()
            .start_date()
            .pred_opt()
            .expect("valid calendar date")
    }

    fn next(&self) -> Self {
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
}

/// A single calendar day in UTC. The rebalance period for a daily-cadence
/// strategy — every `"DAILY"` engine-timer fire rolls it, so the whole book
/// turns over each day.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CalendarDay {
    pub date: NaiveDate,
}

impl CalendarDay {
    /// The calendar day `ts_event` (a Nautilus event's nanosecond timestamp)
    /// falls in, in UTC.
    pub fn from_nanos(ts_event: u64) -> Self {
        let dt = DateTime::<Utc>::from_timestamp_nanos(ts_event as i64);
        Self {
            date: dt.date_naive(),
        }
    }
}

impl RebalancePeriod for CalendarDay {
    fn label(&self) -> String {
        self.date.format("%Y-%m-%d").to_string()
    }

    fn end_date(&self) -> NaiveDate {
        self.date
    }

    fn next(&self) -> Self {
        Self {
            date: self.date.succ_opt().expect("valid calendar date"),
        }
    }
}

/// An ISO-8601 week in UTC. `year` is the ISO week-year (can differ from the
/// calendar year for a few days around New Year's).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct IsoWeek {
    pub year: i32,
    pub week: u32,
}

impl IsoWeek {
    /// The ISO-8601 week `ts_event` (a Nautilus event's nanosecond timestamp)
    /// falls in, in UTC.
    pub fn from_nanos(ts_event: u64) -> Self {
        let dt = DateTime::<Utc>::from_timestamp_nanos(ts_event as i64);
        let iso = dt.iso_week();
        Self {
            year: iso.year(),
            week: iso.week(),
        }
    }

    fn monday(&self) -> NaiveDate {
        NaiveDate::from_isoywd_opt(self.year, self.week, Weekday::Mon)
            .expect("valid ISO week-year/week")
    }
}

impl RebalancePeriod for IsoWeek {
    fn label(&self) -> String {
        format!("{:04}-W{:02}", self.year, self.week)
    }

    fn end_date(&self) -> NaiveDate {
        // ISO weeks run Monday..Sunday.
        self.monday() + chrono::Duration::days(6)
    }

    fn next(&self) -> Self {
        let next_monday = self.monday() + chrono::Duration::days(7);
        let iso = next_monday.iso_week();
        Self {
            year: iso.year(),
            week: iso.week(),
        }
    }
}

/// Which [`RebalancePeriod`] a strategy's rebalance clock runs on, as picked by
/// a `--holding-period` CLI flag. clap renders the arms `day`, `iso-week`,
/// `month`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum HoldingPeriod {
    Day,
    IsoWeek,
    Month,
}

impl HoldingPeriod {
    /// The [`CalendarPeriod`] `ts_event` (a Nautilus event's nanosecond
    /// timestamp) falls in, at this cadence.
    pub fn period_at(self, ts_event: u64) -> CalendarPeriod {
        match self {
            Self::Day => CalendarPeriod::Day(CalendarDay::from_nanos(ts_event)),
            Self::IsoWeek => CalendarPeriod::IsoWeek(IsoWeek::from_nanos(ts_event)),
            Self::Month => CalendarPeriod::Month(YearMonth::from_nanos(ts_event)),
        }
    }

    /// The `runs/<uuid>/config.csv` `holding_period` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::IsoWeek => "iso-week",
            Self::Month => "month",
        }
    }
}

/// A [`RebalancePeriod`] whose cadence is chosen at run time — one of
/// [`CalendarDay`], [`IsoWeek`] or [`YearMonth`], tagged so `label` /
/// `end_date` / `next` dispatch to the right one. `momentum` keys on this so
/// `--holding-period` can pick the cadence per run.
///
/// A single run only ever holds one arm (the one [`HoldingPeriod::period_at`]
/// produces), so the derived `Ord` — which orders by arm first — only ever
/// compares within that arm.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CalendarPeriod {
    Day(CalendarDay),
    IsoWeek(IsoWeek),
    Month(YearMonth),
}

impl RebalancePeriod for CalendarPeriod {
    fn label(&self) -> String {
        match self {
            Self::Day(p) => p.label(),
            Self::IsoWeek(p) => p.label(),
            Self::Month(p) => p.label(),
        }
    }

    fn end_date(&self) -> NaiveDate {
        match self {
            Self::Day(p) => p.end_date(),
            Self::IsoWeek(p) => p.end_date(),
            Self::Month(p) => p.end_date(),
        }
    }

    fn next(&self) -> Self {
        match self {
            Self::Day(p) => Self::Day(p.next()),
            Self::IsoWeek(p) => Self::IsoWeek(p.next()),
            Self::Month(p) => Self::Month(p.next()),
        }
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
        assert_eq!(
            dec.next(),
            YearMonth {
                year: 2026,
                month: 1
            }
        );
    }

    #[test]
    fn year_month_label_and_end_date() {
        let m = YearMonth {
            year: 2026,
            month: 2,
        };
        assert_eq!(m.label(), "2026-02");
        // 2026 is not a leap year.
        assert_eq!(m.end_date(), NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
    }

    #[test]
    fn iso_week_next_wraps_year() {
        // ISO week 52 of 2025 is the last full week of that ISO year;
        // 2025-12-29 is a Monday starting week 1 of 2026.
        let w52 = IsoWeek {
            year: 2025,
            week: 52,
        };
        assert_eq!(
            w52.next(),
            IsoWeek {
                year: 2026,
                week: 1
            }
        );
    }

    #[test]
    fn iso_week_label_and_end_date() {
        let w = IsoWeek {
            year: 2026,
            week: 3,
        };
        assert_eq!(w.label(), "2026-W03");
        // Week 3 of 2026 runs Mon 2026-01-12 .. Sun 2026-01-18.
        assert_eq!(w.monday(), NaiveDate::from_ymd_opt(2026, 1, 12).unwrap());
        assert_eq!(w.end_date(), NaiveDate::from_ymd_opt(2026, 1, 18).unwrap());
    }

    #[test]
    fn iso_week_from_nanos_matches_monday() {
        // 2026-01-12T00:00:00Z is a Monday (start of ISO week 3, 2026).
        let ts = 1_768_176_000_000_000_000u64;
        let week = IsoWeek::from_nanos(ts);
        assert_eq!(
            week,
            IsoWeek {
                year: 2026,
                week: 3
            }
        );
    }

    fn day(year: i32, month: u32, date: u32) -> CalendarDay {
        CalendarDay {
            date: NaiveDate::from_ymd_opt(year, month, date).unwrap(),
        }
    }

    #[test]
    fn calendar_day_next_wraps_month_and_year() {
        assert_eq!(day(2026, 1, 31).next(), day(2026, 2, 1));
        assert_eq!(day(2025, 12, 31).next(), day(2026, 1, 1));
        // 2028 is a leap year: February has a 29th.
        assert_eq!(day(2028, 2, 28).next(), day(2028, 2, 29));
    }

    #[test]
    fn calendar_day_label_and_end_date() {
        let d = day(2026, 1, 12);
        assert_eq!(d.label(), "2026-01-12");
        // A day's period ends on the day itself.
        assert_eq!(d.end_date(), NaiveDate::from_ymd_opt(2026, 1, 12).unwrap());
    }

    #[test]
    fn calendar_day_from_nanos_takes_the_utc_date() {
        // 2026-01-12T00:00:00Z, and one nanosecond before midnight the next day.
        let midnight = 1_768_176_000_000_000_000u64;
        assert_eq!(CalendarDay::from_nanos(midnight), day(2026, 1, 12));
        assert_eq!(
            CalendarDay::from_nanos(midnight + 86_400 * 1_000_000_000 - 1),
            day(2026, 1, 12)
        );
    }

    #[test]
    fn holding_period_buckets_one_timestamp_three_ways() {
        // 2026-01-12T00:00:00Z is a Monday, start of ISO week 3.
        let ts = 1_768_176_000_000_000_000u64;
        assert_eq!(
            HoldingPeriod::Day.period_at(ts),
            CalendarPeriod::Day(day(2026, 1, 12))
        );
        assert_eq!(
            HoldingPeriod::IsoWeek.period_at(ts),
            CalendarPeriod::IsoWeek(IsoWeek {
                year: 2026,
                week: 3
            })
        );
        assert_eq!(
            HoldingPeriod::Month.period_at(ts),
            CalendarPeriod::Month(YearMonth {
                year: 2026,
                month: 1
            })
        );
    }

    #[test]
    fn calendar_period_delegates_label_end_date_and_next() {
        let d = CalendarPeriod::Day(day(2026, 1, 31));
        assert_eq!(d.label(), "2026-01-31");
        assert_eq!(d.end_date(), NaiveDate::from_ymd_opt(2026, 1, 31).unwrap());
        assert_eq!(d.next(), CalendarPeriod::Day(day(2026, 2, 1)));

        let w = CalendarPeriod::IsoWeek(IsoWeek {
            year: 2026,
            week: 3,
        });
        assert_eq!(w.label(), "2026-W03");
        assert_eq!(w.end_date(), NaiveDate::from_ymd_opt(2026, 1, 18).unwrap());
        assert_eq!(
            w.next(),
            CalendarPeriod::IsoWeek(IsoWeek {
                year: 2026,
                week: 4
            })
        );

        let m = CalendarPeriod::Month(YearMonth {
            year: 2026,
            month: 12,
        });
        assert_eq!(m.label(), "2026-12");
        assert_eq!(
            m.next(),
            CalendarPeriod::Month(YearMonth {
                year: 2027,
                month: 1
            })
        );
    }

    #[test]
    fn calendar_period_orders_within_an_arm() {
        let a = CalendarPeriod::Day(day(2026, 1, 5));
        let b = CalendarPeriod::Day(day(2026, 1, 6));
        assert!(a < b);
        assert!(a.next() == b);
    }
}
