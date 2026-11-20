#!/usr/bin/env python3
"""Roll a RoomLog database back from schema version 3 (equipment kinds) to 2.

Only needed to run a release from before equipment kinds against a database
that a newer release has already upgraded. It refuses to run if anything would
be lost: any loan item, booking need, event need or "manage with less"
agreement that names a kind ("any of a kind") has no version 2 equivalent.

Each model's category becomes the name of the top-level kind it sits under.
A copy of the database is saved next to it first.

    python3 tools/downgrade_v3_to_v2.py roomlog.db

Stop RoomLog before running this.
"""
import datetime
import sqlite3
import sys

LINE_TABLES = ("loan_items", "booking_needs", "event_needs", "occurrence_overrides")

# The version 2 shapes, as created by the original schema and migration 2.
V2_TABLES = """
CREATE TABLE equipment_types_v2 (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL UNIQUE,
    category TEXT NOT NULL DEFAULT '',
    notes    TEXT NOT NULL DEFAULT '',
    lendable INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE event_needs_v2 (
    event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    type_id  INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity INTEGER NOT NULL CHECK (quantity >= 0),
    PRIMARY KEY (event_id, type_id)
);
CREATE TABLE booking_needs_v2 (
    booking_id INTEGER NOT NULL REFERENCES bookings(id) ON DELETE CASCADE,
    type_id    INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity   INTEGER NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (booking_id, type_id)
);
CREATE TABLE loan_items_v2 (
    loan_id  INTEGER NOT NULL REFERENCES loans(id) ON DELETE CASCADE,
    type_id  INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (loan_id, type_id)
);
CREATE TABLE occurrence_overrides_v2 (
    id               INTEGER PRIMARY KEY,
    event_id         INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    occurrence_start TEXT NOT NULL,
    type_id          INTEGER REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity         INTEGER NOT NULL DEFAULT 0,
    note             TEXT NOT NULL DEFAULT '',
    loan_id          INTEGER REFERENCES loans(id) ON DELETE SET NULL,
    created_at       TEXT NOT NULL
);
"""

COPY_AND_SWAP = """
INSERT INTO event_needs_v2(event_id, type_id, quantity) SELECT event_id, type_id, quantity FROM event_needs;
INSERT INTO booking_needs_v2(booking_id, type_id, quantity) SELECT booking_id, type_id, quantity FROM booking_needs;
INSERT INTO loan_items_v2(loan_id, type_id, quantity) SELECT loan_id, type_id, quantity FROM loan_items;
INSERT INTO occurrence_overrides_v2(id, event_id, occurrence_start, type_id, quantity, note, loan_id, created_at)
    SELECT id, event_id, occurrence_start, type_id, quantity, note, loan_id, created_at FROM occurrence_overrides;
DROP TABLE event_needs;
DROP TABLE booking_needs;
DROP TABLE loan_items;
DROP TABLE occurrence_overrides;
DROP TABLE equipment_types;
DROP TABLE equipment_kinds;
ALTER TABLE equipment_types_v2 RENAME TO equipment_types;
ALTER TABLE event_needs_v2 RENAME TO event_needs;
ALTER TABLE booking_needs_v2 RENAME TO booking_needs;
ALTER TABLE loan_items_v2 RENAME TO loan_items;
ALTER TABLE occurrence_overrides_v2 RENAME TO occurrence_overrides;
CREATE INDEX idx_overrides_event ON occurrence_overrides(event_id, occurrence_start);
"""


def fail(msg):
    sys.exit(f"Not downgraded: {msg}")


def main(path):
    conn = sqlite3.connect(path, isolation_level=None)
    version = conn.execute("PRAGMA user_version").fetchone()[0]
    if version != 3:
        fail(f"{path} is at schema version {version}, not 3")
    kind_lines = {t: conn.execute(f"SELECT COUNT(*) FROM {t} WHERE kind_id IS NOT NULL").fetchone()[0] for t in LINE_TABLES}
    if any(kind_lines.values()):
        detail = ", ".join(f"{n} in {t}" for t, n in kind_lines.items() if n)
        fail(f"some lines ask for any of a kind ({detail}); change them to exact models first")

    backup = f"{path}.v3-backup-{datetime.datetime.now():%Y%m%d-%H%M%S}"
    conn.execute("VACUUM INTO ?", (backup,))

    # Category = name of the top-level kind above each model.
    parent = dict(conn.execute("SELECT id, parent_id FROM equipment_kinds"))
    name = dict(conn.execute("SELECT id, name FROM equipment_kinds"))

    def top(kind):
        seen = set()
        while parent.get(kind) is not None and kind not in seen:
            seen.add(kind)
            kind = parent[kind]
        return name.get(kind, "")

    # Rebuilding equipment_types with foreign keys on would cascade-delete
    # stock and allocations, so they are off for the swap and checked after.
    conn.execute("PRAGMA foreign_keys = OFF")
    conn.execute("BEGIN")
    try:
        # Statement by statement: executescript() would commit part-way.
        for stmt in filter(str.strip, V2_TABLES.split(";")):
            conn.execute(stmt)
        rows = conn.execute("SELECT id, name, notes, lendable, kind_id FROM equipment_types").fetchall()
        conn.executemany(
            "INSERT INTO equipment_types_v2(id, name, category, notes, lendable) VALUES (?, ?, ?, ?, ?)",
            [(i, n, top(k) if k is not None else "", notes, lendable) for i, n, notes, lendable, k in rows],
        )
        for stmt in filter(str.strip, COPY_AND_SWAP.split(";")):
            conn.execute(stmt)
        problems = conn.execute("PRAGMA foreign_key_check").fetchall()
        if problems:
            raise RuntimeError(f"foreign key check failed: {problems[:5]}")
        conn.execute("PRAGMA user_version = 2")
        conn.execute("COMMIT")
    except Exception as e:
        conn.execute("ROLLBACK")
        fail(f"{e} (database unchanged)")
    finally:
        conn.execute("PRAGMA foreign_keys = ON")
    print(f"{path} is now at schema version 2. The version 3 database was saved as {backup}.")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
