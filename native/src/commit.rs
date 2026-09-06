//! `commit`: validate the fragment completely, then reconcile it into the retained tree.
//!
//! Validation never mutates. Reconciliation is per container: keyed children match by
//! `(key, kind)`, unkeyed children by position among the unkeyed, scope groups by scope id.
//! Old children that nothing reused are detached and disposed at the end of the commit.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::tree::{Inner, Node, NodeId};
use crate::types::*;

/// Ints per record.
pub const RECORD: usize = 8;

/// One record of the fragment.
#[derive(Clone, Copy, Debug)]
pub struct Rec {
    pub kind: i32,
    pub flags: i32,
    pub text: i32,
    pub modifier: i32,
    pub handler: i32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
}

pub const FLAG_KEYED: i32 = 1;
pub const FLAG_STR_KEY: i32 = 2;

/// The frame's flat buffers.
pub struct Fragment<'a> {
    pub ints: &'a [i32],
    pub strs: &'a [&'a str],
}

impl<'a> Fragment<'a> {
    pub fn record_count(&self) -> usize {
        self.ints.len() / RECORD
    }

    /// Index `i` must be `< record_count()` (validation guarantees it).
    pub fn rec(&self, i: usize) -> Rec {
        let r = &self.ints[i * RECORD..i * RECORD + RECORD];
        Rec { kind: r[0], flags: r[1], text: r[2], modifier: r[3], handler: r[4], a: r[5], b: r[6], c: r[7] }
    }

    fn key_str(&self, r: &Rec) -> Option<&'a str> {
        if r.flags & FLAG_STR_KEY != 0 {
            self.strs.get(key_field(r) as usize).copied()
        } else {
            None
        }
    }
}

/// Where a record's key lives: `c` for TEXTFIELD (whose `b` is the placeholder), `b` otherwise.
/// BUTTON keeps its style id in `c` for the same reason (protocol amendment 3).
pub fn key_field(r: &Rec) -> i32 {
    if r.kind == 8 {
        r.c
    } else {
        r.b
    }
}

/// A validated commit, ready to apply. `close[i]` is the index of the END record that closes
/// record `i` (or `i` itself for leaves).
pub struct Plan {
    close: Vec<u32>,
    ranges: Vec<Range>,
}

#[derive(Clone, Copy, Debug)]
struct Range {
    scope: i32,
    first: usize,
    end: usize,
}

macro_rules! bad {
    ($($arg:tt)*) => { return Err(InputError(format!($($arg)*))) };
}

/// The whole commit: intern tables, validate, apply. Returns the disposed scope ids.
/// On `Err` the tree is untouched (the modifier/style tables may have grown, which is harmless).
pub fn commit(
    inner: &mut Inner,
    ints: &[i32],
    strs: &[&str],
    ranges: &[(i64, i64, i64)],
    new_modifiers: &[(i64, Vec<f64>)],
    new_styles: &[(i64, f64, f64, i64)],
) -> Result<Vec<i32>, InputError> {
    let (mods, styles) = parse_tables(inner, new_modifiers, new_styles)?;
    let frag = Fragment { ints, strs };
    let mod_known = |id: u32| inner.modifiers.contains_key(&id) || mods.iter().any(|(m, _)| *m == id);
    let style_known = |id: u32| inner.styles.contains_key(&id) || styles.iter().any(|(s, _)| *s == id);
    let plan = validate(inner, &frag, ranges, &mod_known, &style_known)?;
    for (id, m) in mods {
        inner.modifiers.insert(id, Arc::new(m));
    }
    for (id, s) in styles {
        inner.styles.insert(id, s);
    }
    Ok(apply(inner, &frag, &plan))
}

fn parse_tables(
    inner: &Inner,
    new_modifiers: &[(i64, Vec<f64>)],
    new_styles: &[(i64, f64, f64, i64)],
) -> Result<(Vec<(u32, Modifier)>, Vec<(u32, Style)>), InputError> {
    let mut mods: Vec<(u32, Modifier)> = Vec::with_capacity(new_modifiers.len());
    for (id, ops) in new_modifiers {
        if *id <= 0 || *id > u32::MAX as i64 {
            bad!("modifier id {} is invalid (0 is the empty modifier; ids are positive u32)", id);
        }
        let id = *id as u32;
        let m = Modifier::parse(ops).map_err(|e| InputError(format!("modifier {}: {}", id, e)))?;
        if let Some(existing) = inner.modifiers.get(&id) {
            if **existing != m {
                bad!("modifier id {} is already interned with different ops", id);
            }
            continue;
        }
        if let Some((_, prev)) = mods.iter().find(|(m2, _)| *m2 == id) {
            if *prev != m {
                bad!("modifier id {} appears twice in new_modifiers with different ops", id);
            }
            continue;
        }
        mods.push((id, m));
    }
    let mut styles: Vec<(u32, Style)> = Vec::with_capacity(new_styles.len());
    for (id, size, argb, bold) in new_styles {
        if *id < 0 || *id > u32::MAX as i64 {
            bad!("style id {} is invalid", id);
        }
        let id = *id as u32;
        if !size.is_finite() || *size <= 0.0 || *size > 4096.0 {
            bad!("style {}: size must be in (0, 4096], got {}", id, size);
        }
        let argb = argb_from_f64(*argb).map_err(|e| InputError(format!("style {}: {}", id, e)))?;
        if *bold & !(STYLE_BOLD | STYLE_MONO | STYLE_NOWRAP) != 0 {
            bad!("style {}: unknown flag bits in {} (1 bold, 2 mono, 4 nowrap)", id, bold);
        }
        let s = Style::with_flags(*size as f32, argb, *bold);
        if let Some(existing) = inner.styles.get(&id) {
            if *existing != s {
                bad!("style id {} is already interned with different values", id);
            }
            continue;
        }
        if let Some((_, prev)) = styles.iter().find(|(s2, _)| *s2 == id) {
            if *prev != s {
                bad!("style id {} appears twice in new_styles with different values", id);
            }
            continue;
        }
        styles.push((id, s));
    }
    Ok((mods, styles))
}

