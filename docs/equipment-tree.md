# Equipment tree: design

Status: built. This records why equipment is a tree and what the levels mean.

## The problem it solves

Equipment used to be a flat list of models ("Shure SM58 vocal mic"). Many
borrowers don't care which model they get — they need "a wired vocal mic",
or just "a mic" — and forcing them to name a model made requests fail or
succeed based on a distinction they never intended. It also forced the AV
team to treat near-identical models as separate pools when working out
what could be lent.

**Fix:** equipment is a tree. Internal nodes are groups people can ask for
as a whole; leaves are the stocked models that physically exist. A request
can name any level, and the AV team decides the concrete models when they
approve.

```
Microphones
├── Wired vocal mic
│   ├── Shure SM58 vocal mic      ← stocked model (leaf)
│   └── Shure Beta 58A vocal mic  ← stocked model (leaf)
└── Wireless mic
    ├── Wireless handheld mic
    └── Wireless lapel mic
```

## Data model (migration 3 in `src/db.rs`)

One table stays the source of truth: `equipment_types` gained a
`parent_id`. A row with children is a group; a leaf is a model. Keeping a
single id space meant every existing foreign key kept working — existing
types stayed leaves with the same ids, stock and history.

| Table                   | References    | Why |
|-------------------------|---------------|-----|
| `stock`                 | leaf only     | Physical units are models. A group total is derived. |
| `loan_allocations`      | leaf only     | The collection list must say exactly what to pick up. |
| `loan_items`            | any node      | What the organiser asked for ("2× Wireless mic"). |
| `booking_needs`         | any node      | What a room booking will use from that room. |
| `event_needs`           | any node      | Minimum that must stay in the room ("any 5 wired mics"). |
| `occurrence_overrides`  | any node      | Per-date reductions match the need they reduce. |

The old free-text `category` column became the top level of the tree on
upgrade (one group per distinct category, existing types parented under
it). It is no longer written for new rows; grouping in the UI comes from
tree position.

Invariants, enforced by the API:

* Stock can only be recorded on a leaf, and a stocked row cannot gain
  children. To group a stocked model, create the group above it and move
  the model under the group — the model keeps its id, stock and history.
* Parents must exist and cannot create a loop.
* A group with children cannot be deleted, and neither can equipment on
  open loans (as an item or as an allocation).

## What a need or request means

* `q × N` means *q units from anywhere in N's subtree*. For an event they
  must stay in the event's room; for a loan the AV team is given specific
  models to hand out.
* Needs from **overlapping occurrences add up, including across levels**:
  one event needing 5× Mic and another needing 2× SM58 at the same time
  in the same room keeps 7 units, at least 2 of them SM58s. This matches
  the pre-tree behaviour for same-model needs.
* **One list cannot name both a group and something inside it.**
  "2× Mic + 1× SM58" is ambiguous (2 in total, or 3?), so loan items,
  booking needs and event needs reject it with an explanation. (Two
  separate lines for sibling groups, e.g. Wired + Wireless, are fine.)
* **Lendability lives on models.** A public loan for a group is fulfilled
  only from lendable models under it, so installed gear inside a group
  (e.g. a ceiling projector under Video) is never handed out implicitly,
  and a group with no lendable models doesn't appear in the public
  catalog. Naming a non-lendable model itself still works for
  staff-created loans, as before.
* When one loan contains overlapping items at different levels (only
  possible for requests predating the rejection rule, or via the API),
  the engine allocates the more specific items first so a broad "Mic"
  line can't consume the SM58s a specific line asked for.

## How availability is worked out (`src/engine.rs`)

The pre-tree engine worked per model: `spare = stock − peak(needs +
loans)`. With needs at any level, the constraints are subtree sums. For
each room and each elementary time slice of the request window, the
engine computes per node:

* `supply` — units physically in the room in that subtree
  (stock minus gear out on other loans), and
* `req` — units church events need from that subtree.

Lending *x* from a subtree is safe in a slice when `x ≤ supply − req`;
the binding cap per node is the minimum over slices. The most that can be
lent under a node is then a bottom-up pass: a leaf lends its cap (if
lendable), a group lends the sum of its children bounded by its own cap,
and a request is additionally bounded by every ancestor's cap (a
"5× Mic must stay" need limits SM58 loans too). Tree (laminar)
constraints make this exact — no search or optimiser is needed.

Allocations name concrete models, taking from the rooms and branches
with the most spare first, as before. "Who to ask" works the same way it
did: for a target allocation, each time slice's excess need is reduced
from the biggest users in the affected subtree, deepest groups first, so
an ask is recorded at the level the event stated its need ("Wired vocal
mic: 5 → 4"). `check_allocation` reports the deepest broken constraint,
which may now be a group.

## User interface

* Every equipment picker shows the tree indented; groups are labelled
  "— any model". Public loan and booking forms explain that picking a
  group means the AV team chooses the model.
* The Equipment page manages the tree (add group or model, move under a
  group). The Stock page lists models only; group totals show on
  Equipment.
* A pending loan shows a **Models to hand out** editor seeded with the
  engine's suggestion. AV can swap models or rooms; approving reserves
  exactly that list, and the organiser's status page shows what they
  asked for (the group) plus the concrete collection list.
* Room booking forms offer the room's equipment at every level with
  pooled counts ("Wired vocal mic (8 in room)").

## Alternatives considered

* **A separate "family" table** with models pointing at it. Two id spaces
  would have forced every needs/items table to become polymorphic, or
  would have limited requests to exactly two levels. The tree gives any
  depth with one id space. Rejected.
* **Overlapping groups** (a model in several groups, e.g. both "Any mic"
  and "Wireless mic"). Needs stop being laminar and availability needs a
  flow optimiser; negotiation asks become ambiguous. The AV team instead
  defines the tree at the granularity people actually choose between.
  Rejected for now.
* **Stock recorded at group level** ("6 wireless handhelds, brands
  untracked"). Honest when models truly aren't tracked, but then a
  request for a specific model under that group can't be answered.
  Creating one leaf per tracked model (plus a "generic" leaf if needed)
  keeps one rule: stock is leaves. Rejected.

## Next

1. If the AV team finds they want "2 mics, one must be an SM58", that
   needs an explicit way to express overlapping lines (additive with a
   guaranteed specific portion) rather than today's rejection.
2. Group-level notes/photos for the public catalog, so borrowers picking
   "Wireless mic" can see what kinds of models they might get.
