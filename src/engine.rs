//! The availability / allocation engine.
//!
//! Everything here is pure: the caller loads a [`World`] snapshot (stock,
//! church event occurrences and already-committed loans that overlap the
//! request window) and asks [`evaluate`] whether a loan request can be met.
//!
//! Equipment forms a tree: leaves are stocked models ("Shure SM58"),
//! internal nodes are umbrellas ("Wired vocal mic", "Mic"). Requests,
//! church-event needs and per-date reductions may name any node; stock and
//! loan allocations always name leaves, because physical units are models.
//!
//! Rules:
//! * Each room's church events are covered only from that room's own stock.
//! * Only events/loans that overlap the request window matter (time-aware).
//! * A need for `q` of node `N` keeps `q` units from `N`'s subtree in the
//!   room. Needs from overlapping occurrences add up, including across
//!   levels (5×Mic from one event plus 2×SM58 from another keeps 7 units,
//!   at least 2 of them SM58). Within one occurrence/request, naming both
//!   an umbrella and something under it is rejected by the API, so the
//!   engine never has to guess whether such lines overlap or add.
//! * A room can lend a unit when lending it breaks no need in any time
//!   slice of the window. Because needs are subtree sums (a laminar
//!   family), the most that can be lent under any node is computed exactly
//!   with a bottom-up pass over the tree rather than per-model arithmetic.
//! * Umbrella requests are fulfilled from lendable leaves only (installed
//!   gear is never lent out implicitly); naming a non-lendable leaf itself
//!   is explicit and still works for staff-created loans.
//! * When there is not enough, the engine works out which event
//!   occurrences would need to manage with less, and by how much.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDateTime;
use serde::Serialize;

use crate::recurrence::Span;

pub type Id = i64;

/// A concrete occurrence of a church event, with overrides already applied.
#[derive(Clone, Debug)]
pub struct Occurrence {
    pub event_id: Id,
    pub group_id: Option<Id>,
    pub room_id: Id,
    pub span: Span,
    /// node id (any level) -> quantity that must stay in the room.
    pub needs: BTreeMap<Id, i64>,
}

/// Equipment already promised to another (approved / collected) loan.
/// Allocations are always leaves: a specific model from a specific room.
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
    /// (room_id, leaf type_id) -> quantity owned.
    pub stock: HashMap<(Id, Id), i64>,
    pub occurrences: Vec<Occurrence>,
    pub holdings: Vec<Holding>,
    /// Equipment tree: node id -> parent id (`None` at the top level).
    pub parents: HashMap<Id, Option<Id>>,
    /// Leaf ids that may be lent out (installed gear is excluded).
    pub lendable: HashSet<Id>,
}