#[derive(Clone, Copy)]
enum KeyRef {
    Int(i32),
    Str(usize),
}

struct OpenFrame {
    idx: usize,
    keys_start: usize,
}

/// Every check the protocol promises, before any mutation.
fn validate(
    inner: &Inner,
    frag: &Fragment,
    ranges: &[(i64, i64, i64)],
    mod_known: &dyn Fn(u32) -> bool,
    style_known: &dyn Fn(u32) -> bool,
) -> Result<Plan, InputError> {
    if frag.ints.len() % RECORD != 0 {
        bad!("ints length {} is not a multiple of {} (fixed-width records)", frag.ints.len(), RECORD);
    }
    let n = frag.record_count();
    let mut close = vec![0u32; n];

    // ranges: in bounds, groups exist, unique scopes, disjoint
    let mut plan_ranges = Vec::with_capacity(ranges.len());
    let mut range_scopes: HashSet<i32> = HashSet::new();
    for &(scope, first, end) in ranges {
        if scope < 0 || scope > i32::MAX as i64 {
            bad!("range scope id {} is invalid", scope);
        }
        if first < 0 || end < first || end as u128 > n as u128 {
            let hint = if end > n as i64 && end as u128 <= frag.ints.len() as u128 && end % RECORD as i64 == 0 {
                " (ranges are record indices, not int offsets: divide by 8)"
            } else {
                ""
            };
            bad!("range ({}, {}, {}) is out of bounds for {} records{}", scope, first, end, n, hint);
        }
        let scope = scope as i32;
        if scope != 0 && !inner.scopes.contains_key(&scope) {
            bad!("range for scope {}: no retained group for that scope", scope);
        }
        if !range_scopes.insert(scope) {
            bad!("scope {} has two ranges in one commit", scope);
        }
        plan_ranges.push(Range { scope, first: first as usize, end: end as usize });
    }
    if plan_ranges.len() > 1 {
        let mut spans: Vec<(usize, usize)> = plan_ranges.iter().map(|r| (r.first, r.end)).collect();
        spans.sort_unstable();
        for w in spans.windows(2) {
            if w[0].1 > w[1].0 && w[0].0 != w[0].1 && w[1].0 != w[1].1 {
                bad!("ranges [{}, {}) and [{}, {}) overlap", w[0].0, w[0].1, w[1].0, w[1].1);
            }
        }
    }

    let mut seen_scopes: HashSet<i32> = HashSet::new();
    let mut stack: Vec<OpenFrame> = Vec::new();
    let mut keys: Vec<(i32, KeyRef)> = Vec::new();
    let strs_len = frag.strs.len();

    for range in &plan_ranges {
        let range_group = if range.scope == 0 { inner.root } else { inner.scopes[&range.scope] };
        stack.clear();
        keys.clear();
        let mut i = range.first;
        while i < range.end {
            let r = frag.rec(i);
            match r.kind {
                REC_END => {
                    let Some(frame) = stack.pop() else {
                        bad!("record {}: END closes nothing (range for scope {})", i, range.scope);
                    };
                    close[frame.idx] = i as u32;
                    check_duplicate_keys(&keys[frame.keys_start..], frag, frame.idx)?;
                    keys.truncate(frame.keys_start);
                }
                REC_SCOPE | REC_SCOPE_REF => {
                    if r.flags != 0 {
                        bad!("record {}: scope records carry no key (flags must be 0)", i);
                    }
                    let sid = r.a;
                    if sid == 0 {
                        bad!("record {}: scope id 0 is the root and cannot be nested", i);
                    }
                    if !seen_scopes.insert(sid) {
                        bad!("record {}: scope {} occurs twice in this commit", i, sid);
                    }
                    match inner.scopes.get(&sid) {
                        Some(&g) => {
                            if g == range_group {
                                bad!("record {}: scope {} cannot contain itself", i, sid);
                            }
                            if !inner.is_descendant_or_self(g, range_group) {
                                bad!("record {}: retained scope {} is not inside scope {}", i, sid, range.scope);
                            }
                        }
                        None => {
                            if r.kind == REC_SCOPE_REF {
                                bad!("record {}: SCOPE_REF {} has no retained group", i, sid);
                            }
                        }
                    }
                    if r.kind == REC_SCOPE {
                        if range_scopes.contains(&sid) {
                            bad!("record {}: scope {} is both a range and a SCOPE record", i, sid);
                        }
                        stack.push(OpenFrame { idx: i, keys_start: keys.len() });
                    } else {
                        close[i] = i as u32;
                    }
                }
                k => {
                    let Some(kind) = Kind::from_record(k) else {
                        bad!("record {}: unknown kind {}", i, k);
                    };
                    if r.flags & !(FLAG_KEYED | FLAG_STR_KEY) != 0 {
                        bad!("record {}: unknown flag bits in {}", i, r.flags);
                    }
                    if r.flags & FLAG_KEYED != 0 {
                        let kf = key_field(&r);
                        if r.flags & FLAG_STR_KEY != 0 {
                            if kf < 0 || kf as usize >= strs_len {
                                bad!("record {}: string key index {} out of range ({} strings)", i, kf, strs_len);
                            }
                            keys.push((k, KeyRef::Str(kf as usize)));
                        } else {
                            keys.push((k, KeyRef::Int(kf)));
                        }
                    } else if r.flags & FLAG_STR_KEY != 0 {
                        bad!("record {}: string-key flag without the key flag", i);
                    }
                    if r.text != -1 && (r.text < 0 || r.text as usize >= strs_len) {
                        bad!("record {}: text index {} out of range ({} strings)", i, r.text, strs_len);
                    }
                    if r.modifier < 0 || !mod_known(r.modifier as u32) {
                        bad!("record {}: unknown modifier id {}", i, r.modifier);
                    }
                    if r.handler < -1 {
                        bad!("record {}: handler index {} is invalid", i, r.handler);
                    }
                    match kind {
                        Kind::Column | Kind::Row => {
                            if !(0..=4).contains(&r.a) {
                                bad!("record {}: arrangement {} must be 0..=4", i, r.a);
                            }
                            if !(0..=2).contains(&r.c) {
                                bad!("record {}: cross-axis alignment {} must be 0..=2", i, r.c);
                            }
                        }
                        Kind::Box => {
                            if !(0..=2).contains(&r.a) {
                                bad!("record {}: alignment {} must be 0..=2", i, r.a);
                            }
                        }
                        Kind::Text => {
                            if r.a < 0 || !style_known(r.a as u32) {
                                bad!("record {}: unknown style id {}", i, r.a);
                            }
                        }
                        Kind::Button => {
                            if !(0..=1).contains(&r.a) {
                                bad!("record {}: enabled {} must be 0 or 1", i, r.a);
                            }
                            if r.c < 0 || !style_known(r.c as u32) {
                                bad!("record {}: unknown style id {}", i, r.c);
                            }
                        }
                        Kind::TextField => {
                            if r.b != -1 && (r.b < 0 || r.b as usize >= strs_len) {
                                bad!("record {}: placeholder index {} out of range", i, r.b);
                            }
                            if r.a < 0 || !style_known(r.a as u32) {
                                bad!("record {}: unknown style id {}", i, r.a);
                            }
                        }
                        Kind::Checkbox => {
                            if !(0..=1).contains(&r.a) {
                                bad!("record {}: checked {} must be 0 or 1", i, r.a);
                            }
                        }
                        Kind::Layout => {
                            if r.handler < 0 {
                                bad!("record {}: LAYOUT needs a measure callback handler index", i);
                            }
                        }
                        Kind::Scroll => {
                            // 0 scrolls down only; 1 also lays the content out at its own
                            // width and pans sideways, which is where a long line goes
                            if !(0..=1).contains(&r.a) {
                                bad!("record {}: scroll axis {} must be 0 (down) or 1 (down and across)", i, r.a);
                            }
                            if !(0..=2).contains(&r.c) {
                                bad!("record {}: cross-axis alignment {} must be 0..=2", i, r.c);
                            }
                        }
                        Kind::Popup => {
                            if !(-100_000..=100_000).contains(&r.a) || !(-100_000..=100_000).contains(&r.c) {
                                bad!("record {}: popup position ({}, {}) is out of range", i, r.a, r.c);
                            }
                        }
                        Kind::Spacer | Kind::Canvas | Kind::Scope => {}
                    }
                    if kind.has_children() {
                        stack.push(OpenFrame { idx: i, keys_start: keys.len() });
                    } else {
                        close[i] = i as u32;
                    }
                }
            }
            i += 1;
        }
        if let Some(open) = stack.last() {
            bad!(
                "record {} ({}) is not closed before the end of the range for scope {}",
                open.idx,
                Kind::from_record(frag.rec(open.idx).kind).map(|k| k.name()).unwrap_or("scope"),
                range.scope
            );
        }
        check_duplicate_keys(&keys, frag, usize::MAX)?;
    }
    Ok(Plan { close, ranges: plan_ranges })
}

