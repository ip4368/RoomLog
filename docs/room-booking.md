# Room booking: design

Status: v1 built. This records why it works the way it does.

## The problem it solves

Before RoomLog:

1. The organiser checks a read-only Google Sheet for availability.
2. The organiser emails church staff.
3. Staff reply yes/no, and are meant to mark it on the sheet as well.

Double bookings happened at step 3. Saying yes and recording the booking were two separate
manual steps, and the second one got forgotten. The sheet then showed the room as free, and
the next person who asked was also told yes. Nothing was racing; the record was just out of
date.

**Fix:** approving a booking *is* recording it, so there's no second step to forget. The
public availability page (`/rooms`) reads the same data, so it can't fall behind.

## Decisions (2026-11-17)

| Question | Decision |
|---|---|
| Logins | **Separate staff accounts** with roles: *admin* (users, settings, everything), *bookings* (church office: decide room bookings), *av* (equipment, stock, loans). The approver's name is recorded on each decision. |
| Public calendar | Church events are shown **by name**. External bookings show "Booked" unless the organiser opts in to showing their event name. |
| Who can book | **Anyone** can submit a request; a staff member **always** approves. Nothing auto-approves. |
| Time boundaries | **Arbitrary, minute-precise** start and end. There's no fixed turnaround gap: the booking blocks exactly the times requested, and organisers include set-up and pack-down. Back-to-back bookings don't clash. One-off bookings can run overnight or over several days (up to 7). |
| Migrating the sheet | About 100 future bookings, so **no CSV import**. Staff enter them with *+ New booking → Save & approve*. |

## Key design choice: an approved booking becomes a church event

RoomLog already modelled "something happening in a room at a time, needing some equipment":
the `events` table, with recurrence, per-date cancellation (`occurrence_overrides`) and
equipment needs (`event_needs`). The loan engine, the schedule and the "who to ask"
negotiation are all built on it.

So a booking is a **request** with its own lifecycle (`bookings` table). Approving it
**creates an `events` row** linked by `bookings.event_id`. That gets us all of this without
new code:

* The room shows as busy, both on the schedule and in conflict checks for later requests.
* The equipment the booking needs in that room is protected from external loans.
* If a loan is short, the AV team is told to ask that hirer to manage with less, the same
  way they'd ask a regular group. That's why approval offers to create a group from the
  organiser's details.
* Single dates can be cancelled (holidays, or the dates skipped because they clashed).

The other option was a separate occupancy source that the engine also reads. That would have
duplicated recurrence and override logic, and made "what's in this room?" a two-place
question. Rejected.

Consequences, all handled:

* Taking a booking off approved (cancelled, or back to pending) deletes its event. Its needs
  and overrides go with it by cascade.
* A booked event can't be deleted from the Church events page ("cancel the booking instead"),
  so the booking and the event can't disagree. It *can* be edited there, and the organiser's
  status page shows the live event's times.

## Clash rules (`src/clash.rs`, pure and unit-tested)

* A room holds one thing at a time. A clash is any non-cancelled occurrence in the same room
  overlapping the requested occurrence, using half-open intervals `[start, end)`.
* Repeating requests are expanded in full (they must end within a year). Clashes are
  reported **per date**, so staff can approve and skip just those dates.
* Pending requests don't block, matching loans. Staff see **competing pending requests** for
  the same room and time, and whichever is approved first wins.
* **Approval re-checks clashes inside the same database lock as the write**, so two staff
  approving overlapping requests at once can't both succeed. The only ways to double-book are
  explicit: *Approve anyway*, or saving an overlapping church event, which succeeds but shows
  the clashing dates.

## Equipment

* The booking form lists the equipment that lives in the chosen room, installed gear
  included. The organiser says what they'll use and can ask for AV help. On approval these
  become the event's needs.
* While a booking is pending, its detail page shows any **approved loan it would leave
  short** (`engine::check_allocation` with and without the booking), so the office can talk
  to the AV team first.

## Data model

See migration 2 in `src/db.rs`: `rooms.bookable/capacity/public_notes`, `bookings`
(the request, its rule in the same shape as `events`, contact details, decision and
`event_id`) and `booking_needs`. Migration 1 added `users` and user-linked `sessions`.
Upgrades are automatic: the first start creates an `admin` account from the old
`ADMIN_PASSWORD`.

## Next

1. **iCal feed per room** (`/rooms/{id}.ics`) so staff can subscribe in Google Calendar.
2. **Email notifications** via SMTP settings: new request → office; decision → organiser.
   The status link covers this for now.
3. **Linked equipment loans** for gear that isn't in the booked room: create a loan request
   (pickup = booking start, location = room) that goes through the normal AV approval flow.
4. Optional per-room minimum gap between bookings, if the office finds they want one after
   all (currently organisers include it in their times).
