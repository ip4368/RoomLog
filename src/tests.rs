//! End-to-end API tests through the real router with an in-memory database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::auth::{self, Role};
use crate::{api, app, db};

struct Client {
    app: Router,
    cookie: Option<String>,
}

impl Client {
    /// A fresh system with one admin account (`admin` / `correct horse`).
    fn new() -> Self {
        let conn = db::open(":memory:").unwrap();
        auth::create_user(&conn, "admin", "Admin", "correct horse", &[Role::Admin]).unwrap();
        Client { app: app(api::AppState::new(conn, false)), cookie: None }
    }

    /// Another session against the same system (e.g. a second staff member).
    fn same_system(&self) -> Self {
        Client { app: self.app.clone(), cookie: None }
    }

    async fn login(&mut self, username: &str, password: &str) {
        self.ok("POST", "/api/login", Some(json!({"username": username, "password": password}))).await;
    }

    async fn login_admin(&mut self) {
        self.login("admin", "correct horse").await;
    }

    async fn call(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(c) = &self.cookie {
            req = req.header(header::COOKIE, c);
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        if let Some(sc) = resp.headers().get(header::SET_COOKIE) {
            self.cookie = Some(sc.to_str().unwrap().split(';').next().unwrap().to_string());
        }
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn ok(&mut self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method, path, body).await;
        assert!(s.is_success(), "{method} {path} -> {s}: {v}");
        v
    }
}

fn next_sunday() -> NaiveDate {
    let mut d = Local::now().date_naive() + Duration::days(1);
    while d.weekday() != Weekday::Sun {
        d += Duration::days(1);
    }
    d
}

#[tokio::test]
async fn admin_endpoints_require_login() {
    let mut c = Client::new();
    let (s, _) = c.call("GET", "/api/rooms", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = c.call("POST", "/api/login", Some(json!({"username": "admin", "password": "nope"}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = c.call("POST", "/api/login", Some(json!({"username": "nobody", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    c.login_admin().await;
    c.ok("GET", "/api/rooms", None).await;
    // Public endpoints and the SPA shell work without a session.
    let mut anon = Client::new();
    anon.ok("GET", "/api/public/catalog", None).await;
    let (s, _) = anon.call("GET", "/request", None).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn full_loan_workflow_with_negotiation() {
    let mut c = Client::new();
    c.login_admin().await;

    let hall = c.ok("POST", "/api/rooms", Some(json!({"name": "Main Hall"}))).await["id"].as_i64().unwrap();
    let youth = c.ok("POST", "/api/rooms", Some(json!({"name": "Youth Room"}))).await["id"].as_i64().unwrap();
    let mic = c.ok("POST", "/api/equipment", Some(json!({"name": "SM58", "category": "Mics"}))).await["id"].as_i64().unwrap();
    let fixed = c
        .ok("POST", "/api/equipment", Some(json!({"name": "Installed projector", "lendable": false})))
        .await["id"]
        .as_i64()
        .unwrap();
    c.ok("PUT", "/api/stock", Some(json!({"room_id": hall, "type_id": mic, "quantity": 6}))).await;
    c.ok("PUT", "/api/stock", Some(json!({"room_id": youth, "type_id": mic, "quantity": 2}))).await;
    c.ok("PUT", "/api/stock", Some(json!({"room_id": hall, "type_id": fixed, "quantity": 1}))).await;
    let group = c
        .ok(
            "POST",
            "/api/groups",
            Some(json!({"name": "Sunday Team", "contact_name": "Sam", "contact_phone": "0400"})),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    let sunday = next_sunday();
    let event = c
        .ok(
            "POST",
            "/api/events",
            Some(json!({
                "title": "Morning Service", "room_id": hall, "group_id": group,
                "start_date": sunday.format("%Y-%m-%d").to_string(), "start_time": "09:00",
                "duration_minutes": 180, "recurrence": "weekly", "interval_weeks": 1,
                "needs": [{"type_id": mic, "quantity": 5}]
            })),
        )
        .await["id"]
        .as_i64()
        .unwrap();

    // Non-lendable equipment is hidden from the public catalog and rejected.
    let catalog = c.ok("GET", "/api/public/catalog", None).await;
    assert_eq!(catalog.as_array().unwrap().len(), 1);

    let sat = sunday - Duration::days(1);
    let pickup = format!("{}T10:00", sat.format("%Y-%m-%d"));
    let ret = format!("{}T20:00", sunday.format("%Y-%m-%d"));
    let (s, _) = c
        .call(
            "POST",
            "/api/public/check",
            Some(json!({"pickup_at": pickup, "return_at": ret, "items": [{"type_id": fixed, "quantity": 1}]})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Over the weekend: hall can spare 1, youth 2 → 3 available, organiser wants 4.
    let created = c
        .ok(
            "POST",
            "/api/public/requests",
            Some(json!({
                "organiser_name": "Pat", "email": "pat@example.org", "event_name": "Camp",
                "pickup_at": pickup, "return_at": ret, "items": [{"type_id": mic, "quantity": 4}]
            })),
        )
        .await;
    assert_eq!(created["check"]["fully_available"], false);
    assert_eq!(created["check"]["items"][0]["available"], 3);
    let token = created["token"].as_str().unwrap().to_string();

    let loans = c.ok("GET", "/api/loans?status=pending", None).await;
    let loan_id = loans[0]["id"].as_i64().unwrap();
    let detail = c.ok("GET", &format!("/api/loans/{loan_id}"), None).await;
    let item = &detail["evaluation"]["items"][0];
    assert_eq!(item["shortfall"], 1);
    let ask = &item["resolutions"][0]["asks"][0];
    assert_eq!(ask["event_id"], event);
    assert_eq!(ask["group_id"], group);
    assert_eq!(ask["normally"], 5);
    assert_eq!(ask["reduce_to"], 4);
    // Contact details are visible to the admin…
    assert_eq!(detail["refs"]["groups"][group.to_string()]["contact_name"], "Sam");

    // …but never to the organiser.
    let status = c.ok("GET", &format!("/api/public/requests/{token}"), None).await;
    assert_eq!(status["status"], "pending");
    assert!(!status.to_string().contains("Sam"));

    // Approving without resolving the shortage needs an explicit force.
    let (s, _) = c.call("POST", &format!("/api/loans/{loan_id}/approve"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Sam agrees to manage with 4 that Sunday.
    c.ok(
        "POST",
        "/api/overrides",
        Some(json!({
            "event_id": event, "occurrence_start": ask["start"], "type_id": mic,
            "quantity": 4, "loan_id": loan_id
        })),
    )
    .await;
    let detail = c.ok("GET", &format!("/api/loans/{loan_id}"), None).await;
    assert_eq!(detail["evaluation"]["fully_available"], true);

    c.ok("POST", &format!("/api/loans/{loan_id}/approve"), Some(json!({"message": "See you Saturday"}))).await;
    let status = c.ok("GET", &format!("/api/public/requests/{token}"), None).await;
    assert_eq!(status["status"], "approved");
    assert_eq!(status["message"], "See you Saturday");
    let collected_total: i64 = status["collection"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["items"].as_array().unwrap().iter().map(|i| i["quantity"].as_i64().unwrap()))
        .sum();
    assert_eq!(collected_total, 4);

    // A second organiser for the same weekend finds nothing left.
    let check = c
        .ok(
            "POST",
            "/api/public/check",
            Some(json!({"pickup_at": pickup, "return_at": ret, "items": [{"type_id": mic, "quantity": 1}]})),
        )
        .await;
    assert_eq!(check["items"][0]["available"], 0);

    // Cancelling releases both the equipment and the group's agreement.
    c.ok("POST", &format!("/api/public/requests/{token}/cancel"), None).await;
    let check = c
        .ok(
            "POST",
            "/api/public/check",
            Some(json!({"pickup_at": pickup, "return_at": ret, "items": [{"type_id": mic, "quantity": 4}]})),
        )
        .await;
    assert_eq!(check["items"][0]["available"], 3);
    let detail = c.ok("GET", &format!("/api/loans/{loan_id}"), None).await;
    assert_eq!(detail["agreed_overrides"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn invalid_transitions_and_input_are_rejected() {
    let mut c = Client::new();
    c.login_admin().await;
    let room = c.ok("POST", "/api/rooms", Some(json!({"name": "Hall"}))).await["id"].as_i64().unwrap();
    let mic = c.ok("POST", "/api/equipment", Some(json!({"name": "Mic"}))).await["id"].as_i64().unwrap();
    c.ok("PUT", "/api/stock", Some(json!({"room_id": room, "type_id": mic, "quantity": 2}))).await;

    let (s, v) = c.call("POST", "/api/rooms", Some(json!({"name": "Hall"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");

    let (s, _) = c
        .call(
            "POST",
            "/api/public/requests",
            Some(json!({
                "organiser_name": "Pat", "email": "p@x", "event_name": "E",
                "pickup_at": "2030-01-02T10:00", "return_at": "2030-01-01T10:00",
                "items": [{"type_id": mic, "quantity": 1}]
            })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, _) = c
        .call(
            "POST",
            "/api/public/requests",
            Some(json!({
                "organiser_name": "Pat", "event_name": "E",
                "pickup_at": "2030-01-01T10:00", "return_at": "2030-01-02T10:00",
                "items": [{"type_id": mic, "quantity": 1}]
            })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "contact details are required");

    let id = c
        .ok(
            "POST",
            "/api/loans",
            Some(json!({
                "organiser_name": "Pat", "pickup_at": "2030-01-01T10:00", "return_at": "2030-01-02T10:00",
                "items": [{"type_id": mic, "quantity": 1}]
            })),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    let (s, _) = c.call("POST", &format!("/api/loans/{id}/status"), Some(json!({"status": "returned"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    c.ok("POST", &format!("/api/loans/{id}/approve"), Some(json!({}))).await;
    c.ok("POST", &format!("/api/loans/{id}/status"), Some(json!({"status": "collected"}))).await;
    c.ok("POST", &format!("/api/loans/{id}/status"), Some(json!({"status": "returned"}))).await;

    // Equipment can't be deleted out from under an open request.
    c.ok(
        "POST",
        "/api/loans",
        Some(json!({
            "organiser_name": "Q", "pickup_at": "2030-02-01T10:00", "return_at": "2030-02-02T10:00",
            "items": [{"type_id": mic, "quantity": 1}]
        })),
    )
    .await;
    let (s, _) = c.call("DELETE", &format!("/api/equipment/{mic}"), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[test]
fn demo_seed_loads() {
    let conn = db::open(":memory:").unwrap();
    assert!(crate::demo::seed(&conn).unwrap());
    assert!(!crate::demo::seed(&conn).unwrap());
    let events = db::load_events(&conn).unwrap();
    assert_eq!(events.len(), 6);
}

#[tokio::test]
async fn staff_roles_are_enforced() {
    let mut admin = Client::new();
    admin.login_admin().await;
    let room = admin.ok("POST", "/api/rooms", Some(json!({"name": "Hall"}))).await["id"].as_i64().unwrap();
    let mic = admin.ok("POST", "/api/equipment", Some(json!({"name": "Mic"}))).await["id"].as_i64().unwrap();
    let office_id = admin
        .ok(
            "POST",
            "/api/users",
            Some(json!({"username": "office", "display_name": "Olive Office", "roles": ["bookings"], "password": "office-pass"})),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    admin
        .ok(
            "POST",
            "/api/users",
            Some(json!({"username": "sound", "display_name": "Sam Sound", "roles": ["av"], "password": "sound-pass"})),
        )
        .await;
    let (s, _) = admin
        .call("POST", "/api/users", Some(json!({"username": "x", "display_name": "X", "roles": [], "password": "long-enough"})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "an account needs a role");

    let mut av = admin.same_system();
    av.login("sound", "sound-pass").await;
    let me = av.ok("GET", "/api/me", None).await;
    assert_eq!(me["roles"], json!(["av"]));
    av.ok("PUT", "/api/stock", Some(json!({"room_id": room, "type_id": mic, "quantity": 2}))).await;
    let (s, _) = av.call("GET", "/api/users", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let mut office = admin.same_system();
    office.login("OFFICE", "office-pass").await; // usernames are case-insensitive
    let (s, v) = office.call("PUT", "/api/stock", Some(json!({"room_id": room, "type_id": mic, "quantity": 5}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    office.ok("GET", "/api/stock", None).await; // can still see everything
    office.ok("POST", "/api/groups", Some(json!({"name": "Choir"}))).await; // shared reference data
    let loan = av
        .ok(
            "POST",
            "/api/loans",
            Some(json!({"organiser_name": "Pat", "pickup_at": "2030-01-01T10:00", "return_at": "2030-01-02T10:00",
                        "items": [{"type_id": mic, "quantity": 1}]})),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    let (s, _) = office.call("POST", &format!("/api/loans/{loan}/approve"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    av.ok("POST", &format!("/api/loans/{loan}/approve"), Some(json!({}))).await;
    let detail = av.ok("GET", &format!("/api/loans/{loan}"), None).await;
    assert_eq!(detail["loan"]["decided_by"], "Sam Sound");

    // Changing your own password keeps you logged in and the old password stops working.
    let (s, _) = office
        .call("POST", "/api/me/password", Some(json!({"current_password": "wrong", "new_password": "new-office-pass"})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    office
        .ok("POST", "/api/me/password", Some(json!({"current_password": "office-pass", "new_password": "new-office-pass"})))
        .await;
    office.ok("GET", "/api/me", None).await;
    let mut again = admin.same_system();
    let (s, _) = again.call("POST", "/api/login", Some(json!({"username": "office", "password": "office-pass"}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Disabling an account ends its sessions and blocks login.
    admin
        .ok(
            "PUT",
            &format!("/api/users/{office_id}"),
            Some(json!({"username": "office", "display_name": "Olive Office", "roles": ["bookings"], "active": false})),
        )
        .await;
    let (s, _) = office.call("GET", "/api/me", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = again.call("POST", "/api/login", Some(json!({"username": "office", "password": "new-office-pass"}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // The last admin can't remove their own admin role, disable or delete themselves.
    let me = admin.ok("GET", "/api/me", None).await;
    let admin_id = me["id"].as_i64().unwrap();
    let (s, _) = admin
        .call(
            "PUT",
            &format!("/api/users/{admin_id}"),
            Some(json!({"username": "admin", "display_name": "Admin", "roles": ["av"]})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = admin.call("DELETE", &format!("/api/users/{admin_id}"), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[test]
fn migrations_upgrade_a_pre_accounts_database() {
    // A database from before staff accounts: old sessions table, user_version 0.
    let path = std::env::temp_dir().join(format!("roomlog-migrate-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (token TEXT PRIMARY KEY, expires_at TEXT NOT NULL);
             INSERT INTO sessions VALUES ('old', '2099-01-01T00:00:00');
             CREATE TABLE rooms (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, notes TEXT NOT NULL DEFAULT '');
             INSERT INTO rooms(name) VALUES ('Main Hall');",
        )
        .unwrap();
    }
    let conn = db::open(path.to_str().unwrap()).unwrap();
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert!(version >= 1);
    let rooms: i64 = conn.query_row("SELECT COUNT(*) FROM rooms", [], |r| r.get(0)).unwrap();
    assert_eq!(rooms, 1, "existing data is kept");
    assert_eq!(auth::user_count(&conn).unwrap(), 0);
    drop(conn);
    // Re-opening is a no-op.
    db::open(path.to_str().unwrap()).unwrap();
    let _ = std::fs::remove_file(&path);
}