/// A concrete allocation: `qty` units of leaf `type_id` from `room_id`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AllocQty {
    pub room_id: Id,
    pub type_id: Id,
    pub qty: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoomAvailability {
    pub room_id: Id,
    /// Units owned in the requested node's subtree.
    pub stock: i64,
    /// Peak quantity church events need from that subtree during the window.
    pub peak_event_need: i64,
    /// Peak quantity already out on other loans during the window.
    pub peak_on_loan: i64,
    /// Peak of events + loans combined (they may not coincide).
    pub peak_combined: i64,
    /// Units that can be lent without affecting anyone.
    pub spare: i64,
    /// True if events + loans already exceed stock at some point.
    pub overcommitted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct OccurrenceAsk {
    pub event_id: Id,
    pub group_id: Option<Id>,
    pub room_id: Id,
    /// The node the occurrence needs (the level it was recorded at).
    pub type_id: Id,
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
    /// The requested node (any level of the tree).
    pub type_id: Id,
    pub requested: i64,
    /// Total that can be lent right now without asking anyone.
    pub available: i64,
    /// Where to collect the available units, as concrete models.
    pub allocation: Vec<AllocQty>,
    pub shortfall: i64,
    /// Suggested negotiations that would cover (part of) the shortfall.
    pub resolutions: Vec<RoomResolution>,
    /// Units that cannot be found even if every church event agreed to go
    /// without (stock simply isn't there, or is out on other loans).
    pub unresolvable: i64,
    /// Other loans holding equipment under this node during the window.
    pub blocking_loans: Vec<Id>,
    pub rooms: Vec<RoomAvailability>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Evaluation {
    pub window: Span,
    pub fully_available: bool,
    pub items: Vec<ItemEvaluation>,
}

// ---------------------------------------------------------------- tree utils

/// Children of each node, from a `node -> parent` map. Sorted by id.
pub fn children_map(parents: &HashMap<Id, Option<Id>>) -> HashMap<Id, Vec<Id>> {
    let mut children: HashMap<Id, Vec<Id>> = HashMap::new();
    for (&id, &parent) in parents {
        children.entry(id).or_default();
        if let Some(p) = parent
            && p != id
            && parents.contains_key(&p)
        {
            children.entry(p).or_default().push(id);
        }
    }
    for v in children.values_mut() {
        v.sort_unstable();
    }
    children
}

/// True if `node` is `ancestor` or sits somewhere underneath it.
pub fn is_descendant_or_self(parents: &HashMap<Id, Option<Id>>, node: Id, ancestor: Id) -> bool {
    let mut cur = Some(node);
    let mut guard = 0;
    while let Some(n) = cur {
        if n == ancestor {
            return true;
        }
        guard += 1;
        if guard > parents.len() + 1 {
            return false; // cycle; the API prevents these, stay safe
        }
        cur = parents.get(&n).copied().flatten();
    }
    false
}

/// True if two nodes' subtrees share any stock (one contains the other).
pub fn nodes_overlap(parents: &HashMap<Id, Option<Id>>, a: Id, b: Id) -> bool {
    is_descendant_or_self(parents, a, b) || is_descendant_or_self(parents, b, a)
}

/// Leaf nodes (no children) at or under `node`, including `node` itself
/// when it is a leaf. An unknown id is treated as its own leaf.
pub fn leaves_under(parents: &HashMap<Id, Option<Id>>, node: Id) -> Vec<Id> {
    let children = children_map(parents);
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match children.get(&n) {
            Some(kids) if !kids.is_empty() => stack.extend(kids.iter().copied()),
            _ => out.push(n),
        }
    }
    out.sort_unstable();
    out
}

/// Depth in the tree (top level = 0).
pub fn depth_of(parents: &HashMap<Id, Option<Id>>, node: Id) -> usize {
    let mut depth = 0;
    let mut cur = node;
    while let Some(p) = parents.get(&cur).copied().flatten() {
        depth += 1;
        if depth > parents.len() {
            break;
        }
        cur = p;
    }
    depth
}

/// A working view of the equipment tree for one evaluation.
struct Tree {
    nodes: Vec<Id>,
    parent: HashMap<Id, Option<Id>>,
    children: HashMap<Id, Vec<Id>>,
    depth: HashMap<Id, usize>,
    /// Deepest first, so subtree sums can accumulate into parents.
    postorder: Vec<Id>,
    leaves: HashSet<Id>,
}

impl Tree {
    fn from_world(world: &World, extra_ids: &[Id]) -> Tree {
        let mut parent = world.parents.clone();
        for &(_, t) in world.stock.keys() {
            parent.entry(t).or_insert(None);
        }
        for o in &world.occurrences {
            for &t in o.needs.keys() {
                parent.entry(t).or_insert(None);
            }
        }
        for h in &world.holdings {
            parent.entry(h.type_id).or_insert(None);
        }
        for &t in extra_ids {
            parent.entry(t).or_insert(None);
        }
        // Parents must exist and must not form cycles (the API enforces
        // both; be defensive so a bad row can never hang the engine).
        let ids: Vec<Id> = parent.keys().copied().collect();
        for id in ids {
            if let Some(p) = parent.get(&id).copied().flatten()
                && (p == id || !parent.contains_key(&p))
            {
                parent.insert(id, None);
            }
        }
        for id in parent.keys().copied().collect::<Vec<_>>() {
            let mut seen = HashSet::new();
            let mut cur = id;
            while let Some(p) = parent.get(&cur).copied().flatten() {
                if !seen.insert(p) {
                    parent.insert(id, None);
                    break;
                }
                cur = p;
            }
        }
        let children = children_map(&parent);
        let mut nodes: Vec<Id> = parent.keys().copied().collect();
        nodes.sort_unstable();
        let depth: HashMap<Id, usize> = nodes.iter().map(|&n| (n, depth_of(&parent, n))).collect();
        let mut postorder = nodes.clone();
        postorder.sort_by(|a, b| depth[b].cmp(&depth[a]).then(a.cmp(b)));
        let leaves: HashSet<Id> = nodes.iter().copied().filter(|n| children.get(n).is_none_or(|c| c.is_empty())).collect();
        Tree { nodes, parent, children, depth, postorder, leaves }
    }

    fn is_leaf(&self, node: Id) -> bool {
        !self.parent.contains_key(&node) || self.leaves.contains(&node)
    }

    /// `node`, then each ancestor up to the top level.
    fn ancestors(&self, node: Id) -> Vec<Id> {
        let mut out = vec![node];
        let mut cur = node;
        while let Some(p) = self.parent.get(&cur).copied().flatten() {
            out.push(p);
            cur = p;
        }
        out
    }

    fn is_desc_or_self(&self, node: Id, ancestor: Id) -> bool {
        self.ancestors(node).contains(&ancestor)
    }

    fn leaves_under(&self, node: Id) -> Vec<Id> {
        if self.is_leaf(node) {
            return vec![node];
        }
        let mut out = Vec::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if self.leaves.contains(&n) {
                out.push(n);
            } else if let Some(kids) = self.children.get(&n) {
                stack.extend(kids.iter().copied());
            }
        }
        out.sort_unstable();
        out
    }

    /// Subtree total per node from per-node own values.
    fn subtree_sums(&self, own: &HashMap<Id, i64>) -> HashMap<Id, i64> {
        let mut tot: HashMap<Id, i64> = self.nodes.iter().map(|&n| (n, own.get(&n).copied().unwrap_or(0))).collect();
        for &n in &self.postorder {
            if let Some(p) = self.parent.get(&n).copied().flatten() {
                let v = tot[&n];
                *tot.entry(p).or_insert(0) += v;
            }
        }
        tot
    }
}

// -------------------------------------------------------------- room analysis

struct SegDemand {
    occ: usize,
    node: Id,
    qty: i64,
}

struct RoomSeg {
    demands: Vec<SegDemand>,
    /// Subtree sums of units physically in the room (stock − on loan).
    supply: HashMap<Id, i64>,
    /// Subtree sums of church-event needs.
    req: HashMap<Id, i64>,
}

