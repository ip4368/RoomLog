//! The availability / allocation engine.
//!
//! Everything here is pure: the caller loads a [`World`] snapshot (stock,
//! church event occurrences and already-committed loans that overlap the
//! request window) and asks [`evaluate`] whether a loan request can be met.
//!
//! Rules:
//! * Each room's church events are covered only from that room's own stock.
//! * Only events/loans that overlap the request window matter (time-aware).
//! * A room can lend `stock - peak(event demand + committed loans)` units of a
//!   type, where the peak is taken over the request window.
//! * When that is not enough, the engine works out which event occurrences
//!   would need to manage with less, and by how much, to cover the shortfall.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDateTime;
use serde::Serialize;

use crate::recurrence::Span;

pub type Id = i64;

/// A concrete occurrence of a church event, with overrides already applied.
#[derive(Clone, Debug)]
pub struct Occurrence {
    pub event_id: Id,
    pub group_id: Option<Id>,
    pub room_id: Id,
    pub span: Span,
    /// type_id -> quantity that must stay in the room for this occurrence.
    pub needs: BTreeMap<Id, i64>,
}

/// Equipment already promised to another (approved / collected) loan.
#[derive(Clone, Debug)]
pub struct Holding {
    pub loan_id: Id,
    /// When the gear is out of the room (pickup → return + buffer).
    pub span: Span,
    pub room_id: Id,
    pub type_id: Id,
    pub qty: i64,
}

