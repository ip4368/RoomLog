//! End-to-end API tests through the real router with an in-memory database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::auth::{self, Role};
use crate::{api, app, db, fleet};

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

/// Backups the upgrade made of the database at `path`.
fn backups_of(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let prefix = format!("{}.v", path.file_name().unwrap().to_str().unwrap());
    std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with(&prefix))
        .collect()
}

/// Removes a test database and any backups of it.
fn remove_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for b in backups_of(path) {
        let _ = std::fs::remove_file(b);
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
    assert_eq!(catalog["models"].as_array().unwrap().len(), 1);

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
    assert_eq!(events.len(), 7, "six church events plus one approved booking");
    let bookings: i64 = conn.query_row("SELECT COUNT(*) FROM bookings", [], |r| r.get(0)).unwrap();
    assert_eq!(bookings, 2);
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
    remove_db(&path);
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
    assert_eq!(backups_of(&path).len(), 1, "the old database was copied before upgrading");
    // Re-opening is a no-op, and makes no further backups.
    db::open(path.to_str().unwrap()).unwrap();
    assert_eq!(backups_of(&path).len(), 1);
    remove_db(&path);
}

fn at(d: NaiveDate, hm: &str) -> String {
    format!("{}T{hm}", d.format("%Y-%m-%d"))
}

