//! Room bookings.
//!
//! Anyone can ask to book a room; staff with the bookings role always decide.
//! Approving a booking creates a church event (`bookings.event_id`), so from
//! then on it occupies the room, protects the equipment it needs, and shows
//! on the schedule like any other event. Approval re-checks for clashes while
//! holding the database lock, so two approvals can't both take the same slot.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{ApiError, ApiResult, AppState, clean, load_refs, now, random_token, required, validate_no_overlap};
use crate::auth::{Role, Staff};
use crate::clash::{self, Busy, Clash};
use crate::db::{self, fmt_dt, parse_dt, recurrence_from_db, recurrence_to_db};
use crate::engine::{self, Id, Occurrence};
use crate::recurrence::{Recurrence, Rule, Span, nth_weekday_of_month};

/// One-off bookings can run overnight or over several days.
const MAX_ONE_OFF_MINUTES: i64 = 7 * 24 * 60;
/// Each date of a repeating booking.
const MAX_REPEATING_MINUTES: i64 = 24 * 60;
/// How far ahead a repeating booking can run.
const MAX_REPEAT_DAYS: i64 = 366;

// ------------------------------------------------------------------ input

#[derive(Deserialize, Serialize, Clone)]
pub struct NeedIn {
    type_id: Id,
    quantity: i64,
}

#[derive(Deserialize, Clone)]
pub struct BookingIn {
    room_id: Id,
    #[serde(default)]
    title: String,
    #[serde(default)]
    organiser_name: String,
    #[serde(default)]
    organisation: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    phone: String,
    attendees: Option<i64>,
    /// First (or only) occurrence, `YYYY-MM-DDTHH:MM`.
    start_at: String,
    end_at: String,
    #[serde(default = "once")]
    recurrence: String,
    #[serde(default = "one")]
    interval_weeks: i64,
    #[serde(default = "one")]
    nth: i64,
    until: Option<String>,
    #[serde(default)]
    needs: Vec<NeedIn>,
    #[serde(default)]
    av_support: bool,
    #[serde(default)]
    public_title: bool,
    #[serde(default)]
    notes: String,
}

fn once() -> String {
    "none".into()
}

fn one() -> i64 {
    1
}

/// Room, times and equipment, validated.
struct Slot {
    room_id: Id,
    rule: Rule,
    needs: Vec<(Id, i64)>,
}

fn minute(dt: NaiveDateTime) -> NaiveDateTime {
    dt.with_second(0).and_then(|d| d.with_nanosecond(0)).unwrap_or(dt)
}

fn rule_from_input(b: &BookingIn) -> Result<Rule, ApiError> {
    let start = minute(parse_dt(&b.start_at).ok_or_else(|| ApiError::bad("Start date/time is invalid"))?);
    let end = minute(parse_dt(&b.end_at).ok_or_else(|| ApiError::bad("End date/time is invalid"))?);
    if end <= start {
        return Err(ApiError::bad("The end must be after the start"));
    }
    let minutes = (end - start).num_minutes();
    let recurrence = match b.recurrence.as_str() {
        "none" => Recurrence::None,
        "weekly" if (1..=52).contains(&b.interval_weeks) => Recurrence::Weekly { interval: b.interval_weeks as u32 },
        "monthly" if (1..=5).contains(&b.nth) || b.nth == -1 => {
            let d = start.date();
            if nth_weekday_of_month(d.year(), d.month(), d.weekday(), b.nth as i32) != Some(d) {
                return Err(ApiError::bad("For a monthly booking, the first date must fall in the chosen week of the month"));
            }
            Recurrence::Monthly { nth: b.nth as i32 }
        }
        _ => return Err(ApiError::bad("Invalid repeat option")),
    };
    let until = if recurrence == Recurrence::None {
        if minutes > MAX_ONE_OFF_MINUTES {
            return Err(ApiError::bad("A booking can be at most 7 days long"));
        }
        None
    } else {
        if minutes > MAX_REPEATING_MINUTES {
            return Err(ApiError::bad("Each date of a repeating booking can be at most 24 hours"));
        }
        let until = b
            .until
            .as_deref()
            .filter(|u| !u.is_empty())
            .ok_or_else(|| ApiError::bad("Please say when the repeating booking ends"))?;
        let until = NaiveDate::parse_from_str(until, "%Y-%m-%d").map_err(|_| ApiError::bad("'Until' date is invalid"))?;
        if until < start.date() {
            return Err(ApiError::bad("'Until' must be on or after the first date"));
        }
        if until > start.date() + Duration::days(MAX_REPEAT_DAYS) {
            return Err(ApiError::bad("Repeating bookings can run for at most a year; please send a new request after that"));
        }
        Some(until)
    };
    Ok(Rule { start_date: start.date(), start_time: start.time(), duration_minutes: minutes, recurrence, until })
}

/// Every occurrence of a booking rule. Bounded: one-off, or `until` is required.
fn occurrences(rule: &Rule) -> Vec<Span> {
    let first = rule.start_date.and_time(rule.start_time);
    let last_day = rule.until.unwrap_or(rule.start_date) + Duration::days(1);
    let window = Span::new(first, last_day.and_time(NaiveTime::MIN) + Duration::minutes(rule.duration_minutes));
    rule.occurrences_in(&window)
}

