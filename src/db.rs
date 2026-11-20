//! SQLite schema and the queries that build an engine snapshot.

use std::collections::{BTreeMap, HashMap};

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::engine::{self, Catalog, Holding, Id, Occurrence, Want, World};
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
    // 3: equipment kinds (see docs/equipment-kinds.md). Kinds form a tree;
    // models (equipment_types) hang under a kind. Request lines and needs
    // name either "any of a kind" or "exactly this model". Existing
    // categories become top-level, not-requestable kinds and existing lines
    // become exact-model lines, so nothing behaves differently until the AV
    // team builds out the tree. equipment_types is altered, never rebuilt:
    // dropping it would cascade-delete stock, needs and allocations.
    r#"
    CREATE TABLE equipment_kinds (
        id          INTEGER PRIMARY KEY,
        name        TEXT NOT NULL UNIQUE,
        parent_id   INTEGER REFERENCES equipment_kinds(id) ON DELETE RESTRICT,
        requestable INTEGER NOT NULL DEFAULT 1,
        sort_order  INTEGER NOT NULL DEFAULT 0,
        notes       TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX idx_kinds_parent ON equipment_kinds(parent_id);
    INSERT INTO equipment_kinds(name, requestable)
        SELECT DISTINCT TRIM(category), 0 FROM equipment_types WHERE TRIM(category) <> '';
    ALTER TABLE equipment_types ADD COLUMN kind_id INTEGER REFERENCES equipment_kinds(id) ON DELETE RESTRICT;
    UPDATE equipment_types SET kind_id = (SELECT k.id FROM equipment_kinds k WHERE k.name = TRIM(equipment_types.category));
    ALTER TABLE equipment_types DROP COLUMN category;
    CREATE INDEX idx_types_kind ON equipment_types(kind_id);

    CREATE TABLE event_needs_v3 (
        id       INTEGER PRIMARY KEY,
        event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
        kind_id  INTEGER REFERENCES equipment_kinds(id) ON DELETE CASCADE,
        type_id  INTEGER REFERENCES equipment_types(id) ON DELETE CASCADE,
        quantity INTEGER NOT NULL CHECK (quantity >= 0),
        CHECK ((kind_id IS NULL) <> (type_id IS NULL))
    );
    INSERT INTO event_needs_v3(event_id, type_id, quantity) SELECT event_id, type_id, quantity FROM event_needs;
    DROP TABLE event_needs;
    ALTER TABLE event_needs_v3 RENAME TO event_needs;
    CREATE UNIQUE INDEX ux_event_needs_kind ON event_needs(event_id, kind_id) WHERE kind_id IS NOT NULL;
    CREATE UNIQUE INDEX ux_event_needs_model ON event_needs(event_id, type_id) WHERE type_id IS NOT NULL;

    CREATE TABLE booking_needs_v3 (
        id         INTEGER PRIMARY KEY,
        booking_id INTEGER NOT NULL REFERENCES bookings(id) ON DELETE CASCADE,
        kind_id    INTEGER REFERENCES equipment_kinds(id) ON DELETE CASCADE,
        type_id    INTEGER REFERENCES equipment_types(id) ON DELETE CASCADE,
        quantity   INTEGER NOT NULL CHECK (quantity > 0),
        CHECK ((kind_id IS NULL) <> (type_id IS NULL))
    );
    INSERT INTO booking_needs_v3(booking_id, type_id, quantity) SELECT booking_id, type_id, quantity FROM booking_needs;
    DROP TABLE booking_needs;
    ALTER TABLE booking_needs_v3 RENAME TO booking_needs;
    CREATE UNIQUE INDEX ux_booking_needs_kind ON booking_needs(booking_id, kind_id) WHERE kind_id IS NOT NULL;
    CREATE UNIQUE INDEX ux_booking_needs_model ON booking_needs(booking_id, type_id) WHERE type_id IS NOT NULL;

    CREATE TABLE loan_items_v3 (
        id       INTEGER PRIMARY KEY,
        loan_id  INTEGER NOT NULL REFERENCES loans(id) ON DELETE CASCADE,
        kind_id  INTEGER REFERENCES equipment_kinds(id) ON DELETE CASCADE,
        type_id  INTEGER REFERENCES equipment_types(id) ON DELETE CASCADE,
        quantity INTEGER NOT NULL CHECK (quantity > 0),
        CHECK ((kind_id IS NULL) <> (type_id IS NULL))
    );
    INSERT INTO loan_items_v3(loan_id, type_id, quantity) SELECT loan_id, type_id, quantity FROM loan_items;
    DROP TABLE loan_items;
    ALTER TABLE loan_items_v3 RENAME TO loan_items;
    CREATE UNIQUE INDEX ux_loan_items_kind ON loan_items(loan_id, kind_id) WHERE kind_id IS NOT NULL;
    CREATE UNIQUE INDEX ux_loan_items_model ON loan_items(loan_id, type_id) WHERE type_id IS NOT NULL;

    ALTER TABLE occurrence_overrides ADD COLUMN kind_id INTEGER REFERENCES equipment_kinds(id) ON DELETE CASCADE;
    "#,
];

