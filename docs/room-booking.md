# Room booking: design

Status: proposal, not built yet.

## The problem today

1. The organiser checks a read-only Google Sheet for availability.
2. The organiser emails church staff.
3. Staff reply yes/no, and are meant to mark it on the sheet as well.

Double bookings happen at step 3. Saying yes and recording the booking are two separate
manual steps, and the second one gets forgotten. The sheet then shows the room as free, and
the next person who asks is also told yes. Nothing is racing here; the record is just out of date.

**Fix:** approving a booking *is* recording it. No second step exists to forget. The public
availability page reads the same data, so it can't fall behind.

## Key decision: an approved booking becomes a church event

RoomLog already models "something happening in a room at a time, needing some equipment":
the `events` table, with recurrence, per-date cancellation (`occurrence_overrides`) and
equipment needs (`event_needs`). Everything else is built on that: the loan engine, the
schedule page and the "who to ask" negotiation.

So a booking is a **request** with its own lifecycle. Approving it **creates an `events` row**
(one-off or recurring), linked back to the booking. That gets us all of this without new code:

* The room shows as busy on the schedule, and in conflict checks for later bookings.
* Equipment the booking needs in that room is protected from external loans, because the engine
  treats it like any other event's `needs`.
* If a loan is short, the AV lead is told to ask that hirer to manage with less, the same way
  they'd ask a regular group.
* Staff can cancel a single date of a recurring booking (e.g. school holidays) using the
  existing override mechanism.

The other option was a separate `bookings` table that the engine also reads. That means two
sources of room occupancy, with recurrence and override logic duplicated across them. Rejected.

## Data model

Schema migrations: `db::open` currently only runs `CREATE TABLE IF NOT EXISTS`. Adding columns
to existing tables needs a small `PRAGMA user_version` migration step, which this feature
introduces.

```sql
ALTER TABLE rooms ADD COLUMN bookable INTEGER NOT NULL DEFAULT 1;  -- untick for "AV Store"
ALTER TABLE rooms ADD COLUMN capacity INTEGER;                     -- optional, shown publicly
ALTER TABLE rooms ADD COLUMN public_notes TEXT NOT NULL DEFAULT ''; -- "Kitchen access, piano"

CREATE TABLE bookings (
    id                   INTEGER PRIMARY KEY,
    token                TEXT NOT NULL UNIQUE,         -- organiser's private status link
    status               TEXT NOT NULL,                -- pending|approved|declined|cancelled
    room_id              INTEGER NOT NULL REFERENCES rooms(id),
    title                TEXT NOT NULL,
    organiser_name       TEXT NOT NULL,
    organisation         TEXT NOT NULL DEFAULT '',
    email                TEXT NOT NULL DEFAULT '',
    phone                TEXT NOT NULL DEFAULT '',
    attendees            INTEGER,
    -- same shape as events, so recurrence::Rule is reused unchanged
    start_date TEXT NOT NULL, start_time TEXT NOT NULL, duration_minutes INTEGER NOT NULL,
    recurrence TEXT NOT NULL, interval_weeks INTEGER NOT NULL DEFAULT 1,
    nth INTEGER NOT NULL DEFAULT 1, until TEXT,
    av_support           INTEGER NOT NULL DEFAULT 0,   -- "we'd like someone from the AV team"
    notes                TEXT NOT NULL DEFAULT '',
    admin_notes          TEXT NOT NULL DEFAULT '',
    message_to_organiser TEXT NOT NULL DEFAULT '',
    event_id             INTEGER REFERENCES events(id) ON DELETE SET NULL, -- set on approval
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);

CREATE TABLE booking_needs (   -- equipment they'll use in that room
    booking_id INTEGER NOT NULL REFERENCES bookings(id) ON DELETE CASCADE,
    type_id    INTEGER NOT NULL REFERENCES equipment_types(id) ON DELETE CASCADE,
    quantity   INTEGER NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (booking_id, type_id)
);
```

New setting: `room_turnaround_minutes` (default 30). It's the gap needed between bookings for
setting up and clearing the room, similar to the existing loan `buffer_hours`.

## Lifecycle

```
pending ──approve──▶ approved ──cancel──▶ cancelled
   │                    │
   └──decline──▶ declined      (approved → pending also allowed, as for loans)
```

