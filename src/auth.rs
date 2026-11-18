//! Staff accounts, roles and login sessions.
//!
//! Every staff member has their own login. What they can change depends on
//! their roles; anyone logged in can see everything in the console.

use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{Duration, NaiveDateTime};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{ApiError, ApiResult, AppState, clean, now, random_token, required};
use crate::db::{fmt_dt, parse_dt};
use crate::engine::Id;

const SESSION_COOKIE: &str = "roomlog_session";
const SESSION_DAYS: i64 = 30;
const MIN_PASSWORD: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Manage staff accounts and settings. Implies every other role.
    Admin,
    /// Church office: approve and manage room bookings.
    Bookings,
    /// AV leads: equipment, stock and equipment loans.
    Av,
}

impl Role {
    const ALL: [Role; 3] = [Role::Admin, Role::Bookings, Role::Av];

    fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Bookings => "bookings",
            Role::Av => "av",
        }
    }

    fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// Completes "Your account can't …".
    fn ability(self) -> &'static str {
        match self {
            Role::Admin => "manage staff accounts or settings",
            Role::Bookings => "manage room bookings",
            Role::Av => "manage AV equipment or loans",
        }
    }
}

fn roles_to_db(roles: &[Role]) -> String {
    Role::ALL.iter().filter(|r| roles.contains(r)).map(|r| r.as_str()).collect::<Vec<_>>().join(",")
}

fn roles_from_db(s: &str) -> Vec<Role> {
    s.split(',').filter_map(|r| Role::parse(r.trim())).collect()
}

/// The logged-in staff member. Use as an extractor to require a login.
#[derive(Clone, Debug, Serialize)]
pub struct Staff {
    pub id: Id,
    pub username: String,
    pub display_name: String,
    pub roles: Vec<Role>,
}

impl Staff {
    pub fn has(&self, role: Role) -> bool {
        self.roles.contains(&Role::Admin) || self.roles.contains(&role)
    }

    pub fn require(&self, role: Role) -> Result<(), ApiError> {
        if self.has(role) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!("Your account can't {}", role.ability())))
        }
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

impl FromRequestParts<AppState> for Staff {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = cookie_value(&parts.headers, SESSION_COOKIE).ok_or_else(ApiError::unauthorized)?;
        let conn = state.conn();
        conn.query_row(
            "SELECT u.id, u.username, u.display_name, u.roles FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token = ?1 AND s.expires_at > ?2 AND u.active = 1",
            params![token, fmt_dt(now())],
            |r| {
                Ok(Staff {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    display_name: r.get(2)?,
                    roles: roles_from_db(&r.get::<_, String>(3)?),
                })
            },
        )
        .optional()?
        .ok_or_else(ApiError::unauthorized)
    }
}

// --------------------------------------------------------------- passwords

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(password.as_bytes(), &salt).expect("argon2 hashing cannot fail with default params").to_string()
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// Hashing is CPU-heavy: keep it off the async worker threads.
async fn hash_async(password: String) -> String {
    tokio::task::spawn_blocking(move || hash_password(&password)).await.expect("hash task panicked")
}

async fn verify_async(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &hash)).await.unwrap_or(false)
}

/// Verified against when the username doesn't exist, so response time
/// doesn't reveal which usernames are real.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| hash_password("not-a-real-password"));

fn check_password_strength(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(ApiError::bad(format!("Password must be at least {MIN_PASSWORD} characters")));
    }
    if password.len() > 1000 {
        return Err(ApiError::bad("Password is too long"));
    }
    Ok(())
}

fn check_username(username: &str) -> Result<String, ApiError> {
    let u = username.trim();
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@');
    if u.is_empty() || u.len() > 60 || !u.chars().all(ok_char) {
        return Err(ApiError::bad("Username must be 1–60 letters, digits or . _ - @"));
    }
    Ok(u.to_string())
}

fn check_roles(roles: &[Role]) -> Result<(), ApiError> {
    if roles.is_empty() {
        return Err(ApiError::bad("Give the account at least one role"));
    }
    Ok(())
}

// ---------------------------------------------------------- account admin

pub fn user_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
}