struct RoomAnalysis {
    segs: Vec<RoomSeg>,
    stock_sub: HashMap<Id, i64>,
    /// Per node: minimum over the window of (supply − needs). Lending from
    /// a subtree may never exceed this in total.
    cap: HashMap<Id, i64>,
    /// Per node: minimum supply, i.e. `cap` if every need were waived.
    cap0: HashMap<Id, i64>,
    peak_req: HashMap<Id, i64>,
    peak_hold: HashMap<Id, i64>,
    peak_combined: HashMap<Id, i64>,
}

fn analyse_room(world: &World, tree: &Tree, room: Id, window: &Span) -> RoomAnalysis {
    let occ: Vec<(usize, Span)> = world
        .occurrences
        .iter()
        .enumerate()
        .filter(|(_, o)| o.room_id == room && o.needs.values().any(|&q| q > 0))
        .filter_map(|(i, o)| Some((i, o.span.clip(window)?)))
        .collect();
    let holds: Vec<(Span, Id, i64)> = world
        .holdings
        .iter()
        .filter(|h| h.room_id == room && h.qty > 0)
        .filter_map(|h| Some((h.span.clip(window)?, h.type_id, h.qty)))
        .collect();

    let mut points = vec![window.start, window.end];
    for (_, s) in &occ {
        points.extend([s.start, s.end]);
    }
    for (s, _, _) in &holds {
        points.extend([s.start, s.end]);
    }
    points.sort();
    points.dedup();

    let stock_own: HashMap<Id, i64> =
        tree.leaves.iter().map(|&l| (l, world.stock.get(&(room, l)).copied().unwrap_or(0))).collect();
    let stock_sub = tree.subtree_sums(&stock_own);

    let mut segs = Vec::new();
    let mut seg_holds: Vec<HashMap<Id, i64>> = Vec::new();
    for p in points.windows(2) {
        let span = Span::new(p[0], p[1]);
        if span.start >= span.end {
            continue;
        }
        let mut demands = Vec::new();
        let mut req_own: HashMap<Id, i64> = HashMap::new();
        for &(i, s) in &occ {
            if !s.covers(&span) {
                continue;
            }
            for (&node, &q) in &world.occurrences[i].needs {
                if q > 0 {
                    demands.push(SegDemand { occ: i, node, qty: q });
                    *req_own.entry(node).or_insert(0) += q;
                }
            }
        }
        let mut hold_own: HashMap<Id, i64> = HashMap::new();
        for (s, leaf, q) in &holds {
            if s.covers(&span) {
                *hold_own.entry(*leaf).or_insert(0) += q;
            }
        }
        let supply_own: HashMap<Id, i64> = tree
            .leaves
            .iter()
            .map(|&l| (l, stock_own.get(&l).copied().unwrap_or(0) - hold_own.get(&l).copied().unwrap_or(0)))
            .collect();
        seg_holds.push(tree.subtree_sums(&hold_own));
        segs.push(RoomSeg { demands, supply: tree.subtree_sums(&supply_own), req: tree.subtree_sums(&req_own) });
    }

    let mut cap = stock_sub.clone();
    let mut cap0 = stock_sub.clone();
    let mut peak_req: HashMap<Id, i64> = tree.nodes.iter().map(|&n| (n, 0)).collect();
    let mut peak_hold = peak_req.clone();
    let mut peak_combined = peak_req.clone();
    for (seg, hold_sub) in segs.iter().zip(&seg_holds) {
        for &n in &tree.nodes {
            let supply = seg.supply.get(&n).copied().unwrap_or(0);
            let req = seg.req.get(&n).copied().unwrap_or(0);
            let hold = hold_sub.get(&n).copied().unwrap_or(0);
            cap.insert(n, cap[&n].min(supply - req));
            cap0.insert(n, cap0[&n].min(supply));
            peak_req.insert(n, peak_req[&n].max(req));
            peak_hold.insert(n, peak_hold[&n].max(hold));
            peak_combined.insert(n, peak_combined[&n].max(req + hold));
        }
    }
    RoomAnalysis { segs, stock_sub, cap, cap0, peak_req, peak_hold, peak_combined }
}

/// Most that can be lent from each subtree, given per-node caps and what has
/// already been allocated. Bottom-up: a node can lend the sum of its
/// children, bounded by its own cap; only `eligible` leaves count.
fn lendable_by_node(
    tree: &Tree,
    caps: &HashMap<Id, i64>,
    allocated_sub: &HashMap<Id, i64>,
    eligible: &HashSet<Id>,
) -> HashMap<Id, i64> {
    let eff = |n: Id| (caps.get(&n).copied().unwrap_or(0) - allocated_sub.get(&n).copied().unwrap_or(0)).max(0);
    let mut f: HashMap<Id, i64> = HashMap::new();
    for &n in &tree.postorder {
        let v = if tree.leaves.contains(&n) {
            if eligible.contains(&n) { eff(n) } else { 0 }
        } else {
            let sum: i64 = tree.children.get(&n).map_or(0, |kids| kids.iter().map(|k| f.get(k).copied().unwrap_or(0)).sum());
            eff(n).min(sum)
        };
        f.insert(n, v);
    }
    f
}

