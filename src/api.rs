//! HTTP API (JSON) for the admin console and the public organiser form.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime};
use rand::Rng;
use rand::distributions::Alphanumeric;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{self, Role, Staff};
use crate::bookings;
use crate::db::{self, OccurrenceView, fmt_dt, parse_dt};
use crate::engine::{self, Catalog, Evaluation, Id, Line, Want};
use crate::recurrence::{Recurrence, Span};

const MAX_TEXT: usize = 4000;
const MAX_ITEMS: usize = 100;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub cookie_secure: bool,
}

impl AppState {
    pub fn new(conn: Connection, cookie_secure: bool) -> Self {
        AppState { db: Arc::new(Mutex::new(conn)), cookie_secure }
    }
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn now() -> NaiveDateTime {
    Local::now().naive_local()
}

// ------------------------------------------------------------------ errors

#[derive(Debug)]
pub struct ApiError(StatusCode, String, Option<Value>);

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::BAD_REQUEST, msg.into(), None)
    }
    pub fn not_found() -> Self {
        ApiError(StatusCode::NOT_FOUND, "Not found".into(), None)
    }
    pub fn unauthorized() -> Self {
        Self::unauthorized_msg("Please log in")
    }
    pub fn unauthorized_msg(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::UNAUTHORIZED, msg.into(), None)
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::FORBIDDEN, msg.into(), None)
    }
    pub fn conflict(msg: impl Into<String>, detail: Option<Value>) -> Self {
        ApiError(StatusCode::CONFLICT, msg.into(), detail)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.1 });
        if let Some(d) = self.2 {
            body["detail"] = d;
        }
        (self.0, Json(body)).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(f, msg) = &e
            && f.code == rusqlite::ErrorCode::ConstraintViolation {
                let m = msg.clone().unwrap_or_default();
                let friendly = if m.contains("UNIQUE") {
                    "That name is already in use".to_string()
                } else if m.contains("FOREIGN KEY") {
                    "Refers to something that doesn't exist".to_string()
                } else {
                    format!("Invalid data: {m}")
                };
                return ApiError(StatusCode::CONFLICT, friendly, None);
            }
        tracing::error!("database error: {e}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Database error".into(), None)
    }
}

pub type ApiResult<T> = Result<Json<T>, ApiError>;

pub fn random_token(len: usize) -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(len).map(char::from).collect()
}

pub fn clean(s: &str, field: &str) -> Result<String, ApiError> {
    let s = s.trim();
    if s.len() > MAX_TEXT {
        return Err(ApiError::bad(format!("{field} is too long")));
    }
    Ok(s.to_string())
}

pub fn required(s: &str, field: &str) -> Result<String, ApiError> {
    let s = clean(s, field)?;
    if s.is_empty() {
        return Err(ApiError::bad(format!("{field} is required")));
    }
    Ok(s)
}

// ---------------------------------------------------------------- settings

async fn get_settings(_: Staff, State(st): State<AppState>) -> ApiResult<db::Settings> {
    Ok(Json(db::get_settings(&st.conn())?))
}

#[derive(Deserialize)]
struct SettingsIn {
    church_name: String,
    buffer_hours: i64,
}

async fn put_settings(staff: Staff, State(st): State<AppState>, Json(b): Json<SettingsIn>) -> ApiResult<db::Settings> {
    staff.require(Role::Admin)?;
    if !(0..=24 * 14).contains(&b.buffer_hours) {
        return Err(ApiError::bad("Buffer must be between 0 and 336 hours"));
    }
    let conn = st.conn();
    db::set_setting(&conn, "church_name", &required(&b.church_name, "Church name")?)?;
    db::set_setting(&conn, "buffer_hours", &b.buffer_hours.to_string())?;
    Ok(Json(db::get_settings(&conn)?))
}

// ------------------------------------------------------------------- rooms

#[derive(Serialize, Deserialize)]
struct RoomDto {
    #[serde(default)]
    id: Id,
    name: String,
    /// Private notes for staff.
    #[serde(default)]
    notes: String,
    /// Shown on the public booking pages.
    #[serde(default = "yes")]
    bookable: bool,
    capacity: Option<i64>,
    #[serde(default)]
    public_notes: String,
}

impl RoomDto {
    fn validated(self, id: Id) -> Result<RoomDto, ApiError> {
        if self.capacity.is_some_and(|c| !(1..=100_000).contains(&c)) {
            return Err(ApiError::bad("Capacity must be a positive number"));
        }
        Ok(RoomDto {
            id,
            name: required(&self.name, "Name")?,
            notes: clean(&self.notes, "Notes")?,
            bookable: self.bookable,
            capacity: self.capacity,
            public_notes: clean(&self.public_notes, "Public description")?,
        })
    }
}

async fn list_rooms(_: Staff, State(st): State<AppState>) -> ApiResult<Vec<RoomDto>> {
    let conn = st.conn();
    let mut stmt = conn.prepare("SELECT id, name, notes, bookable, capacity, public_notes FROM rooms ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok(RoomDto {
            id: r.get(0)?,
            name: r.get(1)?,
            notes: r.get(2)?,
            bookable: r.get(3)?,
            capacity: r.get(4)?,
            public_notes: r.get(5)?,
        })
    })?;
    Ok(Json(rows.collect::<Result<_, _>>()?))
}

async fn create_room(_: Staff, State(st): State<AppState>, Json(b): Json<RoomDto>) -> ApiResult<RoomDto> {
    let conn = st.conn();
    let r = b.validated(0)?;
    conn.execute(
        "INSERT INTO rooms(name, notes, bookable, capacity, public_notes) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![r.name, r.notes, r.bookable, r.capacity, r.public_notes],
    )?;
    Ok(Json(RoomDto { id: conn.last_insert_rowid(), ..r }))
}

async fn update_room(_: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<RoomDto>) -> ApiResult<RoomDto> {
    let conn = st.conn();
    let r = b.validated(id)?;
    if conn.execute(
        "UPDATE rooms SET name = ?1, notes = ?2, bookable = ?3, capacity = ?4, public_notes = ?5 WHERE id = ?6",
        params![r.name, r.notes, r.bookable, r.capacity, r.public_notes, id],
    )? == 0
    {
        return Err(ApiError::not_found());
    }
    Ok(Json(r))
}