/// Create an account directly (first-run bootstrap, tests).
pub fn create_user(
    conn: &Connection,
    username: &str,
    display_name: &str,
    password: &str,
    roles: &[Role],
) -> Result<Id, ApiError> {
    let username = check_username(username)?;
    check_password_strength(password)?;
    check_roles(roles)?;
    conn.execute(
        "INSERT INTO users(username, display_name, password_hash, roles, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![username, required(display_name, "Name")?, hash_password(password), roles_to_db(roles), fmt_dt(now())],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Reset a password from the command line and re-enable the account.
/// Returns false if there is no such user.
pub fn reset_password(conn: &Connection, username: &str, password: &str) -> Result<bool, ApiError> {
    check_password_strength(password)?;
    let n = conn.execute(
        "UPDATE users SET password_hash = ?1, active = 1 WHERE username = ?2",
        params![hash_password(password), username.trim()],
    )?;
    conn.execute("DELETE FROM sessions WHERE user_id IN (SELECT id FROM users WHERE username = ?1)", [username.trim()])?;
    Ok(n > 0)
}

fn ensure_an_admin_remains(conn: &Connection) -> Result<(), ApiError> {
    let mut stmt = conn.prepare("SELECT roles FROM users WHERE active = 1")?;
    let any_admin = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .any(|roles| roles_from_db(&roles).contains(&Role::Admin));
    if any_admin {
        Ok(())
    } else {
        Err(ApiError::conflict("There must always be at least one active admin account", None))
    }
}

// ---------------------------------------------------------------- handlers

#[derive(Deserialize)]
struct LoginIn {
    username: String,
    password: String,
}

fn session_cookie(token: &str, max_age: i64, secure: bool) -> HeaderValue {
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    );
    HeaderValue::from_str(&cookie).expect("ascii cookie")
}

async fn login(State(st): State<AppState>, Json(body): Json<LoginIn>) -> Result<Response, ApiError> {
    let found: Option<(Id, String, bool)> = st
        .conn()
        .query_row("SELECT id, password_hash, active FROM users WHERE username = ?1", [body.username.trim()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let hash = found.as_ref().map_or_else(|| DUMMY_HASH.clone(), |f| f.1.clone());
    let password_ok = verify_async(body.password, hash).await;
    let user_id = match found {
        Some((id, _, true)) if password_ok => id,
        _ => {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            return Err(ApiError::unauthorized_msg("Wrong username or password"));
        }
    };
    let token = random_token(48);
    {
        let conn = st.conn();
        conn.execute("DELETE FROM sessions WHERE expires_at <= ?1", [fmt_dt(now())])?;
        conn.execute(
            "INSERT INTO sessions(token, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![token, user_id, fmt_dt(now() + Duration::days(SESSION_DAYS))],
        )?;
    }
    let mut resp = Json(json!({ "ok": true })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, session_cookie(&token, SESSION_DAYS * 86400, st.cookie_secure));
    Ok(resp)
}

async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(t) = cookie_value(&headers, SESSION_COOKIE) {
        st.conn().execute("DELETE FROM sessions WHERE token = ?1", [t])?;
    }
    let mut resp = Json(json!({ "ok": true })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, session_cookie("", 0, st.cookie_secure));
    Ok(resp)
}

async fn me(staff: Staff, State(st): State<AppState>) -> ApiResult<Value> {
    let conn = st.conn();
    let count = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |r| r.get(0)) };
    Ok(Json(json!({
        "id": staff.id,
        "username": staff.username,
        "display_name": staff.display_name,
        "roles": staff.roles,
        "pending_loans": count("SELECT COUNT(*) FROM loans WHERE status = 'pending'")?,
    })))
}

#[derive(Deserialize)]
struct PasswordIn {
    current_password: String,
    new_password: String,
}

async fn change_own_password(
    staff: Staff,
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(b): Json<PasswordIn>,
) -> ApiResult<Value> {
    check_password_strength(&b.new_password)?;
    let hash: String = st.conn().query_row("SELECT password_hash FROM users WHERE id = ?1", [staff.id], |r| r.get(0))?;
    if !verify_async(b.current_password, hash).await {
        return Err(ApiError::bad("Current password is wrong"));
    }
    let new_hash = hash_async(b.new_password).await;
    let current = cookie_value(&headers, SESSION_COOKIE).unwrap_or_default();
    let conn = st.conn();
    conn.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![new_hash, staff.id])?;
    // Log out other devices, keep this one.
    conn.execute("DELETE FROM sessions WHERE user_id = ?1 AND token != ?2", params![staff.id, current])?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize)]