fn check_duplicate_keys(keys: &[(i32, KeyRef)], frag: &Fragment, container: usize) -> Result<(), InputError> {
    if keys.len() < 2 {
        return Ok(());
    }
    let describe = |k: &(i32, KeyRef)| -> String {
        match k.1 {
            KeyRef::Int(v) => format!("kind {} key {}", k.0, v),
            KeyRef::Str(i) => format!("kind {} key {:?}", k.0, frag.strs.get(i).copied().unwrap_or("")),
        }
    };
    let dup = |a: &(i32, KeyRef), b: &(i32, KeyRef)| -> bool {
        a.0 == b.0
            && match (a.1, b.1) {
                (KeyRef::Int(x), KeyRef::Int(y)) => x == y,
                (KeyRef::Str(x), KeyRef::Str(y)) => frag.strs.get(x) == frag.strs.get(y),
                _ => false,
            }
    };
    let found = if keys.len() <= 16 {
        let mut found = None;
        'outer: for i in 0..keys.len() {
            for j in i + 1..keys.len() {
                if dup(&keys[i], &keys[j]) {
                    found = Some(keys[i]);
                    break 'outer;
                }
            }
        }
        found
    } else {
        let mut sorted: Vec<(i32, bool, i32, &str)> = keys
            .iter()
            .map(|k| match k.1 {
                KeyRef::Int(v) => (k.0, false, v, ""),
                KeyRef::Str(i) => (k.0, true, 0, frag.strs.get(i).copied().unwrap_or("")),
            })
            .collect();
        sorted.sort_unstable();
        sorted.windows(2).find(|w| w[0] == w[1]).map(|w| {
            (w[0].0, if w[0].1 { KeyRef::Str(keys.iter().find_map(|k| match k.1 { KeyRef::Str(i) if frag.strs.get(i).copied() == Some(w[0].3) => Some(i), _ => None }).unwrap_or(0)) } else { KeyRef::Int(w[0].2) })
        })
    };
    if let Some(k) = found {
        let where_ = if container == usize::MAX { "the range".to_string() } else { format!("record {}", container) };
        bad!("duplicate key among the children of {}: {}", where_, describe(&k));
    }
    Ok(())
}

struct Ctx<'a> {
    frag: &'a Fragment<'a>,
    close: &'a [u32],
    serial: u32,
    detached: Vec<NodeId>,
}