/// Total lendable from `node`'s subtree: its own lendable quantity, also
/// bounded by every ancestor's cap (an ancestor's need limits the whole
/// umbrella, whichever branch units come from).
fn can_lend(tree: &Tree, caps: &HashMap<Id, i64>, allocated_sub: &HashMap<Id, i64>, f: &HashMap<Id, i64>, node: Id) -> i64 {
    let mut v = f.get(&node).copied().unwrap_or(0);
    if !tree.parent.contains_key(&node) {
        // Unknown node: only its own cap (0 unless stocked, handled as leaf).
        v = (caps.get(&node).copied().unwrap_or(0) - allocated_sub.get(&node).copied().unwrap_or(0)).max(0);
    }
    for a in tree.ancestors(node).into_iter().skip(1) {
        v = v.min((caps.get(&a).copied().unwrap_or(0) - allocated_sub.get(&a).copied().unwrap_or(0)).max(0));
    }
    v.max(0)
}

/// Choose concrete leaves for `qty` units under `node`, taking from the
/// branches with the most lendable first (keeps scarce models available).
fn distribute(tree: &Tree, f: &HashMap<Id, i64>, node: Id, qty: i64, out: &mut Vec<(Id, i64)>) {
    if qty <= 0 {
        return;
    }
    if tree.is_leaf(node) {
        out.push((node, qty));
        return;
    }
    let mut kids: Vec<Id> = tree.children.get(&node).cloned().unwrap_or_default();
    kids.sort_by(|a, b| f.get(b).copied().unwrap_or(0).cmp(&f.get(a).copied().unwrap_or(0)).then(a.cmp(b)));
    let mut remaining = qty;
    for k in kids {
        if remaining <= 0 {
            break;
        }
        let take = f.get(&k).copied().unwrap_or(0).min(remaining);
        if take > 0 {
            distribute(tree, f, k, take, out);
            remaining -= take;
        }
    }
}

/// Which occurrence needs in a room must reduce, and to what, so that the
/// concrete allocation `x_by_leaf` fits alongside church events all window.
fn asks_for_allocation(world: &World, tree: &Tree, ra: &RoomAnalysis, x_by_leaf: &HashMap<Id, i64>) -> Vec<OccurrenceAsk> {
    let x_sub = tree.subtree_sums(x_by_leaf);
    let mut relevant: Vec<Id> = tree.nodes.iter().copied().filter(|n| x_sub.get(n).copied().unwrap_or(0) > 0).collect();
    relevant.sort_by(|a, b| tree.depth[b].cmp(&tree.depth[a]).then(a.cmp(b)));

    // (occurrence index, node) -> units to drop; max over time slices,
    // because a reduction applies to the whole occurrence.
    let mut reduction: BTreeMap<(usize, Id), i64> = BTreeMap::new();
    for seg in &ra.segs {
        let mut seg_reduction: HashMap<(usize, Id), i64> = HashMap::new();
        for &n in &relevant {
            let x = x_sub.get(&n).copied().unwrap_or(0);
            let allowed_req = seg.supply.get(&n).copied().unwrap_or(0) - x;
            loop {
                let dropped: i64 = seg_reduction
                    .iter()
                    .filter(|((_, node), _)| tree.is_desc_or_self(*node, n))
                    .map(|(_, v)| v)
                    .sum();
                let excess = seg.req.get(&n).copied().unwrap_or(0) - dropped - allowed_req;
                if excess <= 0 {
                    break;
                }
                // Ask the biggest user in this subtree first: fewer people.
                let best = seg
                    .demands
                    .iter()
                    .filter(|d| tree.is_desc_or_self(d.node, n))
                    .map(|d| (d, d.qty - seg_reduction.get(&(d.occ, d.node)).copied().unwrap_or(0)))
                    .filter(|(_, remaining)| *remaining > 0)
                    .max_by(|a, b| a.1.cmp(&b.1).then(b.0.occ.cmp(&a.0.occ)).then(b.0.node.cmp(&a.0.node)));
                let Some((d, remaining)) = best else { break };
                let take = remaining.min(excess);
                *seg_reduction.entry((d.occ, d.node)).or_insert(0) += take;
            }
        }
        for (k, v) in seg_reduction {
            let e = reduction.entry(k).or_insert(0);
            *e = (*e).max(v);
        }
    }
    let mut asks: Vec<OccurrenceAsk> = reduction
        .into_iter()
        .filter(|(_, r)| *r > 0)
        .map(|((idx, node), r)| {
            let o = &world.occurrences[idx];
            let normally = o.needs.get(&node).copied().unwrap_or(0);
            OccurrenceAsk {
                event_id: o.event_id,
                group_id: o.group_id,
                room_id: o.room_id,
                type_id: node,
                start: o.span.start,
                end: o.span.end,
                normally,
                reduce_to: normally - r,
            }
        })
        .collect();
    asks.sort_by(|a, b| a.start.cmp(&b.start).then(a.event_id.cmp(&b.event_id)).then(a.type_id.cmp(&b.type_id)));
    asks
}

// ------------------------------------------------------------------ evaluate