/// Turns a `(kind_id, type_id)` pair read from a line table into a [`Want`].
pub fn want_from_db(kind_id: Option<Id>, type_id: Option<Id>) -> Option<Want> {
    match (kind_id, type_id) {
        (Some(k), None) => Some(Want::Kind(k)),
        (None, Some(t)) => Some(Want::Model(t)),
        _ => None,
    }
}

/// Reads the request lines of one parent row from a line table
/// (`loan_items`, `booking_needs` or `event_needs`).
pub fn load_lines(conn: &Connection, table: &str, parent_col: &str, parent: Id) -> rusqlite::Result<Vec<engine::Line>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT kind_id, type_id, quantity FROM {table} WHERE {parent_col} = ?1 ORDER BY kind_id IS NULL, kind_id, type_id"
    ))?;
    let rows = stmt.query_map([parent], |r| Ok((r.get::<_, Option<Id>>(0)?, r.get::<_, Option<Id>>(1)?, r.get::<_, i64>(2)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (k, t, quantity) = row?;
        if let Some(want) = want_from_db(k, t) {
            out.push(engine::Line { want, quantity });
        }
    }
    Ok(out)
}

/// Replaces the request lines of one parent row.
pub fn save_lines(conn: &Connection, table: &str, parent_col: &str, parent: Id, lines: &[(Want, i64)]) -> rusqlite::Result<()> {
    conn.execute(&format!("DELETE FROM {table} WHERE {parent_col} = ?1"), [parent])?;
    let sql = format!("INSERT INTO {table}({parent_col}, kind_id, type_id, quantity) VALUES (?1, ?2, ?3, ?4)");
    for (w, q) in lines {
        conn.execute(&sql, params![parent, w.kind_id(), w.type_id(), q])?;
    }
    Ok(())
}

/// The shape of the equipment tree, for the engine.
pub fn load_catalog(conn: &Connection) -> rusqlite::Result<Catalog> {
    let mut cat = Catalog::default();
    let mut stmt = conn.prepare("SELECT id, parent_id FROM equipment_kinds")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, Option<Id>>(1)?)))? {
        let (id, parent) = row?;
        cat.kind_parent.insert(id, parent);
    }
    let mut stmt = conn.prepare("SELECT id, kind_id, lendable FROM equipment_types")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, Option<Id>>(1)?, r.get::<_, bool>(2)?)))? {
        let (id, kind, lendable) = row?;
        cat.model_kind.insert(id, kind);
        if lendable {
            cat.lendable.insert(id);
        }
    }
    Ok(cat)
}

pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    backup_before_upgrade(&conn, path)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Some upgrades can't be undone (migration 3 drops a column), so before an
/// existing database is migrated it is copied to `<path>.v<version>-backup-<time>`.
/// If the copy fails, nothing is migrated.
fn backup_before_upgrade(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    if path.is_empty() || path == ":memory:" {
        return Ok(());
    }
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let has_data: bool = conn.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table')", [], |r| r.get(0))?;
    if !has_data || usize::try_from(version).unwrap_or(0) >= MIGRATIONS.len() {
        return Ok(());
    }
    let dest = format!("{path}.v{version}-backup-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    conn.execute("VACUUM INTO ?1", [&dest])?;
    tracing::info!("Upgrading {path} from schema version {version}; a copy of the old database is in {dest}");
    Ok(())
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    migrate_to(conn, MIGRATIONS.len())
}

/// Opens a database as an older release would have left it (tests only).
#[cfg(test)]
pub fn open_at_version(path: &str, version: usize) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    migrate_to(&mut conn, version)?;
    Ok(conn)
}

fn migrate_to(conn: &mut Connection, target: usize) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in (1..).zip(MIGRATIONS.iter().take(target)).skip(usize::try_from(version).unwrap_or(0)) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i64::from(i))?;
        tx.commit()?;
    }
    Ok(())
}

