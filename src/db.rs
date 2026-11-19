//! SQLite schema and the queries that build an engine snapshot.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::engine::{Holding, Id, Occurrence, World};
use crate::recurrence::{Recurrence, Rule, Span};

pub const DT_FMT: &str = "%Y-%m-%dT%H:%M:%S";

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rooms (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE,
    notes TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS equipment_types (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL UNIQUE,
    category TEXT NOT NULL DEFAULT '',
    notes    TEXT NOT NULL DEFAULT '',
    lendable INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS stock (
    room_id  INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    type_id  INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity INTEGER NOT NULL CHECK (quantity >= 0),
    PRIMARY KEY (room_id, type_id)
);

CREATE TABLE IF NOT EXISTS groups (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    contact_name  TEXT NOT NULL DEFAULT '',
    contact_phone TEXT NOT NULL DEFAULT '',
    contact_email TEXT NOT NULL DEFAULT '',
    notes         TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS events (
    id               INTEGER PRIMARY KEY,
    title            TEXT NOT NULL,
    room_id          INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    group_id         INTEGER REFERENCES groups(id) ON DELETE SET NULL,
    start_date       TEXT NOT NULL,
    start_time       TEXT NOT NULL,
    duration_minutes INTEGER NOT NULL CHECK (duration_minutes > 0),
    recurrence       TEXT NOT NULL CHECK (recurrence IN ('none','weekly','monthly')),
    interval_weeks   INTEGER NOT NULL DEFAULT 1,
    nth              INTEGER NOT NULL DEFAULT 1,
    until            TEXT,
    notes            TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS event_needs (
    event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    type_id  INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity INTEGER NOT NULL CHECK (quantity >= 0),
    PRIMARY KEY (event_id, type_id)
);

CREATE TABLE IF NOT EXISTS loans (
    id                   INTEGER PRIMARY KEY,
    token                TEXT NOT NULL UNIQUE,
    status               TEXT NOT NULL,
    organiser_name       TEXT NOT NULL,
    organisation         TEXT NOT NULL DEFAULT '',
    email                TEXT NOT NULL DEFAULT '',
    phone                TEXT NOT NULL DEFAULT '',
    event_name           TEXT NOT NULL DEFAULT '',
    event_location       TEXT NOT NULL DEFAULT '',
    pickup_at            TEXT NOT NULL,
    return_at            TEXT NOT NULL,
    notes                TEXT NOT NULL DEFAULT '',
    admin_notes          TEXT NOT NULL DEFAULT '',
    message_to_organiser TEXT NOT NULL DEFAULT '',
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS loan_items (
    loan_id  INTEGER NOT NULL REFERENCES loans(id) ON DELETE CASCADE,
    type_id  INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (loan_id, type_id)
);

CREATE TABLE IF NOT EXISTS loan_allocations (
    loan_id  INTEGER NOT NULL REFERENCES loans(id) ON DELETE CASCADE,
    room_id  INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    type_id  INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (loan_id, room_id, type_id)
);

-- Per-occurrence adjustments: a group agreed to manage with less on one date
-- (type_id set), or the occurrence is cancelled entirely (type_id NULL).
CREATE TABLE IF NOT EXISTS occurrence_overrides (
    id               INTEGER PRIMARY KEY,
    event_id         INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    occurrence_start TEXT NOT NULL,
    type_id          INTEGER REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity         INTEGER NOT NULL DEFAULT 0,
    note             TEXT NOT NULL DEFAULT '',
    loan_id          INTEGER REFERENCES loans(id) ON DELETE SET NULL,
    created_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_overrides_event ON occurrence_overrides(event_id, occurrence_start);

"#;

/// Schema changes after the original release, applied in order and tracked
/// with `PRAGMA user_version`. Append only; never edit a released entry.
const MIGRATIONS: &[&str] = &[
    // 1: individual staff accounts with roles replace the shared password.
    r#"
    DROP TABLE IF EXISTS sessions;
    CREATE TABLE users (
        id            INTEGER PRIMARY KEY,
        username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
        display_name  TEXT NOT NULL,
        email         TEXT NOT NULL DEFAULT '',
        password_hash TEXT NOT NULL,
        roles         TEXT NOT NULL,
        active        INTEGER NOT NULL DEFAULT 1,
        created_at    TEXT NOT NULL
    );
    CREATE TABLE sessions (
        token      TEXT PRIMARY KEY,
        user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        expires_at TEXT NOT NULL
    );
    ALTER TABLE loans ADD COLUMN decided_by TEXT NOT NULL DEFAULT '';
    "#,
    // 2: room booking. An approved booking is materialised as an `events` row
    // (bookings.event_id), so it occupies the room and protects its equipment
    // exactly like any other church event.
    r#"
    ALTER TABLE rooms ADD COLUMN bookable INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE rooms ADD COLUMN capacity INTEGER;
    ALTER TABLE rooms ADD COLUMN public_notes TEXT NOT NULL DEFAULT '';
    CREATE TABLE bookings (
        id                   INTEGER PRIMARY KEY,
        token                TEXT NOT NULL UNIQUE,
        status               TEXT NOT NULL,
        room_id              INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
        title                TEXT NOT NULL,
        organiser_name       TEXT NOT NULL,
        organisation         TEXT NOT NULL DEFAULT '',
        email                TEXT NOT NULL DEFAULT '',
        phone                TEXT NOT NULL DEFAULT '',
        attendees            INTEGER,
        start_date           TEXT NOT NULL,
        start_time           TEXT NOT NULL,
        duration_minutes     INTEGER NOT NULL CHECK (duration_minutes > 0),
        recurrence           TEXT NOT NULL CHECK (recurrence IN ('none','weekly','monthly')),
        interval_weeks       INTEGER NOT NULL DEFAULT 1,
        nth                  INTEGER NOT NULL DEFAULT 1,
        until                TEXT,
        av_support           INTEGER NOT NULL DEFAULT 0,
        public_title         INTEGER NOT NULL DEFAULT 0,
        notes                TEXT NOT NULL DEFAULT '',
        admin_notes          TEXT NOT NULL DEFAULT '',
        message_to_organiser TEXT NOT NULL DEFAULT '',
        event_id             INTEGER REFERENCES events(id) ON DELETE SET NULL,
        decided_by           TEXT NOT NULL DEFAULT '',
        decided_at           TEXT,
        created_at           TEXT NOT NULL,
        updated_at           TEXT NOT NULL
    );
    CREATE INDEX idx_bookings_room_status ON bookings(room_id, status);
    CREATE INDEX idx_bookings_event ON bookings(event_id);
    CREATE TABLE booking_needs (
        booking_id INTEGER NOT NULL REFERENCES bookings(id) ON DELETE CASCADE,
        type_id    INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
        quantity   INTEGER NOT NULL CHECK (quantity > 0),
        PRIMARY KEY (booking_id, type_id)
    );
    "#,
];

pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&mut conn)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in (1..).zip(MIGRATIONS.iter()).skip(usize::try_from(version).unwrap_or(0)) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i64::from(i))?;
        tx.commit()?;
    }
    if version < 3 {
        let tx = conn.transaction()?;
        migrate_to_3(&tx)?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
    }
    Ok(())
}

/// 3: equipment becomes a tree. `equipment_types` gains `parent_id`; a row
/// with children is an umbrella ("Wireless mic"), a leaf is a stocked model
/// ("Sennheiser EW 100"). Existing free-text categories become top-level
/// umbrellas so current data keeps working unchanged: every existing type
/// stays a leaf with the same id, stock and history.
fn migrate_to_3(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute("ALTER TABLE equipment_types ADD COLUMN parent_id INTEGER REFERENCES equipment_types(id)", [])?;
    tx.execute("CREATE INDEX idx_equipment_parent ON equipment_types(parent_id)", [])?;
    let mut stmt = tx.prepare("SELECT DISTINCT category FROM equipment_types WHERE category <> ''")?;
    let cats: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    drop(stmt);
    for cat in cats {
        // Reuse an existing unstocked row with the category's name as the
        // umbrella; otherwise create one (a stocked row must stay a leaf).
        let existing: Option<Id> =
            tx.query_row("SELECT id FROM equipment_types WHERE name = ?1", [&cat], |r| r.get(0)).optional()?;
        let root = match existing {
            Some(id) => {
                let stocked: i64 =
                    tx.query_row("SELECT COUNT(*) FROM stock WHERE type_id = ?1 AND quantity > 0", [id], |r| r.get(0))?;
                if stocked == 0 {
                    id
                } else {
                    let name = format!("{cat} (group)");
                    match tx.query_row("SELECT id FROM equipment_types WHERE name = ?1", [&name], |r| r.get(0)).optional()? {
                        Some(id) => id,
                        None => {
                            tx.execute("INSERT INTO equipment_types(name, category) VALUES (?1, '')", [&name])?;
                            tx.last_insert_rowid()
                        }
                    }
                }
            }
            None => {
                tx.execute("INSERT INTO equipment_types(name, category) VALUES (?1, '')", [&cat])?;
                tx.last_insert_rowid()
            }
        };
        tx.execute(
            "UPDATE equipment_types SET parent_id = ?1 WHERE category = ?2 AND id <> ?1",
            params![root, cat],
        )?;
    }
    Ok(())
}

pub fn fmt_dt(dt: NaiveDateTime) -> String {
    dt.format(DT_FMT).to_string()
}

/// Accepts `YYYY-MM-DDTHH:MM[:SS]` (as produced by `<input type=datetime-local>`).
pub fn parse_dt(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().replace(' ', "T");
    NaiveDateTime::parse_from_str(&s, DT_FMT)
        .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M"))
        .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.f"))
        .ok()
}

// ---------------------------------------------------------------- settings

#[derive(Clone, Debug, Serialize)]
pub struct Settings {
    pub church_name: String,
    pub buffer_hours: i64,
}

pub fn get_settings(conn: &Connection) -> rusqlite::Result<Settings> {
    let get = |k: &str| -> rusqlite::Result<Option<String>> {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [k], |r| r.get(0)).optional()
    };
    Ok(Settings {
        church_name: get("church_name")?.unwrap_or_else(|| "Our Church".into()),
        buffer_hours: get("buffer_hours")?.and_then(|v| v.parse().ok()).unwrap_or(12),
    })
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ events

#[derive(Clone, Debug)]
pub struct EventRow {
    pub id: Id,
    pub title: String,
    pub room_id: Id,
    pub group_id: Option<Id>,
    pub rule: Rule,
    pub notes: String,
    pub needs: BTreeMap<Id, i64>,
}

pub fn recurrence_from_db(kind: &str, interval: i64, nth: i64) -> Recurrence {
    match kind {
        "weekly" => Recurrence::Weekly { interval: interval.clamp(1, 52) as u32 },
        "monthly" => Recurrence::Monthly { nth: nth as i32 },
        _ => Recurrence::None,
    }
}

pub fn load_events(conn: &Connection) -> rusqlite::Result<Vec<EventRow>> {
    let mut needs: HashMap<Id, BTreeMap<Id, i64>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT event_id, type_id, quantity FROM event_needs")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, Id>(1)?, r.get::<_, i64>(2)?)))? {
        let (e, t, q) = row?;
        needs.entry(e).or_default().insert(t, q);
    }
    let mut stmt = conn.prepare(
        "SELECT id, title, room_id, group_id, start_date, start_time, duration_minutes,
                recurrence, interval_weeks, nth, until, notes
         FROM events ORDER BY title",
    )?;
    let rows = stmt.query_map([], |r| {
        let start_date: String = r.get(4)?;
        let start_time: String = r.get(5)?;
        let until: Option<String> = r.get(10)?;
        let kind: String = r.get(7)?;
        Ok(EventRow {
            id: r.get(0)?,
            title: r.get(1)?,
            room_id: r.get(2)?,
            group_id: r.get(3)?,
            rule: Rule {
                start_date: NaiveDate::parse_from_str(&start_date, "%Y-%m-%d").unwrap_or_default(),
                start_time: NaiveTime::parse_from_str(&start_time, "%H:%M").unwrap_or_default(),
                duration_minutes: r.get(6)?,
                recurrence: recurrence_from_db(&kind, r.get(8)?, r.get(9)?),
                until: until.and_then(|u| NaiveDate::parse_from_str(&u, "%Y-%m-%d").ok()),
            },
            notes: r.get(11)?,
            needs: BTreeMap::new(),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        let mut e = row?;
        e.needs = needs.remove(&e.id).unwrap_or_default();
        out.push(e);
    }
    Ok(out)
}

#[derive(Clone, Debug, Serialize)]
pub struct OverrideRow {
    pub id: Id,
    pub event_id: Id,
    pub occurrence_start: NaiveDateTime,
    pub type_id: Option<Id>,
    pub quantity: i64,
    pub note: String,
    pub loan_id: Option<Id>,
}

pub fn load_overrides(conn: &Connection) -> rusqlite::Result<Vec<OverrideRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_id, occurrence_start, type_id, quantity, note, loan_id FROM occurrence_overrides",
    )?;
    let rows = stmt.query_map([], |r| {
        let s: String = r.get(2)?;
        Ok(OverrideRow {
            id: r.get(0)?,
            event_id: r.get(1)?,
            occurrence_start: parse_dt(&s).unwrap_or_default(),
            type_id: r.get(3)?,
            quantity: r.get(4)?,
            note: r.get(5)?,
            loan_id: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// A concrete occurrence with display info and overrides applied.
#[derive(Clone, Debug, Serialize)]
pub struct OccurrenceView {
    pub event_id: Id,
    pub title: String,
    pub room_id: Id,
    pub group_id: Option<Id>,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub cancelled: bool,
    /// Effective needs after overrides.
    pub needs: BTreeMap<Id, i64>,
    /// Usual needs from the event definition.
    pub usual_needs: BTreeMap<Id, i64>,
    pub overrides: Vec<OverrideRow>,
}

pub fn expand_occurrences(conn: &Connection, window: &Span) -> rusqlite::Result<Vec<OccurrenceView>> {
    let events = load_events(conn)?;
    let mut overrides: HashMap<(Id, NaiveDateTime), Vec<OverrideRow>> = HashMap::new();
    for o in load_overrides(conn)? {
        overrides.entry((o.event_id, o.occurrence_start)).or_default().push(o);
    }
    let mut out = Vec::new();
    for e in &events {
        for span in e.rule.occurrences_in(window) {
            let ovs = overrides.remove(&(e.id, span.start)).unwrap_or_default();
            let mut needs = e.needs.clone();
            let mut cancelled = false;
            for o in &ovs {
                match o.type_id {
                    None => cancelled = true,
                    Some(t) => {
                        needs.insert(t, o.quantity);
                    }
                }
            }
            out.push(OccurrenceView {
                event_id: e.id,
                title: e.title.clone(),
                room_id: e.room_id,
                group_id: e.group_id,
                start: span.start,
                end: span.end,
                cancelled,
                needs,
                usual_needs: e.needs.clone(),
                overrides: ovs,
            });
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.title.cmp(&b.title)));
    Ok(out)
}

/// Non-cancelled occurrences in `room_id` overlapping `window`, optionally
/// ignoring one event (e.g. the booking's own).
pub fn room_occupancy(
    conn: &Connection,
    room_id: Id,
    window: &Span,
    exclude_event: Option<Id>,
) -> rusqlite::Result<Vec<OccurrenceView>> {
    Ok(expand_occurrences(conn, window)?
        .into_iter()
        .filter(|o| o.room_id == room_id && !o.cancelled && Some(o.event_id) != exclude_event)
        .collect())
}

/// event_id -> (booking_id, show the booking's title publicly) for events
/// that came from an approved room booking.
pub fn booked_events(conn: &Connection) -> rusqlite::Result<HashMap<Id, (Id, bool)>> {
    let mut stmt = conn.prepare("SELECT event_id, id, public_title FROM bookings WHERE event_id IS NOT NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, (r.get::<_, Id>(1)?, r.get::<_, bool>(2)?))))?;
    rows.collect()
}

pub fn recurrence_to_db(r: Recurrence) -> (&'static str, i64, i64) {
    match r {
        Recurrence::None => ("none", 1, 1),
        Recurrence::Weekly { interval } => ("weekly", i64::from(interval), 1),
        Recurrence::Monthly { nth } => ("monthly", 1, i64::from(nth)),
    }
}

// ------------------------------------------------------------------- loans

pub const COMMITTED: [&str; 2] = ["approved", "collected"];

/// The period a loan's gear is unavailable at church.
pub fn loan_occupancy(pickup: NaiveDateTime, ret: NaiveDateTime, status: &str, buffer_hours: i64, now: NaiveDateTime) -> Span {
    // Overdue gear is still out: extend until it actually comes back.
    let effective_return = if status == "collected" { ret.max(now) } else { ret };
    Span::new(pickup, effective_return + Duration::hours(buffer_hours.max(0)))
}

/// The equipment tree as stored: every node's parent, which leaves may be
/// lent out, and display names. Used for request validation in the API.
#[derive(Clone, Debug, Default)]
pub struct EquipmentTree {
    pub parents: HashMap<Id, Option<Id>>,
    pub lendable: HashSet<Id>,
    pub names: HashMap<Id, String>,
}

impl EquipmentTree {
    pub fn root_id(&self, mut id: Id) -> Id {
        let mut guard = 0;
        while let Some(p) = self.parents.get(&id).copied().flatten() {
            id = p;
            guard += 1;
            if guard > self.parents.len() {
                break;
            }
        }
        id
    }

    /// Top-level umbrella name for grouping in pickers ("" for roots).
    pub fn category_of(&self, id: Id) -> String {
        let root = self.root_id(id);
        if root == id {
            String::new()
        } else {
            self.names.get(&root).cloned().unwrap_or_default()
        }
    }

    pub fn is_leaf(&self, id: Id) -> bool {
        !self.parents.values().any(|p| *p == Some(id))
    }
}

pub fn load_tree(conn: &Connection) -> rusqlite::Result<EquipmentTree> {
    let mut tree = EquipmentTree::default();
    let mut stmt = conn.prepare("SELECT id, name, parent_id, lendable FROM equipment_types")?;
    for row in stmt.query_map([], |r| {
        Ok((r.get::<_, Id>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<Id>>(2)?, r.get::<_, bool>(3)?))
    })? {
        let (id, name, parent, lendable) = row?;
        tree.parents.insert(id, parent);
        tree.names.insert(id, name);
        if lendable {
            tree.lendable.insert(id);
        }
    }
    Ok(tree)
}

/// Build an engine snapshot covering `window`.
pub fn load_world(
    conn: &Connection,
    window: &Span,
    exclude_loan: Option<Id>,
    buffer_hours: i64,
    now: NaiveDateTime,
) -> rusqlite::Result<World> {
    let mut world = World::default();
    let tree = load_tree(conn)?;
    world.parents = tree.parents;
    world.lendable = tree.lendable;
    let mut stmt = conn.prepare(
        "SELECT s.room_id, s.type_id, s.quantity FROM stock s
         JOIN equipment_types t ON t.id = s.type_id WHERE s.quantity > 0",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, Id>(1)?, r.get::<_, i64>(2)?)))? {
        let (room, ty, q) = row?;
        world.stock.insert((room, ty), q);
    }

    for o in expand_occurrences(conn, window)? {
        if o.cancelled {
            continue;
        }
        world.occurrences.push(Occurrence {
            event_id: o.event_id,
            group_id: o.group_id,
            room_id: o.room_id,
            span: Span::new(o.start, o.end),
            needs: o.needs,
        });
    }

    let mut stmt = conn.prepare(
        "SELECT l.id, l.pickup_at, l.return_at, l.status, a.room_id, a.type_id, a.quantity
         FROM loans l JOIN loan_allocations a ON a.loan_id = l.id
         WHERE l.status IN ('approved', 'collected')",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Id>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Id>(4)?,
            r.get::<_, Id>(5)?,
            r.get::<_, i64>(6)?,
        ))
    })?;
    for row in rows {
        let (loan_id, p, rt, status, room_id, type_id, qty) = row?;
        if Some(loan_id) == exclude_loan {
            continue;
        }
        let (Some(p), Some(rt)) = (parse_dt(&p), parse_dt(&rt)) else { continue };
        let span = loan_occupancy(p, rt, &status, buffer_hours, now);
        if span.overlaps(window) {
            world.holdings.push(Holding { loan_id, span, room_id, type_id, qty });
        }
    }
    Ok(world)
}