fn validate_slot(conn: &Connection, b: &BookingIn, public: bool) -> Result<Slot, ApiError> {
    let room: Option<(String, bool)> = conn
        .query_row("SELECT name, bookable FROM rooms WHERE id = ?1", [b.room_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((room_name, bookable)) = room else {
        return Err(ApiError::bad("Please choose a room"));
    };
    if public && !bookable {
        return Err(ApiError::bad("That room can't be booked online"));
    }
    let rule = rule_from_input(b)?;
    if public && rule.start_date.and_time(rule.start_time) < now() {
        return Err(ApiError::bad("Please choose a time in the future"));
    }
    if b.needs.len() > 100 {
        return Err(ApiError::bad("Too many equipment items"));
    }
    let mut merged: BTreeMap<Id, i64> = BTreeMap::new();
    for n in b.needs.iter().filter(|n| n.quantity != 0) {
        if n.quantity < 0 {
            return Err(ApiError::bad("Quantities can't be negative"));
        }
        *merged.entry(n.type_id).or_insert(0) += n.quantity;
    }
    let tree = db::load_tree(conn)?;
    validate_no_overlap(&tree, &merged.iter().map(|(t, q)| (*t, *q)).collect::<Vec<_>>())?;
    let mut room_stock: std::collections::HashMap<Id, i64> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT type_id, quantity FROM stock WHERE room_id = ?1")?;
        for row in stmt.query_map([b.room_id], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, i64>(1)?)))? {
            let (t, q) = row?;
            room_stock.insert(t, q);
        }
    }
    for (&type_id, &qty) in &merged {
        let Some(name) = tree.names.get(&type_id) else {
            return Err(ApiError::bad("Unknown equipment type"));
        };
        // Needs may name any level: the room must hold that many units
        // somewhere under the requested node.
        let in_room: i64 = engine::leaves_under(&tree.parents, type_id)
            .iter()
            .map(|l| room_stock.get(l).copied().unwrap_or(0))
            .sum();
        if qty > in_room {
            return Err(ApiError::bad(format!("{room_name} only has {in_room} × {name}")));
        }
    }
    Ok(Slot { room_id: b.room_id, rule, needs: merged.into_iter().collect() })
}

struct Contact {
    title: String,
    organiser_name: String,
    organisation: String,
    email: String,
    phone: String,
    attendees: Option<i64>,
    notes: String,
}

fn validate_contact(b: &BookingIn, public: bool) -> Result<Contact, ApiError> {
    let c = Contact {
        title: required(&b.title, "Event name")?,
        organiser_name: required(&b.organiser_name, "Your name")?,
        organisation: clean(&b.organisation, "Organisation")?,
        email: clean(&b.email, "Email")?,
        phone: clean(&b.phone, "Phone")?,
        attendees: b.attendees,
        notes: clean(&b.notes, "Notes")?,
    };
    if public && c.email.is_empty() && c.phone.is_empty() {
        return Err(ApiError::bad("Please give an email or phone number so we can reach you"));
    }
    if c.attendees.is_some_and(|n| !(1..=100_000).contains(&n)) {
        return Err(ApiError::bad("Number of people looks wrong"));
    }
    Ok(c)
}

// ------------------------------------------------------------------ model

#[derive(Serialize)]
pub struct BookingDto {
    id: Id,
    token: String,
    status: String,
    room_id: Id,
    title: String,
    organiser_name: String,
    organisation: String,
    email: String,
    phone: String,
    attendees: Option<i64>,
    start_at: NaiveDateTime,
    end_at: NaiveDateTime,
    recurrence: String,
    interval_weeks: i64,
    nth: i64,
    until: Option<NaiveDate>,
    description: String,
    av_support: bool,
    public_title: bool,
    notes: String,
    admin_notes: String,
    message_to_organiser: String,
    event_id: Option<Id>,
    decided_by: String,
    decided_at: Option<NaiveDateTime>,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
    needs: Vec<NeedIn>,
    #[serde(skip)]
    rule: Rule,
}

const BOOKING_COLS: &str = "id, token, status, room_id, title, organiser_name, organisation, email, phone, attendees,
    start_date, start_time, duration_minutes, recurrence, interval_weeks, nth, until, av_support, public_title,
    notes, admin_notes, message_to_organiser, event_id, decided_by, decided_at, created_at, updated_at";

