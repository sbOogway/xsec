//! The cadence-agnostic "what rebalance period is this" abstraction shared by
//! the strategy runtime's rebalance clock ([`crate::strategy::common`]) and
//! the run-artifact capture layer ([`crate::capture`]).
//!
//! A period is whatever a strategy rebalances on — a calendar month
//! ([`YearMonth`]) for `momentum`, an ISO week ([`IsoWeek`]) for a
//! weekly-cadence strategy. Both the rebalance-clock guard and the
//! `legs.csv` / `portfolio.csv` join key derive from the same value via
//! [`RebalancePeriod`], so there is exactly one definition of "what period is
//! this" per strategy, not two independent ones.

use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};

/// A rebalance period: the unit a strategy's clock rolls over on, and the join
/// key [`crate::capture::RunCapture`] groups legs and portfolio rows by.
///
/// Implement this for a new cadence to give a strategy a rebalance clock and a
/// capture join key at once — no other change to the runtime or capture layer
/// is needed.
pub trait RebalancePeriod: Ord + Copy + std::fmt::Debug + std::hash::Hash {
    /// The period `ts_event` (a Nautilus event's nanosecond timestamp) falls in.
    fn from_nanos(ts_event: u64) -> Self;

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
    fn start_date(&self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("valid year/month")
    }
}

impl RebalancePeriod for YearMonth {
    fn from_nanos(ts_event: u64) -> Self {
        let dt = DateTime::<Utc>::from_timestamp_nanos(ts_event as i64);
        Self {
            year: dt.year(),
            month: dt.month(),
        }
    }

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

/// An ISO-8601 week in UTC. `year` is the ISO week-year (can differ from the
/// calendar year for a few days around New Year's).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct IsoWeek {
    pub year: i32,
    pub week: u32,
}

impl IsoWeek {
    fn monday(&self) -> NaiveDate {
        NaiveDate::from_isoywd_opt(self.year, self.week, Weekday::Mon)
            .expect("valid ISO week-year/week")
    }
}

impl RebalancePeriod for IsoWeek {
    fn from_nanos(ts_event: u64) -> Self {
        let dt = DateTime::<Utc>::from_timestamp_nanos(ts_event as i64);
        let iso = dt.iso_week();
        Self {
            year: iso.year(),
            week: iso.week(),
        }
    }

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
}