struct UserDto {
    id: Id,
    username: String,
    display_name: String,
    email: String,
    roles: Vec<Role>,
    active: bool,
    created_at: NaiveDateTime,
}

#[derive(Deserialize)]
struct UserIn {
    username: String,
    display_name: String,
    #[serde(default)]
    email: String,
    roles: Vec<Role>,
    #[serde(default = "yes")]
    active: bool,
    /// Required for new accounts; leave empty to keep the current password.
    #[serde(default)]
    password: String,
}

fn yes() -> bool {
    true
}

async fn list_users(staff: Staff, State(st): State<AppState>) -> ApiResult<Vec<UserDto>> {
    staff.require(Role::Admin)?;
    let conn = st.conn();
    let mut stmt = conn
        .prepare("SELECT id, username, display_name, email, roles, active, created_at FROM users ORDER BY display_name")?;
    let rows = stmt.query_map([], |r| {
        Ok(UserDto {
            id: r.get(0)?,
            username: r.get(1)?,
            display_name: r.get(2)?,
            email: r.get(3)?,
            roles: roles_from_db(&r.get::<_, String>(4)?),
            active: r.get(5)?,
            created_at: parse_dt(&r.get::<_, String>(6)?).unwrap_or_default(),
        })
    })?;
    Ok(Json(rows.collect::<Result<_, _>>()?))
}

async fn create_user_handler(staff: Staff, State(st): State<AppState>, Json(b): Json<UserIn>) -> ApiResult<Value> {
    staff.require(Role::Admin)?;
    let username = check_username(&b.username)?;
    let display_name = required(&b.display_name, "Name")?;
    let email = clean(&b.email, "Email")?;
    check_roles(&b.roles)?;
    check_password_strength(&b.password)?;
    let hash = hash_async(b.password).await;
    let conn = st.conn();
    conn.execute(
        "INSERT INTO users(username, display_name, email, password_hash, roles, active, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![username, display_name, email, hash, roles_to_db(&b.roles), b.active, fmt_dt(now())],
    )?;
    Ok(Json(json!({ "id": conn.last_insert_rowid() })))
}

async fn update_user(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>, Json(b): Json<UserIn>) -> ApiResult<Value> {
    staff.require(Role::Admin)?;
    let username = check_username(&b.username)?;
    let display_name = required(&b.display_name, "Name")?;
    let email = clean(&b.email, "Email")?;
    check_roles(&b.roles)?;
    if id == staff.id && !b.active {
        return Err(ApiError::bad("You can't disable your own account"));
    }
    let new_hash = if b.password.is_empty() {
        None
    } else {
        check_password_strength(&b.password)?;
        Some(hash_async(b.password).await)
    };
    let mut conn = st.conn();
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE users SET username = ?1, display_name = ?2, email = ?3, roles = ?4, active = ?5 WHERE id = ?6",
        params![username, display_name, email, roles_to_db(&b.roles), b.active, id],
    )?;
    if n == 0 {
        return Err(ApiError::not_found());
    }
    if let Some(h) = &new_hash {
        tx.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![h, id])?;
    }
    if !b.active || (new_hash.is_some() && id != staff.id) {
        tx.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
    }
    ensure_an_admin_remains(&tx)?;
    tx.commit()?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_user(staff: Staff, State(st): State<AppState>, Path(id): Path<Id>) -> ApiResult<Value> {
    staff.require(Role::Admin)?;
    if id == staff.id {
        return Err(ApiError::bad("You can't delete your own account"));
    }
    let mut conn = st.conn();
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM users WHERE id = ?1", [id])?;
    ensure_an_admin_remains(&tx)?;
    tx.commit()?;
    Ok(Json(json!({ "ok": true })))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/me/password", post(change_own_password))
        .route("/api/users", get(list_users).post(create_user_handler))
        .route("/api/users/{id}", put(update_user).delete(delete_user))
}