#[derive(Clone, Debug, Default)]
pub struct World {
    /// (room_id, type_id) -> quantity owned.
    pub stock: HashMap<(Id, Id), i64>,
    pub occurrences: Vec<Occurrence>,
    pub holdings: Vec<Holding>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RoomQty {
    pub room_id: Id,
    pub qty: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoomAvailability {
    pub room_id: Id,
    pub stock: i64,
    /// Peak quantity church events need in this room during the window.
    pub peak_event_need: i64,
    /// Peak quantity already out on other loans during the window.
    pub peak_on_loan: i64,
    /// Peak of events + loans combined (they may not coincide).
    pub peak_combined: i64,
    /// Units that can be lent without affecting anyone.
    pub spare: i64,
    /// True if events + loans already exceed stock at some point.
    pub overcommitted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OccurrenceAsk {
    pub event_id: Id,
    pub group_id: Option<Id>,
    pub room_id: Id,
    pub type_id: Id,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub normally: i64,
    pub reduce_to: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoomResolution {
    pub room_id: Id,
    /// Extra units this room could supply if all `asks` are agreed.
    pub extra: i64,
    pub asks: Vec<OccurrenceAsk>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemEvaluation {
    pub type_id: Id,
    pub requested: i64,
    /// Total that can be lent right now without asking anyone.
    pub available: i64,
    /// Where to collect the available units from.
    pub allocation: Vec<RoomQty>,
    pub shortfall: i64,
    /// Suggested negotiations that would cover (part of) the shortfall.
    pub resolutions: Vec<RoomResolution>,
    /// Units that cannot be found even if every church event agreed to go
    /// without (stock simply isn't there, or is out on other loans).
    pub unresolvable: i64,
    /// Other loans holding this type during the window.
    pub blocking_loans: Vec<Id>,
    pub rooms: Vec<RoomAvailability>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Evaluation {
    pub window: Span,
    pub fully_available: bool,
    pub items: Vec<ItemEvaluation>,
}

/// One elementary time segment within the window for a single (room, type).
struct Segment {
    event_need: i64,
    on_loan: i64,
    /// (index into World::occurrences, quantity needed)
    active: Vec<(usize, i64)>,
}

fn segments(world: &World, room: Id, ty: Id, window: &Span) -> Vec<Segment> {
    let occ: Vec<(usize, Span, i64)> = world
        .occurrences
        .iter()
        .enumerate()
        .filter(|(_, o)| o.room_id == room)
        .filter_map(|(i, o)| {
            let q = *o.needs.get(&ty)?;
            (q > 0).then_some(())?;
            Some((i, o.span.clip(window)?, q))
        })
        .collect();
    let loans: Vec<(Span, i64)> = world
        .holdings
        .iter()
        .filter(|h| h.room_id == room && h.type_id == ty && h.qty > 0)
        .filter_map(|h| Some((h.span.clip(window)?, h.qty)))
        .collect();

    let mut points = vec![window.start, window.end];
    for (_, s, _) in &occ {
        points.extend([s.start, s.end]);
    }
    for (s, _) in &loans {
        points.extend([s.start, s.end]);
    }
    points.sort();
    points.dedup();

    points
        .windows(2)
        .map(|p| {
            let span = Span::new(p[0], p[1]);
            let active: Vec<(usize, i64)> = occ
                .iter()
                .filter(|(_, s, _)| s.covers(&span))
                .map(|(i, _, q)| (*i, *q))
                .collect();
            Segment {
                event_need: active.iter().map(|(_, q)| q).sum(),
                on_loan: loans.iter().filter(|(s, _)| s.covers(&span)).map(|(_, q)| q).sum(),
                active,
            }
        })
        .collect()
}

fn room_availability(world: &World, room: Id, ty: Id, window: &Span) -> (RoomAvailability, Vec<Segment>) {
    let stock = world.stock.get(&(room, ty)).copied().unwrap_or(0);
    let segs = segments(world, room, ty, window);
    let peak_event_need = segs.iter().map(|s| s.event_need).max().unwrap_or(0);
    let peak_on_loan = segs.iter().map(|s| s.on_loan).max().unwrap_or(0);
    let peak_combined = segs.iter().map(|s| s.event_need + s.on_loan).max().unwrap_or(0);
    (
        RoomAvailability {
            room_id: room,
            stock,
            peak_event_need,
            peak_on_loan,
            peak_combined,
            spare: (stock - peak_combined).max(0),
            overcommitted: peak_combined > stock,
        },
        segs,
    )
}

/// Work out which occurrences in a room must reduce, and to what, so that the
/// room can lend `target` units in total during the window.
fn asks_for_target(world: &World, ty: Id, stock: i64, target: i64, segs: &[Segment]) -> Vec<OccurrenceAsk> {
    let mut reduction: BTreeMap<usize, i64> = BTreeMap::new();
    for seg in segs {
        let allowed = stock - target - seg.on_loan;
        let mut excess = seg.event_need - allowed;
        if excess <= 0 {
            continue;
        }
        // Ask the biggest user first: fewer people to bother.
        let mut active = seg.active.clone();
        active.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (idx, q) in active {
            if excess <= 0 {
                break;
            }
            let take = q.min(excess);
            excess -= take;
            // A reduction applies to the whole occurrence, so keep the max.
            let r = reduction.entry(idx).or_insert(0);
            *r = (*r).max(take);
        }
    }
    reduction
        .into_iter()
        .map(|(idx, r)| {
            let o = &world.occurrences[idx];
            let normally = o.needs.get(&ty).copied().unwrap_or(0);
            OccurrenceAsk {
                event_id: o.event_id,
                group_id: o.group_id,
                room_id: o.room_id,
                type_id: ty,
                start: o.span.start,
                end: o.span.end,
                normally,
                reduce_to: normally - r,
            }
        })
        .collect()
}

/// Evaluate a loan request. `items` is a list of (type_id, quantity);
/// duplicate type ids are merged.
pub fn evaluate(world: &World, window: Span, items: &[(Id, i64)]) -> Evaluation {
    let mut merged: BTreeMap<Id, i64> = BTreeMap::new();
    for &(ty, q) in items {
        if q > 0 {
            *merged.entry(ty).or_insert(0) += q;
        }
    }

    let mut out = Vec::new();
    for (ty, requested) in merged {
        let mut rooms: Vec<Id> = world
            .stock
            .iter()
            .filter(|((_, t), q)| *t == ty && **q > 0)
            .map(|((r, _), _)| *r)
            .collect();
        rooms.sort();

        let analysed: Vec<(RoomAvailability, Vec<Segment>)> =
            rooms.iter().map(|&r| room_availability(world, r, ty, &window)).collect();

        // 1. Allocate from spare stock, largest spare first (fewest pickups).
        let mut order: Vec<usize> = (0..analysed.len()).collect();
        order.sort_by(|&a, &b| {
            analysed[b].0.spare.cmp(&analysed[a].0.spare).then(analysed[a].0.room_id.cmp(&analysed[b].0.room_id))
        });
        let mut remaining = requested;
        let mut allocation = Vec::new();
        let mut taken: HashMap<Id, i64> = HashMap::new();
        for &i in &order {
            if remaining == 0 {
                break;
            }
            let ra = &analysed[i].0;
            let q = ra.spare.min(remaining);
            if q > 0 {
                allocation.push(RoomQty { room_id: ra.room_id, qty: q });
                taken.insert(ra.room_id, q);
                remaining -= q;
            }
        }
        let available = requested - remaining;
        let shortfall = remaining;

        // 2. If short, find who could be asked to manage with less.
        let mut resolutions = Vec::new();
        if remaining > 0 {
            struct Candidate {
                idx: usize,
                freeable: i64,
            }
            let mut candidates: Vec<Candidate> = analysed
                .iter()
                .enumerate()
                .filter_map(|(idx, (ra, segs))| {
                    let max_if_waived = (ra.stock - segs.iter().map(|s| s.on_loan).max().unwrap_or(0)).max(0);
                    let freeable = max_if_waived - ra.spare;
                    (freeable > 0).then_some(Candidate { idx, freeable })
                })
                .collect();

            while remaining > 0 && !candidates.is_empty() {
                // Prefer a room that covers everything with the fewest asks,
                // otherwise the room that frees the most.
                let scored: Vec<(usize, i64, Vec<OccurrenceAsk>)> = candidates
                    .iter()
                    .enumerate()
                    .map(|(ci, c)| {
                        let (ra, segs) = &analysed[c.idx];
                        let extra = c.freeable.min(remaining);
                        let target = taken.get(&ra.room_id).copied().unwrap_or(0) + extra;
                        (ci, extra, asks_for_target(world, ty, ra.stock, target, segs))
                    })
                    .collect();
                let best = scored
                    .into_iter()
                    .min_by(|a, b| {
                        let a_full = a.1 >= remaining;
                        let b_full = b.1 >= remaining;
                        b_full
                            .cmp(&a_full)
                            .then_with(|| {
                                if a_full && b_full {
                                    a.2.len().cmp(&b.2.len())
                                } else {
                                    b.1.cmp(&a.1)
                                }
                            })
                            .then_with(|| a.2.len().cmp(&b.2.len()))
                            .then_with(|| analysed[candidates[a.0].idx].0.room_id.cmp(&analysed[candidates[b.0].idx].0.room_id))
                    })
                    .expect("non-empty");
                let (ci, extra, asks) = best;
                let room_id = analysed[candidates[ci].idx].0.room_id;
                remaining -= extra;
                resolutions.push(RoomResolution { room_id, extra, asks });
                candidates.remove(ci);
            }
        }

        let mut blocking_loans: Vec<Id> = world
            .holdings
            .iter()
            .filter(|h| h.type_id == ty && h.qty > 0 && h.span.overlaps(&window))
            .map(|h| h.loan_id)
            .collect();
        blocking_loans.sort();
        blocking_loans.dedup();

        out.push(ItemEvaluation {
            type_id: ty,
            requested,
            available,
            allocation,
            shortfall,
            resolutions,
            unresolvable: remaining,
            blocking_loans,
            rooms: analysed.into_iter().map(|(ra, _)| ra).collect(),
        });
    }

    Evaluation {
        window,
        fully_available: out.iter().all(|i| i.shortfall == 0),
        items: out,
    }
}

/// For an already-approved loan, check whether its allocation still fits
/// (e.g. a new event was added after approval). Returns problems found.
pub fn check_allocation(world: &World, window: Span, allocation: &[(Id, Id, i64)]) -> Vec<(Id, Id, i64)> {
    allocation
        .iter()
        .filter_map(|&(room, ty, qty)| {
            let (ra, _) = room_availability(world, room, ty, &window);
            let over = qty - ra.spare;
            (over > 0).then_some((room, ty, over))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALL: Id = 1;
    const YOUTH: Id = 2;
    const STORE: Id = 3;
    const MIC: Id = 10;
    const DI: Id = 11;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }
    fn span(a: &str, b: &str) -> Span {
        Span::new(dt(a), dt(b))
    }
    fn occ(event_id: Id, group: Id, room: Id, s: Span, needs: &[(Id, i64)]) -> Occurrence {
        Occurrence { event_id, group_id: Some(group), room_id: room, span: s, needs: needs.iter().copied().collect() }
    }

    fn world() -> World {
        let mut w = World::default();
        w.stock.insert((HALL, MIC), 8);
        w.stock.insert((YOUTH, MIC), 4);
        w.stock.insert((STORE, MIC), 2);
        w.stock.insert((HALL, DI), 2);
        // Sunday service in the hall needs 6 mics, youth on Sunday evening needs 4.
        w.occurrences.push(occ(100, 1000, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(MIC, 6), (DI, 2)]));
        w.occurrences.push(occ(101, 1001, YOUTH, span("2026-11-22 18:00", "2026-11-22 21:00"), &[(MIC, 4)]));
        w
    }

    #[test]
    fn weekday_loan_takes_everything_when_nothing_scheduled() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-19 09:00"), &[(MIC, 12)]);
        assert!(ev.fully_available);
        let item = &ev.items[0];
        assert_eq!(item.available, 12);
        // Largest spare first: hall (8), then youth (4).
        assert_eq!(item.allocation, vec![RoomQty { room_id: HALL, qty: 8 }, RoomQty { room_id: YOUTH, qty: 4 }]);
    }

    #[test]
    fn weekend_loan_keeps_minimum_in_each_room() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-21 09:00", "2026-11-23 09:00"), &[(MIC, 4)]);
        assert!(ev.fully_available);
        let item = &ev.items[0];
        // Hall spare 2, store spare 2, youth spare 0.
        assert_eq!(item.available, 4);
        let total: i64 = item.allocation.iter().map(|a| a.qty).sum();
        assert_eq!(total, 4);
        assert!(item.allocation.iter().all(|a| a.room_id != YOUTH));
    }

    #[test]
    fn shortage_suggests_who_to_ask() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-21 09:00", "2026-11-23 09:00"), &[(MIC, 6)]);
        assert!(!ev.fully_available);
        let item = &ev.items[0];
        assert_eq!(item.available, 4);
        assert_eq!(item.shortfall, 2);
        assert_eq!(item.unresolvable, 0);
        assert_eq!(item.resolutions.len(), 1);
        let res = &item.resolutions[0];
        assert_eq!(res.extra, 2);
        assert_eq!(res.asks.len(), 1);
        let ask = &res.asks[0];
        // Hall already gives 2 spare; to give 4 the service must drop 6 -> 4.
        // Youth would have to drop 4 -> 2. Either is a single ask; the engine
        // picks deterministically, and the ask must be consistent.
        assert_eq!(ask.normally - ask.reduce_to, 2);
    }

    #[test]
    fn unresolvable_when_stock_does_not_exist() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-18 09:00"), &[(MIC, 20), (DI, 1)]);
        let mic = ev.items.iter().find(|i| i.type_id == MIC).unwrap();
        assert_eq!(mic.available, 14);
        assert_eq!(mic.unresolvable, 6);
        assert!(mic.resolutions.is_empty());
        let di = ev.items.iter().find(|i| i.type_id == DI).unwrap();
        assert_eq!(di.shortfall, 0);
    }

    #[test]
    fn committed_loans_reduce_availability_and_block() {
        let mut w = world();
        w.holdings.push(Holding {
            loan_id: 7,
            span: span("2026-11-16 09:00", "2026-11-18 12:00"),
            room_id: HALL,
            type_id: MIC,
            qty: 8,
        });
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-18 09:00"), &[(MIC, 8)]);
        let mic = &ev.items[0];
        assert_eq!(mic.available, 6);
        assert_eq!(mic.unresolvable, 2);
        assert_eq!(mic.blocking_loans, vec![7]);
    }

    #[test]
    fn overlapping_events_in_same_room_sum_and_asks_are_sufficient() {
        let mut w = World::default();
        w.stock.insert((HALL, MIC), 10);
        w.occurrences.push(occ(1, 1, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(MIC, 6)]));
        w.occurrences.push(occ(2, 2, HALL, span("2026-11-22 11:00", "2026-11-22 13:00"), &[(MIC, 3)]));
        w.occurrences.push(occ(3, 3, HALL, span("2026-11-22 15:00", "2026-11-22 16:00"), &[(MIC, 5)]));
        let window = span("2026-11-22 00:00", "2026-11-23 00:00");
        let ev = evaluate(&w, window, &[(MIC, 6)]);
        let mic = &ev.items[0];
        assert_eq!(mic.available, 1); // peak is 9 between 11:00-12:00
        assert_eq!(mic.shortfall, 5);
        let res = &mic.resolutions[0];
        assert_eq!(res.extra, 5);

        // Apply the asks and re-check: the full request must now fit.
        let mut w2 = w.clone();
        for a in &res.asks {
            let o = w2.occurrences.iter_mut().find(|o| o.event_id == a.event_id).unwrap();
            o.needs.insert(MIC, a.reduce_to);
        }
        let ev2 = evaluate(&w2, window, &[(MIC, 6)]);
        assert!(ev2.fully_available, "{:?}", ev2.items[0].rooms);
    }

    #[test]
    fn check_allocation_flags_new_conflicts() {
        let mut w = world();
        let window = span("2026-11-21 09:00", "2026-11-23 09:00");
        assert!(check_allocation(&w, window, &[(HALL, MIC, 2)]).is_empty());
        w.occurrences.push(occ(200, 1002, HALL, span("2026-11-21 10:00", "2026-11-21 12:00"), &[(MIC, 8)]));
        assert_eq!(check_allocation(&w, window, &[(HALL, MIC, 2)]), vec![(HALL, MIC, 2)]);
    }
}