fn apply(inner: &mut Inner, frag: &Fragment, plan: &Plan) -> Vec<i32> {
    inner.commit_serial = inner.commit_serial.wrapping_add(1);
    if inner.commit_serial == 0 {
        inner.commit_serial = 1;
    }
    let mut ctx = Ctx { frag, close: &plan.close, serial: inner.commit_serial, detached: Vec::new() };
    for range in &plan.ranges {
        let group = if range.scope == 0 {
            inner.root
        } else {
            match inner.scopes.get(&range.scope) {
                Some(&g) => g,
                None => panic!("internal: validated scope {} vanished during commit", range.scope),
            }
        };
        reconcile_children(inner, group, range.first, range.end, &mut ctx);
    }
    let mut disposed = Vec::new();
    for id in ctx.detached {
        if inner.nodes.get(id).map(|n| n.parent.is_none()) == Some(true) {
            inner.dispose(id, &mut disposed);
        }
    }
    disposed
}

fn key_matches(key: &Key, r: &Rec, frag: &Fragment) -> bool {
    if r.flags & FLAG_KEYED == 0 {
        return *key == Key::None;
    }
    match key {
        Key::Str(s) => frag.key_str(r) == Some(&**s),
        Key::Int(v) => r.flags & FLAG_STR_KEY == 0 && *v == key_field(r),
        Key::None => false,
    }
}

fn key_of(r: &Rec, frag: &Fragment) -> Key {
    if r.flags & FLAG_KEYED == 0 {
        Key::None
    } else if let Some(s) = frag.key_str(r) {
        Key::Str(Arc::from(s))
    } else {
        Key::Int(key_field(r))
    }
}

fn matches_positional(inner: &Inner, old_id: NodeId, parent: NodeId, r: &Rec, frag: &Fragment) -> bool {
    let Some(n) = inner.nodes.get(old_id) else { return false };
    if n.parent != Some(parent) {
        return false; // claimed by another container earlier in this commit
    }
    match r.kind {
        REC_SCOPE | REC_SCOPE_REF => n.is_group() && n.scope_id == r.a,
        k => !n.is_group() && Kind::from_record(k) == Some(n.kind) && key_matches(&n.key, r, frag),
    }
}

/// Replace the children of `parent` with the records `[first, end)`.
fn reconcile_children(inner: &mut Inner, parent: NodeId, first: usize, end: usize, ctx: &mut Ctx) {
    let serial = ctx.serial;
    inner.nodes[parent].frame_serial = serial;

    // fast path: everything lines up positionally → no allocation
    let mut oi = 0usize;
    let mut j = first;
    while j < end {
        let Some(&old_id) = inner.nodes[parent].children.get(oi) else { break };
        let r = ctx.frag.rec(j);
        if !matches_positional(inner, old_id, parent, &r, ctx.frag) {
            break;
        }
        reuse(inner, old_id, &r, j, ctx);
        oi += 1;
        j = ctx.close[j] as usize + 1;
    }
    if j >= end && oi == inner.nodes[parent].children.len() {
        return;
    }

    // slow path: structure changed after `oi`
    let old: Vec<NodeId> = inner.nodes[parent].children.clone();
    let mut new: Vec<NodeId> = old[..oi].to_vec();
    let mut keyed_old: Vec<NodeId> = Vec::new();
    let mut unkeyed_old: Vec<NodeId> = Vec::new();
    for &c in &old[oi..] {
        let n = &inner.nodes[c];
        if n.is_group() || n.parent != Some(parent) {
            continue;
        }
        if n.key == Key::None {
            unkeyed_old.push(c);
        } else {
            keyed_old.push(c);
        }
    }
    let mut keyed_used = vec![false; keyed_old.len()];
    let keyed_index: Option<HashMap<(Kind, Key), usize>> = if keyed_old.len() > 32 {
        Some(keyed_old.iter().enumerate().map(|(i, &c)| ((inner.nodes[c].kind, inner.nodes[c].key.clone()), i)).collect())
    } else {
        None
    };
    let mut ucur = 0usize;

    while j < end {
        let r = ctx.frag.rec(j);
        let id = match r.kind {
            REC_SCOPE | REC_SCOPE_REF => {
                let g = group_for(inner, r.a, parent, ctx);
                if r.kind == REC_SCOPE {
                    reconcile_children(inner, g, j + 1, ctx.close[j] as usize, ctx);
                }
                g
            }
            k => {
                let kind = Kind::from_record(k).unwrap_or_else(|| panic!("internal: unvalidated kind {}", k));
                if r.flags & FLAG_KEYED != 0 {
                    let found = match &keyed_index {
                        Some(map) => map.get(&(kind, key_of(&r, ctx.frag))).copied().filter(|&i| !keyed_used[i]),
                        None => keyed_old
                            .iter()
                            .enumerate()
                            .position(|(i, &c)| !keyed_used[i] && inner.nodes[c].kind == kind && key_matches(&inner.nodes[c].key, &r, ctx.frag)),
                    };
                    match found {
                        Some(i) => {
                            keyed_used[i] = true;
                            let c = keyed_old[i];
                            reuse(inner, c, &r, j, ctx);
                            c
                        }
                        None => create(inner, parent, kind, &r, j, ctx),
                    }
                } else {
                    let cand = unkeyed_old.get(ucur).copied();
                    ucur += 1;
                    match cand {
                        Some(c) if inner.nodes[c].kind == kind => {
                            reuse(inner, c, &r, j, ctx);
                            c
                        }
                        _ => create(inner, parent, kind, &r, j, ctx),
                    }
                }
            }
        };
        new.push(id);
        j = ctx.close[j] as usize + 1;
    }

    for &c in &old[oi..] {
        let Some(n) = inner.nodes.get_mut(c) else { continue };
        if n.mark != serial && n.parent == Some(parent) {
            n.parent = None;
            ctx.detached.push(c);
        }
    }
    inner.nodes[parent].children = new;
    inner.mark_dirty(parent);
}