#[tokio::test]
async fn room_booking_workflow() {
    let mut admin = Client::new();
    admin.login_admin().await;
    let hall = admin.ok("POST", "/api/rooms", Some(json!({"name": "Main Hall", "capacity": 200}))).await["id"].as_i64().unwrap();
    let store = admin.ok("POST", "/api/rooms", Some(json!({"name": "AV Store", "bookable": false}))).await["id"].as_i64().unwrap();
    let mic = admin.ok("POST", "/api/equipment", Some(json!({"name": "SM58", "category": "Mics"}))).await["id"].as_i64().unwrap();
    admin.ok("PUT", "/api/stock", Some(json!({"room_id": hall, "type_id": mic, "quantity": 4}))).await;
    for (u, n, r) in [("office", "Olive Office", "bookings"), ("sound", "Sam Sound", "av")] {
        admin
            .ok("POST", "/api/users", Some(json!({"username": u, "display_name": n, "roles": [r], "password": "password1"})))
            .await;
    }
    let mut office = admin.same_system();
    office.login("office", "password1").await;
    let mut av = admin.same_system();
    av.login("sound", "password1").await;
    let mut public = admin.same_system();

    let sunday = next_sunday();
    let tuesday = sunday + Duration::days(2);
    let saturday = sunday + Duration::days(6);
    admin
        .ok(
            "POST",
            "/api/events",
            Some(json!({"title": "Sunday Service", "room_id": hall, "group_id": null, "start_date": sunday.to_string(),
                        "start_time": "09:00", "duration_minutes": 180, "recurrence": "weekly",
                        "needs": [{"type_id": mic, "quantity": 2}]})),
        )
        .await;

    // Only bookable rooms are offered, with the equipment that lives there.
    let rooms = public.ok("GET", "/api/public/rooms", None).await;
    assert_eq!(rooms.as_array().unwrap().len(), 1);
    assert_eq!(rooms[0]["equipment"][0]["quantity"], 4);

    // Exact time boundaries: overlapping clashes, back-to-back doesn't.
    let slot = |start: String, end: String| json!({"room_id": hall, "start_at": start, "end_at": end});
    let check = public.ok("POST", "/api/public/bookings/check", Some(slot(at(sunday, "11:00"), at(sunday, "13:00")))).await;
    assert_eq!(check["clashes"][0]["with"][0]["title"], "Sunday Service", "church events are shown by name");
    let check = public.ok("POST", "/api/public/bookings/check", Some(slot(at(sunday, "12:00"), at(sunday, "14:00")))).await;
    assert_eq!(check["clashes"].as_array().unwrap().len(), 0);

    let request = |room: i64, title: &str, start: String, end: String, extra: Value| {
        let mut b = json!({"room_id": room, "title": title, "organiser_name": "Casey", "organisation": "Community Choir",
                           "email": "choir@example.org", "start_at": start, "end_at": end});
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };
    let (s, _) = public
        .call("POST", "/api/public/bookings", Some(request(store, "X", at(tuesday, "19:00"), at(tuesday, "21:00"), json!({}))))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "store room isn't bookable");
    let (s, _) = public
        .call(
            "POST",
            "/api/public/bookings",
            Some(request(hall, "X", at(tuesday, "19:00"), at(tuesday, "21:00"), json!({"recurrence": "weekly"}))),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "repeating bookings need an end date");
    let (s, v) = public
        .call(
            "POST",
            "/api/public/bookings",
            Some(request(hall, "X", at(tuesday, "19:00"), at(tuesday, "21:00"), json!({"needs": [{"type_id": mic, "quantity": 5}]}))),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("only has 4"), "{v}");

    // A weekly choir rehearsal for four Tuesdays, using two mics.
    let choir = public
        .ok(
            "POST",
            "/api/public/bookings",
            Some(request(
                hall,
                "Choir rehearsal",
                at(tuesday, "19:00"),
                at(tuesday, "21:00"),
                json!({"recurrence": "weekly", "until": (tuesday + Duration::days(21)).to_string(),
                       "needs": [{"type_id": mic, "quantity": 2}], "av_support": true}),
            )),
        )
        .await;
    assert_eq!(choir["check"]["dates"], 4);
    let choir_token = choir["token"].as_str().unwrap().to_string();
    // A competing request for the first Tuesday evening.
    public
        .ok("POST", "/api/public/bookings", Some(request(hall, "Quiz night", at(tuesday, "20:00"), at(tuesday, "22:00"), json!({}))))
        .await;
    let me = office.ok("GET", "/api/me", None).await;
    assert_eq!(me["pending_bookings"], 2);

    let list = office.ok("GET", "/api/bookings?status=pending", None).await;
    let id_of = |title: &str| list.as_array().unwrap().iter().find(|b| b["title"] == title).unwrap()["id"].as_i64().unwrap();
    let (choir_id, quiz_id) = (id_of("Choir rehearsal"), id_of("Quiz night"));
    let detail = office.ok("GET", &format!("/api/bookings/{choir_id}"), None).await;
    assert_eq!(detail["clashes"].as_array().unwrap().len(), 0);
    assert_eq!(detail["competing_pending"][0]["id"], quiz_id);

    // Only staff with the bookings role can decide.
    let (s, _) = av.call("POST", &format!("/api/bookings/{choir_id}/approve"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let approved = office
        .ok("POST", &format!("/api/bookings/{choir_id}/approve"), Some(json!({"create_group": true, "message": "See you then"})))
        .await;
    let choir_event = approved["event_id"].as_i64().unwrap();
    let detail = office.ok("GET", &format!("/api/bookings/{choir_id}"), None).await;
    assert_eq!(detail["booking"]["status"], "approved");
    assert_eq!(detail["booking"]["decided_by"], "Olive Office");
    let groups = office.ok("GET", "/api/groups", None).await;
    assert!(groups.as_array().unwrap().iter().any(|g| g["name"] == "Community Choir" && g["contact_email"] == "choir@example.org"));

    // The approved booking now occupies the room: the competing request clashes.
    let (s, v) = office.call("POST", &format!("/api/bookings/{quiz_id}/approve"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["detail"]["clashes"][0]["with"][0]["event_id"], choir_event);
    let (s, _) = office.call("POST", &format!("/api/bookings/{quiz_id}/approve"), Some(json!({"skip_clashing": true}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "every date clashes");
    office
        .ok("POST", &format!("/api/bookings/{quiz_id}/status"), Some(json!({"status": "declined", "message": "Sorry, the hall is taken"})))
        .await;

    // The public calendar hides external booking names unless the organiser opted in.
    let avail = public.ok("GET", &format!("/api/public/availability?from={sunday}&days=7"), None).await;
    let titles: Vec<&str> = avail["busy"].as_array().unwrap().iter().map(|b| b["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Sunday Service"));
    assert!(titles.contains(&"Booked"));
    assert!(!titles.contains(&"Choir rehearsal"));
    let choir_slot = format!("{}:00", at(tuesday, "19:00"));
    assert!(avail["busy"].as_array().unwrap().iter().any(|b| b["start"] == choir_slot.as_str()), "{avail}");

    // The booking's equipment is protected from loans like any church event's.
    let check = public
        .ok(
            "POST",
            "/api/public/check",
            Some(json!({"pickup_at": at(tuesday, "18:00"), "return_at": at(tuesday, "21:30"),
                        "items": [{"type_id": mic, "quantity": 4}]})),
        )
        .await;
    assert_eq!(check["items"][0]["available"], 2);

    // A booked event can't be deleted out from under its booking.
    let (s, _) = admin.call("DELETE", &format!("/api/events/{choir_event}"), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let sched = admin.ok("GET", &format!("/api/schedule?from={tuesday}&days=1"), None).await;
    assert_eq!(sched["booked_events"][choir_event.to_string()], choir_id);

    // Partly clashing repeat: approve and skip only the clashing date.
    let week3 = tuesday + Duration::days(21);
    office
        .ok(
            "POST",
            "/api/bookings",
            Some(request(hall, "Youth band", at(week3, "20:00"), at(week3, "21:30"),
                         json!({"recurrence": "weekly", "until": (week3 + Duration::days(14)).to_string()}))),
        )
        .await;
    let band_id = office.ok("GET", "/api/bookings?status=pending", None).await[0]["id"].as_i64().unwrap();
    let (s, v) = office.call("POST", &format!("/api/bookings/{band_id}/approve"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"].as_str().unwrap().starts_with("1 of 3 dates"), "{v}");
    let r = office.ok("POST", &format!("/api/bookings/{band_id}/approve"), Some(json!({"skip_clashing": true}))).await;
    assert_eq!(r["skipped_dates"], 1);
    let detail = office.ok("GET", &format!("/api/bookings/{band_id}"), None).await;
    assert_eq!(detail["cancelled_dates"].as_array().unwrap().len(), 1);

    // Editing a church event warns about room clashes but still saves.
    let saved = admin
        .ok(
            "POST",
            "/api/events",
            Some(json!({"title": "Prayer meeting", "room_id": hall, "group_id": null, "start_date": tuesday.to_string(),
                        "start_time": "20:30", "duration_minutes": 60, "recurrence": "none"})),
        )
        .await;
    assert_eq!(saved["clashes"][0]["with"][0]["event_id"], choir_event);

    // The organiser sees the decision and can cancel, which frees the room.
    let status = public.ok("GET", &format!("/api/public/bookings/{choir_token}"), None).await;
    assert_eq!(status["status"], "approved");
    assert_eq!(status["message"], "See you then");
    assert_eq!(status["room"], "Main Hall");
    public.ok("POST", &format!("/api/public/bookings/{choir_token}/cancel"), None).await;
    let avail = public.ok("GET", &format!("/api/public/availability?from={tuesday}&days=1"), None).await;
    assert!(avail["busy"].as_array().unwrap().iter().all(|b| b["start"] != choir_slot.as_str()), "{avail}");
    let events = admin.ok("GET", "/api/events", None).await;
    assert!(events.as_array().unwrap().iter().all(|e| e["id"] != choir_event), "cancelling removes the event");

    // Equipment impact: a pending booking that would leave an approved loan short.
    let loan = av
        .ok(
            "POST",
            "/api/loans",
            Some(json!({"organiser_name": "Pat", "pickup_at": at(saturday, "08:00"), "return_at": at(saturday, "12:00"),
                        "items": [{"type_id": mic, "quantity": 2}]})),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    av.ok("POST", &format!("/api/loans/{loan}/approve"), Some(json!({}))).await;
    office
        .ok(
            "POST",
            "/api/bookings",
            Some(request(hall, "Wedding", at(saturday, "10:00"), at(saturday, "16:00"),
                         json!({"needs": [{"type_id": mic, "quantity": 4}]}))),
        )
        .await;
    let pending = office.ok("GET", "/api/bookings?status=pending", None).await;
    let wedding = pending.as_array().unwrap().iter().find(|b| b["title"] == "Wedding").unwrap()["id"].as_i64().unwrap();
    let detail = office.ok("GET", &format!("/api/bookings/{wedding}"), None).await;
    assert_eq!(detail["equipment_impact"][0]["loan_id"], loan);
    assert_eq!(detail["equipment_impact"][0]["short_by"], 2);
}

#[test]
fn migration_3_turns_categories_into_kinds_and_keeps_everything() {
    let path = std::env::temp_dir().join(format!("roomlog-migrate3-{}.db", std::process::id()));
    remove_db(&path);
    {
        let conn = db::open_at_version(path.to_str().unwrap(), 2).unwrap();
        conn.execute_batch(
            "INSERT INTO rooms(id, name) VALUES (1, 'Hall');
             INSERT INTO equipment_types(id, name, category) VALUES (1, 'SM58', 'Mics'), (2, 'e835', ' Mics '), (3, 'Odd', '');
             INSERT INTO stock VALUES (1, 1, 6), (1, 2, 2), (1, 3, 1);
             INSERT INTO events(id, title, room_id, start_date, start_time, duration_minutes, recurrence)
                 VALUES (1, 'Service', 1, '2026-01-04', '09:00', 60, 'weekly');
             INSERT INTO event_needs VALUES (1, 1, 5);
             INSERT INTO loans(id, token, status, organiser_name, pickup_at, return_at, created_at, updated_at)
                 VALUES (1, 't', 'approved', 'Pat', '2026-01-10T10:00:00', '2026-01-11T10:00:00', 'x', 'x');
             INSERT INTO loan_items VALUES (1, 2, 2);
             INSERT INTO loan_allocations VALUES (1, 1, 2, 2);
             INSERT INTO occurrence_overrides(event_id, occurrence_start, type_id, quantity, created_at)
                 VALUES (1, '2026-01-11T09:00:00', 1, 3, 'x'), (1, '2026-01-18T09:00:00', NULL, 0, 'x');
             INSERT INTO bookings(id, token, status, room_id, title, organiser_name, start_date, start_time,
                 duration_minutes, recurrence, created_at, updated_at)
                 VALUES (1, 'b', 'pending', 1, 'Party', 'Robin', '2026-02-01', '10:00', 60, 'none', 'x', 'x');
             INSERT INTO booking_needs VALUES (1, 1, 2);",
        )
        .unwrap();
    }
    let conn = db::open(path.to_str().unwrap()).unwrap();
    let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    // One not-requestable kind per distinct (trimmed) category.
    assert_eq!(count("SELECT COUNT(*) FROM equipment_kinds WHERE name = 'Mics' AND requestable = 0"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM equipment_kinds"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM equipment_types WHERE kind_id IS NOT NULL"), 2);
    assert_eq!(count("SELECT kind_id IS NULL FROM equipment_types WHERE id = 3"), 1);
    let has_category = conn.prepare("SELECT category FROM equipment_types").is_ok();
    assert!(!has_category, "category column is dropped");
    // Nothing was cascade-deleted, and existing lines are exact-model lines.
    assert_eq!(count("SELECT COUNT(*) FROM stock"), 3);
    assert_eq!(count("SELECT COUNT(*) FROM loan_allocations"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM event_needs WHERE type_id = 1 AND kind_id IS NULL AND quantity = 5"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM loan_items WHERE type_id = 2 AND quantity = 2"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM booking_needs WHERE type_id = 1 AND quantity = 2"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM occurrence_overrides WHERE kind_id IS NULL"), 2);
    let events = db::load_events(&conn).unwrap();
    assert_eq!(events[0].needs.get(&crate::engine::Want::Model(1)), Some(&5));
    let fk: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0)).unwrap();
    assert_eq!(fk, 0);
    drop(conn);
    db::open(path.to_str().unwrap()).unwrap();

    // The pre-upgrade copy is the untouched version 2 database.
    let backups = backups_of(&path);
    assert_eq!(backups.len(), 1, "{backups:?}");
    let old = rusqlite::Connection::open(&backups[0]).unwrap();
    let version: i64 = old.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 2);
    let category: String = old.query_row("SELECT category FROM equipment_types WHERE id = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(category, "Mics");
    drop(old);
    remove_db(&path);
}

#[tokio::test]
async fn equipment_kinds_workflow() {
    let mut c = Client::new();
    c.login_admin().await;
    let id = |v: Value| v["id"].as_i64().unwrap();
    let hall = id(c.ok("POST", "/api/rooms", Some(json!({"name": "Main Hall"}))).await);
    let youth = id(c.ok("POST", "/api/rooms", Some(json!({"name": "Youth Room"}))).await);

    // Microphones > Vocal mic > Wired vocal mic; Cables is only a heading.
    let mics = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Microphones"}))).await);
    let vocal = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Vocal mic", "parent_id": mics}))).await);
    let wired = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Wired vocal mic", "parent_id": vocal}))).await);
    let cables = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Cables", "requestable": false}))).await);
    let (s, _) = c.call("POST", "/api/kinds", Some(json!({"name": "Vocal mic"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "kind names are unique");
    let (s, _) = c.call("PUT", &format!("/api/kinds/{mics}"), Some(json!({"name": "Microphones", "parent_id": wired}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no cycles");

    let sm58 = id(c.ok("POST", "/api/equipment", Some(json!({"name": "SM58", "kind_id": wired}))).await);
    let e835 = id(c.ok("POST", "/api/equipment", Some(json!({"name": "e835", "kind_id": wired}))).await);
    let xlr = id(c.ok("POST", "/api/equipment", Some(json!({"name": "XLR", "kind_id": cables}))).await);
    for (room, t, q) in [(hall, sm58, 3), (hall, e835, 2), (youth, sm58, 1), (hall, xlr, 4)] {
        c.ok("PUT", "/api/stock", Some(json!({"room_id": room, "type_id": t, "quantity": q}))).await;
    }
    let sunday = next_sunday();
    let event = id(c
        .ok(
            "POST",
            "/api/events",
            Some(json!({
                "title": "Service", "room_id": hall, "group_id": null,
                "start_date": sunday.to_string(), "start_time": "09:00", "duration_minutes": 180,
                "recurrence": "weekly", "needs": [{"kind_id": wired, "quantity": 4}]
            })),
        )
        .await);
    let events = c.ok("GET", "/api/events", None).await;
    assert_eq!(events[0]["needs"][0], json!({"kind_id": wired, "type_id": null, "quantity": 4}));

    let catalog = c.ok("GET", "/api/public/catalog", None).await;
    assert_eq!(catalog["kinds"].as_array().unwrap().len(), 4);
    assert_eq!(catalog["models"].as_array().unwrap().len(), 3);

    // Not-requestable kinds are refused; unknown shapes too.
    let weekday = (at(sunday + Duration::days(2), "10:00"), at(sunday + Duration::days(3), "10:00"));
    let check = |items: Value| json!({"pickup_at": weekday.0, "return_at": weekday.1, "items": items});
    let (s, _) = c.call("POST", "/api/public/check", Some(check(json!([{"kind_id": cables, "quantity": 1}])))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = c.call("POST", "/api/public/check", Some(check(json!([{"kind_id": vocal, "type_id": sm58, "quantity": 1}])))).await;
    assert!(s.is_client_error());

    // Midweek all 6 wired mics are free; over Sunday the service keeps 4 of the hall's 5.
    let v = c.ok("POST", "/api/public/check", Some(check(json!([{"kind_id": vocal, "quantity": 6}])))).await;
    assert_eq!(v["fully_available"], true);
    assert_eq!(v["items"][0]["name"], "Vocal mic (any)");
    let weekend = (at(sunday - Duration::days(1), "10:00"), at(sunday, "20:00"));
    let created = c
        .ok(
            "POST",
            "/api/public/requests",
            Some(json!({
                "organiser_name": "Pat", "email": "pat@example.org",
                "pickup_at": weekend.0, "return_at": weekend.1, "items": [{"kind_id": vocal, "quantity": 3}]
            })),
        )
        .await;
    assert_eq!(created["check"]["items"][0]["available"], 2);
    let token = created["token"].as_str().unwrap().to_string();
    let loan_id = c.ok("GET", "/api/loans?status=pending", None).await[0]["id"].as_i64().unwrap();
    let detail = c.ok("GET", &format!("/api/loans/{loan_id}"), None).await;
    let item = &detail["evaluation"]["items"][0];
    assert_eq!(item["kind_id"], vocal);
    assert_eq!(item["shortfall"], 1);
    let ask = &item["resolutions"][0]["asks"][0];
    assert_eq!((ask["kind_id"].as_i64(), ask["normally"].as_i64(), ask["reduce_to"].as_i64()), (Some(wired), Some(4), Some(3)));
    assert_eq!(detail["refs"]["kinds"][wired.to_string()]["name"], "Wired vocal mic");

    // The service agrees to manage with 3 wired mics; now it all fits.
    c.ok(
        "POST",
        "/api/overrides",
        Some(json!({"event_id": event, "occurrence_start": ask["start"], "kind_id": wired, "quantity": 3, "loan_id": loan_id})),
    )
    .await;
    let detail = c.ok("GET", &format!("/api/loans/{loan_id}"), None).await;
    assert_eq!(detail["evaluation"]["fully_available"], true, "{}", detail["evaluation"]);
    c.ok("POST", &format!("/api/loans/{loan_id}/approve"), Some(json!({}))).await;
    let status = c.ok("GET", &format!("/api/public/requests/{token}"), None).await;
    let given: i64 = status["items"][0]["given"].as_array().unwrap().iter().map(|g| g["quantity"].as_i64().unwrap()).sum();
    assert_eq!(given, 3, "{status}");
    assert_eq!(status["items"][0]["name"], "Vocal mic (any)");

    // Kinds in use can't be deleted.
    let (s, _) = c.call("DELETE", &format!("/api/kinds/{wired}"), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "still has models");
    let spare = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Spare", "parent_id": mics}))).await);
    c.ok("PUT", &format!("/api/events/{event}"), Some(json!({
        "title": "Service", "room_id": hall, "group_id": null,
        "start_date": sunday.to_string(), "start_time": "09:00", "duration_minutes": 180,
        "recurrence": "weekly", "needs": [{"kind_id": spare, "quantity": 0}, {"kind_id": vocal, "quantity": 1}]
    }))).await;
    c.ok("DELETE", &format!("/api/kinds/{spare}"), None).await;

    // Booking needs must fit the room together, at every level of the tree.
    let booking = |needs: Value| {
        json!({
            "room_id": hall, "title": "Party", "organiser_name": "Robin", "email": "r@example.org",
            "start_at": at(sunday + Duration::days(4), "18:00"), "end_at": at(sunday + Duration::days(4), "21:00"),
            "needs": needs
        })
    };
    let (s, v) = c.call("POST", "/api/public/bookings/check", Some(booking(json!([{"kind_id": vocal, "quantity": 6}])))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"].as_str().unwrap().contains("only has 5"), "{v}");
    let (s, _) = c
        .call("POST", "/api/public/bookings/check", Some(booking(json!([{"kind_id": vocal, "quantity": 2}, {"type_id": e835, "quantity": 2}]))))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = c
        .call("POST", "/api/public/bookings/check", Some(booking(json!([{"kind_id": vocal, "quantity": 4}, {"type_id": e835, "quantity": 2}]))))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "4 any + 2 e835 is more than the 5 vocal mics");
}

#[tokio::test]
async fn staff_choose_which_models_go_out() {
    let mut c = Client::new();
    c.login_admin().await;
    let id = |v: Value| v["id"].as_i64().unwrap();
    let hall = id(c.ok("POST", "/api/rooms", Some(json!({"name": "Main Hall"}))).await);
    let youth = id(c.ok("POST", "/api/rooms", Some(json!({"name": "Youth Room"}))).await);
    let mics = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Microphones"}))).await);
    let wired = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Wired vocal mic", "parent_id": mics}))).await);
    let ceiling = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Ceiling mic", "parent_id": mics}))).await);
    let cables = id(c.ok("POST", "/api/kinds", Some(json!({"name": "Cables", "requestable": false}))).await);
    let sm58 = id(c.ok("POST", "/api/equipment", Some(json!({"name": "SM58", "kind_id": wired}))).await);
    let e835 = id(c.ok("POST", "/api/equipment", Some(json!({"name": "e835", "kind_id": wired}))).await);
    let fixed = id(c.ok("POST", "/api/equipment", Some(json!({"name": "Array mic", "kind_id": ceiling, "lendable": false}))).await);
    let xlr = id(c.ok("POST", "/api/equipment", Some(json!({"name": "XLR", "kind_id": cables}))).await);
    for (room, t, q) in [(hall, sm58, 3), (hall, e835, 2), (youth, sm58, 1), (hall, xlr, 4), (hall, fixed, 1)] {
        c.ok("PUT", "/api/stock", Some(json!({"room_id": room, "type_id": t, "quantity": q}))).await;
    }
    let sunday = next_sunday();
    let (pickup, ret) = (at(sunday + Duration::days(2), "10:00"), at(sunday + Duration::days(3), "10:00"));

    // The public can't ask for a kind with nothing lendable under it; staff can.
    let request = |items: Value| json!({"organiser_name": "Pat", "email": "p@example.org", "pickup_at": pickup, "return_at": ret, "items": items});
    let (s, v) = c.call("POST", "/api/public/check", Some(request(json!([{"kind_id": ceiling, "quantity": 1}])))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"].as_str().unwrap().contains("Ceiling mic"), "{v}");

    // The youth room's SM58 is needed by an event during the loan.
    c.ok(
        "POST",
        "/api/events",
        Some(json!({
            "title": "Youth night", "room_id": youth, "group_id": null,
            "start_date": (sunday + Duration::days(2)).to_string(), "start_time": "18:00", "duration_minutes": 120,
            "recurrence": "none", "needs": [{"type_id": sm58, "quantity": 1}]
        })),
    )
    .await;

    let loan = id(c.ok("POST", "/api/loans", Some(request(json!([{"kind_id": wired, "quantity": 2}])))).await);
    let approve = |body: Value| (format!("/api/loans/{loan}/approve"), body);
    let alloc = |picks: &[(i64, i64, i64)]| -> Value {
        picks.iter().map(|&(r, t, q)| json!({"room_id": r, "type_id": t, "quantity": q})).collect()
    };

    // Each chosen unit must be owned by the room and serve something requested.
    for (picks, why) in [
        (vec![(hall, xlr, 1), (hall, sm58, 1)], "doesn't match"),
        (vec![(hall, sm58, 5)], "only has 3"),
        (vec![(hall, sm58, 3)], "more than was asked for"),
    ] {
        let (path, body) = approve(json!({"allocations": alloc(&picks)}));
        let (s, v) = c.call("POST", &path, Some(body)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"].as_str().unwrap().contains(why), "{why}: {v}");
    }

    // Handing out less than requested needs `partial`.
    let (path, body) = approve(json!({"allocations": alloc(&[(hall, e835, 1)])}));
    let (s, v) = c.call("POST", &path, Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["detail"]["shortfall"][0]["shortfall"], 1, "{v}");

    // A partial approval still can't overbook; only an explicit force can.
    let (path, body) = approve(json!({"allocations": alloc(&[(youth, sm58, 1)]), "partial": true}));
    let (s, v) = c.call("POST", &path, Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["detail"][0]["room_id"], youth, "{v}");

    // The AV team prefers to send both e835s rather than the suggested SM58s.
    let detail = c.ok("GET", &format!("/api/loans/{loan}"), None).await;
    assert_eq!(detail["evaluation"]["fully_available"], true);
    let (path, body) = approve(json!({"allocations": alloc(&[(hall, e835, 2)])}));
    c.ok("POST", &path, Some(body)).await;
    let detail = c.ok("GET", &format!("/api/loans/{loan}"), None).await;
    assert_eq!(detail["loan"]["status"], "approved");
    assert_eq!(detail["loan"]["allocations"], json!([{"room_id": hall, "type_id": e835, "quantity": 2}]));
    assert_eq!(detail["given"][0][0]["type_id"], e835);

    // Forcing an overbooking is possible, but only on purpose.
    let loan2 = id(c.ok("POST", "/api/loans", Some(request(json!([{"type_id": sm58, "quantity": 1}])))).await);
    let path = format!("/api/loans/{loan2}/approve");
    let (s, _) = c.call("POST", &path, Some(json!({"allocations": alloc(&[(youth, sm58, 1)])}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    c.ok("POST", &path, Some(json!({"allocations": alloc(&[(youth, sm58, 1)]), "force": true}))).await;
}

// ---------------------------------------------------------------- fleet mode

fn test_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("roomlog-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn slug_and_host_rules() {
    for ok in ["st-marys", "a", "church42", "st-marys-2"] {
        assert!(fleet::valid_slug(ok), "{ok}");
    }
    for bad in ["", "www", "St-Marys", "-st-marys", "st-marys-", "st_marys", "st.marys", "../etc", "a/b"] {
        assert!(!fleet::valid_slug(bad), "{bad}");
    }
    assert_eq!(fleet::tenant_from_host("st-marys.example.com", None).as_deref(), Some("st-marys"));
    assert_eq!(fleet::tenant_from_host("st-marys.localhost:8080", None).as_deref(), Some("st-marys"));
    assert_eq!(fleet::tenant_from_host("ST-MARYS.Example.COM:443", None).as_deref(), Some("st-marys"));
    assert_eq!(fleet::tenant_from_host("example.com", Some("example.com")), None);
    assert_eq!(fleet::tenant_from_host("st-marys.example.com", Some("example.com")).as_deref(), Some("st-marys"));
    assert_eq!(fleet::tenant_from_host("st-marys.other.com", Some("example.com")), None);
    assert_eq!(fleet::tenant_from_host("localhost", None), None);
    assert_eq!(fleet::tenant_from_host("127.0.0.1:8080", None), None);
}

#[test]
fn backup_and_check_round_trip() {
    let dir = test_temp_dir("backup");
    let src = dir.join("roomlog.db");
    let dest = dir.join("snapshot.db");
    {
        let conn = db::open(src.to_str().unwrap()).unwrap();
        conn.execute("INSERT INTO rooms(name) VALUES ('Main Hall')", []).unwrap();
    }
    db::backup_database_file(&src, &dest).unwrap();
    assert_eq!(db::check_database_file(&dest).unwrap(), db::current_schema_version());
    let conn = db::open(dest.to_str().unwrap()).unwrap();
    let name: String = conn.query_row("SELECT name FROM rooms", [], |r| r.get(0)).unwrap();
    assert_eq!(name, "Main Hall");
    drop(conn);

    // Refusals: overwrite, missing source, and a file that is not a database.
    assert!(db::backup_database_file(&src, &dest).is_err());
    assert!(db::check_database_file(&dir.join("missing.db")).is_err());
    let junk = dir.join("junk.db");
    std::fs::write(&junk, b"not a database").unwrap();
    assert!(db::check_database_file(&junk).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

struct FleetClient {
    app: Router,
    host: String,
    cookie: Option<String>,
}

impl FleetClient {
    async fn call(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(path).header(header::HOST, &self.host);
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

#[tokio::test]
async fn fleet_tenants_are_isolated() {
    let dir = test_temp_dir("fleet");
    fleet::create_tenant(&dir, "st-marys", "correct horse").unwrap();
    fleet::create_tenant(&dir, "st-pauls", "correct horse").unwrap();
    assert_eq!(fleet::list_tenants(&dir), ["st-marys", "st-pauls"]);
    assert!(fleet::create_tenant(&dir, "st-marys", "correct horse").is_err(), "duplicate tenant refused");
    assert!(fleet::create_tenant(&dir, "../escape", "correct horse").is_err(), "escaping slug refused");

    let router = fleet::fleet_app(fleet::FleetState::new(dir.clone(), Some("example.com".into()), false, 4));
    let mut marys = FleetClient { app: router.clone(), host: "st-marys.example.com".into(), cookie: None };
    let mut pauls = FleetClient { app: router.clone(), host: "st-pauls.example.com".into(), cookie: None };

    // Fleet health needs no tenant; unknown churches 404 without a folder.
    let (s, v) = FleetClient { app: router.clone(), host: "example.com".into(), cookie: None }.call("GET", "/healthz", None).await;
    assert_eq!((s, v["mode"].as_str()), (StatusCode::OK, Some("fleet")));
    let (s, _) = FleetClient { app: router.clone(), host: "unknown.example.com".into(), cookie: None }.call("GET", "/api/public/info", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!dir.join("unknown").exists(), "unknown tenant must not be created on disk");

    // Same room name in both churches: data never crosses over.
    for c in [&mut marys, &mut pauls] {
        c.ok("POST", "/api/login", Some(json!({"username": "admin", "password": "correct horse"}))).await;
    }
    marys.ok("POST", "/api/rooms", Some(json!({"name": "Main Hall"}))).await;
    assert_eq!(marys.ok("GET", "/api/rooms", None).await.as_array().unwrap().len(), 1);
    assert_eq!(pauls.ok("GET", "/api/rooms", None).await.as_array().unwrap().len(), 0);
    // A session cookie from one church means nothing at the other.
    let mut thief = FleetClient { app: router.clone(), host: "st-pauls.example.com".into(), cookie: marys.cookie.clone() };
    let (s, _) = thief.call("GET", "/api/rooms", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Portability: snapshot St Mary's, import it as a new tenant, data travels.
    let snapshot = dir.join("marys-snapshot.db");
    db::backup_database_file(&fleet::tenant_db_path(&dir, "st-marys"), &snapshot).unwrap();
    fleet::import_tenant(&dir, "st-marys-copy", &snapshot).unwrap();
    let mut copy = FleetClient { app: router, host: "st-marys-copy.example.com".into(), cookie: None };
    copy.ok("POST", "/api/login", Some(json!({"username": "admin", "password": "correct horse"}))).await;
    let rooms = copy.ok("GET", "/api/rooms", None).await;
    assert_eq!(rooms[0]["name"], "Main Hall");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backup_all_verify_and_drill() {
    let dir = test_temp_dir("backup-all");
    fleet::create_tenant(&dir, "st-marys", "correct horse").unwrap();
    fleet::create_tenant(&dir, "st-pauls", "correct horse").unwrap();
    {
        let conn = db::open(fleet::tenant_db_path(&dir, "st-marys").to_str().unwrap()).unwrap();
        conn.execute("INSERT INTO rooms(name) VALUES ('Main Hall')", []).unwrap();
    }
    let backups = dir.join("backup-set");
    let entries = fleet::backup_all(&dir, &backups).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(backups.join("manifest.json").is_file());
    assert_eq!(fleet::verify_backups(&backups).unwrap().len(), 2);

    let report = fleet::restore_drill(&dir, "st-marys").unwrap();
    assert_eq!((report.rooms, report.users, report.schema_version), (1, 1, db::current_schema_version()));
    let file_report = fleet::restore_drill_file(&backups.join("st-pauls.db")).unwrap();
    assert_eq!(file_report.rooms, 0);
    assert!(fleet::restore_drill(&dir, "nobody").is_err());

    // A tampered backup must fail verification, naming the tenant.
    let mut bytes = std::fs::read(backups.join("st-marys.db")).unwrap();
    bytes.extend_from_slice(b"tampered");
    std::fs::write(backups.join("st-marys.db"), bytes).unwrap();
    let err = fleet::verify_backups(&backups).unwrap_err();
    assert!(err.contains("st-marys"), "{err}");

    // Generated replication configs name the right paths and replica.
    let cfg = fleet::litestream_config_fleet(&dir, &dir.join("meta"), "s3://bucket/roomlog");
    assert!(cfg.contains("roomlog.db") && cfg.contains("s3://bucket/roomlog") && cfg.contains("watch: true"), "{cfg}");
    let _ = std::fs::remove_dir_all(&dir);
}