fn row_to_booking(r: &rusqlite::Row) -> rusqlite::Result<BookingDto> {
    let dt = |i: usize| -> rusqlite::Result<NaiveDateTime> { Ok(parse_dt(&r.get::<_, String>(i)?).unwrap_or_default()) };
    let start_date: String = r.get(10)?;
    let start_time: String = r.get(11)?;
    let kind: String = r.get(13)?;
    let until: Option<String> = r.get(16)?;
    let rule = Rule {
        start_date: NaiveDate::parse_from_str(&start_date, "%Y-%m-%d").unwrap_or_default(),
        start_time: NaiveTime::parse_from_str(&start_time, "%H:%M").unwrap_or_default(),
        duration_minutes: r.get(12)?,
        recurrence: recurrence_from_db(&kind, r.get(14)?, r.get(15)?),
        until: until.and_then(|u| NaiveDate::parse_from_str(&u, "%Y-%m-%d").ok()),
    };
    let start_at = rule.start_date.and_time(rule.start_time);
    let (recurrence, interval_weeks, nth) = recurrence_to_db(rule.recurrence);
    Ok(BookingDto {
        id: r.get(0)?,
        token: r.get(1)?,
        status: r.get(2)?,
        room_id: r.get(3)?,
        title: r.get(4)?,
        organiser_name: r.get(5)?,
        organisation: r.get(6)?,
        email: r.get(7)?,
        phone: r.get(8)?,
        attendees: r.get(9)?,
        start_at,
        end_at: start_at + Duration::minutes(rule.duration_minutes),
        recurrence: recurrence.into(),
        interval_weeks,
        nth,
        until: rule.until,
        description: rule.describe(),
        av_support: r.get(17)?,
        public_title: r.get(18)?,
        notes: r.get(19)?,
        admin_notes: r.get(20)?,
        message_to_organiser: r.get(21)?,
        event_id: r.get(22)?,
        decided_by: r.get(23)?,
        decided_at: r.get::<_, Option<String>>(24)?.and_then(|s| parse_dt(&s)),
        created_at: dt(25)?,
        updated_at: dt(26)?,
        needs: vec![],
        rule,
    })
}

fn fill_needs(conn: &Connection, mut b: BookingDto) -> rusqlite::Result<BookingDto> {
    let mut stmt = conn.prepare("SELECT type_id, quantity FROM booking_needs WHERE booking_id = ?1 ORDER BY type_id")?;
    b.needs = stmt.query_map([b.id], |r| Ok(NeedIn { type_id: r.get(0)?, quantity: r.get(1)? }))?.collect::<Result<_, _>>()?;
    Ok(b)
}

fn get_booking(conn: &Connection, id: Id) -> Result<BookingDto, ApiError> {
    let b = conn
        .query_row(&format!("SELECT {BOOKING_COLS} FROM bookings WHERE id = ?1"), [id], row_to_booking)
        .optional()?
        .ok_or_else(ApiError::not_found)?;
    Ok(fill_needs(conn, b)?)
}

fn get_booking_by_token(conn: &Connection, token: &str) -> Result<BookingDto, ApiError> {
    let b = conn
        .query_row(&format!("SELECT {BOOKING_COLS} FROM bookings WHERE token = ?1"), [token], row_to_booking)
        .optional()?
        .ok_or_else(ApiError::not_found)?;
    Ok(fill_needs(conn, b)?)
}

fn load_bookings(conn: &Connection, where_sql: &str, args: &[&dyn rusqlite::ToSql]) -> rusqlite::Result<Vec<BookingDto>> {
    let sql = format!("SELECT {BOOKING_COLS} FROM bookings {where_sql} ORDER BY start_date, start_time, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(args, row_to_booking)?.collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(|b| fill_needs(conn, b)).collect()
}

fn write_booking(tx: &Connection, id: Option<Id>, slot: &Slot, c: &Contact, b: &BookingIn) -> Result<(Id, String), ApiError> {
    let (kind, interval, nth) = recurrence_to_db(slot.rule.recurrence);
    let ts = fmt_dt(now());
    let fields = params![
        slot.room_id,
        c.title,
        c.organiser_name,
        c.organisation,
        c.email,
        c.phone,
        c.attendees,
        slot.rule.start_date.format("%Y-%m-%d").to_string(),
        slot.rule.start_time.format("%H:%M").to_string(),
        slot.rule.duration_minutes,
        kind,
        interval,
        nth,
        slot.rule.until.map(|u| u.format("%Y-%m-%d").to_string()),
        b.av_support,
        b.public_title,
        c.notes,
        ts,
    ];
    let (id, token) = match id {
        Some(id) => {
            let mut args: Vec<&dyn rusqlite::ToSql> = fields.to_vec();
            args.push(&id);
            tx.execute(
                "UPDATE bookings SET room_id=?1, title=?2, organiser_name=?3, organisation=?4, email=?5, phone=?6,
                 attendees=?7, start_date=?8, start_time=?9, duration_minutes=?10, recurrence=?11, interval_weeks=?12,
                 nth=?13, until=?14, av_support=?15, public_title=?16, notes=?17, updated_at=?18 WHERE id = ?19",
                args.as_slice(),
            )?;
            tx.execute("DELETE FROM booking_needs WHERE booking_id = ?1", [id])?;
            let token: String = tx.query_row("SELECT token FROM bookings WHERE id = ?1", [id], |r| r.get(0))?;
            (id, token)
        }
        None => {
            let token = random_token(32);
            let mut args: Vec<&dyn rusqlite::ToSql> = fields.to_vec();
            args.push(&token);
            tx.execute(
                "INSERT INTO bookings(room_id, title, organiser_name, organisation, email, phone, attendees,
                 start_date, start_time, duration_minutes, recurrence, interval_weeks, nth, until, av_support,
                 public_title, notes, created_at, updated_at, token, status)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?18,?19,'pending')",
                args.as_slice(),
            )?;
            (tx.last_insert_rowid(), token)
        }
    };
    for (t, q) in &slot.needs {
        tx.execute("INSERT INTO booking_needs(booking_id, type_id, quantity) VALUES (?1, ?2, ?3)", params![id, t, q])?;
    }
    Ok((id, token))
}