/// The group for `sid`, created if needed, attached under `parent`.
fn group_for(inner: &mut Inner, sid: i32, parent: NodeId, ctx: &mut Ctx) -> NodeId {
    if let Some(&g) = inner.scopes.get(&sid) {
        attach_group(inner, g, parent, ctx);
        return g;
    }
    let mut node = Node::new(Kind::Scope, inner.empty_modifier.clone());
    node.scope_id = sid;
    node.parent = Some(parent);
    node.mark = ctx.serial;
    node.dirty = false;
    let g = inner.nodes.insert(node);
    inner.scopes.insert(sid, g);
    g
}

fn attach_group(inner: &mut Inner, g: NodeId, parent: NodeId, ctx: &mut Ctx) {
    inner.nodes[g].mark = ctx.serial;
    let current = inner.nodes[g].parent;
    if current == Some(parent) {
        return;
    }
    if inner.is_descendant_or_self(parent, g) {
        panic!("internal: attaching scope {} under its own subtree (validation missed a cycle)", inner.nodes[g].scope_id);
    }
    if let Some(q) = current {
        // `q` still lists `g`; if `q` is being reconciled right now its frame drops the stale
        // entry itself (parent pointer mismatch), otherwise remove it here.
        if inner.nodes[q].frame_serial != ctx.serial {
            inner.nodes[q].children.retain(|&c| c != g);
            inner.mark_dirty(q);
        }
    }
    inner.nodes[g].parent = Some(parent);
    inner.mark_dirty(g);
}

fn resolve_style(inner: &Inner, id: i32) -> Style {
    inner.styles.get(&(id.max(0) as u32)).copied().unwrap_or(Style::DEFAULT)
}

/// Update a retained node from its record and reconcile its children.
fn reuse(inner: &mut Inner, id: NodeId, r: &Rec, j: usize, ctx: &mut Ctx) {
    let serial = ctx.serial;
    let close = ctx.close[j] as usize;
    let is_group = inner.nodes[id].is_group();
    if is_group {
        inner.nodes[id].mark = serial;
        if r.kind == REC_SCOPE {
            reconcile_children(inner, id, j + 1, close, ctx);
        }
        return;
    }
    let new_modifier = if inner.nodes[id].modifier_id != r.modifier as u32 {
        Some(inner.modifiers.get(&(r.modifier as u32)).cloned().unwrap_or_else(|| panic!("internal: unvalidated modifier {}", r.modifier)))
    } else {
        None
    };
    let style = match Kind::from_record(r.kind) {
        Some(Kind::Text) | Some(Kind::TextField) => Some(resolve_style(inner, r.a)),
        Some(Kind::Button) => Some(resolve_style(inner, r.c)),
        _ => None,
    };
    let strs = ctx.frag.strs;
    let reveal_now = new_modifier.as_ref().map(|m| m.reveal && !inner.nodes[id].modifier.reveal).unwrap_or(false);
    let node = &mut inner.nodes[id];
    node.mark = serial;
    let kind = node.kind;
    let mut changed = false;
    let mut text_changed = false;

    let new_text: Option<&str> = if r.text >= 0 { strs.get(r.text as usize).copied() } else { None };
    if node.text.as_deref() != new_text {
        node.text = new_text.map(Arc::from);
        changed = true;
        text_changed = true;
    }
    if kind == Kind::TextField {
        let ph: Option<&str> = if r.b >= 0 { strs.get(r.b as usize).copied() } else { None };
        if node.placeholder.as_deref() != ph {
            node.placeholder = ph.map(Arc::from);
            changed = true;
        }
    }
    if let Some(m) = new_modifier {
        node.modifier_id = r.modifier as u32;
        node.modifier = m;
        changed = true;
    }
    node.handler = r.handler;
    if node.a != r.a || node.b != r.b || node.c != r.c {
        node.a = r.a;
        node.b = r.b;
        node.c = r.c;
        changed = true;
    }
    if let Some(s) = style {
        if node.style != s {
            node.style = s;
            changed = true;
        }
    }
    let flag_canvas = kind == Kind::Canvas && !node.canvas_pending;
    if flag_canvas {
        node.canvas_pending = true;
    }
    if flag_canvas {
        inner.canvas_pending.push(id);
    }
    if reveal_now {
        inner.reveal_pending.push(id);
    }
    if changed {
        inner.mark_dirty(id);
    }
    if text_changed && kind == Kind::TextField {
        sync_focus(inner, id);
    }
    if kind.has_children() {
        reconcile_children(inner, id, j + 1, close, ctx);
    }
}

/// Python's value is the source of truth for a focused field: reload the edit buffer.
fn sync_focus(inner: &mut Inner, id: NodeId) {
    let text = inner.nodes[id].text.clone();
    if let Some(f) = inner.focus.as_mut() {
        if f.node == id {
            let t = text.as_deref().unwrap_or("");
            if f.buffer != t {
                f.buffer = t.to_string();
                f.caret = f.caret.min(f.buffer.chars().count());
            }
        }
    }
}