/// Schema version written by this build (`PRAGMA user_version`).
pub fn current_schema_version() -> i64 {
    MIGRATIONS.len() as i64
}

/// Copies a live database file to `dest` as one consistent snapshot.
/// Refuses to overwrite; the source is opened without migrating it.
pub fn backup_database_file(source: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("No database file at {}", source.display()));
    }
    if dest.exists() {
        return Err(format!("{} already exists; refusing to overwrite", dest.display()));
    }
    let conn = Connection::open(source).map_err(|e| e.to_string())?;
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()]).map_err(|e| e.to_string())?;
    Ok(())
}

/// Quick schema version read without an integrity check (fleet listings).
pub fn schema_version_of(path: &std::path::Path) -> Option<i64> {
    let conn = Connection::open(path).ok()?;
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).ok()
}

/// Validates a database file for import or use: integrity, a schema version
/// this build can open, and the tables every released schema has had.
/// Returns the file's schema version.
pub fn check_database_file(path: &std::path::Path) -> Result<i64, String> {
    if !path.is_file() {
        return Err(format!("No database file at {}", path.display()));
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if integrity != "ok" {
        return Err(format!("Integrity check failed: {integrity}"));
    }
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if !(0..=current_schema_version()).contains(&version) {
        return Err(format!(
            "Schema version {version} is newer than this RoomLog ({}); upgrade RoomLog first",
            current_schema_version()
        ));
    }
    for table in ["settings", "rooms", "events", "loans"] {
        let found: bool = conn
            .query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)", [table], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if !found {
            return Err(format!("Not a RoomLog database: missing table {table}"));
        }
    }
    Ok(version)
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
    pub needs: BTreeMap<Want, i64>,
}

pub fn recurrence_from_db(kind: &str, interval: i64, nth: i64) -> Recurrence {
    match kind {
        "weekly" => Recurrence::Weekly { interval: interval.clamp(1, 52) as u32 },
        "monthly" => Recurrence::Monthly { nth: nth as i32 },
        _ => Recurrence::None,
    }
}

pub fn load_events(conn: &Connection) -> rusqlite::Result<Vec<EventRow>> {
    let mut needs: HashMap<Id, BTreeMap<Want, i64>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT event_id, kind_id, type_id, quantity FROM event_needs")?;
    for row in stmt.query_map([], |r| {
        Ok((r.get::<_, Id>(0)?, r.get::<_, Option<Id>>(1)?, r.get::<_, Option<Id>>(2)?, r.get::<_, i64>(3)?))
    })? {
        let (e, k, t, q) = row?;
        if let Some(w) = want_from_db(k, t) {
            needs.entry(e).or_default().insert(w, q);
        }
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
    /// Set when a group agreed to manage with less "any of this kind".
    pub kind_id: Option<Id>,
    /// Set when a group agreed to manage with less of this exact model.
    pub type_id: Option<Id>,
    pub quantity: i64,
    pub note: String,
    pub loan_id: Option<Id>,
}

pub fn load_overrides(conn: &Connection) -> rusqlite::Result<Vec<OverrideRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_id, occurrence_start, type_id, quantity, note, loan_id, kind_id FROM occurrence_overrides",
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
            kind_id: r.get(7)?,
        })
    })?;
    rows.collect()
}

impl OverrideRow {
    /// The need this override changes; `None` means the occurrence is cancelled.
    pub fn want(&self) -> Option<Want> {
        want_from_db(self.kind_id, self.type_id)
    }
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
    #[serde(serialize_with = "engine::ser_lines")]
    pub needs: BTreeMap<Want, i64>,
    /// Usual needs from the event definition.
    #[serde(serialize_with = "engine::ser_lines")]
    pub usual_needs: BTreeMap<Want, i64>,
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
                match o.want() {
                    None => cancelled = true,
                    Some(w) => {
                        needs.insert(w, o.quantity);
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

/// Build an engine snapshot covering `window`.
pub fn load_world(
    conn: &Connection,
    window: &Span,
    exclude_loan: Option<Id>,
    buffer_hours: i64,
    now: NaiveDateTime,
) -> rusqlite::Result<World> {
    let mut world = World { catalog: load_catalog(conn)?, ..World::default() };
    let mut stmt = conn.prepare("SELECT room_id, type_id, quantity FROM stock WHERE quantity > 0")?;
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
