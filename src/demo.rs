//! Sample data so the system can be tried out straight away (`roomlog seed-demo`).

use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use rusqlite::{Connection, params};

pub fn seed(conn: &Connection) -> rusqlite::Result<bool> {
    let rooms: i64 = conn.query_row("SELECT COUNT(*) FROM rooms", [], |r| r.get(0))?;
    if rooms > 0 {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;

    // (name, bookable, capacity, public description)
    let rooms = [
        ("Main Hall", true, Some(250), "Stage, ceiling projector and PA. Kitchen next door."),
        ("Youth Room", true, Some(60), "Couches, projector and games. Ground floor, step-free."),
        ("Chapel", true, Some(80), "Quiet space with piano and a small PA."),
        ("AV Store", false, None, ""),
    ];
    for (name, bookable, capacity, public_notes) in rooms {
        tx.execute(
            "INSERT INTO rooms(name, bookable, capacity, public_notes) VALUES (?1, ?2, ?3, ?4)",
            params![name, bookable, capacity, public_notes],
        )?;
    }
    // The equipment tree: groups first, then the stocked models under them.
    // Anything can be requested at any level — a borrower who just needs
    // "a mic" picks Microphones and the AV team chooses the model.
    let types: [(&str, Option<&str>, bool); 18] = [
        ("Microphones", None, true),
        ("Wired vocal mic", Some("Microphones"), true),
        ("Shure SM58 vocal mic", Some("Wired vocal mic"), true),
        ("Shure Beta 58A vocal mic", Some("Wired vocal mic"), true),
        ("Wireless mic", Some("Microphones"), true),
        ("Wireless handheld mic", Some("Wireless mic"), true),
        ("Wireless lapel mic", Some("Wireless mic"), true),
        ("Stands", None, true),
        ("Mic stand", Some("Stands"), true),
        ("Speaker stand", Some("Stands"), true),
        ("Cables", None, true),
        ("XLR cable 10m", Some("Cables"), true),
        ("DI box", Some("Cables"), true),
        ("Speakers", None, true),
        ("Powered speaker", Some("Speakers"), true),
        ("12-channel mixer", None, true),
        ("Video", None, true),
        ("Projector", Some("Video"), true),
    ];
    for (n, parent, l) in types {
        let parent_id = match parent {
            Some(p) => Some(tx.query_row("SELECT id FROM equipment_types WHERE name = ?1", [p], |r| r.get::<_, i64>(0))?),
            None => None,
        };
        tx.execute(
            "INSERT INTO equipment_types(name, lendable, parent_id) VALUES (?1, ?2, ?3)",
            params![n, l, parent_id],
        )?;
    }
    // One-off rows that don't fit the table above.
    tx.execute(
        "INSERT INTO equipment_types(name, lendable, parent_id) VALUES ('Portable screen', 1,
         (SELECT id FROM equipment_types WHERE name = 'Video'))",
        [],
    )?;
    tx.execute(
        "INSERT INTO equipment_types(name, lendable, parent_id) VALUES ('Ceiling projector (installed)', 0,
         (SELECT id FROM equipment_types WHERE name = 'Video'))",
        [],
    )?;
    let id = |table: &str, name: &str| -> rusqlite::Result<i64> {
        let col = if table == "events" { "title" } else { "name" };
        tx.query_row(&format!("SELECT id FROM {table} WHERE {col} = ?1"), [name], |r| r.get(0))
    };

    // (room, type, qty)
    let stock = [
        ("Main Hall", "Shure SM58 vocal mic", 6),
        ("Main Hall", "Shure Beta 58A vocal mic", 2),
        ("AV Store", "Shure Beta 58A vocal mic", 1),
        ("Main Hall", "Wireless handheld mic", 4),
        ("Main Hall", "Wireless lapel mic", 2),
        ("Main Hall", "Mic stand", 6),
        ("Main Hall", "XLR cable 10m", 12),
        ("Main Hall", "DI box", 4),
        ("Main Hall", "Ceiling projector (installed)", 1),
        ("Youth Room", "Shure SM58 vocal mic", 3),
        ("Youth Room", "Mic stand", 3),
        ("Youth Room", "XLR cable 10m", 4),
        ("Youth Room", "Powered speaker", 2),
        ("Youth Room", "Speaker stand", 2),
        ("Youth Room", "12-channel mixer", 1),
        ("Youth Room", "Projector", 1),
        ("Chapel", "Wireless handheld mic", 2),
        ("Chapel", "Powered speaker", 2),
        ("Chapel", "Speaker stand", 2),
        ("Chapel", "12-channel mixer", 1),
        ("AV Store", "Shure SM58 vocal mic", 2),
        ("AV Store", "Mic stand", 4),
        ("AV Store", "XLR cable 10m", 10),
        ("AV Store", "Powered speaker", 2),
        ("AV Store", "Speaker stand", 2),
        ("AV Store", "Projector", 1),
        ("AV Store", "Portable screen", 2),
    ];
    for (r, t, q) in stock {
        tx.execute(
            "INSERT INTO stock(room_id, type_id, quantity) VALUES (?1, ?2, ?3)",
            params![id("rooms", r)?, id("equipment_types", t)?, q],
        )?;
    }

    let groups = [
        ("Sunday Service Team", "Sam Taylor", "0400 000 001", "sunday@example.org"),
        ("Youth Group", "Jordan Lee", "0400 000 002", "youth@example.org"),
        ("Mandarin Congregation", "Wei Chen", "0400 000 003", "mandarin@example.org"),
        ("Prayer Ministry", "Alex Morgan", "0400 000 004", "prayer@example.org"),
        ("Men's Breakfast", "Chris Brown", "0400 000 005", "breakfast@example.org"),
    ];
    for (n, c, p, e) in groups {
        tx.execute(
            "INSERT INTO groups(name, contact_name, contact_phone, contact_email) VALUES (?1, ?2, ?3, ?4)",
            params![n, c, p, e],
        )?;
    }

    // (title, room, group, start_date, time, minutes, recurrence, interval, nth, needs)
    #[allow(clippy::type_complexity)]
    let events: [(&str, &str, &str, &str, &str, i64, &str, i64, i64, &[(&str, i64)]); 6] = [
        (
            "Sunday Morning Service",
            "Main Hall",
            "Sunday Service Team",
            "2026-01-04",
            "08:30",
            240,
            "weekly",
            1,
            1,
            &[
                ("Shure SM58 vocal mic", 5),
                ("Wireless handheld mic", 3),
                ("Wireless lapel mic", 1),
                ("Mic stand", 5),
                ("XLR cable 10m", 10),
                ("DI box", 4),
            ],
        ),
        (
            "Mandarin Service",
            "Chapel",
            "Mandarin Congregation",
            "2026-01-04",
            "13:30",
            150,
            "weekly",
            1,
            1,
            &[("Wireless handheld mic", 2), ("Powered speaker", 2), ("Speaker stand", 2), ("12-channel mixer", 1)],
        ),
        (
            "Youth Sunday Night",
            "Youth Room",
            "Youth Group",
            "2026-01-04",
            "18:00",
            180,
            "weekly",
            1,
            1,
            &[
                ("Shure SM58 vocal mic", 3),
                ("Mic stand", 3),
                ("XLR cable 10m", 4),
                ("Powered speaker", 2),
                ("12-channel mixer", 1),
                ("Projector", 1),
            ],
        ),
        ("Wednesday Prayer", "Chapel", "Prayer Ministry", "2026-01-07", "19:30", 90, "weekly", 1, 1, &[
            ("Wireless handheld mic", 1),
            ("Powered speaker", 1),
        ]),
        ("Friday Games Night", "Youth Room", "Youth Group", "2026-01-09", "19:00", 150, "weekly", 2, 1, &[
            ("Powered speaker", 2),
            ("Projector", 1),
        ]),
        // Any wireless mic will do: the need is recorded at umbrella level.
        ("Men's Breakfast", "Main Hall", "Men's Breakfast", "2026-01-03", "07:30", 120, "monthly", 1, 1, &[
            ("Wireless mic", 1),
        ]),
    ];
    for (title, room, group, date, time, mins, rec, interval, nth, needs) in events {
        tx.execute(
            "INSERT INTO events(title, room_id, group_id, start_date, start_time, duration_minutes, recurrence, interval_weeks, nth)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![title, id("rooms", room)?, id("groups", group)?, date, time, mins, rec, interval, nth],
        )?;
        let eid = tx.last_insert_rowid();
        for (t, q) in needs {
            tx.execute(
                "INSERT INTO event_needs(event_id, type_id, quantity) VALUES (?1, ?2, ?3)",
                params![eid, id("equipment_types", t)?, q],
            )?;
        }
    }

    // Room bookings, relative to today so they show up as upcoming.
    let next = |wd: Weekday| -> NaiveDate {
        let mut d = Local::now().date_naive() + Duration::days(1);
        while d.weekday() != wd {
            d += Duration::days(1);
        }
        d
    };
    let ts = crate::db::fmt_dt(Local::now().naive_local());
    let tuesday = next(Weekday::Tue);
    let saturday = next(Weekday::Sat);
    // Approved: a community choir rehearsing in the Chapel every Tuesday for ten weeks.
    tx.execute(
        "INSERT INTO groups(name, contact_name, contact_phone, contact_email, notes)
         VALUES ('Harbour Community Choir', 'Casey Nguyen', '0400 000 006', 'choir@example.org', 'Created from room booking #1')",
        [],
    )?;
    tx.execute(
        "INSERT INTO events(title, room_id, group_id, start_date, start_time, duration_minutes, recurrence, interval_weeks, nth, until, notes)
         VALUES ('Choir rehearsal', ?1, ?2, ?3, '19:00', 120, 'weekly', 1, 1, ?4, 'Room booking #1')",
        params![
            id("rooms", "Chapel")?,
            id("groups", "Harbour Community Choir")?,
            tuesday.to_string(),
            (tuesday + Duration::weeks(9)).to_string()
        ],
    )?;
    let choir_event = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO event_needs(event_id, type_id, quantity) VALUES (?1, ?2, 1)",
        params![choir_event, id("equipment_types", "Wireless handheld mic")?],
    )?;
    tx.execute(
        "INSERT INTO bookings(id, token, status, room_id, title, organiser_name, organisation, email, phone, attendees,
             start_date, start_time, duration_minutes, recurrence, until, public_title, event_id, decided_by, decided_at,
             message_to_organiser, created_at, updated_at)
         VALUES (1, ?6, 'approved', ?1, 'Choir rehearsal', 'Casey Nguyen',
             'Harbour Community Choir', 'choir@example.org', '0400 000 006', 30, ?2, '19:00', 120, 'weekly', ?3, 1, ?4,
             'Administrator', ?5, 'Use the side door; the code is on your confirmation email.', ?5, ?5)",
        params![
            id("rooms", "Chapel")?,
            tuesday.to_string(),
            (tuesday + Duration::weeks(9)).to_string(),
            choir_event,
            ts,
            crate::api::random_token(32)
        ],
    )?;
    tx.execute(
        "INSERT INTO booking_needs(booking_id, type_id, quantity) VALUES (1, ?1, 1)",
        [id("equipment_types", "Wireless handheld mic")?],
    )?;
    // Pending: a birthday party in the Main Hall that wants the PA.
    tx.execute(
        "INSERT INTO bookings(id, token, status, room_id, title, organiser_name, email, attendees, start_date, start_time,
             duration_minutes, recurrence, av_support, notes, created_at, updated_at)
         VALUES (2, ?4, 'pending', ?1, '50th birthday party', 'Robin Patel',
             'robin@example.org', 90, ?2, '17:00', 330, 'none', 1, 'Includes an hour to decorate and an hour to clean up.', ?3, ?3)",
        params![id("rooms", "Main Hall")?, saturday.to_string(), ts, crate::api::random_token(32)],
    )?;
    for (t, q) in [("Wireless handheld mic", 2), ("Ceiling projector (installed)", 1)] {
        tx.execute(
            "INSERT INTO booking_needs(booking_id, type_id, quantity) VALUES (2, ?1, ?2)",
            params![id("equipment_types", t)?, q],
        )?;
    }

    tx.commit()?;
    Ok(true)
}