// ----------------------------------------------------------------- checks

struct ClashCheck {
    dates: Vec<Span>,
    clashes: Vec<Clash>,
}

fn check_clashes(conn: &Connection, room_id: Id, rule: &Rule, exclude_event: Option<Id>) -> Result<ClashCheck, ApiError> {
    let dates = occurrences(rule);
    let (Some(first), Some(last_end)) = (dates.first(), dates.iter().map(|s| s.end).max()) else {
        return Err(ApiError::bad("No dates match that repeat pattern"));
    };
    let window = Span::new(first.start, last_end);
    let busy: Vec<Busy> = db::room_occupancy(conn, room_id, &window, exclude_event)?
        .into_iter()
        .map(|o| Busy { event_id: o.event_id, title: o.title, start: o.start, end: o.end })
        .collect();
    Ok(ClashCheck { clashes: clash::find_clashes(&dates, &busy), dates })
}

/// Title the public may see for an occupied slot: church events by name;
/// external bookings only if the organiser chose to show it.
fn public_title(event_id: Id, title: &str, booked: &HashMap<Id, (Id, bool)>) -> String {
    match booked.get(&event_id) {
        Some((_, false)) => "Booked".into(),
        _ => title.to_string(),
    }
}

fn public_check_json(conn: &Connection, check: &ClashCheck) -> Result<Value, ApiError> {
    let booked = db::booked_events(conn)?;
    let clashes: Vec<Value> = check
        .clashes
        .iter()
        .map(|c| {
            json!({
                "start": c.start,
                "end": c.end,
                "with": c.with.iter().map(|b| json!({
                    "title": public_title(b.event_id, &b.title, &booked),
                    "start": b.start,
                    "end": b.end,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({ "dates": check.dates.len(), "first": check.dates.first(), "clashes": clashes }))
}

/// Other pending requests wanting the same room at overlapping times.
fn competing_pending(conn: &Connection, booking_id: Id, room_id: Id, dates: &[Span]) -> rusqlite::Result<Vec<Value>> {
    let others = load_bookings(conn, "WHERE status = 'pending' AND room_id = ?1 AND id != ?2", &[&room_id, &booking_id])?;
    Ok(others
        .into_iter()
        .filter(|o| occurrences(&o.rule).iter().any(|s| dates.iter().any(|d| d.overlaps(s))))
        .map(|o| json!({ "id": o.id, "title": o.title, "organiser_name": o.organiser_name, "description": o.description }))
        .collect())
}

/// Approved equipment loans that this booking's equipment needs would leave
/// short. Only meaningful while pending: once approved, the booking is a
/// church event and the loan page itself warns.
fn equipment_impact(conn: &Connection, room_id: Id, dates: &[Span], needs: &[(Id, i64)]) -> Result<Vec<Value>, ApiError> {
    if needs.is_empty() || dates.is_empty() {
        return Ok(vec![]);
    }
    let settings = db::get_settings(conn)?;
    let now = now();
    let window = Span::new(dates[0].start, dates.iter().map(|d| d.end).max().unwrap_or(dates[0].end));
    let needs: BTreeMap<Id, i64> = needs.iter().copied().collect();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT l.id, l.pickup_at, l.return_at, l.status, l.event_name, l.organiser_name
         FROM loans l JOIN loan_allocations a ON a.loan_id = l.id
         WHERE l.status IN ('approved','collected') AND a.room_id = ?1",
    )?;
    let loans = stmt
        .query_map([room_id], |r| {
            Ok((
                r.get::<_, Id>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (loan_id, pickup, ret, status, event_name, organiser) in loans {
        let (Some(pickup), Some(ret)) = (parse_dt(&pickup), parse_dt(&ret)) else { continue };
        let occupancy = db::loan_occupancy(pickup, ret, &status, settings.buffer_hours, now);
        if !occupancy.overlaps(&window) {
            continue;
        }
        let mut stmt = conn.prepare("SELECT room_id, type_id, quantity FROM loan_allocations WHERE loan_id = ?1")?;
        let alloc = stmt
            .query_map([loan_id], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, Id>(1)?, r.get::<_, i64>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut world = db::load_world(conn, &occupancy, Some(loan_id), settings.buffer_hours, now)?;
        let before = engine::check_allocation(&world, occupancy, &alloc);
        for span in dates.iter().filter(|d| d.overlaps(&occupancy)) {
            world.occurrences.push(Occurrence { event_id: 0, group_id: None, room_id, span: *span, needs: needs.clone() });
        }
        for (r, t, over) in engine::check_allocation(&world, occupancy, &alloc) {
            let was = before.iter().find(|(br, bt, _)| *br == r && *bt == t).map_or(0, |b| b.2);
            if over > was {
                out.push(json!({
                    "loan_id": loan_id,
                    "loan_name": if event_name.is_empty() { organiser.clone() } else { event_name.clone() },
                    "room_id": r,
                    "type_id": t,
                    "short_by": over - was,
                }));
            }
        }
    }
    Ok(out)
}

// --------------------------------------------------------------- lifecycle

fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "declined")
            | ("pending", "cancelled")
            | ("approved", "cancelled")
            | ("approved", "pending")
            | ("declined", "pending")
            | ("cancelled", "pending")
    )
}

fn set_status(conn: &mut Connection, b: &BookingDto, to: &str, message: Option<&str>, by: &str) -> Result<(), ApiError> {
    if !transition_allowed(&b.status, to) {
        return Err(ApiError::conflict(format!("Can't change a {} booking to {to}", b.status), None));
    }
    let tx = conn.transaction()?;
    if let Some(event_id) = b.event_id {
        // Frees the room; its equipment needs and per-date changes go with it.
        tx.execute("DELETE FROM events WHERE id = ?1", [event_id])?;
    }
    let (by, decided_at) = if to == "pending" { ("", None) } else { (by, Some(fmt_dt(now()))) };
    tx.execute(
        "UPDATE bookings SET status = ?1, event_id = NULL, decided_by = ?2, decided_at = ?3, updated_at = ?4 WHERE id = ?5",
        params![to, by, decided_at, fmt_dt(now()), b.id],
    )?;
    if let Some(m) = message {
        tx.execute("UPDATE bookings SET message_to_organiser = ?1 WHERE id = ?2", params![clean(m, "Message")?, b.id])?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Deserialize)]
struct ApproveIn {
    /// Link the booking's event to an existing regular group…
    group_id: Option<Id>,
    /// …or create one from the organiser's details (reused if the name exists).
    #[serde(default)]
    create_group: bool,
    /// Approve even though some dates overlap something already in the room.
    #[serde(default)]
    force: bool,
    /// Approve, but cancel the dates that clash.
    #[serde(default)]
    skip_clashing: bool,
    message: Option<String>,
}

async fn approve(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<ApproveIn>) -> ApiResult<Value> {
    staff.require(Role::Bookings)?;
    let mut conn = st.conn();
    let bk = get_booking(&conn, id)?;
    if bk.status != "pending" {
        return Err(ApiError::conflict("Only pending bookings can be approved", None));
    }
    let check = check_clashes(&conn, bk.room_id, &bk.rule, None)?;
    if !check.clashes.is_empty() && !b.force && !b.skip_clashing {
        return Err(ApiError::conflict(
            format!("{} of {} dates clash with something already in the room", check.clashes.len(), check.dates.len()),
            Some(json!({ "clashes": check.clashes })),
        ));
    }
    if b.skip_clashing && !b.force && check.clashes.len() == check.dates.len() {
        return Err(ApiError::conflict("Every date clashes, so there is nothing left to approve", None));
    }

    let tx = conn.transaction()?;
    let group_id = if b.create_group {
        let name = if bk.organisation.is_empty() { bk.organiser_name.clone() } else { bk.organisation.clone() };
        match tx.query_row("SELECT id FROM groups WHERE name = ?1", [&name], |r| r.get::<_, Id>(0)).optional()? {
            Some(g) => Some(g),
            None => {
                tx.execute(
                    "INSERT INTO groups(name, contact_name, contact_phone, contact_email, notes) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![name, bk.organiser_name, bk.phone, bk.email, format!("Created from room booking #{id}")],
                )?;
                Some(tx.last_insert_rowid())
            }
        }
    } else {
        b.group_id
    };
    let (kind, interval, nth) = recurrence_to_db(bk.rule.recurrence);
    tx.execute(
        "INSERT INTO events(title, room_id, group_id, start_date, start_time, duration_minutes, recurrence,
         interval_weeks, nth, until, notes) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            bk.title,
            bk.room_id,
            group_id,
            bk.rule.start_date.format("%Y-%m-%d").to_string(),
            bk.rule.start_time.format("%H:%M").to_string(),
            bk.rule.duration_minutes,
            kind,
            interval,
            nth,
            bk.rule.until.map(|u| u.format("%Y-%m-%d").to_string()),
            format!("Room booking #{id}"),
        ],
    )?;
    let event_id = tx.last_insert_rowid();
    for n in &bk.needs {
        tx.execute(
            "INSERT INTO event_needs(event_id, type_id, quantity) VALUES (?1, ?2, ?3)",
            params![event_id, n.type_id, n.quantity],
        )?;
    }
    let skipped = if b.skip_clashing && !b.force { check.clashes.len() } else { 0 };
    for c in check.clashes.iter().take(skipped) {
        tx.execute(
            "INSERT INTO occurrence_overrides(event_id, occurrence_start, type_id, quantity, note, created_at)
             VALUES (?1, ?2, NULL, 0, 'Room already booked', ?3)",
            params![event_id, fmt_dt(c.start), fmt_dt(now())],
        )?;
    }
    tx.execute(
        "UPDATE bookings SET status = 'approved', event_id = ?1, decided_by = ?2, decided_at = ?3, updated_at = ?3 WHERE id = ?4",
        params![event_id, staff.display_name, fmt_dt(now()), id],
    )?;
    if let Some(m) = &b.message {
        tx.execute("UPDATE bookings SET message_to_organiser = ?1 WHERE id = ?2", params![clean(m, "Message")?, id])?;
    }
    tx.commit()?;
    Ok(Json(json!({ "ok": true, "event_id": event_id, "skipped_dates": skipped })))
}

#[derive(Deserialize)]
struct StatusIn {
    status: String,
    message: Option<String>,
}

async fn change_status(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<StatusIn>) -> ApiResult<Value> {
    staff.require(Role::Bookings)?;
    let mut conn = st.conn();
    let bk = get_booking(&conn, id)?;
    set_status(&mut conn, &bk, &b.status, b.message.as_deref(), &staff.display_name)?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------- staff API

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
}

async fn list(_: Staff, State(st): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<Vec<BookingDto>> {
    let conn = st.conn();
    let today = now().date().format("%Y-%m-%d").to_string();
    let rows = match q.status.as_deref().filter(|s| !s.is_empty() && *s != "all") {
        // Waiting for a decision, or approved and not over yet.
        Some("open") => load_bookings(
            &conn,
            "WHERE status = 'pending' OR (status = 'approved' AND COALESCE(until, start_date) >= ?1)",
            &[&today],
        )?,
        Some(s) => load_bookings(&conn, "WHERE status = ?1", &[&s])?,
        None => load_bookings(&conn, "", &[])?,
    };
    Ok(Json(rows))
}

async fn create(staff: Staff, State(st): State<AppState>, Json(b): Json<BookingIn>) -> ApiResult<Value> {
    staff.require(Role::Bookings)?;
    let mut conn = st.conn();
    let slot = validate_slot(&conn, &b, false)?;
    let contact = validate_contact(&b, false)?;
    let tx = conn.transaction()?;
    let (id, token) = write_booking(&tx, None, &slot, &contact, &b)?;
    tx.commit()?;
    Ok(Json(json!({ "id": id, "token": token })))
}

async fn update(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<BookingIn>) -> ApiResult<Value> {
    staff.require(Role::Bookings)?;
    let mut conn = st.conn();
    if get_booking(&conn, id)?.status != "pending" {
        return Err(ApiError::conflict("Only pending bookings can be edited. Move it back to pending first.", None));
    }
    let slot = validate_slot(&conn, &b, false)?;
    let contact = validate_contact(&b, false)?;
    let tx = conn.transaction()?;
    write_booking(&tx, Some(id), &slot, &contact, &b)?;
    tx.commit()?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct NotesIn {
    admin_notes: Option<String>,
    message_to_organiser: Option<String>,
}

async fn patch_notes(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<NotesIn>) -> ApiResult<Value> {
    staff.require(Role::Bookings)?;
    let conn = st.conn();
    get_booking(&conn, id)?;
    let ts = fmt_dt(now());
    if let Some(n) = &b.admin_notes {
        conn.execute("UPDATE bookings SET admin_notes = ?1, updated_at = ?2 WHERE id = ?3", params![clean(n, "Notes")?, ts, id])?;
    }
    if let Some(m) = &b.message_to_organiser {
        conn.execute(
            "UPDATE bookings SET message_to_organiser = ?1, updated_at = ?2 WHERE id = ?3",
            params![clean(m, "Message")?, ts, id],
        )?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    staff.require(Role::Bookings)?;
    let conn = st.conn();
    if get_booking(&conn, id)?.status == "approved" {
        return Err(ApiError::conflict("Cancel the booking before deleting it", None));
    }
    conn.execute("DELETE FROM bookings WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

async fn detail(_: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    let conn = st.conn();
    let bk = get_booking(&conn, id)?;
    let check = check_clashes(&conn, bk.room_id, &bk.rule, bk.event_id)?;
    let pending = bk.status == "pending";
    let competing = if pending { competing_pending(&conn, id, bk.room_id, &check.dates)? } else { vec![] };
    let needs: Vec<(Id, i64)> = bk.needs.iter().map(|n| (n.type_id, n.quantity)).collect();
    let impact = if pending { equipment_impact(&conn, bk.room_id, &check.dates, &needs)? } else { vec![] };
    // Dates staff cancelled on the live event (e.g. skipped clashes, holidays).
    let cancelled_dates: Vec<NaiveDateTime> = match bk.event_id {
        Some(e) => db::load_overrides(&conn)?
            .into_iter()
            .filter(|o| o.event_id == e && o.type_id.is_none())
            .map(|o| o.occurrence_start)
            .collect(),
        None => vec![],
    };
    let booked: HashMap<Id, Id> = db::booked_events(&conn)?.into_iter().map(|(e, (b, _))| (e, b)).collect();
    Ok(Json(json!({
        "booking": bk,
        "dates": check.dates,
        "clashes": check.clashes,
        "competing_pending": competing,
        "equipment_impact": impact,
        "cancelled_dates": cancelled_dates,
        "booked_events": booked,
        "refs": load_refs(&conn)?,
    })))
}

// ------------------------------------------------------------- public API

#[derive(Serialize)]
struct PublicEquipment {
    type_id: Id,
    name: String,
    category: String,
    /// Units in this room at or under this node, so any level of the tree
    /// can be picked for a booking.
    quantity: i64,
    parent_id: Option<Id>,
    is_leaf: bool,
    depth: usize,
}

async fn public_rooms(State(st): State<AppState>) -> ApiResult<Value> {
    let conn = st.conn();
    let tree = db::load_tree(&conn)?;
    let children = engine::children_map(&tree.parents);
    let mut leaf_stock: HashMap<Id, HashMap<Id, i64>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT room_id, type_id, quantity FROM stock WHERE quantity > 0")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, Id>(1)?, r.get::<_, i64>(2)?)))? {
            let (room, t, q) = row?;
            leaf_stock.entry(room).or_default().insert(t, q);
        }
    }
    // Tree order, parents before children, siblings alphabetical.
    fn walk(
        id: Id,
        children: &HashMap<Id, Vec<Id>>,
        names: &HashMap<Id, String>,
        out: &mut Vec<Id>,
    ) {
        out.push(id);
        let mut kids = children.get(&id).cloned().unwrap_or_default();
        kids.sort_by(|a, b| names.get(a).cmp(&names.get(b)).then(a.cmp(b)));
        for k in kids {
            walk(k, children, names, out);
        }
    }
    let mut roots: Vec<Id> = tree.parents.iter().filter(|(_, p)| p.is_none()).map(|(id, _)| *id).collect();
    roots.sort_by(|a, b| tree.names.get(a).cmp(&tree.names.get(b)).then(a.cmp(b)));
    let mut order = Vec::new();
    for r in roots {
        walk(r, &children, &tree.names, &mut order);
    }
    let mut by_depth = order.clone();
    by_depth.sort_by_key(|a| std::cmp::Reverse(engine::depth_of(&tree.parents, *a)));

    let mut equipment: HashMap<Id, Vec<PublicEquipment>> = HashMap::new();
    for (room, leaves) in leaf_stock {
        let mut sub: HashMap<Id, i64> = tree.parents.keys().map(|&id| (id, leaves.get(&id).copied().unwrap_or(0))).collect();
        for &id in &by_depth {
            if let Some(p) = tree.parents.get(&id).copied().flatten() {
                let v = sub[&id];
                *sub.entry(p).or_insert(0) += v;
            }
        }
        let list: Vec<PublicEquipment> = order
            .iter()
            .copied()
            .filter(|id| sub.get(id).copied().unwrap_or(0) > 0)
            .map(|id| PublicEquipment {
                type_id: id,
                name: tree.names.get(&id).cloned().unwrap_or_default(),
                category: tree.category_of(id),
                quantity: sub[&id],
                parent_id: tree.parents.get(&id).copied().flatten(),
                is_leaf: tree.is_leaf(id),
                depth: engine::depth_of(&tree.parents, id),
            })
            .collect();
        equipment.insert(room, list);
    }
    let mut stmt =
        conn.prepare("SELECT id, name, capacity, public_notes FROM rooms WHERE bookable = 1 ORDER BY name")?;
    let rooms: Vec<Value> = stmt
        .query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, String>(3)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(id, name, capacity, notes)| {
            json!({ "id": id, "name": name, "capacity": capacity, "public_notes": notes,
                    "equipment": equipment.remove(&id).unwrap_or_default() })
        })
        .collect();
    Ok(Json(json!(rooms)))
}

#[derive(Deserialize)]
struct AvailabilityQuery {
    from: Option<String>,
    days: Option<i64>,
}

/// Busy times per bookable room: the public replacement for the old spreadsheet.
async fn public_availability(State(st): State<AppState>, Query(q): Query<AvailabilityQuery>) -> ApiResult<Value> {
    let from = q
        .from
        .as_deref()
        .and_then(|f| NaiveDate::parse_from_str(f, "%Y-%m-%d").ok())
        .unwrap_or_else(|| now().date());
    let days = q.days.unwrap_or(7).clamp(1, 42);
    let start = from.and_time(NaiveTime::MIN);
    let window = Span::new(start, start + Duration::days(days));
    let conn = st.conn();
    let bookable: HashMap<Id, ()> = conn
        .prepare("SELECT id FROM rooms WHERE bookable = 1")?
        .query_map([], |r| r.get::<_, Id>(0))?
        .map(|r| r.map(|id| (id, ())))
        .collect::<Result<_, _>>()?;
    let booked = db::booked_events(&conn)?;
    let busy: Vec<Value> = db::expand_occurrences(&conn, &window)?
        .into_iter()
        .filter(|o| !o.cancelled && bookable.contains_key(&o.room_id))
        .map(|o| json!({ "room_id": o.room_id, "start": o.start, "end": o.end,
                         "title": public_title(o.event_id, &o.title, &booked) }))
        .collect();
    Ok(Json(json!({ "from": from, "days": days, "busy": busy })))
}

async fn public_check(State(st): State<AppState>, Json(b): Json<BookingIn>) -> ApiResult<Value> {
    let conn = st.conn();
    let slot = validate_slot(&conn, &b, true)?;
    let check = check_clashes(&conn, slot.room_id, &slot.rule, None)?;
    Ok(Json(public_check_json(&conn, &check)?))
}

async fn public_create(State(st): State<AppState>, Json(b): Json<BookingIn>) -> ApiResult<Value> {
    let mut conn = st.conn();
    let slot = validate_slot(&conn, &b, true)?;
    let contact = validate_contact(&b, true)?;
    let check = check_clashes(&conn, slot.room_id, &slot.rule, None)?;
    let tx = conn.transaction()?;
    let (_, token) = write_booking(&tx, None, &slot, &contact, &b)?;
    tx.commit()?;
    Ok(Json(json!({ "token": token, "check": public_check_json(&conn, &check)? })))
}

async fn public_status(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult<Value> {
    let conn = st.conn();
    let bk = get_booking_by_token(&conn, &token)?;
    let refs = load_refs(&conn)?;
    // Once approved, staff may have adjusted the live event; show that.
    let live = match bk.event_id {
        Some(e) => db::load_events(&conn)?.into_iter().find(|ev| ev.id == e),
        None => None,
    };
    let (rule, room_id) = live.as_ref().map_or((&bk.rule, bk.room_id), |e| (&e.rule, e.room_id));
    let cancelled_dates: Vec<NaiveDateTime> = match bk.event_id {
        Some(e) => db::load_overrides(&conn)?
            .into_iter()
            .filter(|o| o.event_id == e && o.type_id.is_none())
            .map(|o| o.occurrence_start)
            .collect(),
        None => vec![],
    };
    let first = rule.start_date.and_time(rule.start_time);
    Ok(Json(json!({
        "church_name": db::get_settings(&conn)?.church_name,
        "status": bk.status,
        "title": bk.title,
        "organiser_name": bk.organiser_name,
        "room": refs.rooms.get(&room_id).cloned().unwrap_or_default(),
        "description": rule.describe(),
        "start_at": first,
        "end_at": first + Duration::minutes(rule.duration_minutes),
        "repeats": rule.recurrence != Recurrence::None,
        "cancelled_dates": cancelled_dates,
        "needs": bk.needs.iter().map(|n| json!({
            "name": refs.types.get(&n.type_id).map(|t| t.name.clone()).unwrap_or_default(),
            "quantity": n.quantity,
        })).collect::<Vec<_>>(),
        "av_support": bk.av_support,
        "message": bk.message_to_organiser,
    })))
}

async fn public_cancel(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult<Value> {
    let mut conn = st.conn();
    let bk = get_booking_by_token(&conn, &token)?;
    if !matches!(bk.status.as_str(), "pending" | "approved") {
        return Err(ApiError::conflict("This booking can no longer be cancelled online", None));
    }
    set_status(&mut conn, &bk, "cancelled", None, "Organiser")?;
    Ok(Json(json!({ "ok": true })))
}

// --------------------------------------------------- church event support

/// Clashes for a church event over the next year (or until it ends), used to
/// warn staff when they create or edit an event directly.
pub fn event_clashes(conn: &Connection, event_id: Id) -> Result<Vec<Clash>, ApiError> {
    let Some(ev) = db::load_events(conn)?.into_iter().find(|e| e.id == event_id) else {
        return Ok(vec![]);
    };
    let from = ev.rule.start_date.max(now().date()).and_time(NaiveTime::MIN);
    let horizon = from + Duration::days(MAX_REPEAT_DAYS);
    let to = match ev.rule.until {
        Some(u) => horizon.min(u.and_time(NaiveTime::MIN) + Duration::days(1)),
        None => horizon,
    };
    if to <= from {
        return Ok(vec![]);
    }
    let window = Span::new(from, to + Duration::minutes(ev.rule.duration_minutes));
    let occupancy = db::room_occupancy(conn, ev.room_id, &window, None)?;
    let (own, others): (Vec<_>, Vec<_>) = occupancy.into_iter().partition(|o| o.event_id == event_id);
    let dates: Vec<Span> = own.iter().map(|o| Span::new(o.start, o.end)).collect();
    let busy: Vec<Busy> =
        others.into_iter().map(|o| Busy { event_id: o.event_id, title: o.title, start: o.start, end: o.end }).collect();
    Ok(clash::find_clashes(&dates, &busy))
}

/// The booking an event came from, if any.
pub fn booking_for_event(conn: &Connection, event_id: Id) -> rusqlite::Result<Option<Id>> {
    conn.query_row("SELECT id FROM bookings WHERE event_id = ?1", [event_id], |r| r.get(0)).optional()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/bookings", get(list).post(create))
        .route("/api/bookings/{id}", get(detail).put(update).patch(patch_notes).delete(delete))
        .route("/api/bookings/{id}/approve", post(approve))
        .route("/api/bookings/{id}/status", post(change_status))
        .route("/api/public/rooms", get(public_rooms))
        .route("/api/public/availability", get(public_availability))
        .route("/api/public/bookings/check", post(public_check))
        .route("/api/public/bookings", post(public_create))
        .route("/api/public/bookings/{token}", get(public_status))
        .route("/api/public/bookings/{token}/cancel", post(public_cancel))
}
