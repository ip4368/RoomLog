//! Room clash detection.
//!
//! A room holds one thing at a time. A requested occurrence clashes with
//! anything already in the room that overlaps it. Times are exact: a booking
//! ending at 12:00 and another starting at 12:00 don't clash, so organisers
//! include their own set-up and pack-down time.

use chrono::NaiveDateTime;
use serde::Serialize;

use crate::engine::Id;
use crate::recurrence::Span;

/// Something already occupying the room.
#[derive(Clone, Debug, Serialize)]
pub struct Busy {
    pub event_id: Id,
    pub title: String,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

/// One requested occurrence and what it overlaps.
#[derive(Clone, Debug, Serialize)]
pub struct Clash {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub with: Vec<Busy>,
}

/// Requested occurrences that overlap anything in `busy`, in request order.
pub fn find_clashes(requested: &[Span], busy: &[Busy]) -> Vec<Clash> {
    requested
        .iter()
        .filter_map(|r| {
            let with: Vec<Busy> =
                busy.iter().filter(|b| r.overlaps(&Span::new(b.start, b.end))).cloned().collect();
            (!with.is_empty()).then_some(Clash { start: r.start, end: r.end, with })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(day: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2030, 3, day).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn busy(id: Id, start: NaiveDateTime, end: NaiveDateTime) -> Busy {
        Busy { event_id: id, title: format!("Event {id}"), start, end }
    }

    #[test]
    fn back_to_back_is_not_a_clash() {
        let existing = [busy(1, at(4, 10, 0), at(4, 12, 0))];
        assert!(find_clashes(&[Span::new(at(4, 12, 0), at(4, 14, 0))], &existing).is_empty());
        assert!(find_clashes(&[Span::new(at(4, 8, 0), at(4, 10, 0))], &existing).is_empty());
    }

    #[test]
    fn any_overlap_clashes_at_minute_precision() {
        let existing = [busy(1, at(4, 10, 0), at(4, 12, 0))];
        let c = find_clashes(&[Span::new(at(4, 11, 59), at(4, 13, 0))], &existing);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].with[0].event_id, 1);
        // Fully inside and fully covering both count.
        assert_eq!(find_clashes(&[Span::new(at(4, 10, 30), at(4, 11, 0))], &existing).len(), 1);
        assert_eq!(find_clashes(&[Span::new(at(4, 9, 0), at(4, 13, 0))], &existing).len(), 1);
    }

    #[test]
    fn reports_each_clashing_date_and_everything_it_overlaps() {
        let existing = [
            busy(1, at(11, 18, 0), at(11, 20, 0)),
            busy(2, at(11, 19, 30), at(11, 21, 0)),
            busy(3, at(25, 9, 0), at(25, 10, 0)),
        ];
        // Weekly Tuesday 19:00–20:00 on the 4th, 11th, 18th and 25th.
        let requested: Vec<Span> = [4, 11, 18, 25].iter().map(|&d| Span::new(at(d, 19, 0), at(d, 20, 0))).collect();
        let c = find_clashes(&requested, &existing);
        assert_eq!(c.len(), 1, "only the 11th clashes");
        assert_eq!(c[0].start, at(11, 19, 0));
        assert_eq!(c[0].with.iter().map(|b| b.event_id).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn overnight_bookings_clash_across_midnight() {
        let existing = [busy(1, at(4, 22, 0), at(5, 2, 0))];
        assert_eq!(find_clashes(&[Span::new(at(5, 1, 0), at(5, 3, 0))], &existing).len(), 1);
        assert!(find_clashes(&[Span::new(at(5, 2, 0), at(5, 3, 0))], &existing).is_empty());
    }
}
