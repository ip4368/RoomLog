//! Sample data so the system can be tried out straight away (`roomlog seed-demo`).

use rusqlite::{Connection, params};

pub fn seed(conn: &Connection) -> rusqlite::Result<bool> {
    let rooms: i64 = conn.query_row("SELECT COUNT(*) FROM rooms", [], |r| r.get(0))?;
    if rooms > 0 {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;

    let rooms = ["Main Hall", "Youth Room", "Chapel", "AV Store"];
    for r in rooms {
        tx.execute("INSERT INTO rooms(name) VALUES (?1)", [r])?;
    }
    let types = [
        ("Shure SM58 vocal mic", "Microphones", true),
        ("Wireless handheld mic", "Microphones", true),
        ("Wireless lapel mic", "Microphones", true),
        ("Mic stand", "Stands", true),
        ("XLR cable 10m", "Cables", true),
        ("DI box", "Cables", true),
        ("Powered speaker", "Speakers", true),
        ("Speaker stand", "Stands", true),
        ("12-channel mixer", "Mixing", true),
        ("Projector", "Video", true),
        ("Portable screen", "Video", true),
        ("Ceiling projector (installed)", "Video", false),
    ];
    for (n, c, l) in types {
        tx.execute("INSERT INTO equipment_types(name, category, lendable) VALUES (?1, ?2, ?3)", params![n, c, l])?;
    }
    let id = |table: &str, name: &str| -> rusqlite::Result<i64> {
        let col = if table == "events" { "title" } else { "name" };
        tx.query_row(&format!("SELECT id FROM {table} WHERE {col} = ?1"), [name], |r| r.get(0))
    };

    // (room, type, qty)
    let stock = [
        ("Main Hall", "Shure SM58 vocal mic", 6),
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
        ("Men's Breakfast", "Main Hall", "Men's Breakfast", "2026-01-03", "07:30", 120, "monthly", 1, 1, &[
            ("Wireless handheld mic", 1),
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
    tx.commit()?;
    Ok(true)
}