fn create(inner: &mut Inner, parent: NodeId, kind: Kind, r: &Rec, j: usize, ctx: &mut Ctx) -> NodeId {
    let modifier = inner
        .modifiers
        .get(&(r.modifier as u32))
        .cloned()
        .unwrap_or_else(|| panic!("internal: unvalidated modifier {}", r.modifier));
    let strs = ctx.frag.strs;
    let mut node = Node::new(kind, modifier);
    node.modifier_id = r.modifier as u32;
    node.key = key_of(r, ctx.frag);
    node.parent = Some(parent);
    node.mark = ctx.serial;
    node.text = if r.text >= 0 { strs.get(r.text as usize).map(|s| Arc::from(*s)) } else { None };
    if kind == Kind::TextField && r.b >= 0 {
        node.placeholder = strs.get(r.b as usize).map(|s| Arc::from(*s));
    }
    node.handler = r.handler;
    node.a = r.a;
    node.b = r.b;
    node.c = r.c;
    node.style = match kind {
        Kind::Text | Kind::TextField => resolve_style(inner, r.a),
        Kind::Button => resolve_style(inner, r.c),
        _ => Style::DEFAULT,
    };
    node.canvas_pending = kind == Kind::Canvas;
    let reveal = node.modifier.reveal;
    let id = inner.nodes.insert(node);
    if kind == Kind::Canvas {
        inner.canvas_pending.push(id);
    }
    if kind == Kind::Popup {
        inner.popups.push(id);
    }
    if reveal {
        inner.reveal_pending.push(id);
    }
    if kind.has_children() {
        reconcile_children(inner, id, j + 1, ctx.close[j] as usize, ctx);
    }
    id
}

