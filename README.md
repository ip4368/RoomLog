# Quippy: church AV equipment & loans

A small self-hosted service for a church AV team:

* Record **what equipment lives in each room**.
* Record **church events** (regular or one-off) and the **minimum equipment each needs to stay in its room**.
* Give external **event organisers a form** to request a loan. The system works out
  whether the gear can be lent **without affecting regular church users**, and
  **which room to collect each item from**.
* If there isn't enough, it tells the AV lead **which regular group to ask** (and
  their point of contact) to manage with less on specific dates, and drafts the message.

It is a single Rust binary (axum + SQLite) with an installable PWA frontend built in.

## Quick start

```sh
cargo build --release

# Optional: load sample rooms, equipment, groups and events to try it out
DATABASE_PATH=quippy.db ./target/release/quippy seed-demo

ADMIN_PASSWORD='choose-a-good-password' DATABASE_PATH=quippy.db ./target/release/quippy
# → http://localhost:8080
```

| Environment variable | Default        | Meaning                                                       |
|----------------------|----------------|---------------------------------------------------------------|
| `ADMIN_PASSWORD`     | *(required)*   | Password for the AV team console (min 8 chars)                |
| `DATABASE_PATH`      | `quippy.db`    | SQLite file. Back this file up (it's the whole system).       |
| `BIND`               | `0.0.0.0:8080` | Listen address                                                |
| `COOKIE_SECURE`      | off            | Set to `1` when served over HTTPS (recommended)               |

Put it behind HTTPS (e.g. Caddy, or a Cloudflare Tunnel) if it's reachable from the internet.
Browsers only offer to install the PWA, and only enable the service worker, over
HTTPS or on `localhost`.

## Using it

**Set up (once):** go to `/admin/login`, then:

1. **Rooms**: e.g. Main Hall, Youth Room, Chapel, AV Store.
2. **Equipment**: types of gear (e.g. "Shure SM58"). Untick *lendable* for installed equipment.
3. **Stock**: a grid of how many of each type live in each room.
4. **Groups**: regular users and their point of contact.
5. **Church events**: e.g. "Sunday Morning Service, Main Hall, every Sunday 08:30–12:30,
   needs 5 × SM58, 4 × DI box…". Events can be one-off, every N weeks, or the
   nth/last weekday of the month.

**When an organiser gets in touch:** send them the link from *Settings* (`/request`).
They pick dates and equipment, can check availability straight away, and submit.
They get a private status link. Once the loan is approved, that page shows
which room to collect each item from. They never see group contact details.

**Reviewing a request** (`Requests` → click one):

* **Availability & where to collect**: per item, how many can be lent and from which rooms.
* **Who to ask** (only if short): each group whose event would need to manage with less,
  with the exact date, item and quantity (e.g. "SM58: 5 → 3"), their contact details,
  and *Copy message* / *Text* / email links with a drafted request.
  When they agree, click **Agreed** and the plan recalculates. The agreement is stored
  against that occurrence only, and is released automatically if the loan is
  declined, cancelled or returned.
* **Approve & reserve** fixes the allocation (or **Approve partial loan** to lend what's
  available), then **Mark collected** → **Mark returned**.

Other tools: **Quick check** (ad-hoc "can we lend X on these dates?"), **Schedule**
(upcoming church events, cancel a single date such as school holidays, see what's out on loan).

## How availability is worked out

* A loan occupies its equipment from **pickup** until **return + turnaround buffer**
  (Settings, default 12 h). Gear that's collected but overdue stays occupied until it's marked returned.
* Only church events that **overlap that window** matter, so a Tuesday–Thursday loan
  ignores Sunday's needs.
* Each room's events are covered **only from that room's own stock**.
* For each room and item: `spare = stock − peak(event needs + other approved loans)` over
  the window. Overlapping events in the same room add up.
* Items are collected from the rooms with the most spare first (fewest pickups).
* If short, the engine picks the room/groups that cover the shortfall while asking the fewest
  people, biggest users first, and states exactly what each would have to drop to.
  Anything that can't be covered even if every group agreed is reported separately
  (not enough owned, or out on another loan).
* Pending requests don't reserve anything. The request page warns you about other
  pending requests competing for the same gear. Approved loans are re-checked
  when you open them, so if a new church event clashes later you get a warning.

## Development

```sh
cargo test          # engine, recurrence and end-to-end API tests
cargo run -- seed-demo && ADMIN_PASSWORD=devpassword cargo run
```

* `src/engine.rs`: pure availability/allocation/negotiation logic (unit-tested)
* `src/recurrence.rs`: expands recurring events into occurrences
* `src/db.rs`: SQLite schema and snapshot loading
* `src/api.rs`: JSON API (admin + public)
* `static/`: the PWA (vanilla JS, no build step), embedded into the binary at compile time