async fn delete_room(_: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    let conn = st.conn();
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM loan_allocations a JOIN loans l ON l.id = a.loan_id
         WHERE a.room_id = ?1 AND l.status IN ('approved','collected')",
        [id],
        |r| r.get(0),
    )?;
    if active > 0 {
        return Err(ApiError::conflict("This room has equipment on active loans; finish those first", None));
    }
    let booked: i64 =
        conn.query_row("SELECT COUNT(*) FROM bookings WHERE room_id = ?1 AND status IN ('pending','approved')", [id], |r| {
            r.get(0)
        })?;
    if booked > 0 {
        return Err(ApiError::conflict("This room has open bookings; decline or cancel them first", None));
    }
    conn.execute("DELETE FROM rooms WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------- kinds

#[derive(Serialize, Deserialize)]
struct KindDto {
    #[serde(default)]
    id: Id,
    name: String,
    #[serde(default)]
    parent_id: Option<Id>,
    #[serde(default = "yes")]
    requestable: bool,
    #[serde(default)]
    sort_order: i64,
    #[serde(default)]
    notes: String,
}

fn yes() -> bool {
    true
}

fn list_kind_rows(conn: &Connection) -> rusqlite::Result<Vec<KindDto>> {
    let mut stmt =
        conn.prepare("SELECT id, name, parent_id, requestable, sort_order, notes FROM equipment_kinds ORDER BY sort_order, name")?;
    let rows = stmt.query_map([], |r| {
        Ok(KindDto {
            id: r.get(0)?,
            name: r.get(1)?,
            parent_id: r.get(2)?,
            requestable: r.get(3)?,
            sort_order: r.get(4)?,
            notes: r.get(5)?,
        })
    })?;
    rows.collect()
}

async fn list_kinds(_: Staff, State(st): State<AppState>) -> ApiResult<Vec<KindDto>> {
    Ok(Json(list_kind_rows(&st.conn())?))
}

fn kind_exists(conn: &Connection, id: Id) -> rusqlite::Result<bool> {
    conn.query_row("SELECT COUNT(*) FROM equipment_kinds WHERE id = ?1", [id], |r| r.get::<_, i64>(0)).map(|n| n > 0)
}

/// Validates a kind; `id` is set when updating (to refuse moving a kind
/// under itself or one of its own sub-kinds).
fn clean_kind(conn: &Connection, id: Option<Id>, b: &KindDto) -> Result<(String, String), ApiError> {
    let name = required(&b.name, "Name")?;
    let notes = clean(&b.notes, "Notes")?;
    if let Some(p) = b.parent_id {
        if !kind_exists(conn, p)? {
            return Err(ApiError::bad("The parent kind doesn't exist"));
        }
        if let Some(id) = id {
            let cat = db::load_catalog(conn)?;
            if cat.path(Want::Kind(p)).contains(&Want::Kind(id)) {
                return Err(ApiError::bad("A kind can't go inside itself or one of its own sub-kinds"));
            }
        }
    }
    Ok((name, notes))
}

async fn create_kind(staff: Staff, State(st): State<AppState>, Json(b): Json<KindDto>) -> ApiResult<KindDto> {
    staff.require(Role::Av)?;
    let conn = st.conn();
    let (name, notes) = clean_kind(&conn, None, &b)?;
    conn.execute(
        "INSERT INTO equipment_kinds(name, parent_id, requestable, sort_order, notes) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, b.parent_id, b.requestable, b.sort_order, notes],
    )?;
    Ok(Json(KindDto { id: conn.last_insert_rowid(), name, notes, ..b }))
}

async fn update_kind(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<KindDto>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let conn = st.conn();
    let (name, notes) = clean_kind(&conn, Some(id), &b)?;
    if conn.execute(
        "UPDATE equipment_kinds SET name = ?1, parent_id = ?2, requestable = ?3, sort_order = ?4, notes = ?5 WHERE id = ?6",
        params![name, b.parent_id, b.requestable, b.sort_order, notes, id],
    )? == 0
    {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_kind(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let conn = st.conn();
    let count = |sql: &str| conn.query_row(sql, [id], |r| r.get::<_, i64>(0));
    if count("SELECT (SELECT COUNT(*) FROM equipment_kinds WHERE parent_id = ?1) + (SELECT COUNT(*) FROM equipment_types WHERE kind_id = ?1)")? > 0 {
        return Err(ApiError::conflict("This kind still has sub-kinds or models; move or delete them first", None));
    }
    if count(
        "SELECT COUNT(*) FROM loan_items i JOIN loans l ON l.id = i.loan_id
         WHERE i.kind_id = ?1 AND l.status IN ('pending','approved','collected')",
    )? > 0
        || count(
            "SELECT COUNT(*) FROM booking_needs n JOIN bookings b ON b.id = n.booking_id
             WHERE n.kind_id = ?1 AND b.status IN ('pending','approved')",
        )? > 0
    {
        return Err(ApiError::conflict("Open loan requests or bookings ask for this kind; resolve those first", None));
    }
    if count("SELECT COUNT(*) FROM event_needs WHERE kind_id = ?1")? > 0 {
        return Err(ApiError::conflict("Church events need this kind; change their equipment first", None));
    }
    conn.execute("DELETE FROM equipment_kinds WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------- equipment (models)

#[derive(Serialize, Deserialize)]
struct EquipmentDto {
    #[serde(default)]
    id: Id,
    name: String,
    #[serde(default)]
    kind_id: Option<Id>,
    #[serde(default)]
    notes: String,
    #[serde(default = "yes")]
    lendable: bool,
    #[serde(default)]
    total: i64,
}

fn list_equipment_rows(conn: &Connection) -> rusqlite::Result<Vec<EquipmentDto>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.kind_id, t.notes, t.lendable, COALESCE(SUM(s.quantity), 0)
         FROM equipment_types t LEFT JOIN stock s ON s.type_id = t.id
         GROUP BY t.id ORDER BY t.name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(EquipmentDto {
            id: r.get(0)?,
            name: r.get(1)?,
            kind_id: r.get(2)?,
            notes: r.get(3)?,
            lendable: r.get(4)?,
            total: r.get(5)?,
        })
    })?;
    rows.collect()
}

async fn list_equipment(_: Staff, State(st): State<AppState>) -> ApiResult<Vec<EquipmentDto>> {
    Ok(Json(list_equipment_rows(&st.conn())?))
}

fn clean_equipment(conn: &Connection, b: &EquipmentDto) -> Result<(String, String), ApiError> {
    if let Some(k) = b.kind_id
        && !kind_exists(conn, k)? {
            return Err(ApiError::bad("That kind doesn't exist"));
        }
    Ok((required(&b.name, "Name")?, clean(&b.notes, "Notes")?))
}

async fn create_equipment(staff: Staff, State(st): State<AppState>, Json(b): Json<EquipmentDto>) -> ApiResult<EquipmentDto> {
    staff.require(Role::Av)?;
    let conn = st.conn();
    let (name, notes) = clean_equipment(&conn, &b)?;
    conn.execute(
        "INSERT INTO equipment_types(name, kind_id, notes, lendable) VALUES (?1, ?2, ?3, ?4)",
        params![name, b.kind_id, notes, b.lendable],
    )?;
    Ok(Json(EquipmentDto { id: conn.last_insert_rowid(), name, notes, total: 0, ..b }))
}

async fn update_equipment(
    staff: Staff,
    State(st): State<AppState>,
    Path(id): Path<Id>,
    Json(b): Json<EquipmentDto>,
) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let conn = st.conn();
    let (name, notes) = clean_equipment(&conn, &b)?;
    if conn.execute(
        "UPDATE equipment_types SET name = ?1, kind_id = ?2, notes = ?3, lendable = ?4 WHERE id = ?5",
        params![name, b.kind_id, notes, b.lendable, id],
    )? == 0
    {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_equipment(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let conn = st.conn();
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM loan_items i JOIN loans l ON l.id = i.loan_id
         WHERE i.type_id = ?1 AND l.status IN ('pending','approved','collected')",
        [id],
        |r| r.get(0),
    )?;
    if active > 0 {
        return Err(ApiError::conflict("This equipment is part of open loan requests; resolve those first", None));
    }
    conn.execute("DELETE FROM equipment_types WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------ request lines

/// Validates request lines (loan items, booking or event needs) and merges
/// duplicates. Kinds must exist and be requestable; models must exist, and on
/// public forms be lendable. `min_qty` is 0 for event needs (0 = remove).
pub fn validate_lines(
    conn: &Connection,
    lines: &[Line],
    public: bool,
    min_qty: i64,
) -> Result<BTreeMap<Want, i64>, ApiError> {
    if lines.len() > MAX_ITEMS {
        return Err(ApiError::bad("Too many equipment items"));
    }
    let mut merged: BTreeMap<Want, i64> = BTreeMap::new();
    for l in lines {
        if !(min_qty..=10_000).contains(&l.quantity) {
            return Err(ApiError::bad(format!("Quantities must be between {min_qty} and 10000")));
        }
        match l.want {
            Want::Model(t) => {
                let lendable: Option<bool> = conn
                    .query_row("SELECT lendable FROM equipment_types WHERE id = ?1", [t], |r| r.get(0))
                    .optional()?;
                match lendable {
                    None => return Err(ApiError::bad("Unknown equipment type")),
                    Some(false) if public => return Err(ApiError::bad("That equipment is not available for loan")),
                    _ => {}
                }
            }
            Want::Kind(k) => {
                let kind: Option<(String, bool)> = conn
                    .query_row("SELECT name, requestable FROM equipment_kinds WHERE id = ?1", [k], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                match kind {
                    None => return Err(ApiError::bad("Unknown kind of equipment")),
                    Some((name, false)) => {
                        return Err(ApiError::bad(format!("Please choose something more specific than \"{name}\"")));
                    }
                    _ => {}
                }
            }
        }
        if l.quantity > 0 {
            *merged.entry(l.want).or_insert(0) += l.quantity;
        }
    }
    Ok(merged)
}

// ------------------------------------------------------------------- stock

#[derive(Serialize, Deserialize)]
struct StockDto {
    room_id: Id,
    type_id: Id,
    quantity: i64,
}

async fn list_stock(_: Staff, State(st): State<AppState>) -> ApiResult<Vec<StockDto>> {
    let conn = st.conn();
    let mut stmt = conn.prepare("SELECT room_id, type_id, quantity FROM stock")?;
    let rows = stmt.query_map([], |r| Ok(StockDto { room_id: r.get(0)?, type_id: r.get(1)?, quantity: r.get(2)? }))?;
    Ok(Json(rows.collect::<Result<_, _>>()?))
}

async fn put_stock(staff: Staff, State(st): State<AppState>, Json(b): Json<StockDto>) -> ApiResult<StockDto> {
    staff.require(Role::Av)?;
    if !(0..=100_000).contains(&b.quantity) {
        return Err(ApiError::bad("Quantity must be between 0 and 100000"));
    }
    let conn = st.conn();
    if b.quantity == 0 {
        conn.execute("DELETE FROM stock WHERE room_id = ?1 AND type_id = ?2", params![b.room_id, b.type_id])?;
    } else {
        conn.execute(
            "INSERT INTO stock(room_id, type_id, quantity) VALUES (?1, ?2, ?3)
             ON CONFLICT(room_id, type_id) DO UPDATE SET quantity = excluded.quantity",
            params![b.room_id, b.type_id, b.quantity],
        )?;
    }
    Ok(Json(b))
}

// ------------------------------------------------------------------ groups

#[derive(Clone, Serialize, Deserialize)]
pub struct GroupDto {
    #[serde(default)]
    id: Id,
    name: String,
    #[serde(default)]
    contact_name: String,
    #[serde(default)]
    contact_phone: String,
    #[serde(default)]
    contact_email: String,
    #[serde(default)]
    notes: String,
}

fn list_group_rows(conn: &Connection) -> rusqlite::Result<Vec<GroupDto>> {
    let mut stmt =
        conn.prepare("SELECT id, name, contact_name, contact_phone, contact_email, notes FROM groups ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok(GroupDto {
            id: r.get(0)?,
            name: r.get(1)?,
            contact_name: r.get(2)?,
            contact_phone: r.get(3)?,
            contact_email: r.get(4)?,
            notes: r.get(5)?,
        })
    })?;
    rows.collect()
}

async fn list_groups(_: Staff, State(st): State<AppState>) -> ApiResult<Vec<GroupDto>> {
    Ok(Json(list_group_rows(&st.conn())?))
}

fn clean_group(b: &GroupDto) -> Result<GroupDto, ApiError> {
    Ok(GroupDto {
        id: b.id,
        name: required(&b.name, "Name")?,
        contact_name: clean(&b.contact_name, "Contact name")?,
        contact_phone: clean(&b.contact_phone, "Phone")?,
        contact_email: clean(&b.contact_email, "Email")?,
        notes: clean(&b.notes, "Notes")?,
    })
}

async fn create_group(_: Staff, State(st): State<AppState>, Json(b): Json<GroupDto>) -> ApiResult<GroupDto> {
    let conn = st.conn();
    let mut g = clean_group(&b)?;
    conn.execute(
        "INSERT INTO groups(name, contact_name, contact_phone, contact_email, notes) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![g.name, g.contact_name, g.contact_phone, g.contact_email, g.notes],
    )?;
    g.id = conn.last_insert_rowid();
    Ok(Json(g))
}

async fn update_group(_: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<GroupDto>) -> ApiResult<GroupDto> {
    let conn = st.conn();
    let mut g = clean_group(&b)?;
    g.id = id;
    if conn.execute(
        "UPDATE groups SET name = ?1, contact_name = ?2, contact_phone = ?3, contact_email = ?4, notes = ?5 WHERE id = ?6",
        params![g.name, g.contact_name, g.contact_phone, g.contact_email, g.notes, id],
    )? == 0
    {
        return Err(ApiError::not_found());
    }
    Ok(Json(g))
}

async fn delete_group(_: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    st.conn().execute("DELETE FROM groups WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ events

#[derive(Serialize, Deserialize)]
struct EventDto {
    #[serde(default)]
    id: Id,
    title: String,
    room_id: Id,
    group_id: Option<Id>,
    start_date: String,
    start_time: String,
    duration_minutes: i64,
    recurrence: String,
    #[serde(default = "one")]
    interval_weeks: i64,
    #[serde(default = "one")]
    nth: i64,
    until: Option<String>,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    needs: Vec<Line>,
    #[serde(default)]
    description: String,
    /// Set (output only) when the event came from an approved room booking.
    #[serde(default)]
    booking_id: Option<Id>,
}

fn one() -> i64 {
    1
}

fn event_to_dto(e: &db::EventRow) -> EventDto {
    let (recurrence, interval_weeks, nth) = match e.rule.recurrence {
        Recurrence::None => ("none", 1, 1),
        Recurrence::Weekly { interval } => ("weekly", i64::from(interval), 1),
        Recurrence::Monthly { nth } => ("monthly", 1, i64::from(nth)),
    };
    EventDto {
        id: e.id,
        title: e.title.clone(),
        room_id: e.room_id,
        group_id: e.group_id,
        start_date: e.rule.start_date.format("%Y-%m-%d").to_string(),
        start_time: e.rule.start_time.format("%H:%M").to_string(),
        duration_minutes: e.rule.duration_minutes,
        recurrence: recurrence.into(),
        interval_weeks,
        nth,
        until: e.rule.until.map(|u| u.format("%Y-%m-%d").to_string()),
        notes: e.notes.clone(),
        needs: engine::to_lines(&e.needs),
        description: e.rule.describe(),
        booking_id: None,
    }
}

async fn list_events(_: Staff, State(st): State<AppState>) -> ApiResult<Vec<EventDto>> {
    let conn = st.conn();
    let booked = db::booked_events(&conn)?;
    let events = db::load_events(&conn)?
        .iter()
        .map(|e| EventDto { booking_id: booked.get(&e.id).map(|b| b.0), ..event_to_dto(e) })
        .collect();
    Ok(Json(events))
}

fn validate_event(b: &EventDto) -> Result<(), ApiError> {
    required(&b.title, "Title")?;
    NaiveDate::parse_from_str(&b.start_date, "%Y-%m-%d").map_err(|_| ApiError::bad("Start date is invalid"))?;
    NaiveTime::parse_from_str(&b.start_time, "%H:%M").map_err(|_| ApiError::bad("Start time is invalid"))?;
    if !(1..=60 * 24 * 7).contains(&b.duration_minutes) {
        return Err(ApiError::bad("Duration must be between 1 minute and 7 days"));
    }
    match b.recurrence.as_str() {
        "none" => {}
        "weekly" if (1..=52).contains(&b.interval_weeks) => {}
        "monthly" if (1..=5).contains(&b.nth) || b.nth == -1 => {}
        _ => return Err(ApiError::bad("Invalid recurrence")),
    }
    if let Some(u) = b.until.as_deref().filter(|u| !u.is_empty()) {
        NaiveDate::parse_from_str(u, "%Y-%m-%d").map_err(|_| ApiError::bad("'Until' date is invalid"))?;
    }
    let mut seen = HashSet::new();
    if b.needs.iter().filter(|n| n.quantity > 0).any(|n| !seen.insert(n.want)) {
        return Err(ApiError::bad("Each kind or model can only be listed once per event"));
    }
    Ok(())
}

fn save_event(conn: &mut Connection, id: Option<Id>, b: &EventDto) -> Result<Id, ApiError> {
    validate_event(b)?;
    let needs: Vec<(Want, i64)> = validate_lines(conn, &b.needs, false, 0)?.into_iter().collect();
    let tx = conn.transaction()?;
    let until = b.until.clone().filter(|u| !u.is_empty());
    let title = b.title.trim().to_string();
    let notes = clean(&b.notes, "Notes")?;
    let (interval, nth) = (b.interval_weeks, b.nth);
    let args = params![
        title,
        b.room_id,
        b.group_id,
        b.start_date,
        b.start_time,
        b.duration_minutes,
        b.recurrence,
        interval,
        nth,
        until,
        notes
    ];
    let id = match id {
        Some(id) => {
            let mut with_id: Vec<&dyn rusqlite::ToSql> = args.to_vec();
            with_id.push(&id);
            let n = tx.execute(
                "UPDATE events SET title=?1, room_id=?2, group_id=?3, start_date=?4, start_time=?5,
                 duration_minutes=?6, recurrence=?7, interval_weeks=?8, nth=?9, until=?10, notes=?11
                 WHERE id = ?12",
                with_id.as_slice(),
            )?;
            if n == 0 {
                return Err(ApiError::not_found());
            }
            id
        }
        None => {
            tx.execute(
                "INSERT INTO events(title, room_id, group_id, start_date, start_time, duration_minutes,
                 recurrence, interval_weeks, nth, until, notes) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                args,
            )?;
            tx.last_insert_rowid()
        }
    };
    db::save_lines(&tx, "event_needs", "event_id", id, &needs)?;
    tx.commit()?;
    Ok(id)
}

// Saving an event never fails because of a room clash (overlapping church
// events may be intended), but the response lists clashes so staff notice.
async fn create_event(_: Staff, State(st): State<AppState>, Json(b): Json<EventDto>) -> ApiResult<Value> {
    let mut conn = st.conn();
    let id = save_event(&mut conn, None, &b)?;
    Ok(Json(json!({ "id": id, "clashes": bookings::event_clashes(&conn, id)? })))
}

async fn update_event(_: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<EventDto>) -> ApiResult<Value> {
    let mut conn = st.conn();
    save_event(&mut conn, Some(id), &b)?;
    Ok(Json(json!({ "id": id, "clashes": bookings::event_clashes(&conn, id)? })))
}

async fn delete_event(_: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    let conn = st.conn();
    if let Some(booking) = bookings::booking_for_event(&conn, id)? {
        return Err(ApiError::conflict(
            format!("This event comes from room booking #{booking}. Cancel the booking instead."),
            Some(json!({ "booking_id": booking })),
        ));
    }
    conn.execute("DELETE FROM events WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

// -------------------------------------------------------- refs (name maps)

#[derive(Serialize)]
pub struct TypeRef {
    pub name: String,
    pub kind_id: Option<Id>,
    pub lendable: bool,
}

#[derive(Serialize)]
pub struct KindRef {
    pub name: String,
    pub parent_id: Option<Id>,
    pub requestable: bool,
    pub sort_order: i64,
}

/// id -> display name lookups sent alongside API responses.
#[derive(Serialize)]
pub struct Refs {
    pub rooms: BTreeMap<Id, String>,
    pub types: BTreeMap<Id, TypeRef>,
    pub kinds: BTreeMap<Id, KindRef>,
    groups: BTreeMap<Id, GroupDto>,
    events: BTreeMap<Id, String>,
}

impl Refs {
    /// "Shure SM58", or "Wired vocal mic (any)" for any model of a kind.
    pub fn want_name(&self, w: Want) -> String {
        match w {
            Want::Model(t) => self.types.get(&t).map_or_else(|| format!("#{t}"), |x| x.name.clone()),
            Want::Kind(k) => self.kinds.get(&k).map_or_else(|| format!("#{k}"), |x| format!("{} (any)", x.name)),
        }
    }
    pub fn type_name(&self, t: Id) -> String {
        self.want_name(Want::Model(t))
    }
}

pub fn load_refs(conn: &Connection) -> rusqlite::Result<Refs> {
    let mut rooms = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT id, name FROM rooms")?;
    for r in stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, String>(1)?)))? {
        let (id, n) = r?;
        rooms.insert(id, n);
    }
    let mut types = BTreeMap::new();
    for t in list_equipment_rows(conn)? {
        types.insert(t.id, TypeRef { name: t.name, kind_id: t.kind_id, lendable: t.lendable });
    }
    let kinds = list_kind_rows(conn)?
        .into_iter()
        .map(|k| (k.id, KindRef { name: k.name, parent_id: k.parent_id, requestable: k.requestable, sort_order: k.sort_order }))
        .collect();
    let groups = list_group_rows(conn)?.into_iter().map(|g| (g.id, g)).collect();
    let mut events = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT id, title FROM events")?;
    for r in stmt.query_map([], |r| Ok((r.get::<_, Id>(0)?, r.get::<_, String>(1)?)))? {
        let (id, n) = r?;
        events.insert(id, n);
    }
    Ok(Refs { rooms, types, kinds, groups, events })
}

// ---------------------------------------------------------------- schedule

#[derive(Deserialize)]
struct ScheduleQuery {
    from: Option<String>,
    days: Option<i64>,
}

async fn schedule(_: Staff, State(st): State<AppState>, Query(q): Query<ScheduleQuery>) -> ApiResult<Value> {
    let from = q
        .from
        .as_deref()
        .and_then(|f| NaiveDate::parse_from_str(f, "%Y-%m-%d").ok())
        .unwrap_or_else(|| now().date());
    let days = q.days.unwrap_or(14).clamp(1, 366);
    let start = from.and_time(NaiveTime::MIN);
    let window = Span::new(start, start + Duration::days(days));
    let conn = st.conn();
    let occ: Vec<OccurrenceView> = db::expand_occurrences(&conn, &window)?;
    let loans = loans_overlapping(&conn, &window)?;
    let booked: BTreeMap<Id, Id> = db::booked_events(&conn)?.into_iter().map(|(e, (b, _))| (e, b)).collect();
    Ok(Json(json!({ "occurrences": occ, "loans": loans, "booked_events": booked, "refs": load_refs(&conn)? })))
}

#[derive(Deserialize)]
struct OverrideIn {
    event_id: Id,
    occurrence_start: String,
    /// Both unset: cancel the occurrence. Otherwise the need being reduced.
    #[serde(default)]
    kind_id: Option<Id>,
    #[serde(default)]
    type_id: Option<Id>,
    #[serde(default)]
    quantity: i64,
    #[serde(default)]
    note: String,
    loan_id: Option<Id>,
}

async fn create_override(staff: Staff, State(st): State<AppState>, Json(b): Json<OverrideIn>) -> ApiResult<Value> {
    if b.kind_id.is_some() && b.type_id.is_some() {
        return Err(ApiError::bad("An override reduces either a kind or a model, not both"));
    }
    if b.kind_id.is_some() || b.type_id.is_some() {
        staff.require(Role::Av)?;
    }
    let start = parse_dt(&b.occurrence_start).ok_or_else(|| ApiError::bad("Invalid occurrence start"))?;
    if b.quantity < 0 {
        return Err(ApiError::bad("Quantity cannot be negative"));
    }
    let conn = st.conn();
    let events = db::load_events(&conn)?;
    let ev = events.iter().find(|e| e.id == b.event_id).ok_or_else(ApiError::not_found)?;
    let probe = Span::new(start, start + Duration::minutes(1));
    if !ev.rule.occurrences_in(&probe).iter().any(|s| s.start == start) {
        return Err(ApiError::bad("That event does not occur at that time"));
    }
    let start_s = fmt_dt(start);
    conn.execute(
        "DELETE FROM occurrence_overrides
         WHERE event_id = ?1 AND occurrence_start = ?2 AND type_id IS ?3 AND kind_id IS ?4",
        params![b.event_id, start_s, b.type_id, b.kind_id],
    )?;
    conn.execute(
        "INSERT INTO occurrence_overrides(event_id, occurrence_start, kind_id, type_id, quantity, note, loan_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![b.event_id, start_s, b.kind_id, b.type_id, b.quantity, clean(&b.note, "Note")?, b.loan_id, fmt_dt(now())],
    )?;
    Ok(Json(json!({ "id": conn.last_insert_rowid() })))
}

async fn delete_override(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    let conn = st.conn();
    let is_equipment: Option<bool> = conn
        .query_row(
            "SELECT type_id IS NOT NULL OR kind_id IS NOT NULL FROM occurrence_overrides WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if is_equipment == Some(true) {
        staff.require(Role::Av)?;
    }
    conn.execute("DELETE FROM occurrence_overrides WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------- loans

/// Could two lines end up wanting the same physical units?
fn wants_overlap(cat: &Catalog, a: Want, b: Want) -> bool {
    cat.is_under(a, b) || cat.is_under(b, a)
}

#[derive(Deserialize)]
pub struct LoanIn {
    #[serde(default)]
    organiser_name: String,
    #[serde(default)]
    organisation: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    event_name: String,
    #[serde(default)]
    event_location: String,
    pickup_at: String,
    return_at: String,
    #[serde(default)]
    notes: String,
    items: Vec<Line>,
}

#[derive(Serialize, Clone)]
struct AllocDto {
    room_id: Id,
    type_id: Id,
    quantity: i64,
}

#[derive(Serialize)]
struct LoanDto {
    id: Id,
    token: String,
    status: String,
    organiser_name: String,
    organisation: String,
    email: String,
    phone: String,
    event_name: String,
    event_location: String,
    pickup_at: NaiveDateTime,
    return_at: NaiveDateTime,
    notes: String,
    admin_notes: String,
    message_to_organiser: String,
    decided_by: String,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
    items: Vec<Line>,
    allocations: Vec<AllocDto>,
}

const LOAN_COLS: &str = "id, token, status, organiser_name, organisation, email, phone, event_name, event_location,
    pickup_at, return_at, notes, admin_notes, message_to_organiser, created_at, updated_at, decided_by";

fn row_to_loan(r: &rusqlite::Row) -> rusqlite::Result<LoanDto> {
    let dt = |i: usize| -> rusqlite::Result<NaiveDateTime> { Ok(parse_dt(&r.get::<_, String>(i)?).unwrap_or_default()) };
    Ok(LoanDto {
        id: r.get(0)?,
        token: r.get(1)?,
        status: r.get(2)?,
        organiser_name: r.get(3)?,
        organisation: r.get(4)?,
        email: r.get(5)?,
        phone: r.get(6)?,
        event_name: r.get(7)?,
        event_location: r.get(8)?,
        pickup_at: dt(9)?,
        return_at: dt(10)?,
        notes: r.get(11)?,
        admin_notes: r.get(12)?,
        message_to_organiser: r.get(13)?,
        created_at: dt(14)?,
        updated_at: dt(15)?,
        decided_by: r.get(16)?,
        items: vec![],
        allocations: vec![],
    })
}

fn fill_loan(conn: &Connection, mut l: LoanDto) -> rusqlite::Result<LoanDto> {
    l.items = db::load_lines(conn, "loan_items", "loan_id", l.id)?;
    let mut stmt = conn.prepare(
        "SELECT room_id, type_id, quantity FROM loan_allocations WHERE loan_id = ?1 ORDER BY room_id, type_id",
    )?;
    l.allocations = stmt
        .query_map([l.id], |r| Ok(AllocDto { room_id: r.get(0)?, type_id: r.get(1)?, quantity: r.get(2)? }))?
        .collect::<Result<_, _>>()?;
    Ok(l)
}

fn get_loan(conn: &Connection, id: Id) -> Result<LoanDto, ApiError> {
    let l = conn
        .query_row(&format!("SELECT {LOAN_COLS} FROM loans WHERE id = ?1"), [id], row_to_loan)
        .optional()?
        .ok_or_else(ApiError::not_found)?;
    Ok(fill_loan(conn, l)?)
}

fn get_loan_by_token(conn: &Connection, token: &str) -> Result<LoanDto, ApiError> {
    let l = conn
        .query_row(&format!("SELECT {LOAN_COLS} FROM loans WHERE token = ?1"), [token], row_to_loan)
        .optional()?
        .ok_or_else(ApiError::not_found)?;
    Ok(fill_loan(conn, l)?)
}

#[derive(Serialize)]
struct LoanSummary {
    id: Id,
    status: String,
    organiser_name: String,
    organisation: String,
    event_name: String,
    pickup_at: NaiveDateTime,
    return_at: NaiveDateTime,
    created_at: NaiveDateTime,
    item_count: i64,
}

fn loan_summaries(conn: &Connection, where_sql: &str, args: &[&dyn rusqlite::ToSql]) -> rusqlite::Result<Vec<LoanSummary>> {
    let sql = format!(
        "SELECT l.id, l.status, l.organiser_name, l.organisation, l.event_name, l.pickup_at, l.return_at, l.created_at,
                COALESCE((SELECT SUM(quantity) FROM loan_items i WHERE i.loan_id = l.id), 0)
         FROM loans l {where_sql} ORDER BY l.pickup_at"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(args, |r| {
        let dt = |i: usize| -> rusqlite::Result<NaiveDateTime> { Ok(parse_dt(&r.get::<_, String>(i)?).unwrap_or_default()) };
        Ok(LoanSummary {
            id: r.get(0)?,
            status: r.get(1)?,
            organiser_name: r.get(2)?,
            organisation: r.get(3)?,
            event_name: r.get(4)?,
            pickup_at: dt(5)?,
            return_at: dt(6)?,
            created_at: dt(7)?,
            item_count: r.get(8)?,
        })
    })?;
    rows.collect()
}

fn loans_overlapping(conn: &Connection, window: &Span) -> rusqlite::Result<Vec<LoanSummary>> {
    loan_summaries(
        conn,
        "WHERE l.status IN ('pending','approved','collected') AND l.pickup_at < ?1 AND l.return_at > ?2",
        &[&fmt_dt(window.end), &fmt_dt(window.start)],
    )
}

#[derive(Deserialize)]
struct LoanQuery {
    status: Option<String>,
}

async fn list_loans(_: Staff, State(st): State<AppState>, Query(q): Query<LoanQuery>) -> ApiResult<Vec<LoanSummary>> {
    let conn = st.conn();
    let rows = match q.status.as_deref().filter(|s| !s.is_empty() && *s != "all") {
        Some("open") => loan_summaries(&conn, "WHERE l.status IN ('pending','approved','collected')", &[])?,
        Some(s) => loan_summaries(&conn, "WHERE l.status = ?1", &[&s])?,
        None => loan_summaries(&conn, "", &[])?,
    };
    Ok(Json(rows))
}

struct ValidLoan {
    pickup: NaiveDateTime,
    ret: NaiveDateTime,
    items: Vec<(Want, i64)>,
}

fn validate_request_core(conn: &Connection, pickup: &str, ret: &str, items: &[Line], public: bool) -> Result<ValidLoan, ApiError> {
    let pickup = parse_dt(pickup).ok_or_else(|| ApiError::bad("Pickup date/time is invalid"))?;
    let ret = parse_dt(ret).ok_or_else(|| ApiError::bad("Return date/time is invalid"))?;
    if ret <= pickup {
        return Err(ApiError::bad("Return must be after pickup"));
    }
    if ret - pickup > Duration::days(90) {
        return Err(ApiError::bad("Loans longer than 90 days are not supported"));
    }
    if items.is_empty() || items.len() > MAX_ITEMS {
        return Err(ApiError::bad("Please request at least one item"));
    }
    let merged = validate_lines(conn, items, public, 1)?;
    Ok(ValidLoan { pickup, ret, items: merged.into_iter().collect() })
}

fn evaluate_request(conn: &Connection, v: &ValidLoan, exclude: Option<Id>) -> Result<Evaluation, ApiError> {
    let s = db::get_settings(conn)?;
    let now = now();
    let window = db::loan_occupancy(v.pickup, v.ret, "pending", s.buffer_hours, now);
    let world = db::load_world(conn, &window, exclude, s.buffer_hours, now)?;
    Ok(engine::evaluate(&world, window, &v.items))
}

fn insert_loan(conn: &mut Connection, b: &LoanIn, public: bool) -> Result<(Id, String, ValidLoan), ApiError> {
    let v = validate_request_core(conn, &b.pickup_at, &b.return_at, &b.items, public)?;
    let organiser_name = required(&b.organiser_name, "Your name")?;
    let email = clean(&b.email, "Email")?;
    let phone = clean(&b.phone, "Phone")?;
    if public && email.is_empty() && phone.is_empty() {
        return Err(ApiError::bad("Please give an email or phone number so we can reach you"));
    }
    let token = random_token(32);
    let ts = fmt_dt(now());
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO loans(token, status, organiser_name, organisation, email, phone, event_name, event_location,
                           pickup_at, return_at, notes, created_at, updated_at)
         VALUES (?1, 'pending', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            token,
            organiser_name,
            clean(&b.organisation, "Organisation")?,
            email,
            phone,
            clean(&b.event_name, "Event name")?,
            clean(&b.event_location, "Location")?,
            fmt_dt(v.pickup),
            fmt_dt(v.ret),
            clean(&b.notes, "Notes")?,
            ts
        ],
    )?;
    let id = tx.last_insert_rowid();
    db::save_lines(&tx, "loan_items", "loan_id", id, &v.items)?;
    tx.commit()?;
    Ok((id, token, v))
}

/// What organisers see: availability per item, no contact or room detail.
#[derive(Serialize)]
struct PublicItem {
    #[serde(flatten)]
    want: Want,
    name: String,
    requested: i64,
    available: i64,
}

#[derive(Serialize)]
struct PublicCheck {
    fully_available: bool,
    items: Vec<PublicItem>,
}

fn public_check(conn: &Connection, ev: &Evaluation) -> rusqlite::Result<PublicCheck> {
    let refs = load_refs(conn)?;
    Ok(PublicCheck {
        fully_available: ev.fully_available,
        items: ev
            .items
            .iter()
            .map(|i| PublicItem {
                want: i.want,
                name: refs.want_name(i.want),
                requested: i.requested,
                available: i.available,
            })
            .collect(),
    })
}

async fn create_loan_admin(staff: Staff, State(st): State<AppState>, Json(b): Json<LoanIn>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let (id, token, _) = insert_loan(&mut st.conn(), &b, false)?;
    Ok(Json(json!({ "id": id, "token": token })))
}

async fn loan_detail(_: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    let conn = st.conn();
    let loan = get_loan(&conn, id)?;
    let s = db::get_settings(&conn)?;
    let now = now();
    let window = db::loan_occupancy(loan.pickup_at, loan.return_at, &loan.status, s.buffer_hours, now);
    let world = db::load_world(&conn, &window, Some(id), s.buffer_hours, now)?;
    let items: Vec<(Want, i64)> = loan.items.iter().map(|i| (i.want, i.quantity)).collect();

    let evaluation = (loan.status == "pending").then(|| engine::evaluate(&world, window, &items));
    let problems: Vec<engine::Problem> = if db::COMMITTED.contains(&loan.status.as_str()) {
        let alloc: Vec<(Id, Id, i64)> = loan.allocations.iter().map(|a| (a.room_id, a.type_id, a.quantity)).collect();
        engine::check_allocation(&world, window, &alloc)
    } else {
        vec![]
    };
    // Which allocated models serve which requested line.
    let picks: Vec<engine::Pick> =
        loan.allocations.iter().map(|a| engine::Pick { room_id: a.room_id, type_id: a.type_id, qty: a.quantity }).collect();
    let given = engine::match_allocation(&world.catalog, &items, &picks);

    // Other pending requests competing for the same equipment in the same period.
    let mut competing = Vec::new();
    for other in loans_overlapping(&conn, &window)? {
        if other.id == id || other.status != "pending" {
            continue;
        }
        let ol = get_loan(&conn, other.id)?;
        if ol.items.iter().any(|o| items.iter().any(|(w, _)| wants_overlap(&world.catalog, o.want, *w))) {
            competing.push(other);
        }
    }

    let overrides: Vec<db::OverrideRow> = db::load_overrides(&conn)?.into_iter().filter(|o| o.loan_id == Some(id)).collect();

    Ok(Json(json!({
        "loan": loan,
        "evaluation": evaluation,
        "allocation_problems": problems,
        "given": given,
        "competing_pending": competing,
        "agreed_overrides": overrides,
        "buffer_hours": s.buffer_hours,
        "refs": load_refs(&conn)?,
    })))
}

#[derive(Deserialize)]
struct LoanPatch {
    admin_notes: Option<String>,
    message_to_organiser: Option<String>,
    pickup_at: Option<String>,
    return_at: Option<String>,
    items: Option<Vec<Line>>,
}

async fn patch_loan(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<LoanPatch>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let mut conn = st.conn();
    let loan = get_loan(&conn, id)?;
    let ts = fmt_dt(now());
    let tx = conn.transaction()?;
    if let Some(n) = &b.admin_notes {
        tx.execute("UPDATE loans SET admin_notes = ?1, updated_at = ?2 WHERE id = ?3", params![clean(n, "Notes")?, ts, id])?;
    }
    if let Some(m) = &b.message_to_organiser {
        tx.execute(
            "UPDATE loans SET message_to_organiser = ?1, updated_at = ?2 WHERE id = ?3",
            params![clean(m, "Message")?, ts, id],
        )?;
    }
    if b.pickup_at.is_some() || b.return_at.is_some() || b.items.is_some() {
        if loan.status != "pending" {
            return Err(ApiError::conflict("Dates and items can only be changed while the request is pending", None));
        }
        let items: Vec<Line> = b.items.clone().unwrap_or_else(|| loan.items.clone());
        let v = validate_request_core(
            &tx,
            b.pickup_at.as_deref().unwrap_or(&fmt_dt(loan.pickup_at)),
            b.return_at.as_deref().unwrap_or(&fmt_dt(loan.return_at)),
            &items,
            false,
        )?;
        tx.execute(
            "UPDATE loans SET pickup_at = ?1, return_at = ?2, updated_at = ?3 WHERE id = ?4",
            params![fmt_dt(v.pickup), fmt_dt(v.ret), ts, id],
        )?;
        db::save_lines(&tx, "loan_items", "loan_id", id, &v.items)?;
    }
    tx.commit()?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AllocIn {
    room_id: Id,
    type_id: Id,
    quantity: i64,
}

#[derive(Deserialize)]
struct ApproveIn {
    allocations: Option<Vec<AllocIn>>,
    #[serde(default)]
    force: bool,
    message: Option<String>,
}

async fn approve_loan(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<ApproveIn>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let mut conn = st.conn();
    let loan = get_loan(&conn, id)?;
    if loan.status != "pending" {
        return Err(ApiError::conflict("Only pending requests can be approved", None));
    }
    let s = db::get_settings(&conn)?;
    let now = now();
    let window = db::loan_occupancy(loan.pickup_at, loan.return_at, "pending", s.buffer_hours, now);
    let world = db::load_world(&conn, &window, Some(id), s.buffer_hours, now)?;

    let allocations: Vec<(Id, Id, i64)> = match &b.allocations {
        Some(a) => {
            let mut merged: BTreeMap<(Id, Id), i64> = BTreeMap::new();
            for x in a.iter().filter(|x| x.quantity > 0) {
                *merged.entry((x.room_id, x.type_id)).or_insert(0) += x.quantity;
            }
            merged.into_iter().map(|((r, t), q)| (r, t, q)).collect()
        }
        None => {
            let items: Vec<(Want, i64)> = loan.items.iter().map(|i| (i.want, i.quantity)).collect();
            let ev = engine::evaluate(&world, window, &items);
            if !ev.fully_available && !b.force {
                let short: Vec<Value> = ev
                    .items
                    .iter()
                    .filter(|i| i.shortfall > 0)
                    .map(|i| json!({ "kind_id": i.want.kind_id(), "type_id": i.want.type_id(), "shortfall": i.shortfall }))
                    .collect();
                return Err(ApiError::conflict(
                    "Not everything requested is available. Approve a partial loan, or resolve the shortage first.",
                    Some(json!({ "shortfall": short })),
                ));
            }
            let mut merged: BTreeMap<(Id, Id), i64> = BTreeMap::new();
            for p in ev.items.iter().flat_map(|i| &i.allocation) {
                *merged.entry((p.room_id, p.type_id)).or_insert(0) += p.qty;
            }
            merged.into_iter().map(|((r, t), q)| (r, t, q)).collect()
        }
    };
    if allocations.is_empty() {
        return Err(ApiError::conflict("Nothing can be allocated for this request", None));
    }
    let problems = engine::check_allocation(&world, window, &allocations);
    if !problems.is_empty() && !b.force {
        return Err(ApiError::conflict(
            "Some rooms can't spare that many during this period",
            Some(json!(problems)),
        ));
    }

    let tx = conn.transaction()?;
    tx.execute("DELETE FROM loan_allocations WHERE loan_id = ?1", [id])?;
    for (r, t, q) in &allocations {
        tx.execute(
            "INSERT INTO loan_allocations(loan_id, room_id, type_id, quantity) VALUES (?1, ?2, ?3, ?4)",
            params![id, r, t, q],
        )?;
    }
    tx.execute(
        "UPDATE loans SET status = 'approved', decided_by = ?1, updated_at = ?2 WHERE id = ?3",
        params![staff.display_name, fmt_dt(now), id],
    )?;
    if let Some(m) = &b.message {
        tx.execute("UPDATE loans SET message_to_organiser = ?1 WHERE id = ?2", params![clean(m, "Message")?, id])?;
    }
    tx.commit()?;
    Ok(Json(json!({ "ok": true })))
}

fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "declined")
            | ("pending", "cancelled")
            | ("approved", "collected")
            | ("approved", "cancelled")
            | ("approved", "pending")
            | ("collected", "returned")
            | ("declined", "pending")
            | ("cancelled", "pending")
    )
}