/// Parse canvas draw commands (`set_canvas_commands`).
pub fn parse_canvas(inner: &Inner, floats: &[f64], strs: &[&str]) -> Result<Vec<CanvasCmd>, InputError> {
    let mut out = Vec::new();
    let mut i = 0;
    let fin = |v: f64, what: &str, at: usize| -> Result<f32, InputError> {
        if v.is_finite() {
            Ok(v as f32)
        } else {
            Err(InputError(format!("canvas op at {}: {} must be finite, got {}", at, what, v)))
        }
    };
    while i < floats.len() {
        let op = floats[i];
        let need = |n: usize| -> Result<&[f64], InputError> {
            if i + 1 + n > floats.len() {
                Err(InputError(format!("canvas op {} at {} needs {} args", op, i, n)))
            } else {
                Ok(&floats[i + 1..i + 1 + n])
            }
        };
        if op == 1.0 {
            let a = need(5)?;
            out.push(CanvasCmd::Rect {
                x: fin(a[0], "x", i)?,
                y: fin(a[1], "y", i)?,
                w: fin(a[2], "w", i)?,
                h: fin(a[3], "h", i)?,
                argb: argb_from_f64(a[4]).map_err(InputError)?,
            });
            i += 6;
        } else if op == 2.0 {
            let a = need(4)?;
            out.push(CanvasCmd::Circle {
                cx: fin(a[0], "cx", i)?,
                cy: fin(a[1], "cy", i)?,
                r: fin(a[2], "r", i)?,
                argb: argb_from_f64(a[3]).map_err(InputError)?,
            });
            i += 5;
        } else if op == 3.0 {
            let a = need(6)?;
            out.push(CanvasCmd::Line {
                x1: fin(a[0], "x1", i)?,
                y1: fin(a[1], "y1", i)?,
                x2: fin(a[2], "x2", i)?,
                y2: fin(a[3], "y2", i)?,
                argb: argb_from_f64(a[4]).map_err(InputError)?,
                stroke: fin(a[5], "stroke", i)?.max(0.0),
            });
            i += 7;
        } else if op == 5.0 {
            let a = need(8)?;
            out.push(CanvasCmd::Curve {
                x1: fin(a[0], "x1", i)?,
                y1: fin(a[1], "y1", i)?,
                cx: fin(a[2], "cx", i)?,
                cy: fin(a[3], "cy", i)?,
                x2: fin(a[4], "x2", i)?,
                y2: fin(a[5], "y2", i)?,
                argb: argb_from_f64(a[6]).map_err(InputError)?,
                stroke: fin(a[7], "stroke", i)?.max(0.0),
            });
            i += 9;
        } else if op == 4.0 {
            let a = need(4)?;
            let ti = a[2];
            if !ti.is_finite() || ti < 0.0 || ti.fract() != 0.0 || ti as usize >= strs.len() {
                bad!("canvas text op at {}: text index {} out of range ({} strings)", i, ti, strs.len());
            }
            let si = a[3];
            if !si.is_finite() || si < 0.0 || si.fract() != 0.0 || si > u32::MAX as f64 {
                bad!("canvas text op at {}: bad style id {}", i, si);
            }
            let Some(style) = inner.styles.get(&(si as u32)).copied() else {
                bad!("canvas text op at {}: unknown style id {}", i, si);
            };
            out.push(CanvasCmd::Text { x: fin(a[0], "x", i)?, y: fin(a[1], "y", i)?, text: Arc::from(strs[ti as usize]), style });
            i += 5;
        } else {
            bad!("canvas: unknown op {} at {}", op, i);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextSystem;

    pub(crate) fn core() -> Inner {
        Inner::new(800.0, 600.0, 1.0, TextSystem::monospace_only())
    }

    /// Build records with a tiny DSL: each tuple is (kind, flags, text, modifier, handler, a, b, c).
    pub(crate) fn recs(list: &[[i32; 8]]) -> Vec<i32> {
        list.iter().flatten().copied().collect()
    }

    const END: [i32; 8] = [0, 0, -1, 0, -1, 0, 0, 0];
    fn column() -> [i32; 8] {
        [3, 0, -1, 0, -1, 0, 0, 0]
    }
    fn text(idx: i32) -> [i32; 8] {
        [6, 0, idx, 0, -1, 0, 0, 0]
    }
    fn keyed_text(idx: i32, key: i32) -> [i32; 8] {
        [6, 1, idx, 0, -1, 0, key, 0]
    }
    fn button(idx: i32, handler: i32) -> [i32; 8] {
        [7, 0, idx, 0, handler, 1, 0, 0]
    }
    fn scope(id: i32) -> [i32; 8] {
        [1, 0, -1, 0, -1, id, 0, 0]
    }
    fn scope_ref(id: i32) -> [i32; 8] {
        [2, 0, -1, 0, -1, id, 0, 0]
    }

    fn do_commit(inner: &mut Inner, ints: &[i32], strs: &[&str], ranges: &[(i64, i64, i64)]) -> Result<Vec<i32>, InputError> {
        commit(inner, ints, strs, ranges, &[], &[])
    }

    fn child_ids(inner: &Inner, id: NodeId) -> Vec<NodeId> {
        inner.nodes[id].children.clone()
    }

    #[test]
    fn keyed_children_reorder_without_recreating() {
        let mut inner = core();
        let strs = ["a", "b", "c"];
        let ints = recs(&[column(), keyed_text(0, 10), keyed_text(1, 11), keyed_text(2, 12), END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 5)]).unwrap();
        let col = child_ids(&inner, inner.root)[0];
        let before = child_ids(&inner, col);
        assert_eq!(before.len(), 3);
        // reverse the order
        let ints = recs(&[column(), keyed_text(2, 12), keyed_text(1, 11), keyed_text(0, 10), END]);
        let disposed = do_commit(&mut inner, &ints, &strs, &[(0, 0, 5)]).unwrap();
        assert!(disposed.is_empty());
        let after = child_ids(&inner, col);
        assert_eq!(after, vec![before[2], before[1], before[0]]);
        assert_eq!(inner.node_count(), 4);
        assert_eq!(inner.nodes[after[0]].text_str(), "c");
    }

    #[test]
    fn unkeyed_children_match_by_position() {
        let mut inner = core();
        let strs = ["a", "b", "x"];
        let ints = recs(&[column(), text(0), text(1), END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 4)]).unwrap();
        let col = child_ids(&inner, inner.root)[0];
        let before = child_ids(&inner, col);
        // same shape, different text: nodes kept, text updated
        let ints = recs(&[column(), text(2), text(1), END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 4)]).unwrap();
        let after = child_ids(&inner, col);
        assert_eq!(before, after);
        assert_eq!(inner.nodes[after[0]].text_str(), "x");
        // a button at position 0 replaces the text there; the second text survives
        let ints = recs(&[column(), button(0, 3), text(1), END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 4)]).unwrap();
        let after2 = child_ids(&inner, col);
        assert_ne!(after2[0], before[0]);
        assert_eq!(after2[1], before[1]);
        assert!(!inner.nodes.contains_key(before[0]));
        assert_eq!(inner.node_count(), 3);
    }

    #[test]
    fn scope_ref_keeps_subtree_and_disposed_scopes_are_returned() {
        let mut inner = core();
        let strs = ["a", "b", "c"];
        let ints = recs(&[column(), scope(1), text(0), END, scope(2), text(1), END, END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 8)]).unwrap();
        let col = child_ids(&inner, inner.root)[0];
        let g1 = inner.scopes[&1];
        let g2 = inner.scopes[&2];
        let t1 = child_ids(&inner, g1)[0];
        assert_eq!(inner.layout_children(col).len(), 2);
        // root re-runs: scope 1 skipped (ref), scope 2 gone, new text after
        let ints = recs(&[column(), scope_ref(1), text(2), END]);
        let disposed = do_commit(&mut inner, &ints, &strs, &[(0, 0, 4)]).unwrap();
        assert_eq!(disposed, vec![2]);
        assert!(!inner.nodes.contains_key(g2));
        assert_eq!(inner.scopes.get(&1), Some(&g1));
        assert_eq!(child_ids(&inner, g1), vec![t1]);
        assert_eq!(inner.nodes[t1].text_str(), "a");
        assert_eq!(inner.layout_children(col).len(), 2);
        // a range for scope 1 alone replaces only its content
        let ints = recs(&[text(1), text(2)]);
        let disposed = do_commit(&mut inner, &ints, &strs, &[(1, 0, 2)]).unwrap();
        assert!(disposed.is_empty());
        assert_eq!(child_ids(&inner, g1).len(), 2);
        assert_eq!(child_ids(&inner, g1)[0], t1);
        assert_eq!(inner.nodes[t1].text_str(), "b");
        assert_eq!(inner.node_count(), 4);
    }

    #[test]
    fn malformed_input_leaves_tree_untouched() {
        let mut inner = core();
        let strs = ["a"];
        let ints = recs(&[column(), text(0), END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 3)]).unwrap();
        let dump = inner.dump();
        let cases: Vec<(Vec<i32>, Vec<(i64, i64, i64)>)> = vec![
            (recs(&[column(), text(0)]), vec![(0, 0, 2)]),                    // unclosed
            (recs(&[column(), text(0), END, END]), vec![(0, 0, 4)]),          // extra END
            (recs(&[[99, 0, -1, 0, -1, 0, 0, 0]]), vec![(0, 0, 1)]),          // unknown kind
            (recs(&[text(7)]), vec![(0, 0, 1)]),                              // text index
            (recs(&[[6, 0, 0, 5, -1, 0, 0, 0]]), vec![(0, 0, 1)]),            // modifier id
            (recs(&[text(0)]), vec![(0, 0, 2)]),                              // range out of bounds
            (recs(&[text(0)]), vec![(9, 0, 1)]),                              // unknown range scope
            (recs(&[scope_ref(5)]), vec![(0, 0, 1)]),                         // unknown scope ref
            (recs(&[scope(1), END, scope(1), END]), vec![(0, 0, 4)]),         // scope twice
            (recs(&[column(), keyed_text(0, 1), keyed_text(0, 1), END]), vec![(0, 0, 4)]), // dup key
            (vec![1, 2, 3], vec![(0, 0, 0)]),                                 // not a multiple of 8
            (recs(&[[3, 0, -1, 0, -1, 7, 0, 0], END]), vec![(0, 0, 2)]),      // bad arrangement
            (recs(&[[3, 0, -1, 0, -1, 0, 0, 5], END]), vec![(0, 0, 2)]),      // bad cross alignment
        ];
        for (ints, ranges) in cases {
            let r = do_commit(&mut inner, &ints, &strs, &ranges);
            assert!(r.is_err(), "expected error for {:?}", ints);
            assert_eq!(inner.dump(), dump);
        }
    }

    #[test]
    fn duplicate_scope_content_rejected_and_self_containment_rejected() {
        let mut inner = core();
        let strs = ["a"];
        let ints = recs(&[scope(1), text(0), END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 3)]).unwrap();
        // range for scope 1 that references itself
        let ints = recs(&[scope_ref(1)]);
        assert!(do_commit(&mut inner, &ints, &strs, &[(1, 0, 1)]).is_err());
        // scope 1 both as range and as SCOPE record
        let ints = recs(&[scope(1), text(0), END, text(0)]);
        assert!(do_commit(&mut inner, &ints, &strs, &[(0, 0, 3), (1, 3, 4)]).is_err());
        // scope_ref 1 in root plus range for 1 is fine (parent skipped it, child re-ran)
        let ints = recs(&[scope_ref(1), text(0), text(0)]);
        assert!(do_commit(&mut inner, &ints, &strs, &[(0, 0, 1), (1, 1, 3)]).is_ok());
        assert_eq!(inner.nodes[inner.scopes[&1]].children.len(), 2);
    }

    #[test]
    fn tables_are_validated_before_interning() {
        let mut inner = core();
        assert!(commit(&mut inner, &[], &[], &[], &[(1, vec![1.0, 1.0])], &[]).is_err());
        assert!(!inner.modifiers.contains_key(&1));
        assert!(commit(&mut inner, &[], &[], &[], &[(1, vec![2.0, 10.0])], &[(1, 20.0, 0xFF000000u32 as f64, 1)]).is_ok());
        assert!(commit(&mut inner, &[], &[], &[], &[(1, vec![2.0, 11.0])], &[]).is_err());
        assert!(commit(&mut inner, &[], &[], &[], &[(1, vec![2.0, 10.0])], &[]).is_ok());
        assert!(commit(&mut inner, &[], &[], &[], &[], &[(0, 15.0, 0.0, 0)]).is_err()); // redefine default
        assert!(commit(&mut inner, &[], &[], &[], &[], &[(2, 12.0, 0.0, 8)]).is_err()); // unknown style flag
        assert!(commit(&mut inner, &[], &[], &[], &[], &[(2, 12.0, 0.0, 7)]).is_ok());
        assert!(inner.styles[&2].mono && inner.styles[&2].nowrap && inner.styles[&2].bold);
    }

    #[test]
    fn scroll_records_and_reveal_are_tracked() {
        let mut inner = core();
        let strs = ["a"];
        // axis 1 also pans sideways; anything past that is rejected
        let ints = recs(&[[13, 0, -1, 0, -1, 1, 0, 0], END]);
        assert!(do_commit(&mut inner, &ints, &strs, &[(0, 0, 2)]).is_ok());
        let ints = recs(&[[13, 0, -1, 0, -1, 2, 0, 0], END]);
        assert!(do_commit(&mut inner, &ints, &strs, &[(0, 0, 2)]).is_err());
        let ints = recs(&[[13, 0, -1, 0, -1, 0, 0, 0], text(0), END]);
        do_commit(&mut inner, &ints, &strs, &[(0, 0, 3)]).unwrap();
        assert!(inner.reveal_pending.is_empty());
        // a modifier that gains `reveal` queues the node once
        let mods = vec![(1, vec![14.0])];
        let ints = recs(&[[13, 0, -1, 0, -1, 0, 0, 0], [6, 0, 0, 1, -1, 0, 0, 0], END]);
        commit(&mut inner, &ints, &strs, &[(0, 0, 3)], &mods, &[]).unwrap();
        assert_eq!(inner.reveal_pending.len(), 1);
        inner.reveal_pending.clear();
        commit(&mut inner, &ints, &strs, &[(0, 0, 3)], &[], &[]).unwrap();
        assert!(inner.reveal_pending.is_empty(), "unchanged modifier: no new request");
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::text::TextSystem;
    use std::time::Instant;

    /// Not an assertion, a reference number: `cargo test --release --no-default-features bench -- --nocapture`.
    #[test]
    fn unchanged_commit_10k_reference() {
        let mut inner = Inner::new(800.0, 600.0, 1.0, TextSystem::monospace_only());
        let strs: Vec<String> = (0..50).map(|i| format!("item {}", i)).chain(["Row".to_string(), "go".to_string()]).collect();
        let strs: Vec<&str> = strs.iter().map(|s| s.as_str()).collect();
        let mut ints: Vec<i32> = vec![3, 0, -1, 0, -1, 0, 0, 0];
        let mut n = 1;
        while n < 10_000 {
            ints.extend([4, 1, -1, 0, -1, 0, n, 0]);
            ints.extend([6, 0, n % 50, 0, -1, 0, 0, 0]);
            ints.extend([6, 0, (n * 7) % 50, 0, -1, 0, 0, 0]);
            ints.extend([7, 0, 51, 0, n % 100, 1, 0, 0]);
            ints.extend([0, 0, -1, 0, -1, 0, 0, 0]);
            n += 4;
        }
        ints.extend([0, 0, -1, 0, -1, 0, 0, 0]);
        let ranges = [(0i64, 0i64, (ints.len() / 8) as i64)];
        let t = Instant::now();
        commit(&mut inner, &ints, &strs, &ranges, &[], &[]).unwrap();
        let first = t.elapsed();
        let mut best = std::time::Duration::MAX;
        for _ in 0..50 {
            let t = Instant::now();
            commit(&mut inner, &ints, &strs, &ranges, &[], &[]).unwrap();
            best = best.min(t.elapsed());
        }
        println!(
            "rust-only: {} nodes, first commit {:.3} ms, unchanged commit best {:.3} ms",
            inner.node_count(),
            first.as_secs_f64() * 1e3,
            best.as_secs_f64() * 1e3
        );
    }
}
