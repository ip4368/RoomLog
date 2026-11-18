//! Expansion of recurring church events into concrete occurrences.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};

/// A half-open time interval `[start, end)` in church-local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Span {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

impl Span {
    pub fn new(start: NaiveDateTime, end: NaiveDateTime) -> Self {
        Span { start, end }
    }

    pub fn overlaps(&self, other: &Span) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Intersection of two spans, if non-empty.
    pub fn clip(&self, other: &Span) -> Option<Span> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(Span { start, end })
    }

    pub fn covers(&self, other: &Span) -> bool {
        self.start <= other.start && self.end >= other.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recurrence {
    /// Happens once, on `start_date`.
    None,
    /// Every `interval` weeks on the weekday of `start_date`.
    Weekly { interval: u32 },
    /// The `nth` (1..=5, or -1 for "last") weekday-of-`start_date` in each month.
    Monthly { nth: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub start_date: NaiveDate,
    pub start_time: NaiveTime,
    pub duration_minutes: i64,
    pub recurrence: Recurrence,
    pub until: Option<NaiveDate>,
}

impl Rule {
    fn span_on(&self, date: NaiveDate) -> Span {
        let start = date.and_time(self.start_time);
        Span::new(start, start + Duration::minutes(self.duration_minutes.max(1)))
    }

    fn date_allowed(&self, date: NaiveDate) -> bool {
        date >= self.start_date && self.until.is_none_or(|u| date <= u)
    }

    /// All occurrences whose span overlaps `window`, in chronological order.
    pub fn occurrences_in(&self, window: &Span) -> Vec<Span> {
        let mut out = Vec::new();
        // Occurrences can start before the window and still overlap it.
        let lookback = Duration::minutes(self.duration_minutes.max(1)) + Duration::days(1);
        let from = (window.start - lookback).date();
        let to = window.end.date();
        match self.recurrence {
            Recurrence::None => {
                let s = self.span_on(self.start_date);
                if self.date_allowed(self.start_date) && s.overlaps(window) {
                    out.push(s);
                }
            }
            Recurrence::Weekly { interval } => {
                let step = 7 * i64::from(interval.max(1));
                let offset = (from - self.start_date).num_days();
                let k0 = if offset > 0 { offset / step } else { 0 };
                let mut date = self.start_date + Duration::days(k0 * step);
                while date <= to {
                    if self.until.is_some_and(|u| date > u) {
                        break;
                    }
                    let s = self.span_on(date);
                    if self.date_allowed(date) && s.overlaps(window) {
                        out.push(s);
                    }
                    date += Duration::days(step);
                }
            }
            Recurrence::Monthly { nth } => {
                let weekday = self.start_date.weekday();
                let first = from.max(self.start_date);
                let (mut y, mut m) = (first.year(), first.month());
                loop {
                    let month_start = NaiveDate::from_ymd_opt(y, m, 1).expect("valid month");
                    if month_start > to {
                        break;
                    }
                    if let Some(date) = nth_weekday_of_month(y, m, weekday, nth) {
                        if self.until.is_some_and(|u| date > u) {
                            break;
                        }
                        let s = self.span_on(date);
                        if self.date_allowed(date) && s.overlaps(window) {
                            out.push(s);
                        }
                    }
                    (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                }
            }
        }
        out
    }

    pub fn describe(&self) -> String {
        let wd = weekday_name(self.start_date.weekday());
        let time = format!(
            "{}–{}",
            self.start_time.format("%H:%M"),
            (self.start_date.and_time(self.start_time) + Duration::minutes(self.duration_minutes))
                .format("%H:%M")
        );
        let base = match self.recurrence {
            Recurrence::None => format!("Once on {} {}", self.start_date.format("%a %d %b %Y"), time),
            Recurrence::Weekly { interval: 1 } => format!("Every {wd} {time}"),
            Recurrence::Weekly { interval } => format!("Every {interval} weeks on {wd} {time}"),
            Recurrence::Monthly { nth } => format!("{} {wd} of each month {time}", ordinal(nth)),
        };
        match (self.recurrence, self.until) {
            (Recurrence::None, _) | (_, None) => base,
            (_, Some(u)) => format!("{base} until {}", u.format("%d %b %Y")),
        }
    }
}

pub fn nth_weekday_of_month(year: i32, month: u32, weekday: Weekday, nth: i32) -> Option<NaiveDate> {
    if nth == -1 {
        let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
        let mut d = NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()?;
        while d.weekday() != weekday {
            d = d.pred_opt()?;
        }
        return Some(d);
    }
    if !(1..=5).contains(&nth) {
        return None;
    }
    NaiveDate::from_weekday_of_month_opt(year, month, weekday, nth as u8)
}

fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn ordinal(n: i32) -> &'static str {
    match n {
        1 => "1st",
        2 => "2nd",
        3 => "3rd",
        4 => "4th",
        5 => "5th",
        -1 => "Last",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn rule(start: &str, time: &str, mins: i64, rec: Recurrence) -> Rule {
        Rule {
            start_date: NaiveDate::parse_from_str(start, "%Y-%m-%d").unwrap(),
            start_time: NaiveTime::parse_from_str(time, "%H:%M").unwrap(),
            duration_minutes: mins,
            recurrence: rec,
            until: None,
        }
    }

    #[test]
    fn weekly_occurrences_in_window() {
        // 2026-11-01 is a Sunday.
        let r = rule("2026-11-01", "09:00", 180, Recurrence::Weekly { interval: 1 });
        let w = Span::new(dt("2026-11-10 00:00"), dt("2026-11-30 00:00"));
        let occ = r.occurrences_in(&w);
        let starts: Vec<_> = occ.iter().map(|s| s.start).collect();
        assert_eq!(
            starts,
            vec![dt("2026-11-15 09:00"), dt("2026-11-22 09:00"), dt("2026-11-29 09:00")]
        );
    }

    #[test]
    fn fortnightly_respects_phase_and_until() {
        let mut r = rule("2026-11-06", "19:00", 120, Recurrence::Weekly { interval: 2 });
        r.until = Some(NaiveDate::from_ymd_opt(2026, 12, 10).unwrap());
        let w = Span::new(dt("2026-11-01 00:00"), dt("2027-01-01 00:00"));
        let starts: Vec<_> = r.occurrences_in(&w).iter().map(|s| s.start).collect();
        assert_eq!(
            starts,
            vec![dt("2026-11-06 19:00"), dt("2026-11-20 19:00"), dt("2026-12-04 19:00")]
        );
    }

    #[test]
    fn occurrence_starting_before_window_still_counts() {
        let r = rule("2026-11-01", "22:00", 240, Recurrence::None);
        let w = Span::new(dt("2026-11-02 01:00"), dt("2026-11-03 00:00"));
        assert_eq!(r.occurrences_in(&w).len(), 1);
    }

    #[test]
    fn monthly_nth_and_last() {
        // First Saturday of the month starting Nov 2026 (2026-11-07 is a Saturday).
        let r = rule("2026-11-07", "10:00", 60, Recurrence::Monthly { nth: 1 });
        let w = Span::new(dt("2026-11-01 00:00"), dt("2027-02-01 00:00"));
        let starts: Vec<_> = r.occurrences_in(&w).iter().map(|s| s.start.date()).collect();
        assert_eq!(
            starts,
            vec![
                NaiveDate::from_ymd_opt(2026, 11, 7).unwrap(),
                NaiveDate::from_ymd_opt(2026, 12, 5).unwrap(),
                NaiveDate::from_ymd_opt(2027, 1, 2).unwrap(),
            ]
        );
        let last = rule("2026-11-28", "10:00", 60, Recurrence::Monthly { nth: -1 });
        let starts: Vec<_> = last.occurrences_in(&w).iter().map(|s| s.start.date()).collect();
        assert_eq!(
            starts,
            vec![
                NaiveDate::from_ymd_opt(2026, 11, 28).unwrap(),
                NaiveDate::from_ymd_opt(2026, 12, 26).unwrap(),
                NaiveDate::from_ymd_opt(2027, 1, 30).unwrap(),
            ]
        );
    }

    #[test]
    fn describe_is_human_readable() {
        let r = rule("2026-11-01", "09:00", 180, Recurrence::Weekly { interval: 1 });
        assert_eq!(r.describe(), "Every Sunday 09:00–12:00");
    }
}