/// `by` is who made the decision, shown on the request.
fn set_status(conn: &mut Connection, loan: &LoanDto, to: &str, message: Option<&str>, by: &str) -> Result<(), ApiError> {
    if !transition_allowed(&loan.status, to) {
        return Err(ApiError::conflict(format!("Can't change a {} request to {to}", loan.status), None));
    }
    let tx = conn.transaction()?;
    let by = if to == "pending" { "" } else { by };
    tx.execute(
        "UPDATE loans SET status = ?1, decided_by = ?2, updated_at = ?3 WHERE id = ?4",
        params![to, by, fmt_dt(now()), loan.id],
    )?;
    if to == "pending" {
        tx.execute("DELETE FROM loan_allocations WHERE loan_id = ?1", [loan.id])?;
    }
    if matches!(to, "declined" | "cancelled" | "returned") {
        // Groups only agreed to manage with less for this loan.
        tx.execute("DELETE FROM occurrence_overrides WHERE loan_id = ?1", [loan.id])?;
    }
    if let Some(m) = message {
        tx.execute("UPDATE loans SET message_to_organiser = ?1 WHERE id = ?2", params![clean(m, "Message")?, loan.id])?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Deserialize)]
struct StatusIn {
    status: String,
    message: Option<String>,
}

async fn change_status(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<StatusIn>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    let mut conn = st.conn();
    let loan = get_loan(&conn, id)?;
    set_status(&mut conn, &loan, &b.status, b.message.as_deref(), &staff.display_name)?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_loan(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    staff.require(Role::Av)?;
    st.conn().execute("DELETE FROM loans WHERE id = ?1", [id])?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CheckIn {
    pickup_at: String,
    return_at: String,
    items: Vec<Line>,
}

async fn admin_check(_: Staff, State(st): State<AppState>, Json(b): Json<CheckIn>) -> ApiResult<Value> {
    let conn = st.conn();
    let v = validate_request_core(&conn, &b.pickup_at, &b.return_at, &b.items, false)?;
    let ev = evaluate_request(&conn, &v, None)?;
    Ok(Json(json!({ "evaluation": ev, "refs": load_refs(&conn)? })))
}

// ------------------------------------------------------------------ public

async fn public_info(State(st): State<AppState>) -> ApiResult<Value> {
    let s = db::get_settings(&st.conn())?;
    Ok(Json(json!({ "church_name": s.church_name })))
}

/// What organisers can ask for: lendable models with stock, and the kinds
/// above them (so they can ask for "any" at a requestable level).
async fn public_catalog(State(st): State<AppState>) -> ApiResult<Value> {
    let conn = st.conn();
    let cat = db::load_catalog(&conn)?;
    let models: Vec<EquipmentDto> = list_equipment_rows(&conn)?.into_iter().filter(|e| e.lendable && e.total > 0).collect();
    let used: HashSet<Id> = models
        .iter()
        .flat_map(|m| cat.path(Want::Model(m.id)))
        .filter_map(Want::kind_id)
        .collect();
    let kinds: Vec<Value> = list_kind_rows(&conn)?
        .into_iter()
        .filter(|k| used.contains(&k.id))
        .map(|k| json!({ "id": k.id, "name": k.name, "parent_id": k.parent_id, "requestable": k.requestable,
                         "sort_order": k.sort_order, "notes": k.notes }))
        .collect();
    let models: Vec<Value> = models
        .into_iter()
        .map(|e| json!({ "id": e.id, "name": e.name, "kind_id": e.kind_id, "notes": e.notes }))
        .collect();
    Ok(Json(json!({ "kinds": kinds, "models": models })))
}

async fn public_check_handler(State(st): State<AppState>, Json(b): Json<CheckIn>) -> ApiResult<PublicCheck> {
    let conn = st.conn();
    let v = validate_request_core(&conn, &b.pickup_at, &b.return_at, &b.items, true)?;
    let ev = evaluate_request(&conn, &v, None)?;
    Ok(Json(public_check(&conn, &ev)?))
}

async fn public_create(State(st): State<AppState>, Json(b): Json<LoanIn>) -> ApiResult<Value> {
    let mut conn = st.conn();
    let (_, token, v) = insert_loan(&mut conn, &b, true)?;
    let ev = evaluate_request(&conn, &v, None)?;
    Ok(Json(json!({ "token": token, "check": public_check(&conn, &ev)? })))
}

async fn public_status(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult<Value> {
    let conn = st.conn();
    let loan = get_loan_by_token(&conn, &token)?;
    let refs = load_refs(&conn)?;
    // Collection list (which room to collect from) once approved.
    let show_collection = matches!(loan.status.as_str(), "approved" | "collected" | "returned");
    let lines: Vec<(Want, i64)> = loan.items.iter().map(|i| (i.want, i.quantity)).collect();
    let picks: Vec<engine::Pick> =
        loan.allocations.iter().map(|a| engine::Pick { room_id: a.room_id, type_id: a.type_id, qty: a.quantity }).collect();
    let given = engine::match_allocation(&db::load_catalog(&conn)?, &lines, &picks);
    let items: Vec<Value> = loan
        .items
        .iter()
        .zip(&given)
        .map(|(i, g)| {
            // For "any of a kind", say which models they're getting.
            let models: BTreeMap<String, i64> = g.iter().fold(BTreeMap::new(), |mut m, p| {
                *m.entry(refs.type_name(p.type_id)).or_insert(0) += p.qty;
                m
            });
            let given: Vec<Value> = if show_collection && matches!(i.want, Want::Kind(_)) {
                models.into_iter().map(|(name, quantity)| json!({ "name": name, "quantity": quantity })).collect()
            } else {
                vec![]
            };
            json!({ "name": refs.want_name(i.want), "quantity": i.quantity, "given": given })
        })
        .collect();
    let mut by_room: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if show_collection {
        for a in &loan.allocations {
            let room = refs.rooms.get(&a.room_id).cloned().unwrap_or_default();
            by_room.entry(room).or_default().push(json!({ "name": refs.type_name(a.type_id), "quantity": a.quantity }));
        }
    }
    let collection: Vec<Value> = by_room.into_iter().map(|(room, items)| json!({ "room": room, "items": items })).collect();
    Ok(Json(json!({
        "church_name": db::get_settings(&conn)?.church_name,
        "status": loan.status,
        "organiser_name": loan.organiser_name,
        "event_name": loan.event_name,
        "pickup_at": loan.pickup_at,
        "return_at": loan.return_at,
        "items": items,
        "collection": collection,
        "message": loan.message_to_organiser,
    })))
}

async fn public_cancel(State(st): State<AppState>, Path(token): Path<String>) -> ApiResult<Value> {
    let mut conn = st.conn();
    let loan = get_loan_by_token(&conn, &token)?;
    if !matches!(loan.status.as_str(), "pending" | "approved") {
        return Err(ApiError::conflict("This request can no longer be cancelled online", None));
    }
    set_status(&mut conn, &loan, "cancelled", None, "Organiser")?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ router

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(auth::routes())
        .merge(bookings::routes())
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/rooms", get(list_rooms).post(create_room))
        .route("/api/rooms/{id}", put(update_room).delete(delete_room))
        .route("/api/kinds", get(list_kinds).post(create_kind))
        .route("/api/kinds/{id}", put(update_kind).delete(delete_kind))
        .route("/api/equipment", get(list_equipment).post(create_equipment))
        .route("/api/equipment/{id}", put(update_equipment).delete(delete_equipment))
        .route("/api/stock", get(list_stock).put(put_stock))
        .route("/api/groups", get(list_groups).post(create_group))
        .route("/api/groups/{id}", put(update_group).delete(delete_group))
        .route("/api/events", get(list_events).post(create_event))
        .route("/api/events/{id}", put(update_event).delete(delete_event))
        .route("/api/schedule", get(schedule))
        .route("/api/overrides", post(create_override))
        .route("/api/overrides/{id}", delete(delete_override))
        .route("/api/check", post(admin_check))
        .route("/api/loans", get(list_loans).post(create_loan_admin))
        .route("/api/loans/{id}", get(loan_detail).patch(patch_loan).delete(delete_loan))
        .route("/api/loans/{id}/approve", post(approve_loan))
        .route("/api/loans/{id}/status", post(change_status))
        .route("/api/public/info", get(public_info))
        .route("/api/public/catalog", get(public_catalog))
        .route("/api/public/check", post(public_check_handler))
        .route("/api/public/requests", post(public_create))
        .route("/api/public/requests/{token}", get(public_status))
        .route("/api/public/requests/{token}/cancel", post(public_cancel))
        .with_state(state)
}