/// Evaluate a loan request. `items` is a list of (node id, quantity) at any
/// level of the tree; duplicate node ids are merged. Items whose subtrees
/// overlap compete for the same units: more specific requests are
/// allocated first, so "1× SM58" is not starved by a sibling "2× Mic".
pub fn evaluate(world: &World, window: Span, items: &[(Id, i64)]) -> Evaluation {
    let mut merged: BTreeMap<Id, i64> = BTreeMap::new();
    for &(ty, q) in items {
        if q > 0 {
            *merged.entry(ty).or_insert(0) += q;
        }
    }
    let requested_ids: Vec<Id> = merged.keys().copied().collect();
    let tree = Tree::from_world(world, &requested_ids);

    let mut rooms: Vec<Id> = world.stock.keys().map(|(r, _)| *r).collect::<HashSet<_>>().into_iter().collect();
    rooms.sort_unstable();
    let analyses: HashMap<Id, RoomAnalysis> = rooms.iter().map(|&r| (r, analyse_room(world, &tree, r, &window))).collect();

    let mut order: Vec<(Id, i64)> = merged.into_iter().collect();
    order.sort_by(|a, b| {
        tree.depth.get(&b.0).copied().unwrap_or(0).cmp(&tree.depth.get(&a.0).copied().unwrap_or(0)).then(a.0.cmp(&b.0))
    });

    // Units already promised to earlier items of this same request.
    let mut allocated: HashMap<(Id, Id), i64> = HashMap::new();
    let alloc_sub = |allocated: &HashMap<(Id, Id), i64>, room: Id, tree: &Tree| -> HashMap<Id, i64> {
        let own: HashMap<Id, i64> = allocated.iter().filter(|((r, _), _)| *r == room).map(|((_, l), q)| (*l, *q)).collect();
        tree.subtree_sums(&own)
    };

    let mut out = Vec::new();
    for (ty, requested) in order {
        let eligible: HashSet<Id> = if tree.is_leaf(ty) {
            HashSet::from([ty])
        } else {
            tree.leaves_under(ty).into_iter().filter(|l| world.lendable.contains(l)).collect()
        };

        // Rooms holding anything under this node, with spare right now.
        struct RoomPlan {
            room: Id,
            can: i64,
            can_waived: i64,
            f: HashMap<Id, i64>,
            f_waived: HashMap<Id, i64>,
        }
        let mut plans: Vec<RoomPlan> = Vec::new();
        let mut room_avails: Vec<RoomAvailability> = Vec::new();
        for &room in &rooms {
            let ra = &analyses[&room];
            let stock = ra.stock_sub.get(&ty).copied().unwrap_or(0);
            if stock <= 0 {
                continue;
            }
            let before = alloc_sub(&allocated, room, &tree);
            let f = lendable_by_node(&tree, &ra.cap, &before, &eligible);
            let can = can_lend(&tree, &ra.cap, &before, &f, ty);
            let f_waived = lendable_by_node(&tree, &ra.cap0, &before, &eligible);
            let can_waived = can_lend(&tree, &ra.cap0, &before, &f_waived, ty);
            room_avails.push(RoomAvailability {
                room_id: room,
                stock,
                peak_event_need: ra.peak_req.get(&ty).copied().unwrap_or(0),
                peak_on_loan: ra.peak_hold.get(&ty).copied().unwrap_or(0),
                peak_combined: ra.peak_combined.get(&ty).copied().unwrap_or(0),
                spare: can,
                overcommitted: ra.peak_combined.get(&ty).copied().unwrap_or(0) > stock,
            });
            plans.push(RoomPlan { room, can, can_waived, f, f_waived });
        }

        // 1. Allocate from spare stock, largest spare first (fewest pickups).
        let mut plan_order: Vec<usize> = (0..plans.len()).collect();
        plan_order.sort_by(|&a, &b| plans[b].can.cmp(&plans[a].can).then(plans[a].room.cmp(&plans[b].room)));
        let mut remaining = requested;
        let mut allocation: Vec<AllocQty> = Vec::new();
        let mut taken: HashMap<Id, i64> = HashMap::new();
        for &i in &plan_order {
            if remaining <= 0 {
                break;
            }
            let plan = &plans[i];
            let q = plan.can.min(remaining);
            if q <= 0 {
                continue;
            }
            let mut leaves: Vec<(Id, i64)> = Vec::new();
            if tree.parent.contains_key(&ty) {
                distribute(&tree, &plan.f, ty, q, &mut leaves);
            } else {
                leaves.push((ty, q));
            }
            for (leaf, qty) in leaves {
                allocation.push(AllocQty { room_id: plan.room, type_id: leaf, qty });
                *allocated.entry((plan.room, leaf)).or_insert(0) += qty;
                *taken.entry(plan.room).or_insert(0) += qty;
                remaining -= qty;
            }
        }
        allocation.sort_by(|a, b| a.room_id.cmp(&b.room_id).then(a.type_id.cmp(&b.type_id)));
        let available = requested - remaining;
        let shortfall = remaining;

        // 2. If short, find who could be asked to manage with less.
        let mut resolutions = Vec::new();
        if remaining > 0 {
            let mut candidates: Vec<usize> = plans
                .iter()
                .enumerate()
                .filter(|(_, p)| p.can_waived - taken.get(&p.room).copied().unwrap_or(0) > 0)
                .map(|(i, _)| i)
                .collect();
            while remaining > 0 && !candidates.is_empty() {
                let scored: Vec<(usize, i64, Vec<OccurrenceAsk>)> = candidates
                    .iter()
                    .enumerate()
                    .map(|(ci, &pi)| {
                        let plan = &plans[pi];
                        let freeable = plan.can_waived - taken.get(&plan.room).copied().unwrap_or(0);
                        let extra = freeable.min(remaining);
                        let target = taken.get(&plan.room).copied().unwrap_or(0) + extra;
                        let mut leaves: Vec<(Id, i64)> = Vec::new();
                        if tree.parent.contains_key(&ty) {
                            distribute(&tree, &plan.f_waived, ty, target, &mut leaves);
                        } else {
                            leaves.push((ty, target));
                        }
                        let x: HashMap<Id, i64> = leaves.into_iter().collect();
                        let asks = asks_for_allocation(world, &tree, &analyses[&plan.room], &x);
                        (ci, extra, asks)
                    })
                    .collect();
                let best = scored
                    .into_iter()
                    .min_by(|a, b| {
                        let a_full = a.1 >= remaining;
                        let b_full = b.1 >= remaining;
                        b_full
                            .cmp(&a_full)
                            .then_with(|| if a_full && b_full { a.2.len().cmp(&b.2.len()) } else { b.1.cmp(&a.1) })
                            .then_with(|| a.2.len().cmp(&b.2.len()))
                            .then_with(|| plans[candidates[a.0]].room.cmp(&plans[candidates[b.0]].room))
                    })
                    .expect("non-empty");
                let (ci, extra, asks) = best;
                let room_id = plans[candidates[ci]].room;
                remaining -= extra;
                resolutions.push(RoomResolution { room_id, extra, asks });
                candidates.remove(ci);
            }
        }

        let mut blocking_loans: Vec<Id> = world
            .holdings
            .iter()
            .filter(|h| h.qty > 0 && h.span.overlaps(&window) && tree.is_desc_or_self(h.type_id, ty))
            .map(|h| h.loan_id)
            .collect();
        blocking_loans.sort_unstable();
        blocking_loans.dedup();

        out.push(ItemEvaluation {
            type_id: ty,
            requested,
            available,
            allocation,
            shortfall,
            resolutions,
            unresolvable: remaining,
            blocking_loans,
            rooms: room_avails,
        });
    }
    out.sort_by_key(|a| a.type_id);

    Evaluation { window, fully_available: out.iter().all(|i| i.shortfall == 0), items: out }
}

