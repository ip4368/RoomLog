# Equipment kinds: design

Status: v1 built (migration 3). This records why it works the way it does.

## The problem

Today everything is in terms of `equipment_types`, which is really a **model**: "Shure SM58",
"Sennheiser e835". Stock, church event needs, booking needs, loan requests and loan
allocations all point at a single model.

But usually nobody needs a particular model. One organiser wants "3 mics, any kind". Another
wants "2 wireless vocal mics". The Sunday service needs "5 wired vocal mics in the hall".
Asking for a model causes two problems:

1. **False shortages.** Someone asks for 4 × SM58, only 2 are free, but 3 e835s sit spare in the
   same room. The engine says "short by 2" and suggests asking a group to manage with less,
   when the AV team would happily hand out e835s.
2. **The wrong person chooses.** The requester has to pick a model they don't care about. The
   AV team, who know which mics are in good shape or easiest to collect, don't get to choose.

## Decisions (2026-11-18)

| Question | Decision |
|---|---|
| Name for the groups | **Kind**, in the code and the UI. "Type" is avoided because the existing table `equipment_types` holds models. |
| How deep | A **tree of any depth**: Microphones → Vocal mic → Wireless vocal mic → Shure SLX2. |
| What can be requested | **Any level**: a kind at any depth ("any mic", "any wireless vocal mic") or an exact model. This applies to everyone, including external organisers. |

## The tree

```
Microphones                         (kind)
├─ Vocal mic                        (kind)
│  ├─ Wired vocal mic               (kind)
│  │  ├─ Shure SM58                 (model)
│  │  └─ Sennheiser e835            (model)
│  └─ Wireless vocal mic            (kind)
│     └─ Shure SLX2                 (model)
├─ Lapel mic                        (kind)
│  └─ Sennheiser ME2                (model)
└─ Shure Beta 91 boundary mic       (model: can sit directly under any kind)
Cables                              (kind, not requestable as "any": it's just a heading)
├─ XLR cable 10m                    (model)
└─ DI box                           (model)
```

* **Kind**: a node in the tree. Requesting a kind means "any model anywhere below this, the
  AV team picks". Kinds replace today's free-text `category`: the top-level kinds *are* the
  headings.
* **Model**: the existing `equipment_types` row, always a leaf. Stock, lendability and loan
  allocations stay at this level, because that's what physically gets picked up.

## Core rules