* **Approve**: everything runs in one transaction, under the single DB connection mutex that
  already serialises writes.
  1. Re-run the clash check. Return 409 with the clashing dates unless `force`.
  2. Insert the `events` row and copy `booking_needs` into `event_needs`.
  3. Link the event to a **group**: pick an existing one, or "create from organiser's details".
     This is what makes "who to ask" work for regular hirers.
  4. Set `event_id` and `status = approved`.
* **Cancel / decline / back to pending** from approved: delete the linked event. Its needs and
  overrides go with it by cascade.
* **Deleting a booked event** from the Church events page is blocked with "cancel the booking
  instead", so the booking and the event can't disagree.

## Clash rules (pure, unit-tested, next to `engine.rs`)

* A room holds one thing at a time. A clash is any non-cancelled occurrence in the same room
  that overlaps the requested occurrence ± turnaround. That covers church events and approved
  bookings, which are the same thing once approved.
* For recurring requests, expand both sides over the request's date range. Report a clash
  **per date** ("3 of 12 Tuesdays clash: 4 Mar, 11 Mar, 1 Apr"), so staff can approve and
  cancel just those dates.
* Recurring requests must have an end date (max 12 months), so expansion stays bounded.
* Pending requests don't block anything, matching how loans work. The booking page warns about
  **competing pending requests** for the same room and time, so staff decide which comes first.
* **New guard:** creating or editing a church event now warns when it clashes with existing
  occupancy in that room. Otherwise staff editing events directly becomes the new way to
  double-book.
* Overlapping church events that already exist in the data are flagged, never blocked
  retroactively.

## Equipment (where AV leads come in)

* **v1:** the booking form lists the lendable equipment that lives in the chosen room
  (e.g. "Main Hall: projector, 4 × SM58"). The organiser ticks what they'll use, and there's an
  "AV support wanted" box. On approval these become `event_needs`, so the gear is protected from
  external loans for that slot. The booking detail page also checks whether those needs would
  leave an **already approved loan** short, using `engine::check_allocation`. That way staff
  approving a room can see they'd break an AV loan before they do it.
* **v2:** if they need gear that **isn't in that room**, create a linked loan request (pickup =
  booking start, location = room, `loans.booking_id`). It goes through the existing loan
  approval and room-allocation flow, so the AV lead sees it in the normal Requests queue.

## Screens

Public (no login):

* **`/rooms`: availability**, which replaces the Google Sheet. A week view per bookable room
  with busy blocks. Church events show their title. External bookings show just "Booked",
  with no names or contacts.
* **`/book`: booking form.** Room, date, time, optional weekly/fortnightly/monthly repeat with
  an end date, attendees, equipment in that room, AV support, notes, and contact details.
  **Check availability** gives live clash feedback before submitting. Submitting gives a
  private status link, the same pattern as loan requests.
* **`/booking/{token}`: status page.** Shows the decision, the staff message, and a cancel button.

Admin:

* **Bookings** queue in the nav, with a pending count.
* **Booking detail:** clashes per date, competing pending requests, equipment impact,
  approve (with group picker) or decline with a message.
* **Schedule:** booked events are marked as bookings and link back to the request.

## Plan

1. **v1, which stops double booking:** migrations, room flags, bookings API (public + admin),
   transactional approve-with-clash-check, booking → event materialisation, event-edit clash
   warning, `/rooms`, `/book`, booking status page, admin queue and detail, in-room equipment
   needs, demo data, tests (clash engine unit tests plus an end-to-end booking workflow like
   `tests::full_loan_workflow_with_negotiation`).
2. **v1.1:** iCal feed per room (`/rooms/{id}.ics`) so staff can subscribe in Google Calendar,
   and CSV import of the bookings already in the Google Sheet.
3. **v2:** linked equipment loans for gear from other rooms, email notifications (new request
   → staff; decision → organiser) via SMTP settings, and separate staff logins with roles
   (church admin vs AV lead) with "approved by" recorded.

## Open questions

1. **Logins:** can church admin staff and AV leads share the single `ADMIN_PASSWORD` for v1,
   or do they need separate accounts from day one?
2. **Public calendar privacy:** is showing church-event titles publicly OK, with external
   bookings shown only as "Booked"?
3. **Who can book:** anyone with the link (as with loans today), or only known groups?
4. **Turnaround:** is 30 minutes between bookings right, and is it the same for every room?
5. **Existing bookings:** how many future bookings are in the sheet? That decides whether CSV
   import belongs in v1.
