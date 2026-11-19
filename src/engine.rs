//! The availability / allocation engine.
//!
//! Everything here is pure: the caller loads a [`World`] snapshot (catalog,
//! stock, church event occurrences and already-committed loans that overlap
//! the request window) and asks [`evaluate`] whether a loan request can be met.
//!
//! Equipment is a tree: kinds ("Microphones" > "Vocal mic" > "Wired vocal
//! mic") with models (the stocked `equipment_types`) as leaves. A request line
//! or an event need is a [`Want`]: either "any model below this kind" or
//! "exactly this model". Stock and loan allocations are always per model.
//!
//! Rules:
//! * Each room's church events are covered only from that room's own stock.
//! * Only events/loans that overlap the request window matter (time-aware).
//! * A room's events can be covered iff, for every node `v` of the tree, the
//!   needs at or below `v` fit in the free stock below `v`. Kinds nest (two
//!   kinds are either nested or disjoint), so this per-node check is exact.
//! * So a room can lend `cap(v) = min over the window of (supply − demand)`
//!   from below each node `v`, and a set of models taken from the room fits
//!   iff it stays within `cap(v)` for every node.
//! * Requests spanning several rooms and lines are allocated with a small
//!   max-flow. When that is not enough, the engine works out which event
//!   occurrences would need to manage with less, and by how much.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize, Serializer};

use crate::recurrence::Span;

pub type Id = i64;

// ------------------------------------------------------------------- wants

/// What a request line, event need or override refers to: any model below a
/// kind, or exactly one model. Serialised as `{"kind_id": …, "type_id": …}`
/// with exactly one of the two set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(try_from = "WantRaw")]
pub enum Want {
    Model(Id),
    Kind(Id),
}

#[derive(Deserialize)]
struct WantRaw {
    #[serde(default)]
    kind_id: Option<Id>,
    #[serde(default)]
    type_id: Option<Id>,
}

impl TryFrom<WantRaw> for Want {
    type Error = &'static str;
    fn try_from(r: WantRaw) -> Result<Self, Self::Error> {
        match (r.kind_id, r.type_id) {
            (Some(k), None) => Ok(Want::Kind(k)),
            (None, Some(t)) => Ok(Want::Model(t)),
            _ => Err("each item needs exactly one of kind_id or type_id"),
        }
    }
}

impl Serialize for Want {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("kind_id", &self.kind_id())?;
        m.serialize_entry("type_id", &self.type_id())?;
        m.end()
    }
}

impl Want {
    pub fn kind_id(self) -> Option<Id> {
        match self {
            Want::Kind(k) => Some(k),
            Want::Model(_) => None,
        }
    }
    pub fn type_id(self) -> Option<Id> {
        match self {
            Want::Model(t) => Some(t),
            Want::Kind(_) => None,
        }
    }
}

/// A want with a quantity, as sent to and from the API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    #[serde(flatten)]
    pub want: Want,
    pub quantity: i64,
}

pub fn to_lines(map: &BTreeMap<Want, i64>) -> Vec<Line> {
    map.iter().map(|(&want, &quantity)| Line { want, quantity }).collect()
}

pub fn ser_lines<S: Serializer>(map: &BTreeMap<Want, i64>, s: S) -> Result<S::Ok, S::Error> {
    to_lines(map).serialize(s)
}

// ----------------------------------------------------------------- catalog

/// The shape of the equipment tree.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    /// kind_id -> parent kind.
    pub kind_parent: HashMap<Id, Option<Id>>,
    /// type_id (model) -> its kind.
    pub model_kind: HashMap<Id, Option<Id>>,
    /// Models that can go out on loan when someone asks for "any of a kind".
    pub lendable: HashSet<Id>,
}

/// Deeper than any sensible tree; stops a corrupt (cyclic) tree looping.
const MAX_DEPTH: usize = 32;

impl Catalog {
    pub fn parent(&self, n: Want) -> Option<Want> {
        let p = match n {
            Want::Model(m) => self.model_kind.get(&m).copied().flatten(),
            Want::Kind(k) => self.kind_parent.get(&k).copied().flatten(),
        };
        p.map(Want::Kind)
    }

    /// `n` followed by its ancestors, nearest first.
    pub fn path(&self, n: Want) -> Vec<Want> {
        let mut out = vec![n];
        let mut cur = n;
        while let Some(p) = self.parent(cur) {
            if out.contains(&p) || out.len() >= MAX_DEPTH {
                break;
            }
            out.push(p);
            cur = p;
        }
        out
    }

    /// Is `w` at or below `v`?
    pub fn is_under(&self, w: Want, v: Want) -> bool {
        self.path(w).contains(&v)
    }

    pub fn depth(&self, n: Want) -> usize {
        self.path(n).len()
    }

    /// Can `model` be handed out for a request line `line`? Exact model lines
    /// accept the model even if it isn't normally lendable (staff decided);
    /// "any of a kind" only draws on lendable models.
    pub fn serves(&self, line: Want, model: Id) -> bool {
        match line {
            Want::Model(m) => m == model,
            Want::Kind(_) => self.lendable.contains(&model) && self.is_under(Want::Model(model), line),
        }
    }
}

// ------------------------------------------------------------------- world

/// A concrete occurrence of a church event, with overrides already applied.
#[derive(Clone, Debug)]
pub struct Occurrence {
    pub event_id: Id,
    pub group_id: Option<Id>,
    pub room_id: Id,
    pub span: Span,
    /// What must stay in the room for this occurrence.
    pub needs: BTreeMap<Want, i64>,
}

/// Equipment already promised to another (approved / collected) loan.
#[derive(Clone, Debug)]
pub struct Holding {
    pub loan_id: Id,
    /// When the gear is out of the room (pickup → return + buffer).
    pub span: Span,
    pub room_id: Id,
    pub type_id: Id,
    pub qty: i64,
}

#[derive(Clone, Debug, Default)]
pub struct World {
    pub catalog: Catalog,
    /// (room_id, type_id) -> quantity owned.
    pub stock: HashMap<(Id, Id), i64>,
    pub occurrences: Vec<Occurrence>,
    pub holdings: Vec<Holding>,
}

// ----------------------------------------------------------------- results