1. **The kinds form a real tree.** Each kind has at most one parent, and each model has at most
   one kind. There are no cycles, and no kind or model under two parents. This keeps the engine exact (see
   [Engine](#engine)). A model with no kind sits at the top level on its own.
2. **Models can hang under any kind, not just the deepest.** The boundary mic above doesn't need
   a one-model sub-kind.
3. **Kinds can be marked *not requestable*.** "Any cable" is meaningless, because a DI box is not
   a substitute for an XLR cable. A non-requestable kind is just a heading: you can still request
   requestable kinds and models below it.
4. **A request line is either "any of this kind" or "exactly this model".** This applies to loan
   items, booking needs, church event needs and "manage with less" overrides.
5. **Allocations are always concrete models.** When a loan is approved, the AV team (helped by
   the engine's suggestion) decides *which* models from *which* rooms. `loan_allocations` is
   unchanged.
6. **Lendable stays per model.** "Any mic" on a loan only draws from lendable models. Installed
   models still count towards covering church events in their own room.
7. **Kind names are unique across the whole tree.** A line reads "2 × Wireless vocal mic", never
   an ambiguous "2 × Wireless".

## Schema

```sql
CREATE TABLE equipment_kinds (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    parent_id   INTEGER REFERENCES equipment_kinds(id) ON DELETE RESTRICT,  -- NULL = top level
    requestable INTEGER NOT NULL DEFAULT 1,  -- 0 = heading only, can't ask for "any of these"
    sort_order  INTEGER NOT NULL DEFAULT 0,
    notes       TEXT NOT NULL DEFAULT ''     -- shown to requesters, e.g. "dynamic, with clip"
);
CREATE INDEX idx_kinds_parent ON equipment_kinds(parent_id);

ALTER TABLE equipment_types ADD COLUMN kind_id INTEGER
    REFERENCES equipment_kinds(id) ON DELETE RESTRICT;              -- NULL = top level
ALTER TABLE equipment_types DROP COLUMN category;                   -- replaced by kinds
```

* `ON DELETE RESTRICT`: a kind can't be deleted while it still has sub-kinds or models. Move
  them first. The app also refuses to delete a kind that church event needs or open loans and
  bookings ask for. Only lines on closed requests can be cascade-deleted, the same as for models.
* **No cycles** (a kind under its own descendant): SQL can't express this, so the app checks it.
  On save, it walks up from the new parent and refuses if it reaches the kind itself.

Each request line is "kind XOR model". Shown here for `loan_items`. `booking_needs` and
`event_needs` get the same shape:

```sql
CREATE TABLE loan_items (
    id       INTEGER PRIMARY KEY,
    loan_id  INTEGER NOT NULL REFERENCES loans(id) ON DELETE CASCADE,
    kind_id  INTEGER REFERENCES equipment_kinds(id) ON DELETE CASCADE,  -- "any of this kind"
    type_id  INTEGER REFERENCES equipment_types(id) ON DELETE CASCADE,  -- "exactly this model"
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    CHECK ((kind_id IS NULL) <> (type_id IS NULL))
);
CREATE UNIQUE INDEX ux_loan_items_kind  ON loan_items(loan_id, kind_id) WHERE kind_id IS NOT NULL;
CREATE UNIQUE INDEX ux_loan_items_model ON loan_items(loan_id, type_id) WHERE type_id IS NOT NULL;
```

A line stores **only one** of the two columns. Lines at different levels of the same branch
are allowed together, e.g. "1 × any mic" plus "2 × SLX2".

`occurrence_overrides` gets a nullable `kind_id`. A row with `kind_id` set means "this
occurrence's need for *any of that kind* is reduced to `quantity`". `type_id` set means the
same for an exact model. Both NULL still means "occurrence cancelled".

`stock` and `loan_allocations` are **unchanged** (always `room × model`). Which line each
allocated unit serves is worked out rather than stored. Units are matched to the most specific
line first, which is always correct on a tree. This lets the status page say
"Any mic → Shure SM58 × 1".

## Engine

Today the engine evaluates each `(room, model)` on its own. With kinds it has to look at a
room's whole tree at once, because "any vocal mic" and "SM58" draw on the same mics.

**Always per model, never per kind:**

* Stock.
* Other approved loans (allocations are concrete).

So kind-level demand only comes from church event needs and from the request being evaluated.

### Can a room still cover its own events?

For one room and one time segment, every node `v` (a kind or a model) has:

* `supply(v)`: free units of all models under `v`, after other loans
* `demand(v)`: event needs at `v` *or anywhere below it*

The room's events can all be covered **if and only if `demand(v) ≤ supply(v)` for every node**.
Checking each node on its own is enough because two kinds are always either nested or
disjoint (Hall's theorem on a tree). No search is needed.

### How much can a room lend?

Each node's headroom is the worst case over the request window:

```
cap(v) = min over segments of ( supply(v) − demand(v) )
```

A loan taking `L_m` units of each model `m` from this room fits **if and only if**, for every node
`v`, the units taken from under `v` are at most `cap(v)`. Non-lendable models get cap 0 for loans.
They still count in `supply`, so they still cover events.

### Allocating a request across rooms

The request's lines may sit at different levels and can be served from several rooms. This is
a small max-flow, which is exact for any tree shape:

```
source ─(qty)→ line ─→ (room, lendable model under the line's node)
       (room, model) ─cap→ (room, its kind) ─cap→ (room, parent kind) ─ … ─cap→ sink
```

Every model has exactly one path up its room's tree, so the network stays a tree into the sink.
Sizes are tiny (tens of models × a handful of rooms). A plain augmenting-path implementation
is around 60 lines with no new dependency. The current preferences break ties: fill the most
specific lines first, use the fewest rooms, take from the most spare first, and use the
fewest different models.

### Who to ask when short

The min cut shows which `(room, node)` caps are binding. To free `x` units at a binding node
`v`, reduce event needs that count against `v`, meaning needs at `v` or below it. Order:

1. **Broadest need first.** "Sunday service: vocal mics 5 → 4" frees whichever model is handiest,
   whereas "SM58 5 → 4" frees only that model.
2. **Biggest user first** (as today: fewer people to bother).

An ask is stored as an override on the need's own line (`kind_id` or `type_id`), exactly as it was
asked.

`check_allocation` (re-checking approved loans when they're opened) becomes "does the stored
allocation still fit every `cap(v)` in its rooms".

### Worked example

Main Hall stock: SM58 × 6, e835 × 2, SLX2 × 4. Sunday service needs **5 × Wired vocal mic** and
**2 × SLX2** (pinned for the worship leaders).

| Node | supply | demand | cap |
|---|---|---|---|
| SLX2 | 4 | 2 | 2 |
| Wired vocal mic | 8 | 5 | 3 |
| Wireless vocal mic | 4 | 2 | 2 |
| Vocal mic | 12 | 7 | **5** |

* "4 × any vocal mic" during the service fits. The engine suggests, say, 3 wired + 1 SLX2.
* "4 × Wired vocal mic" is short by 1. Ask: "Sunday service: wired vocal mics 5 → 4".
* Under today's model-only scheme the service would need 5 × SM58 + 2 × SLX2. A request for
  4 × SM58 would then show only 1 spare.

Room booking clash checks don't care about equipment, so they're unaffected. When a booking's
equipment needs are copied to its church event, both kind lines and model lines are copied.

## API

* `GET/POST /api/kinds`, `PUT/DELETE /api/kinds/{id}`: `{id, name, parent_id, requestable, sort_order, notes}`
  (AV role to change).
* Models (`/api/equipment`) have `kind_id` instead of `category`.
* Every request line, need, ask, override and evaluated item is `{kind_id, type_id, …}`, with
  exactly one of the two set. Anything else is rejected.
* `refs` (sent with most staff responses) gains `kinds`. `types` entries have `kind_id` and `lendable`.
* `/api/public/catalog` returns `{kinds, models}`. `/api/public/rooms` gives each room its
  `kinds` (with units below each) alongside `equipment`.
* `allocation_problems` entries are `{room_id, kind_id, type_id, over}`, naming the most specific
  node that's over. `GET /api/loans/{id}` adds `given`: which allocated units serve each line.

## UI

* **Equipment page**: the tree, with a "Inside" / "Kind" dropdown to move a kind or model. Each kind
  shows its total stock and whether it's requestable. "+ Kind" and "+ Model" go on each kind row.
* **Stock grid**: rows per model as now, grouped under kind headings with subtotals.
* **Request forms** (loan `/request`, quick check, church events): one picker listing the whole
  tree, indented. Requestable kinds read "Wireless vocal mic (any)"; non-requestable kinds are
  disabled headings. The **booking form** lists the chosen room's own tree with a quantity box
  per requestable kind and per model, and how many of each the room has.
* **Availability / quick check**: one row per request line ("Any vocal mic: 4 of 4, from Main
  Hall"), with a per-model breakdown in the staff view.
* **Approve**: the loan page has a **Models to hand out** editor seeded with the engine's
  allocation; the AV team can swap models, rooms and quantities. The server accepts a chosen
  allocation only if each unit is stocked in that room and matches a requested line (most
  specific line first, via `match_allocation`), and no line gets more than requested. It is then
  re-validated against the caps. Handing out fewer than requested needs `partial`; leaving a
  room's church events short needs an explicit `force`, which the UI asks the user to confirm.
* **Organiser status page**: once approved it lists the concrete models and rooms (it already
  lists allocations), e.g. "Any vocal mic → 3 × SM58, 1 × SLX2, collect from Main Hall".

## Migration

One new entry in `MIGRATIONS`, applied in a transaction like the others:

1. Create `equipment_kinds`. Insert **one top-level kind per distinct non-empty `category`**
   ("Microphones", "Cables", …) with **`requestable = 0`**, because today they're only headings.
   Set each model's `kind_id`. Models with no category stay at the top level. Then drop
   `equipment_types.category`.
2. Rebuild `loan_items`, `booking_needs` and `event_needs` in the new shape. Copy existing rows as
   **model lines** (`type_id` set), which keeps their current meaning exactly. These are leaf
   tables (nothing references them), so `DROP` + recreate is safe with foreign keys on.
3. `ALTER TABLE occurrence_overrides ADD COLUMN kind_id INTEGER REFERENCES equipment_kinds(id)`.
4. **Don't rebuild `equipment_types`**: with `foreign_keys = ON`, dropping it would
   cascade-delete all stock, needs and allocations. `ADD COLUMN` and `DROP COLUMN` are enough.

Dropping `category` can't be undone, so `db::open` copies an existing database to
`<file>.v<N>-backup-<time>` (with `VACUUM INTO`) before applying any migration. If the copy
fails, nothing is migrated.

Immediately after the migration nothing behaves differently: no kind is requestable yet, and
every existing line names a model. The AV team then builds out the tree (adds "Vocal mic",
"Wired vocal mic", moves models in, ticks *requestable*). Whoever owns each church event can
switch "5 × SM58" to "5 × Wired vocal mic" when ready.

The demo data (`seed-demo`) gets a small tree like the one above so the feature is visible.

## Not in scope

* **One model in two places** (e.g. a mic that is both "vocal" and "instrument"). That breaks the
  tree, and with it the simple per-node check. Pick the more useful parent, or record it as two
  models if it's stocked separately.
* **Substitutes across branches** ("a lapel will do if the handhelds are gone"). The requester can
  ask one level higher ("any mic") instead.
* **Tracking individual units** (serial numbers, condition). Kinds and models are both counts.