/// For an already-approved loan, check whether its allocation still fits
/// (e.g. a new event was added after approval). `allocation` is concrete
/// (room, leaf, qty) triples. Returns (room, node, over by) for the
/// deepest subtree constraints the allocation now breaks.
pub fn check_allocation(world: &World, window: Span, allocation: &[(Id, Id, i64)]) -> Vec<(Id, Id, i64)> {
    let extra: Vec<Id> = allocation.iter().map(|a| a.1).collect();
    let tree = Tree::from_world(world, &extra);
    let mut by_room: HashMap<Id, HashMap<Id, i64>> = HashMap::new();
    for &(room, leaf, qty) in allocation {
        if qty > 0 {
            *by_room.entry(room).or_default().entry(leaf).or_insert(0) += qty;
        }
    }
    let mut out = Vec::new();
    let mut room_ids: Vec<Id> = by_room.keys().copied().collect();
    room_ids.sort_unstable();
    for room in room_ids {
        let ra = analyse_room(world, &tree, room, &window);
        let x_sub = tree.subtree_sums(&by_room[&room]);
        let mut over: HashMap<Id, i64> = HashMap::new();
        for &n in &tree.nodes {
            let x = x_sub.get(&n).copied().unwrap_or(0);
            if x <= 0 {
                continue;
            }
            let worst = if ra.segs.is_empty() {
                x - ra.stock_sub.get(&n).copied().unwrap_or(0)
            } else {
                ra.segs
                    .iter()
                    .map(|s| x - (s.supply.get(&n).copied().unwrap_or(0) - s.req.get(&n).copied().unwrap_or(0)))
                    .max()
                    .unwrap_or(0)
            };
            if worst > 0 {
                over.insert(n, worst);
            }
        }
        // Deepest broken constraints only: a broken child explains its parent.
        let violated: Vec<Id> = over.keys().copied().collect();
        for n in violated {
            let has_violated_descendant =
                over.keys().any(|&m| m != n && tree.is_desc_or_self(m, n));
            if !has_violated_descendant {
                out.push((room, n, over[&n]));
            }
        }
    }
    out.sort_unstable();
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

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }
    fn span(a: &str, b: &str) -> Span {
        Span::new(dt(a), dt(b))
    }
    fn occ(event_id: Id, group: Id, room: Id, s: Span, needs: &[(Id, i64)]) -> Occurrence {
        Occurrence { event_id, group_id: Some(group), room_id: room, span: s, needs: needs.iter().copied().collect() }
    }

    fn world() -> World {
        let mut w = World::default();
        w.stock.insert((HALL, MIC), 8);
        w.stock.insert((YOUTH, MIC), 4);
        w.stock.insert((STORE, MIC), 2);
        w.stock.insert((HALL, DI), 2);
        w.parents.insert(MIC, None);
        w.parents.insert(DI, None);
        w.lendable.insert(MIC);
        w.lendable.insert(DI);
        // Sunday service in the hall needs 6 mics, youth on Sunday evening needs 4.
        w.occurrences.push(occ(100, 1000, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(MIC, 6), (DI, 2)]));
        w.occurrences.push(occ(101, 1001, YOUTH, span("2026-11-22 18:00", "2026-11-22 21:00"), &[(MIC, 4)]));
        w
    }

    #[test]
    fn weekday_loan_takes_everything_when_nothing_scheduled() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-19 09:00"), &[(MIC, 12)]);
        assert!(ev.fully_available);
        let item = &ev.items[0];
        assert_eq!(item.available, 12);
        // Largest spare first: hall (8), then youth (4).
        assert_eq!(
            item.allocation,
            vec![
                AllocQty { room_id: HALL, type_id: MIC, qty: 8 },
                AllocQty { room_id: YOUTH, type_id: MIC, qty: 4 }
            ]
        );
    }

    #[test]
    fn weekend_loan_keeps_minimum_in_each_room() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-21 09:00", "2026-11-23 09:00"), &[(MIC, 4)]);
        assert!(ev.fully_available);
        let item = &ev.items[0];
        // Hall spare 2, store spare 2, youth spare 0.
        assert_eq!(item.available, 4);
        let total: i64 = item.allocation.iter().map(|a| a.qty).sum();
        assert_eq!(total, 4);
        assert!(item.allocation.iter().all(|a| a.room_id != YOUTH));
    }

    #[test]
    fn shortage_suggests_who_to_ask() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-21 09:00", "2026-11-23 09:00"), &[(MIC, 6)]);
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
        // Hall already gives 2 spare; to give 4 the service must drop 6 -> 4.
        // Youth would have to drop 4 -> 2. Either is a single ask; the engine
        // picks deterministically, and the ask must be consistent.
        assert_eq!(ask.normally - ask.reduce_to, 2);
    }

    #[test]
    fn unresolvable_when_stock_does_not_exist() {
        let w = world();
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-18 09:00"), &[(MIC, 20), (DI, 1)]);
        let mic = ev.items.iter().find(|i| i.type_id == MIC).unwrap();
        assert_eq!(mic.available, 14);
        assert_eq!(mic.unresolvable, 6);
        assert!(mic.resolutions.is_empty());
        let di = ev.items.iter().find(|i| i.type_id == DI).unwrap();
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
        let ev = evaluate(&w, span("2026-11-17 09:00", "2026-11-18 09:00"), &[(MIC, 8)]);
        let mic = &ev.items[0];
        assert_eq!(mic.available, 6);
        assert_eq!(mic.unresolvable, 2);
        assert_eq!(mic.blocking_loans, vec![7]);
    }

    #[test]
    fn overlapping_events_in_same_room_sum_and_asks_are_sufficient() {
        let mut w = World::default();
        w.stock.insert((HALL, MIC), 10);
        w.parents.insert(MIC, None);
        w.lendable.insert(MIC);
        w.occurrences.push(occ(1, 1, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(MIC, 6)]));
        w.occurrences.push(occ(2, 2, HALL, span("2026-11-22 11:00", "2026-11-22 13:00"), &[(MIC, 3)]));
        w.occurrences.push(occ(3, 3, HALL, span("2026-11-22 15:00", "2026-11-22 16:00"), &[(MIC, 5)]));
        let window = span("2026-11-22 00:00", "2026-11-23 00:00");
        let ev = evaluate(&w, window, &[(MIC, 6)]);
        let mic = &ev.items[0];
        assert_eq!(mic.available, 1); // peak is 9 between 11:00-12:00
        assert_eq!(mic.shortfall, 5);
        let res = &mic.resolutions[0];
        assert_eq!(res.extra, 5);

        // Apply the asks and re-check: the full request must now fit.
        let mut w2 = w.clone();
        for a in &res.asks {
            let o = w2.occurrences.iter_mut().find(|o| o.event_id == a.event_id).unwrap();
            o.needs.insert(a.type_id, a.reduce_to);
        }
        let ev2 = evaluate(&w2, window, &[(MIC, 6)]);
        assert!(ev2.fully_available, "{:?}", ev2.items[0].rooms);
    }

    #[test]
    fn check_allocation_flags_new_conflicts() {
        let mut w = world();
        let window = span("2026-11-21 09:00", "2026-11-23 09:00");
        assert!(check_allocation(&w, window, &[(HALL, MIC, 2)]).is_empty());
        w.occurrences.push(occ(200, 1002, HALL, span("2026-11-21 10:00", "2026-11-21 12:00"), &[(MIC, 8)]));
        assert_eq!(check_allocation(&w, window, &[(HALL, MIC, 2)]), vec![(HALL, MIC, 2)]);
    }

    // ------------------------------------------------------------ tree tests

    // Mic(20) -> Wired(21) -> SM58(22), Beta(23); -> Wireless(24) ->
    // Handheld(25), Lapel(26); -> Installed(27, not lendable).
    const MIC_ALL: Id = 20;
    const WIRED: Id = 21;
    const SM58: Id = 22;
    const BETA: Id = 23;
    const WIRELESS: Id = 24;
    const HANDHELD: Id = 25;
    const LAPEL: Id = 26;
    const INSTALLED: Id = 27;

    fn tree_world() -> World {
        let mut w = World::default();
        for (id, parent) in [
            (MIC_ALL, None),
            (WIRED, Some(MIC_ALL)),
            (SM58, Some(WIRED)),
            (BETA, Some(WIRED)),
            (WIRELESS, Some(MIC_ALL)),
            (HANDHELD, Some(WIRELESS)),
            (LAPEL, Some(WIRELESS)),
            (INSTALLED, Some(MIC_ALL)),
        ] {
            w.parents.insert(id, parent);
        }
        w.lendable.extend([SM58, BETA, HANDHELD, LAPEL]);
        w.stock.insert((HALL, SM58), 4);
        w.stock.insert((HALL, BETA), 2);
        w.stock.insert((HALL, HANDHELD), 3);
        w.stock.insert((HALL, LAPEL), 1);
        w.stock.insert((HALL, INSTALLED), 5);
        w.stock.insert((YOUTH, SM58), 2);
        w.stock.insert((YOUTH, HANDHELD), 1);
        w
    }

    const WEEKDAY: (&str, &str) = ("2026-11-17 09:00", "2026-11-19 09:00");
    const SUNDAY: (&str, &str) = ("2026-11-21 09:00", "2026-11-23 09:00");

    #[test]
    fn umbrella_request_pools_models_and_names_concrete_leaves() {
        let w = tree_world();
        let ev = evaluate(&w, span(WEEKDAY.0, WEEKDAY.1), &[(WIRED, 5)]);
        assert!(ev.fully_available, "{:?}", ev.items[0].rooms);
        let item = &ev.items[0];
        // Hall holds 6 wired (4 SM58 + 2 Beta): all 5 from there, SM58 first.
        assert_eq!(
            item.allocation,
            vec![
                AllocQty { room_id: HALL, type_id: SM58, qty: 4 },
                AllocQty { room_id: HALL, type_id: BETA, qty: 1 }
            ]
        );
        let hall = item.rooms.iter().find(|r| r.room_id == HALL).unwrap();
        assert_eq!(hall.stock, 6);
        assert_eq!(hall.spare, 6);
    }

    #[test]
    fn broad_request_never_takes_installed_but_explicit_leaf_does() {
        let w = tree_world();
        let ev = evaluate(&w, span(WEEKDAY.0, WEEKDAY.1), &[(MIC_ALL, 20)]);
        let item = &ev.items[0];
        // 13 lendable mics owned; the 5 installed ones are not lendable.
        assert_eq!(item.available, 13);
        assert_eq!(item.unresolvable, 7);
        assert!(item.allocation.iter().all(|a| a.type_id != INSTALLED));

        let ev = evaluate(&w, span(WEEKDAY.0, WEEKDAY.1), &[(INSTALLED, 3)]);
        assert_eq!(ev.items[0].available, 3);
    }

    #[test]
    fn umbrella_event_need_protects_the_pool_not_a_model() {
        let mut w = tree_world();
        // Sunday in the hall needs any 5 wired mics to stay.
        w.occurrences.push(occ(100, 1000, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(WIRED, 5)]));
        let ev = evaluate(&w, span(SUNDAY.0, SUNDAY.1), &[(SM58, 4)]);
        let item = &ev.items[0];
        // Hall can spare only 1 wired unit (6 owned − 5 needed), youth 2 SM58.
        assert_eq!(item.available, 3);
        assert_eq!(item.shortfall, 1);
        assert_eq!(item.unresolvable, 0);
        let ask = &item.resolutions[0].asks[0];
        assert_eq!(ask.type_id, WIRED);
        assert_eq!((ask.normally, ask.reduce_to), (5, 4));

        // Asks are sufficient: applying them makes the request fit.
        let mut w2 = w.clone();
        w2.occurrences[0].needs.insert(WIRED, 4);
        assert!(evaluate(&w2, span(SUNDAY.0, SUNDAY.1), &[(SM58, 4)]).fully_available);
    }

    #[test]
    fn leaf_and_umbrella_needs_from_overlapping_events_add() {
        let mut w = tree_world();
        let slot = span("2026-11-22 09:00", "2026-11-22 12:00");
        w.occurrences.push(occ(100, 1000, HALL, slot, &[(SM58, 3)]));
        w.occurrences.push(occ(101, 1001, HALL, slot, &[(WIRED, 2)]));
        // 3 SM58 + 2 more wired of any model must stay: of 6 wired in the
        // hall, only 1 can go out, whichever model is asked for.
        let ev = evaluate(&w, span(SUNDAY.0, SUNDAY.1), &[(BETA, 2)]);
        assert_eq!(ev.items[0].available, 1);
        let ev = evaluate(&w, span(SUNDAY.0, SUNDAY.1), &[(WIRED, 3)]);
        let item = &ev.items[0];
        // Hall spares 1, youth spares its 2 SM58.
        assert_eq!(item.available, 3);
    }

    #[test]
    fn specific_item_is_allocated_before_overlapping_umbrella_item() {
        let w = tree_world();
        let ev = evaluate(&w, span(WEEKDAY.0, WEEKDAY.1), &[(MIC_ALL, 10), (SM58, 2)]);
        let sm58 = ev.items.iter().find(|i| i.type_id == SM58).unwrap();
        assert_eq!(sm58.available, 2);
        assert!(sm58.allocation.iter().all(|a| a.type_id == SM58));
        let mic = ev.items.iter().find(|i| i.type_id == MIC_ALL).unwrap();
        assert_eq!(mic.available, 10);
        // The two items together never promise a room's SM58s twice.
        for room in [HALL, YOUTH] {
            let total: i64 = [sm58, mic]
                .iter()
                .flat_map(|i| i.allocation.iter())
                .filter(|a| a.room_id == room && a.type_id == SM58)
                .map(|a| a.qty)
                .sum();
            assert!(total <= w.stock[&(room, SM58)]);
        }
    }

    #[test]
    fn check_allocation_reports_broken_umbrella_need() {
        let mut w = tree_world();
        w.occurrences.push(occ(100, 1000, HALL, span("2026-11-22 09:00", "2026-11-22 12:00"), &[(WIRED, 5)]));
        let window = span(SUNDAY.0, SUNDAY.1);
        assert!(check_allocation(&w, window, &[(HALL, SM58, 1)]).is_empty());
        // The leaf itself has slack, but the umbrella need is broken by 1.
        assert_eq!(check_allocation(&w, window, &[(HALL, SM58, 2)]), vec![(HALL, WIRED, 1)]);
    }
}