/// Units of one model to collect from one room.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Pick {
    pub room_id: Id,
    pub type_id: Id,
    pub qty: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoomAvailability {
    pub room_id: Id,
    /// Units this room owns that could serve the line.
    pub stock: i64,
    /// Peak quantity church events need from below this node during the window.
    pub peak_event_need: i64,
    /// Peak quantity already out on other loans during the window.
    pub peak_on_loan: i64,
    /// Peak of events + loans combined (they may not coincide).
    pub peak_combined: i64,
    /// Units this room alone could lend for this line without affecting anyone.
    pub spare: i64,
    /// True if events + loans already exceed stock at some point.
    pub overcommitted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OccurrenceAsk {
    pub event_id: Id,
    pub group_id: Option<Id>,
    pub room_id: Id,
    /// The need being reduced, exactly as the event states it.
    #[serde(flatten)]
    pub want: Want,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub normally: i64,
    pub reduce_to: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoomResolution {
    pub room_id: Id,
    /// Extra units this room could supply if all `asks` are agreed.
    pub extra: i64,
    pub asks: Vec<OccurrenceAsk>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemEvaluation {
    #[serde(flatten)]
    pub want: Want,
    pub requested: i64,
    /// Total that can be lent right now without asking anyone.
    pub available: i64,
    /// Which models to collect from which rooms.
    pub allocation: Vec<Pick>,
    pub shortfall: i64,
    /// Suggested negotiations that would cover (part of) the shortfall.
    pub resolutions: Vec<RoomResolution>,
    /// Units that cannot be found even if every church event agreed to go
    /// without (stock simply isn't there, or is out on other loans).
    pub unresolvable: i64,
    /// Other loans holding models that could serve this line during the window.
    pub blocking_loans: Vec<Id>,
    pub rooms: Vec<RoomAvailability>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Evaluation {
    pub window: Span,
    pub fully_available: bool,
    pub items: Vec<ItemEvaluation>,
}

/// An allocated model doesn't fit any more: `node` (the model or one of its
/// kinds) in `room_id` is over its headroom by `over`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Problem {
    pub room_id: Id,
    #[serde(flatten)]
    pub node: Want,
    pub over: i64,
}

// ------------------------------------------------------------ room analysis

/// One elementary time segment within the window for a room.
struct Segment {
    /// model -> units out on other loans.
    on_loan: HashMap<Id, i64>,
    /// (index into World::occurrences, need, quantity) active in this segment.
    active: Vec<(usize, Want, i64)>,
}

/// A room's tree and how much headroom each node has over the window.
struct RoomTree {
    room_id: Id,
    /// Models owned (or allocated) here.
    models: Vec<Id>,
    /// Every node on the path of a model or of an event need here.
    nodes: BTreeSet<Want>,
    children: HashMap<Want, Vec<Want>>,
    stock: HashMap<Id, i64>,
    /// Total owned below each node.
    stock_under: HashMap<Want, i64>,
    /// min over segments of (supply − demand); may be negative.
    cap: HashMap<Want, i64>,
    /// min over segments of supply: the headroom if every event went without.
    cap_no_events: HashMap<Want, i64>,
    peak_demand: HashMap<Want, i64>,
    peak_on_loan: HashMap<Want, i64>,
    peak_combined: HashMap<Want, i64>,
    segs: Vec<Segment>,
}

impl RoomTree {
    fn get(map: &HashMap<Want, i64>, n: Want) -> i64 {
        map.get(&n).copied().unwrap_or(0)
    }

    fn supply(&self, cat: &Catalog, seg: &Segment) -> HashMap<Want, i64> {
        let mut out: HashMap<Want, i64> = HashMap::new();
        for &m in &self.models {
            let free = self.stock.get(&m).copied().unwrap_or(0) - seg.on_loan.get(&m).copied().unwrap_or(0);
            for a in cat.path(Want::Model(m)) {
                *out.entry(a).or_insert(0) += free;
            }
        }
        out
    }

    /// Demand at or below each node, after `reduced` ((occ, want) -> units) is taken off.
    fn demand(cat: &Catalog, seg: &Segment, reduced: &HashMap<(usize, Want), i64>) -> HashMap<Want, i64> {
        let mut out: HashMap<Want, i64> = HashMap::new();
        for &(idx, w, q) in &seg.active {
            let q = q - reduced.get(&(idx, w)).copied().unwrap_or(0);
            for a in cat.path(w) {
                *out.entry(a).or_insert(0) += q;
            }
        }
        out
    }

    /// Most this room can supply for `line` alone, given per-node headroom `rc`.
    /// Exact for one line in one room, because the constraints nest.
    fn line_capacity(&self, cat: &Catalog, line: Want, rc: &HashMap<Want, i64>) -> i64 {
        if !self.nodes.contains(&line) {
            return 0;
        }
        let above = cat.path(line).into_iter().skip(1).map(|a| Self::get(rc, a).max(0)).min().unwrap_or(i64::MAX);
        self.sub_capacity(cat, line, line, rc).min(above)
    }

    fn sub_capacity(&self, cat: &Catalog, line: Want, v: Want, rc: &HashMap<Want, i64>) -> i64 {
        let own = Self::get(rc, v).max(0);
        match v {
            Want::Model(m) => {
                if cat.serves(line, m) {
                    own
                } else {
                    0
                }
            }
            Want::Kind(_) => {
                let below: i64 = self
                    .children
                    .get(&v)
                    .map(|cs| cs.iter().map(|&c| self.sub_capacity(cat, line, c, rc)).sum())
                    .unwrap_or(0);
                own.min(below)
            }
        }
    }

    /// Choose `n` units for `line` within headroom `rc`, preferring the
    /// models with the most headroom (fewest different models to hand out).
    fn pick_models(&self, cat: &Catalog, line: Want, rc: &HashMap<Want, i64>, n: i64) -> BTreeMap<Id, i64> {
        let mut out = BTreeMap::new();
        let n = n.min(self.line_capacity(cat, line, rc));
        self.pick_below(cat, line, line, rc, n, &mut out);
        out
    }

    fn pick_below(&self, cat: &Catalog, line: Want, v: Want, rc: &HashMap<Want, i64>, n: i64, out: &mut BTreeMap<Id, i64>) -> i64 {
        if n <= 0 {
            return 0;
        }
        match v {
            Want::Model(m) => {
                let q = n.min(self.sub_capacity(cat, line, v, rc));
                if q > 0 {
                    *out.entry(m).or_insert(0) += q;
                }
                q
            }
            Want::Kind(_) => {
                let mut kids: Vec<(Want, i64)> = self
                    .children
                    .get(&v)
                    .map(|cs| cs.iter().map(|&c| (c, self.sub_capacity(cat, line, c, rc))).collect())
                    .unwrap_or_default();
                kids.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let mut left = n.min(Self::get(rc, v).max(0));
                let mut got = 0;
                for (c, _) in kids {
                    let q = self.pick_below(cat, line, c, rc, left, out);
                    left -= q;
                    got += q;
                }
                got
            }
        }
    }

    /// Units taken below each node, from per-model quantities.
    fn under(&self, cat: &Catalog, per_model: &HashMap<Id, i64>) -> HashMap<Want, i64> {
        let mut out: HashMap<Want, i64> = HashMap::new();
        for (&m, &q) in per_model {
            for a in cat.path(Want::Model(m)) {
                *out.entry(a).or_insert(0) += q;
            }
        }
        out
    }

    /// `base` minus what's already taken below each node.
    fn residual(&self, base: &HashMap<Want, i64>, taken_under: &HashMap<Want, i64>) -> HashMap<Want, i64> {
        self.nodes.iter().map(|&n| (n, Self::get(base, n) - Self::get(taken_under, n))).collect()
    }
}

fn analyse_room(world: &World, room: Id, window: &Span, extra_models: &[Id]) -> RoomTree {
    let cat = &world.catalog;
    let mut models: BTreeSet<Id> =
        world.stock.iter().filter(|((r, _), q)| *r == room && **q > 0).map(|((_, t), _)| *t).collect();
    models.extend(extra_models.iter().copied());

    // (index into World::occurrences, clipped span, needs)
    type Occ = (usize, Span, Vec<(Want, i64)>);
    let occ: Vec<Occ> = world
        .occurrences
        .iter()
        .enumerate()
        .filter(|(_, o)| o.room_id == room)
        .filter_map(|(i, o)| {
            let needs: Vec<(Want, i64)> = o.needs.iter().filter(|(_, q)| **q > 0).map(|(w, q)| (*w, *q)).collect();
            (!needs.is_empty()).then_some(())?;
            Some((i, o.span.clip(window)?, needs))
        })
        .collect();
    let loans: Vec<(Span, Id, i64)> = world
        .holdings
        .iter()
        .filter(|h| h.room_id == room && h.qty > 0)
        .filter_map(|h| Some((h.span.clip(window)?, h.type_id, h.qty)))
        .collect();
    models.extend(loans.iter().map(|l| l.1));

    let mut nodes: BTreeSet<Want> = BTreeSet::new();
    for &m in &models {
        nodes.extend(cat.path(Want::Model(m)));
    }
    for (_, _, needs) in &occ {
        for (w, _) in needs {
            nodes.extend(cat.path(*w));
        }
    }
    let mut children: HashMap<Want, Vec<Want>> = HashMap::new();
    for &n in &nodes {
        if let Some(p) = cat.parent(n) {
            children.entry(p).or_default().push(n);
        }
    }
    let stock: HashMap<Id, i64> = models.iter().map(|&m| (m, world.stock.get(&(room, m)).copied().unwrap_or(0))).collect();
    let mut stock_under: HashMap<Want, i64> = HashMap::new();
    for (&m, &q) in &stock {
        for a in cat.path(Want::Model(m)) {
            *stock_under.entry(a).or_insert(0) += q;
        }
    }

    let mut points = vec![window.start, window.end];
    for (_, s, _) in &occ {
        points.extend([s.start, s.end]);
    }
    for (s, _, _) in &loans {
        points.extend([s.start, s.end]);
    }
    points.sort();
    points.dedup();
    let segs: Vec<Segment> = points
        .windows(2)
        .map(|p| {
            let span = Span::new(p[0], p[1]);
            let mut on_loan: HashMap<Id, i64> = HashMap::new();
            for (s, m, q) in &loans {
                if s.covers(&span) {
                    *on_loan.entry(*m).or_insert(0) += q;
                }
            }
            let active = occ
                .iter()
                .filter(|(_, s, _)| s.covers(&span))
                .flat_map(|(i, _, needs)| needs.iter().map(move |(w, q)| (*i, *w, *q)))
                .collect();
            Segment { on_loan, active }
        })
        .collect();

    let mut tree = RoomTree {
        room_id: room,
        models: models.into_iter().collect(),
        nodes,
        children,
        stock,
        stock_under,
        cap: HashMap::new(),
        cap_no_events: HashMap::new(),
        peak_demand: HashMap::new(),
        peak_on_loan: HashMap::new(),
        peak_combined: HashMap::new(),
        segs: vec![],
    };
    let none = HashMap::new();
    for seg in &segs {
        let supply = tree.supply(cat, seg);
        let demand = RoomTree::demand(cat, seg, &none);
        for &n in &tree.nodes {
            let (s, d) = (RoomTree::get(&supply, n), RoomTree::get(&demand, n));
            let l = RoomTree::get(&tree.stock_under, n) - s;
            let min = |m: &mut HashMap<Want, i64>, v: i64| {
                m.entry(n).and_modify(|x| *x = (*x).min(v)).or_insert(v);
            };
            let max = |m: &mut HashMap<Want, i64>, v: i64| {
                m.entry(n).and_modify(|x| *x = (*x).max(v)).or_insert(v);
            };
            min(&mut tree.cap, s - d);
            min(&mut tree.cap_no_events, s);
            max(&mut tree.peak_demand, d);
            max(&mut tree.peak_on_loan, l);
            max(&mut tree.peak_combined, d + l);
        }
    }
    tree.segs = segs;
    tree
}

fn rooms_with_stock(world: &World) -> Vec<Id> {
    let rooms: BTreeSet<Id> = world.stock.iter().filter(|(_, q)| **q > 0).map(|((r, _), _)| *r).collect();
    rooms.into_iter().collect()
}

// ---------------------------------------------------------------- max-flow

/// A tiny augmenting-path max-flow. Networks here have tens of nodes.
struct Flow {
    adj: Vec<Vec<usize>>,
    to: Vec<usize>,
    cap: Vec<i64>,
}

impl Flow {
    fn new(n: usize) -> Self {
        Flow { adj: vec![vec![]; n], to: vec![], cap: vec![] }
    }
    fn node(&mut self) -> usize {
        self.adj.push(vec![]);
        self.adj.len() - 1
    }
    fn edge(&mut self, u: usize, v: usize, c: i64) -> usize {
        let e = self.to.len();
        self.to.extend([v, u]);
        self.cap.extend([c.max(0), 0]);
        self.adj[u].push(e);
        self.adj[v].push(e + 1);
        e
    }
    /// Flow currently on forward edge `e`.
    fn flow(&self, e: usize) -> i64 {
        self.cap[e + 1]
    }
    fn dfs(&mut self, u: usize, t: usize, f: i64, seen: &mut [bool]) -> i64 {
        if u == t {
            return f;
        }
        seen[u] = true;
        for i in 0..self.adj[u].len() {
            let e = self.adj[u][i];
            let v = self.to[e];
            if self.cap[e] > 0 && !seen[v] {
                let got = self.dfs(v, t, f.min(self.cap[e]), seen);
                if got > 0 {
                    self.cap[e] -= got;
                    self.cap[e ^ 1] += got;
                    return got;
                }
            }
        }
        0
    }
    fn run(&mut self, s: usize, t: usize) {
        loop {
            let mut seen = vec![false; self.adj.len()];
            if self.dfs(s, t, i64::MAX, &mut seen) == 0 {
                break;
            }
        }
    }
}

/// Most specific first: exact models, then the deepest kinds.
fn by_specificity(cat: &Catalog, lines: &mut [(Want, i64)]) {
    lines.sort_by(|a, b| {
        let key = |w: Want| (matches!(w, Want::Kind(_)), std::cmp::Reverse(cat.depth(w)));
        key(a.0).cmp(&key(b.0)).then(a.0.cmp(&b.0))
    });
}

/// Allocate `lines` across rooms. Returns per-line picks, in `lines` order.
fn allocate(world: &World, trees: &[RoomTree], lines: &[(Want, i64)]) -> Vec<Vec<Pick>> {
    let cat = &world.catalog;
    let mut f = Flow::new(2);
    let (s, t) = (0, 1);
    let mut node_of: HashMap<(Id, Want), usize> = HashMap::new();
    for tree in trees {
        for &n in &tree.nodes {
            let id = f.node();
            node_of.insert((tree.room_id, n), id);
        }
    }
    for tree in trees {
        for &n in &tree.nodes {
            let up = cat.parent(n).and_then(|p| node_of.get(&(tree.room_id, p)).copied()).unwrap_or(t);
            f.edge(node_of[&(tree.room_id, n)], up, RoomTree::get(&tree.cap, n));
        }
    }
    // Lines are added most specific first. Augmenting one line at a time
    // still reaches a maximum flow: a line that can't augment now can't
    // later either, because new residual edges only start on paths that
    // already reached the sink.
    let mut line_edges: Vec<Vec<(usize, Id, Id)>> = vec![vec![]; lines.len()];
    for (li, &(want, qty)) in lines.iter().enumerate() {
        let ln = f.node();
        f.edge(s, ln, qty);
        // Prefer rooms that can supply the most for this line (fewest pickups),
        // then the models with the most headroom.
        let mut rooms: Vec<(&RoomTree, i64)> = trees.iter().map(|tr| (tr, tr.line_capacity(cat, want, &tr.cap))).collect();
        rooms.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.room_id.cmp(&b.0.room_id)));
        for (tree, _) in rooms {
            let mut models: Vec<Id> = tree.models.iter().copied().filter(|&m| cat.serves(want, m)).collect();
            models.sort_by_key(|&m| (std::cmp::Reverse(RoomTree::get(&tree.cap, Want::Model(m))), m));
            for m in models {
                let e = f.edge(ln, node_of[&(tree.room_id, Want::Model(m))], i64::MAX / 4);
                line_edges[li].push((e, tree.room_id, m));
            }
        }
        f.run(s, t);
    }
    line_edges
        .into_iter()
        .map(|edges| {
            edges
                .into_iter()
                .filter_map(|(e, room_id, type_id)| {
                    let qty = f.flow(e);
                    (qty > 0).then_some(Pick { room_id, type_id, qty })
                })
                .collect()
        })
        .collect()
}

// ------------------------------------------------------------ negotiation

/// Which occurrences in `tree`'s room must reduce, and to what, so the room can
/// hand out `extra` (per model) on top of what's `taken` already.
fn asks_for(
    world: &World,
    original: &World,
    tree: &RoomTree,
    taken: &HashMap<Id, i64>,
    extra: &BTreeMap<Id, i64>,
) -> Vec<OccurrenceAsk> {
    let cat = &world.catalog;
    let mut total = taken.clone();
    for (&m, &q) in extra {
        *total.entry(m).or_insert(0) += q;
    }
    let total_under = tree.under(cat, &total);
    let extra_under = tree.under(cat, &extra.iter().map(|(&m, &q)| (m, q)).collect());
    // Only nodes affected by the extra units, deepest first: a deep node's
    // excess can only be fixed by needs below it, and fixing it helps every
    // node above.
    let mut relevant: Vec<Want> = tree.nodes.iter().copied().filter(|n| RoomTree::get(&extra_under, *n) > 0).collect();
    relevant.sort_by_key(|&n| (std::cmp::Reverse(cat.depth(n)), n));

    let mut reduction: BTreeMap<(usize, Want), i64> = BTreeMap::new();
    for seg in &tree.segs {
        let supply = tree.supply(cat, seg);
        let mut red: HashMap<(usize, Want), i64> = HashMap::new();
        for &v in &relevant {
            let demand = RoomTree::demand(cat, seg, &red);
            let allowed = RoomTree::get(&supply, v) - RoomTree::get(&total_under, v);
            let mut excess = RoomTree::get(&demand, v) - allowed;
            if excess <= 0 {
                continue;
            }
            // Broadest need first (it frees whichever model is handiest), then
            // the biggest user: fewer people to bother.
            let mut active: Vec<(usize, Want, i64)> = seg
                .active
                .iter()
                .filter(|(_, w, _)| cat.is_under(*w, v))
                .map(|&(i, w, q)| (i, w, q - red.get(&(i, w)).copied().unwrap_or(0)))
                .filter(|a| a.2 > 0)
                .collect();
            active.sort_by(|a, b| cat.depth(a.1).cmp(&cat.depth(b.1)).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
            for (i, w, q) in active {
                if excess <= 0 {
                    break;
                }
                let take = q.min(excess);
                excess -= take;
                *red.entry((i, w)).or_insert(0) += take;
            }
        }
        // A reduction applies to the whole occurrence, so keep the max.
        for (k, r) in red {
            let e = reduction.entry(k).or_insert(0);
            *e = (*e).max(r);
        }
    }
    reduction
        .into_iter()
        .filter(|(_, r)| *r > 0)
        .map(|((idx, want), r)| {
            let o = &world.occurrences[idx];
            let now = o.needs.get(&want).copied().unwrap_or(0);
            let normally = original.occurrences[idx].needs.get(&want).copied().unwrap_or(now);
            OccurrenceAsk {
                event_id: o.event_id,
                group_id: o.group_id,
                room_id: o.room_id,
                want,
                start: o.span.start,
                end: o.span.end,
                normally,
                reduce_to: now - r,
            }
        })
        .collect()
}

/// For each short line, find rooms whose events could manage with less.
/// Works on a copy of the world, applying each suggestion before the next.
fn negotiate(
    world: &World,
    window: &Span,
    rooms: &[Id],
    lines: &[(Want, i64)],
    picks: &[Vec<Pick>],
) -> Vec<(Vec<RoomResolution>, i64)> {
    let cat = &world.catalog;
    let mut work = world.clone();
    let mut taken: HashMap<Id, HashMap<Id, i64>> = HashMap::new();
    for p in picks.iter().flatten() {
        *taken.entry(p.room_id).or_default().entry(p.type_id).or_insert(0) += p.qty;
    }
    let mut out = Vec::new();
    for (li, &(want, qty)) in lines.iter().enumerate() {
        let mut remaining = qty - picks[li].iter().map(|p| p.qty).sum::<i64>();
        let mut resolutions = Vec::new();
        let mut candidates: Vec<Id> = rooms.to_vec();
        while remaining > 0 && !candidates.is_empty() {
            struct Choice {
                room: Id,
                extra: i64,
                models: BTreeMap<Id, i64>,
                asks: Vec<OccurrenceAsk>,
            }
            let options: Vec<Choice> = candidates
                .iter()
                .filter_map(|&room| {
                    let room_taken = taken.get(&room).cloned().unwrap_or_default();
                    let extra_models: Vec<Id> = room_taken.keys().copied().collect();
                    let tree = analyse_room(&work, room, window, &extra_models);
                    let taken_under = tree.under(cat, &room_taken);
                    let rc = tree.residual(&tree.cap, &taken_under);
                    let rc0 = tree.residual(&tree.cap_no_events, &taken_under);
                    let now = tree.line_capacity(cat, want, &rc);
                    let possible = tree.line_capacity(cat, want, &rc0) - now;
                    if possible <= 0 {
                        return None;
                    }
                    let extra = possible.min(remaining);
                    let models = tree.pick_models(cat, want, &rc0, extra);
                    let asks = asks_for(&work, world, &tree, &room_taken, &models);
                    Some(Choice { room, extra: models.values().sum(), models, asks })
                })
                .filter(|o| o.extra > 0)
                .collect();
            // Prefer a room that covers everything with the fewest asks,
            // otherwise the room that frees the most.
            let Some(best) = options.into_iter().min_by(|a, b| {
                let (af, bf) = (a.extra >= remaining, b.extra >= remaining);
                bf.cmp(&af)
                    .then_with(|| if af && bf { a.asks.len().cmp(&b.asks.len()) } else { b.extra.cmp(&a.extra) })
                    .then_with(|| a.asks.len().cmp(&b.asks.len()))
                    .then_with(|| a.room.cmp(&b.room))
            }) else {
                break;
            };
            for a in &best.asks {
                if let Some(o) = work.occurrences.iter_mut().find(|o| o.event_id == a.event_id && o.span.start == a.start) {
                    o.needs.insert(a.want, a.reduce_to);
                }
            }
            let t = taken.entry(best.room).or_default();
            for (&m, &q) in &best.models {
                *t.entry(m).or_insert(0) += q;
            }
            remaining -= best.extra;
            candidates.retain(|&r| r != best.room);
            resolutions.push(RoomResolution { room_id: best.room, extra: best.extra, asks: best.asks });
        }
        out.push((resolutions, remaining));
    }
    out
}

// -------------------------------------------------------------- evaluation

/// Evaluate a loan request. Duplicate wants are merged.
pub fn evaluate(world: &World, window: Span, items: &[(Want, i64)]) -> Evaluation {
    let cat = &world.catalog;
    let mut merged: BTreeMap<Want, i64> = BTreeMap::new();
    for &(w, q) in items {
        if q > 0 {
            *merged.entry(w).or_insert(0) += q;
        }
    }
    let mut lines: Vec<(Want, i64)> = merged.into_iter().collect();
    by_specificity(cat, &mut lines);

    let rooms = rooms_with_stock(world);
    let trees: Vec<RoomTree> = rooms.iter().map(|&r| analyse_room(world, r, &window, &[])).collect();
    let picks = allocate(world, &trees, &lines);
    let short = lines.iter().zip(&picks).any(|((_, q), p)| p.iter().map(|x| x.qty).sum::<i64>() < *q);
    let negotiated = if short { negotiate(world, &window, &rooms, &lines, &picks) } else { vec![] };

    let items: Vec<ItemEvaluation> = lines
        .iter()
        .enumerate()
        .map(|(li, &(want, requested))| {
            let available: i64 = picks[li].iter().map(|p| p.qty).sum();
            let (resolutions, unresolvable) =
                negotiated.get(li).cloned().unwrap_or_else(|| (vec![], requested - available));
            let mut blocking_loans: Vec<Id> = world
                .holdings
                .iter()
                .filter(|h| h.qty > 0 && h.span.overlaps(&window) && cat.serves(want, h.type_id))
                .map(|h| h.loan_id)
                .collect();
            blocking_loans.sort();
            blocking_loans.dedup();
            let room_rows = trees
                .iter()
                .filter(|tr| tr.models.iter().any(|&m| cat.serves(want, m) && tr.stock.get(&m).copied().unwrap_or(0) > 0))
                .map(|tr| {
                    let stock = tr.models.iter().filter(|&&m| cat.serves(want, m)).map(|m| tr.stock[m]).sum();
                    let overcommitted = cat.path(want).iter().any(|&n| tr.nodes.contains(&n) && RoomTree::get(&tr.cap, n) < 0);
                    RoomAvailability {
                        room_id: tr.room_id,
                        stock,
                        peak_event_need: RoomTree::get(&tr.peak_demand, want),
                        peak_on_loan: RoomTree::get(&tr.peak_on_loan, want),
                        peak_combined: RoomTree::get(&tr.peak_combined, want),
                        spare: tr.line_capacity(cat, want, &tr.cap),
                        overcommitted,
                    }
                })
                .collect();
            let mut allocation = picks[li].clone();
            allocation.sort_by_key(|p| (p.room_id, p.type_id));
            ItemEvaluation {
                want,
                requested,
                available,
                allocation,
                shortfall: requested - available,
                resolutions,
                unresolvable,
                blocking_loans,
                rooms: room_rows,
            }
        })
        .collect();

    Evaluation { window, fully_available: items.iter().all(|i| i.shortfall == 0), items }
}

/// For an already-approved loan, check whether its allocation still fits
/// (e.g. a new event was added after approval). `allocation` is
/// (room, model, qty). Reports the most specific nodes that are over.
pub fn check_allocation(world: &World, window: Span, allocation: &[(Id, Id, i64)]) -> Vec<Problem> {
    let cat = &world.catalog;
    let mut by_room: BTreeMap<Id, HashMap<Id, i64>> = BTreeMap::new();
    for &(r, m, q) in allocation {
        *by_room.entry(r).or_default().entry(m).or_insert(0) += q;
    }
    let mut out = Vec::new();
    for (room, taken) in by_room {
        let models: Vec<Id> = taken.keys().copied().collect();
        let tree = analyse_room(world, room, &window, &models);
        let under = tree.under(cat, &taken);
        let mut nodes: Vec<Want> = under.keys().copied().filter(|n| under[n] > 0).collect();
        nodes.sort_by_key(|&n| (std::cmp::Reverse(cat.depth(n)), n));
        let mut explained: HashMap<Want, i64> = HashMap::new();
        for n in nodes {
            let over = under[&n] - RoomTree::get(&tree.cap, n).max(0);
            let below = explained.get(&n).copied().unwrap_or(0);
            let shown = if over > below {
                out.push(Problem { room_id: room, node: n, over });
                over
            } else {
                below
            };
            if let Some(p) = cat.parent(n) {
                *explained.entry(p).or_insert(0) += shown.max(0);
            }
        }
    }
    out
}

/// Which allocated units serve which request line: most specific line first,
/// which is always a correct matching on a tree. Returns, per line (in input
/// order), the picks assigned to it.
pub fn match_allocation(cat: &Catalog, lines: &[(Want, i64)], allocation: &[Pick]) -> Vec<Vec<Pick>> {
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|&a, &b| {
        let key = |w: Want| (matches!(w, Want::Kind(_)), std::cmp::Reverse(cat.depth(w)));
        key(lines[a].0).cmp(&key(lines[b].0)).then(a.cmp(&b))
    });
    let mut left: Vec<Pick> = allocation.to_vec();
    let mut out = vec![vec![]; lines.len()];
    for li in order {
        let (want, mut need) = lines[li];
        for p in left.iter_mut() {
            if need == 0 {
                break;
            }
            let fits = match want {
                Want::Model(m) => p.type_id == m,
                Want::Kind(_) => cat.is_under(Want::Model(p.type_id), want),
            };
            if p.qty > 0 && fits {
                let q = p.qty.min(need);
                p.qty -= q;
                need -= q;
                out[li].push(Pick { room_id: p.room_id, type_id: p.type_id, qty: q });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALL: Id = 1;
    const YOUTH: Id = 2;
    const STORE: Id = 3;
    const MIC: Id = 10;
    const DI: Id = 11;
    const E835: Id = 12;
    const SLX2: Id = 13;
    const INSTALLED: Id = 14;

    // Kinds: Microphones > Vocal mic > {Wired vocal mic, Wireless vocal mic}
    const MICS: Id = 100;
    const VOCAL: Id = 101;
    const WIRED: Id = 102;
    const WIRELESS: Id = 103;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }
    fn span(a: &str, b: &str) -> Span {
        Span::new(dt(a), dt(b))
    }
    fn m(id: Id) -> Want {
        Want::Model(id)
    }
    fn k(id: Id) -> Want {
        Want::Kind(id)
    }
    fn occ(event_id: Id, group: Id, room: Id, s: Span, needs: &[(Want, i64)]) -> Occurrence {
        Occurrence { event_id, group_id: Some(group), room_id: room, span: s, needs: needs.iter().copied().collect() }
    }
    fn total(item: &ItemEvaluation) -> i64 {
        item.allocation.iter().map(|a| a.qty).sum()
    }

    fn catalog() -> Catalog {
        let mut c = Catalog::default();
        c.kind_parent.insert(MICS, None);
        c.kind_parent.insert(VOCAL, Some(MICS));
        c.kind_parent.insert(WIRED, Some(VOCAL));
        c.kind_parent.insert(WIRELESS, Some(VOCAL));
        c.model_kind.insert(MIC, Some(WIRED));
        c.model_kind.insert(E835, Some(WIRED));
        c.model_kind.insert(SLX2, Some(WIRELESS));
        c.model_kind.insert(INSTALLED, Some(WIRED));
        c.model_kind.insert(DI, None);
        c.lendable.extend([MIC, E835, SLX2, DI]);
        c
    }

    /// The original model-only scenario: every line is an exact model.
    fn world() -> World {
        let mut w = World { catalog: catalog(), ..World::default() };
        w.stock.insert((HALL, MIC), 8);
        w.stock.insert((YOUTH, MIC), 4);
        w.stock.insert((STORE, MIC), 2);
        w.stock.insert((HALL, DI), 2);
        // Sunday service in the hall needs 6 mics, youth on Sunday evening needs 4.
        w.occurrences.push(occ(100, 1000, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(m(MIC), 6), (m(DI), 2)]));
        w.occurrences.push(occ(101, 1001, YOUTH, span("2026-11-22 18:00", "2026-11-22 21:00"), &[(m(MIC), 4)]));
        w
    }

    #[test]
    fn weekday_loan_takes_everything_when_nothing_scheduled() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-19 09:00"), &[(m(MIC), 12)]);
        assert!(ev.fully_available);
        let item = &ev.items[0];
        assert_eq!(item.available, 12);
        // Largest spare first: hall (8), then youth (4).
        assert_eq!(
            item.allocation,
            vec![Pick { room_id: HALL, type_id: MIC, qty: 8 }, Pick { room_id: YOUTH, type_id: MIC, qty: 4 }]
        );
    }

    #[test]
    fn weekend_loan_keeps_minimum_in_each_room() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-21 09:00", "2026-11-23 09:00"), &[(m(MIC), 4)]);
        assert!(ev.fully_available);
        let item = &ev.items[0];
        // Hall spare 2, store spare 2, youth spare 0.
        assert_eq!(item.available, 4);
        assert_eq!(total(item), 4);
        assert!(item.allocation.iter().all(|a| a.room_id != YOUTH));
    }

    #[test]
    fn shortage_suggests_who_to_ask() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-21 09:00", "2026-11-23 09:00"), &[(m(MIC), 6)]);
        assert!(!ev.fully_available);
        let item = &ev.items[0];
        assert_eq!(item.available, 4);
        assert_eq!(item.shortfall, 2);
        assert_eq!(item.unresolvable, 0);
        assert_eq!(item.resolutions.len(), 1);
        let res = &item.resolutions[0];
        assert_eq!(res.extra, 2);
        assert_eq!(res.asks.len(), 1);
        let ask = &res.asks[0];
        assert_eq!(ask.want, m(MIC));
        assert_eq!(ask.normally - ask.reduce_to, 2);
    }

    #[test]
    fn unresolvable_when_stock_does_not_exist() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-18 09:00"), &[(m(MIC), 20), (m(DI), 1)]);
        let mic = ev.items.iter().find(|i| i.want == m(MIC)).unwrap();
        assert_eq!(mic.available, 14);
        assert_eq!(mic.unresolvable, 6);
        assert!(mic.resolutions.is_empty());
        let di = ev.items.iter().find(|i| i.want == m(DI)).unwrap();
        assert_eq!(di.shortfall, 0);
    }

    #[test]
    fn committed_loans_reduce_availability_and_block() {
        let mut w = world();
        w.holdings.push(Holding {
            loan_id: 7,
            span: span("2026-11-16 09:00", "2026-11-18 12:00"),
            room_id: HALL,
            type_id: MIC,
            qty: 8,
        });
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-18 09:00"), &[(m(MIC), 8)]);
        let mic = &ev.items[0];
        assert_eq!(mic.available, 6);
        assert_eq!(mic.unresolvable, 2);
        assert_eq!(mic.blocking_loans, vec![7]);
    }

    #[test]
    fn overlapping_events_in_same_room_sum_and_asks_are_sufficient() {
        let mut w = World { catalog: catalog(), ..World::default() };
        w.stock.insert((HALL, MIC), 10);
        w.occurrences.push(occ(1, 1, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(m(MIC), 6)]));
        w.occurrences.push(occ(2, 2, HALL, span("2026-11-22 11:00", "2026-11-22 13:00"), &[(m(MIC), 3)]));
        w.occurrences.push(occ(3, 3, HALL, span("2026-11-22 15:00", "2026-11-22 16:00"), &[(m(MIC), 5)]));
        let window = span("2026-11-22 00:00", "2026-11-23 00:00");
        let ev = evaluate(&w, window, &[(m(MIC), 6)]);
        let mic = &ev.items[0];
        assert_eq!(mic.available, 1); // peak is 9 between 11:00-12:00
        assert_eq!(mic.shortfall, 5);
        let res = &mic.resolutions[0];
        assert_eq!(res.extra, 5);
        assert_fixes(&w, window, &[(m(MIC), 6)], &ev);
    }

    #[test]
    fn check_allocation_flags_new_conflicts() {
        let mut w = world();
        let window = span("2026-11-21 09:00", "2026-11-23 09:00");
        assert!(check_allocation(&w, window, &[(HALL, MIC, 2)]).is_empty());
        w.occurrences.push(occ(200, 1002, HALL, span("2026-11-21 10:00", "2026-11-21 12:00"), &[(m(MIC), 8)]));
        assert_eq!(check_allocation(&w, window, &[(HALL, MIC, 2)]), vec![Problem { room_id: HALL, node: m(MIC), over: 2 }]);
    }

    // ------------------------------------------------------------ kinds

    /// The worked example from docs/equipment-kinds.md.
    fn kinds_world() -> World {
        let mut w = World { catalog: catalog(), ..World::default() };
        w.stock.insert((HALL, MIC), 6);
        w.stock.insert((HALL, E835), 2);
        w.stock.insert((HALL, SLX2), 4);
        w.occurrences.push(occ(
            100,
            1000,
            HALL,
            span("2026-11-22 09:00", "2026-11-22 12:00"),
            &[(k(WIRED), 5), (m(SLX2), 2)],
        ));
        w
    }
    fn service() -> Span {
        span("2026-11-22 08:00", "2026-11-22 13:00")
    }

    /// Apply every suggested ask and check the request then fits.
    fn assert_fixes(w: &World, window: Span, items: &[(Want, i64)], ev: &Evaluation) {
        let mut w2 = w.clone();
        for item in &ev.items {
            for res in &item.resolutions {
                for a in &res.asks {
                    let o = w2.occurrences.iter_mut().find(|o| o.event_id == a.event_id && o.span.start == a.start).unwrap();
                    o.needs.insert(a.want, a.reduce_to);
                }
            }
        }
        let ev2 = evaluate(&w2, window, items);
        let expect_short: i64 = ev.items.iter().map(|i| i.unresolvable).sum();
        let short: i64 = ev2.items.iter().map(|i| i.shortfall).sum();
        assert_eq!(short, expect_short, "{:#?}", ev2.items);
    }

    #[test]
    fn any_of_a_kind_pools_models() {
        let w = kinds_world();
        let ev = evaluate(&w, service(), &[(k(VOCAL), 4)]);
        assert!(ev.fully_available, "{:#?}", ev.items);
        let item = &ev.items[0];
        assert_eq!(total(item), 4);
        // Within caps: wired ≤ 3, SLX2 ≤ 2.
        let wired: i64 = item.allocation.iter().filter(|p| p.type_id != SLX2).map(|p| p.qty).sum();
        let slx: i64 = item.allocation.iter().filter(|p| p.type_id == SLX2).map(|p| p.qty).sum();
        assert!(wired <= 3 && slx <= 2, "{:?}", item.allocation);
        assert_eq!(item.rooms[0].spare, 5);
    }

    #[test]
    fn kind_need_limits_every_model_below_it() {
        let w = kinds_world();
        let ev = evaluate(&w, service(), &[(k(WIRED), 4)]);
        let item = &ev.items[0];
        assert_eq!(item.available, 3);
        assert_eq!(item.shortfall, 1);
        assert_eq!(item.unresolvable, 0);
        let ask = &item.resolutions[0].asks[0];
        assert_eq!((ask.want, ask.normally, ask.reduce_to), (k(WIRED), 5, 4));
        assert_fixes(&w, service(), &[(k(WIRED), 4)], &ev);

        // An exact model is limited by the kind need too.
        let ev = evaluate(&w, service(), &[(m(MIC), 4)]);
        assert_eq!(ev.items[0].available, 3);
    }

    #[test]
    fn exact_lines_are_served_before_broad_ones() {
        let w = kinds_world();
        // 2 × SLX2 uses all the spare wireless; "any vocal mic" must then use wired.
        let items = [(k(VOCAL), 3), (m(SLX2), 2)];
        let ev = evaluate(&w, service(), &items);
        assert!(ev.fully_available, "{:#?}", ev.items);
        let any = ev.items.iter().find(|i| i.want == k(VOCAL)).unwrap();
        assert!(any.allocation.iter().all(|p| p.type_id != SLX2));
        // And a request beyond the vocal cap (5) is short overall.
        let ev = evaluate(&w, service(), &[(k(VOCAL), 4), (m(SLX2), 2)]);
        let short: i64 = ev.items.iter().map(|i| i.shortfall).sum();
        assert_eq!(short, 1);
        assert_fixes(&w, service(), &[(k(VOCAL), 4), (m(SLX2), 2)], &ev);
    }

    #[test]
    fn kind_requests_skip_unlendable_models_but_events_use_them() {
        let mut w = World { catalog: catalog(), ..World::default() };
        w.stock.insert((HALL, INSTALLED), 4);
        w.stock.insert((HALL, MIC), 2);
        // The event needs 4 wired mics; the installed ones cover it.
        w.occurrences.push(occ(1, 1, HALL, service(), &[(k(WIRED), 4)]));
        let ev = evaluate(&w, service(), &[(k(WIRED), 3)]);
        let item = &ev.items[0];
        assert_eq!(item.available, 2);
        assert!(item.allocation.iter().all(|p| p.type_id == MIC));
        // Freeing the third would need the installed ones, which can't be lent.
        assert_eq!(item.unresolvable, 1);
    }

    #[test]
    fn spreads_across_rooms_and_reroutes() {
        let mut w = World { catalog: catalog(), ..World::default() };
        // Hall: 2 SM58 + 1 e835, wired cap 3. Youth: 2 SM58.
        w.stock.insert((HALL, MIC), 2);
        w.stock.insert((HALL, E835), 1);
        w.stock.insert((YOUTH, MIC), 2);
        let window = span("2026-11-17 09:00", "2026-11-18 09:00");
        let items = [(m(MIC), 2), (k(WIRED), 3)];
        let ev = evaluate(&w, window, &items);
        assert!(ev.fully_available, "{:#?}", ev.items);
        let used: i64 = ev.items.iter().map(total).sum();
        assert_eq!(used, 5);
    }

    #[test]
    fn check_allocation_reports_the_kind_that_is_over() {
        let w = kinds_world();
        // SM58 × 3 fits the wired cap; × 4 doesn't, and the kind is to blame.
        assert!(check_allocation(&w, service(), &[(HALL, MIC, 3)]).is_empty());
        let p = check_allocation(&w, service(), &[(HALL, MIC, 2), (HALL, E835, 2)]);
        assert_eq!(p, vec![Problem { room_id: HALL, node: k(WIRED), over: 1 }]);
    }

    #[test]
    fn matching_serves_specific_lines_first() {
        let cat = catalog();
        let lines = [(k(MICS), 2), (m(SLX2), 1)];
        let alloc = [Pick { room_id: HALL, type_id: SLX2, qty: 2 }, Pick { room_id: HALL, type_id: MIC, qty: 1 }];
        let got = match_allocation(&cat, &lines, &alloc);
        assert_eq!(got[1], vec![Pick { room_id: HALL, type_id: SLX2, qty: 1 }]);
        assert_eq!(got[0].iter().map(|p| p.qty).sum::<i64>(), 2);
    }

    #[test]
    fn want_json_round_trip() {
        let l: Line = serde_json::from_str(r#"{"kind_id": 5, "quantity": 2}"#).unwrap();
        assert_eq!(l.want, k(5));
        let l: Line = serde_json::from_str(r#"{"type_id": 7, "kind_id": null, "quantity": 1}"#).unwrap();
        assert_eq!(l.want, m(7));
        assert!(serde_json::from_str::<Line>(r#"{"type_id": 7, "kind_id": 5, "quantity": 1}"#).is_err());
        assert_eq!(serde_json::to_value(l).unwrap(), serde_json::json!({"kind_id": null, "type_id": 7, "quantity": 1}));
    }
}
