//! Layout: single pass, constraints down, sizes up, with a per-node `(constraints → size)` cache
//! that a commit invalidates along the path to the root. Scope groups are transparent.
//!
//! Custom `LAYOUT` nodes go through a `MeasureHost` (the Python callback on the real core, a
//! Rust closure in tests). They are never cached: their measure policy lives outside the tree.

use std::sync::Arc;

use crate::text::TextKey;
use crate::tree::{Inner, NodeId};
use crate::types::*;

pub const BUTTON_PAD_X: f32 = 12.0;
pub const BUTTON_PAD_Y: f32 = 6.0;
pub const FIELD_PAD_X: f32 = 8.0;
pub const FIELD_PAD_Y: f32 = 6.0;
pub const FIELD_MIN_WIDTH: f32 = 120.0;
pub const CHECKBOX_SIZE: f32 = 18.0;
/// The scrollbar thumb and the space around it, reserved beside a scroll container's
/// content once it overflows, so the thumb never sits on top of what it scrolls.
pub const SCROLLBAR_WIDTH: f32 = 4.0;
pub const SCROLLBAR_INSET: f32 = 2.0;
pub const SCROLLBAR_GUTTER: f32 = SCROLLBAR_WIDTH + 2.0 * SCROLLBAR_INSET;

/// What a custom layout decided.
pub struct CustomResult {
    pub size: Size,
    /// Per child: was `measure_child` called for it?
    pub measured: Vec<bool>,
    /// Per child: its placement, if `place_child` was called.
    pub placed: Vec<Option<(f32, f32)>>,
}

impl CustomResult {
    pub fn zero(n: usize) -> CustomResult {
        CustomResult { size: Size::ZERO, measured: vec![false; n], placed: vec![None; n] }
    }
}

/// Supplies the measure policy of `LAYOUT` nodes. `measure_custom` may lay out the children
/// through `layout_node` (that is what `measure_child` does) and report what it placed.
pub trait MeasureHost {
    type Error;
    fn measure_custom(
        &mut self,
        inner: &mut Inner,
        node: NodeId,
        handler: i32,
        children: &[NodeId],
        constraints: Constraints,
    ) -> Result<CustomResult, Self::Error>;
    /// A callback failed: the pass continues with the node zero-sized; the first error is kept.
    fn record_error(&mut self, err: Self::Error);
}

/// A host for trees without `LAYOUT` nodes: any custom layout is an error.
#[derive(Default)]
pub struct NoHost {
    pub error: Option<String>,
}

impl MeasureHost for NoHost {
    type Error = String;
    fn measure_custom(&mut self, _: &mut Inner, _: NodeId, _: i32, _: &[NodeId], _: Constraints) -> Result<CustomResult, String> {
        Err("layout callback required: the tree contains a Layout node".to_string())
    }
    fn record_error(&mut self, err: String) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
}

/// Lay out the whole tree against the window size. Cheap when nothing is dirty.
pub fn layout_tree<H: MeasureHost>(inner: &mut Inner, host: &mut H) {
    let root = inner.root;
    let c = Constraints::tight(inner.width, inner.height);
    let window = Size::new(inner.width, inner.height);
    {
        let r = &inner.nodes[root];
        if !r.dirty && r.cache == Some((c, window)) {
            inner.text.end_layout();
            return;
        }
    }
    let children = inner.layout_children(root);
    let mut volatile = false;
    for &child in &children {
        layout_node(inner, child, c.loosened(), host);
        let n = &mut inner.nodes[child];
        n.offset = (0.0, 0.0);
        volatile |= n.volatile;
    }
    let r = &mut inner.nodes[root];
    r.size = window;
    r.cache = Some((c, window));
    r.volatile = volatile;
    r.dirty = volatile;
    r.offset = (0.0, 0.0);
    layout_popups(inner, host);
    assign_abs(inner);
    apply_reveals(inner);
    inner.text.end_layout();
}

/// Lay every popup out against the window and place it where it asked to be, moved back
/// inside the window when it would not fit. A popup is outside the flow: the container it
/// was written in neither measures nor places it.
fn layout_popups<H: MeasureHost>(inner: &mut Inner, host: &mut H) {
    let (ww, wh) = (inner.width, inner.height);
    for id in inner.live_popups() {
        let children = inner.layout_children(id);
        let loose = Constraints::loose(ww, wh);
        let mut w = 0.0f32;
        let mut h = 0.0f32;
        let mut sizes = Vec::with_capacity(children.len());
        for &ch in &children {
            let s = layout_node(inner, ch, loose, host);
            w = w.max(s.w);
            h = h.max(s.h);
            sizes.push(s);
        }
        for (i, &ch) in children.iter().enumerate() {
            let align = inner.nodes[ch].modifier.align.unwrap_or(0);
            inner.nodes[ch].offset = ((w - sizes[i].w) * align_factor(align), 0.0);
        }
        let node = &mut inner.nodes[id];
        let x = (node.a as f32).min(ww - w).max(0.0);
        let y = (node.c as f32).min(wh - h).max(0.0);
        node.popup_at = (x, y);
        node.size = Size::new(w, h);
        node.dirty = false;
    }
}

/// Scroll the nearest scroll container of every node that asked to be revealed this commit.
fn apply_reveals(inner: &mut Inner) {
    let pending = std::mem::take(&mut inner.reveal_pending);
    let mut changed = false;
    for id in pending {
        changed |= reveal(inner, id);
    }
    if changed {
        assign_abs(inner);
    }
}

/// Adjust the nearest enclosing SCROLL so `id` is inside its viewport; true when it moved.
pub fn reveal(inner: &mut Inner, id: NodeId) -> bool {
    let Some(node) = inner.nodes.get(id) else { return false };
    let target = node.abs;
    let mut cursor = node.parent;
    while let Some(p) = cursor {
        let Some(parent) = inner.nodes.get(p) else { return false };
        if parent.kind == Kind::Scroll {
            let viewport = parent.content_rect();
            let delta = if target.y < viewport.y {
                target.y - viewport.y
            } else if target.y + target.h > viewport.y + viewport.h {
                (target.y + target.h - viewport.y - viewport.h).min(target.y - viewport.y)
            } else {
                0.0
            };
            let limit = parent.scroll_limit();
            let new = (parent.scroll + delta).clamp(0.0, limit);
            if new != parent.scroll {
                inner.nodes[p].scroll = new;
                return true;
            }
            return false;
        }
        cursor = parent.parent;
    }
    false
}

/// Lay out one node (never a scope group) under `c`; returns its size. Served from the cache
/// when the node is clean and the constraints are unchanged.
pub fn layout_node<H: MeasureHost>(inner: &mut Inner, id: NodeId, c: Constraints, host: &mut H) -> Size {
    let c = sanitize(c);
    let node = &inner.nodes[id];
    if node.is_group() {
        panic!("internal: layout_node on a scope group");
    }
    if !node.dirty {
        if let Some((cc, s)) = node.cache {
            if cc == c {
                return s;
            }
        }
    }
    let modifier = node.modifier.clone();
    let old_size = node.size;
    let kind = node.kind;
    let n = &mut inner.nodes[id];
    n.layers.clear();
    n.volatile = false;
    let size = layout_layer(inner, id, &modifier, 0, c, (0.0, 0.0), Corners::NONE, host);
    let n = &mut inner.nodes[id];
    n.layers.reverse();
    n.size = size;
    n.cache = Some((c, size));
    n.dirty = n.volatile;
    if kind == Kind::Canvas && size != old_size && !n.canvas_pending {
        n.canvas_pending = true;
        inner.canvas_pending.push(id);
    }
    size
}

fn sanitize(c: Constraints) -> Constraints {
    let fix_min = |v: f32| if v.is_finite() && v > 0.0 { v } else { 0.0 };
    let fix_max = |v: f32| if v.is_nan() || v < 0.0 { 0.0 } else { v };
    let min_w = fix_min(c.min_w);
    let min_h = fix_min(c.min_h);
    let max_w = fix_max(c.max_w).max(min_w);
    let max_h = fix_max(c.max_h).max(min_h);
    Constraints { min_w, max_w, min_h, max_h }
}

/// Apply modifier op `i` and the rest of the chain, ending in the content. `corners` are the
/// radii in effect (set by `Rounded`, read by the layers after it).
#[allow(clippy::too_many_arguments)]
fn layout_layer<H: MeasureHost>(
    inner: &mut Inner,
    id: NodeId,
    m: &Arc<Modifier>,
    i: usize,
    c: Constraints,
    origin: (f32, f32),
    corners: Corners,
    host: &mut H,
) -> Size {
    let Some(op) = m.ops.get(i).copied() else {
        return layout_content(inner, id, c, origin, host);
    };
    match op {
        ModOp::Padding { l, t, r, b } => {
            let inner_c = c.deflate(l + r, t + b);
            let s = layout_layer(inner, id, m, i + 1, inner_c, (origin.0 + l, origin.1 + t), corners, host);
            c.constrain(Size::new(s.w + l + r, s.h + t + b))
        }
        ModOp::Width(v) => {
            let w = clamp(v, c.min_w, c.max_w);
            layout_layer(inner, id, m, i + 1, Constraints { min_w: w, max_w: w, ..c }, origin, corners, host)
        }
        ModOp::Height(v) => {
            let h = clamp(v, c.min_h, c.max_h);
            layout_layer(inner, id, m, i + 1, Constraints { min_h: h, max_h: h, ..c }, origin, corners, host)
        }
        ModOp::FillMaxWidth => {
            let cc = if c.max_w.is_finite() { Constraints { min_w: c.max_w, ..c } } else { c };
            layout_layer(inner, id, m, i + 1, cc, origin, corners, host)
        }
        ModOp::FillMaxHeight => {
            let cc = if c.max_h.is_finite() { Constraints { min_h: c.max_h, ..c } } else { c };
            layout_layer(inner, id, m, i + 1, cc, origin, corners, host)
        }
        ModOp::Background(argb) => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Background { rect: Rect::new(origin.0, origin.1, s.w, s.h), argb, corners });
            s
        }
        ModOp::Clickable(handler) => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Click { rect: Rect::new(origin.0, origin.1, s.w, s.h), handler, hover: m.hover, corners });
            s
        }
        ModOp::Rounded(r) => layout_layer(inner, id, m, i + 1, c, origin, r, host),
        ModOp::Border { width, argb } => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Border { rect: Rect::new(origin.0, origin.1, s.w, s.h), argb, width, corners });
            s
        }
        ModOp::Shadow { elevation, argb } => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Shadow { rect: Rect::new(origin.0, origin.1, s.w, s.h), corners, elevation, argb });
            s
        }
        ModOp::Secondary(handler) => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Secondary { rect: Rect::new(origin.0, origin.1, s.w, s.h), handler });
            s
        }
        ModOp::Hoverable(handler) => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Hoverable { rect: Rect::new(origin.0, origin.1, s.w, s.h), handler });
            s
        }
        ModOp::Drag(handler) => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Drag { rect: Rect::new(origin.0, origin.1, s.w, s.h), handler });
            s
        }
        ModOp::Selectable(argb) => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Select { rect: Rect::new(origin.0, origin.1, s.w, s.h), argb });
            s
        }
        ModOp::Dismiss(handler) => {
            let s = layout_layer(inner, id, m, i + 1, c, origin, corners, host);
            inner.nodes[id].layers.push(Layer::Dismiss { rect: Rect::new(origin.0, origin.1, s.w, s.h), handler });
            s
        }
        ModOp::Weight(_)
        | ModOp::Align(_)
        | ModOp::Hover(_)
        | ModOp::Scrollbar(_)
        | ModOp::Reveal
        | ModOp::Clip
        | ModOp::Cursor(_)
        | ModOp::Multiline => {
            layout_layer(inner, id, m, i + 1, c, origin, corners, host)
        }
    }
}

fn align_factor(a: u8) -> f32 {
    match a {
        1 => 0.5,
        2 => 1.0,
        _ => 0.0,
    }
}

fn layout_content<H: MeasureHost>(inner: &mut Inner, id: NodeId, c: Constraints, origin: (f32, f32), host: &mut H) -> Size {
    let kind = inner.nodes[id].kind;
    let size = match kind {
        Kind::Text => {
            let node = &inner.nodes[id];
            let key = TextKey::new(node.text.clone().unwrap_or_else(|| inner.empty_text.clone()), node.style, c.max_w);
            let s = inner.text.measure(&key);
            inner.nodes[id].text_key = Some(key);
            c.constrain(s)
        }
        Kind::Button => {
            let node = &inner.nodes[id];
            let label_max = (c.max_w - 2.0 * BUTTON_PAD_X).max(0.0);
            let key = TextKey::new(node.text.clone().unwrap_or_else(|| inner.empty_text.clone()), node.style, label_max);
            let s = inner.text.measure(&key);
            inner.nodes[id].text_key = Some(key);
            c.constrain(Size::new(s.w + 2.0 * BUTTON_PAD_X, s.h + 2.0 * BUTTON_PAD_Y))
        }
        Kind::TextField => {
            let node = &inner.nodes[id];
            let focused = inner.focus.as_ref().filter(|f| f.node == id);
            let shown: Arc<str> = match focused {
                Some(f) => Arc::from(f.buffer.as_str()),
                None => match &node.text {
                    Some(t) if !t.is_empty() => t.clone(),
                    _ => node.placeholder.clone().unwrap_or_else(|| inner.empty_text.clone()),
                },
            };
            // a field of one line is measured unwrapped and is as wide as it needs to be;
            // one that takes several wraps inside the width it was given and grows down
            let multiline = node.modifier.multiline;
            let wrap = if multiline && c.max_w.is_finite() {
                (c.max_w - 2.0 * FIELD_PAD_X).max(FIELD_MIN_WIDTH)
            } else {
                f32::INFINITY
            };
            let key = TextKey::new(shown, node.style, wrap);
            let s = inner.text.measure(&key);
            inner.nodes[id].text_key = Some(key);
            let line = crate::text::TextSystem::line_height(inner.nodes[id].style);
            c.constrain(Size::new(s.w.max(FIELD_MIN_WIDTH) + 2.0 * FIELD_PAD_X, line.max(s.h) + 2.0 * FIELD_PAD_Y))
        }
        Kind::Checkbox => c.constrain(Size::new(CHECKBOX_SIZE, CHECKBOX_SIZE)),
        Kind::Spacer | Kind::Canvas => Size::new(c.min_w, c.min_h),
        Kind::Column | Kind::Row => layout_flex(inner, id, c, origin, kind == Kind::Column, host),
        Kind::Box => layout_box(inner, id, c, origin, host),
        Kind::Layout => layout_custom(inner, id, c, origin, host),
        Kind::Scroll => layout_scroll(inner, id, c, origin, host),
        Kind::Popup => Size::ZERO,
        Kind::Scope => panic!("internal: layout_content on a scope group"),
    };
    let node = &mut inner.nodes[id];
    node.content_origin = origin;
    node.content_size = size;
    size
}

fn layout_flex<H: MeasureHost>(inner: &mut Inner, id: NodeId, c: Constraints, origin: (f32, f32), vertical: bool, host: &mut H) -> Size {
    let children = inner.layout_children(id);
    let arrangement = inner.nodes[id].a;
    // the container's cross-axis alignment; a child's own `align` modifier overrides it
    let cross_align = inner.nodes[id].c.clamp(0, 2) as u8;
    let n = children.len();
    let (main_min, main_max, cross_min, cross_max) =
        if vertical { (c.min_h, c.max_h, c.min_w, c.max_w) } else { (c.min_w, c.max_w, c.min_h, c.max_h) };
    let main_of = |s: Size| if vertical { s.h } else { s.w };
    let cross_of = |s: Size| if vertical { s.w } else { s.h };
    let make = |main_lo: f32, main_hi: f32| -> Constraints {
        if vertical {
            Constraints { min_w: 0.0, max_w: cross_max, min_h: main_lo, max_h: main_hi }
        } else {
            Constraints { min_w: main_lo, max_w: main_hi, min_h: 0.0, max_h: cross_max }
        }
    };

    let weights: Vec<f32> = children.iter().map(|&ch| inner.nodes[ch].modifier.weight).collect();
    let mut sizes = vec![Size::ZERO; n];
    let mut total_weight = 0.0f32;
    let mut fixed_main = 0.0f32;
    let mut cross = 0.0f32;
    let mut volatile = false;

    for i in 0..n {
        if weights[i] > 0.0 {
            total_weight += weights[i];
            continue;
        }
        let remaining = if main_max.is_finite() { (main_max - fixed_main).max(0.0) } else { f32::INFINITY };
        let s = layout_node(inner, children[i], make(0.0, remaining), host);
        sizes[i] = s;
        fixed_main += main_of(s);
        cross = cross.max(cross_of(s));
        volatile |= inner.nodes[children[i]].volatile;
    }
    if total_weight > 0.0 {
        let remaining = if main_max.is_finite() { (main_max - fixed_main).max(0.0) } else { f32::INFINITY };
        let weighted = weights.iter().filter(|&&w| w > 0.0).count();
        let mut seen = 0usize;
        let mut allotted = 0.0f32;
        for i in 0..n {
            if weights[i] <= 0.0 {
                continue;
            }
            seen += 1;
            let cc = if remaining.is_finite() {
                let share = if seen == weighted { (remaining - allotted).max(0.0) } else { remaining * weights[i] / total_weight };
                allotted += share;
                make(share, share)
            } else {
                make(0.0, f32::INFINITY)
            };
            let s = layout_node(inner, children[i], cc, host);
            sizes[i] = s;
            cross = cross.max(cross_of(s));
            volatile |= inner.nodes[children[i]].volatile;
        }
    }

    let main_total: f32 = sizes.iter().map(|&s| main_of(s)).sum();
    let main_size = if total_weight > 0.0 && main_max.is_finite() { main_max.max(main_min) } else { clamp(main_total, main_min, main_max) };
    let cross_size = clamp(cross, cross_min, cross_max);
    let free = (main_size - main_total).max(0.0);
    let (start, gap) = match arrangement {
        1 => (free / 2.0, 0.0),
        2 => (free, 0.0),
        3 => if n > 1 { (0.0, free / (n as f32 - 1.0)) } else { (0.0, 0.0) },
        4 => {
            let g = free / (n as f32 + 1.0);
            (g, g)
        }
        _ => (0.0, 0.0),
    };
    let mut pos = start;
    for i in 0..n {
        let s = sizes[i];
        let align = inner.nodes[children[i]].modifier.align.unwrap_or(cross_align);
        let cross_off = (cross_size - cross_of(s)) * align_factor(align);
        let off = if vertical { (origin.0 + cross_off, origin.1 + pos) } else { (origin.0 + pos, origin.1 + cross_off) };
        inner.nodes[children[i]].offset = off;
        pos += main_of(s) + gap;
    }
    inner.nodes[id].volatile = volatile;
    if vertical { Size::new(cross_size, main_size) } else { Size::new(main_size, cross_size) }
}

/// A vertical scroll container: children are stacked with an unbounded height, the node
/// itself takes what its constraints give it (so `fill_max_height` / `weight` make it a
/// viewport), and `assign_abs` shifts the children up by the scroll offset.
fn layout_scroll<H: MeasureHost>(inner: &mut Inner, id: NodeId, c: Constraints, origin: (f32, f32), host: &mut H) -> Size {
    let children = inner.layout_children(id);
    let cross_align = inner.nodes[id].c.clamp(0, 2) as u8;
    let (mut total, mut cross, mut sizes, mut volatile) = measure_column(inner, &children, c.max_w, host);
    let mut size = c.constrain(Size::new(cross, total));
    // the thumb needs room of its own: a container whose content overflows lays that content
    // out one gutter narrower, so the two never overlap. narrowing can only make the content
    // taller, so an overflowing container still overflows and the second pass is the last
    let gutter = if total > size.h && c.max_w.is_finite() && c.max_w > SCROLLBAR_GUTTER { SCROLLBAR_GUTTER } else { 0.0 };
    if gutter > 0.0 {
        let (t, cr, s, v) = measure_column(inner, &children, c.max_w - gutter, host);
        total = t;
        cross = cr;
        sizes = s;
        volatile = v;
        size = c.constrain(Size::new(cross + gutter, total));
    }
    let content_w = (size.w - gutter).max(0.0);
    let mut pos = 0.0f32;
    for (i, &ch) in children.iter().enumerate() {
        let align = inner.nodes[ch].modifier.align.unwrap_or(cross_align);
        let cross_off = (content_w - sizes[i].w).max(0.0) * align_factor(align);
        inner.nodes[ch].offset = (origin.0 + cross_off, origin.1 + pos);
        pos += sizes[i].h;
    }
    let node = &mut inner.nodes[id];
    node.content_len = total;
    node.scroll = node.scroll.clamp(0.0, (total - size.h).max(0.0));
    node.volatile = volatile;
    size
}

/// Measure a stack of children with an unbounded height: their total, the widest, their
/// sizes, and whether any of them is volatile.
fn measure_column<H: MeasureHost>(inner: &mut Inner, children: &[NodeId], max_w: f32, host: &mut H) -> (f32, f32, Vec<Size>, bool) {
    let child_c = Constraints { min_w: 0.0, max_w: max_w.max(0.0), min_h: 0.0, max_h: f32::INFINITY };
    let mut total = 0.0f32;
    let mut cross = 0.0f32;
    let mut sizes = Vec::with_capacity(children.len());
    let mut volatile = false;
    for &ch in children {
        let s = layout_node(inner, ch, child_c, host);
        total += s.h;
        cross = cross.max(s.w);
        sizes.push(s);
        volatile |= inner.nodes[ch].volatile;
    }
    (total, cross, sizes, volatile)
}

fn layout_box<H: MeasureHost>(inner: &mut Inner, id: NodeId, c: Constraints, origin: (f32, f32), host: &mut H) -> Size {
    let children = inner.layout_children(id);
    let box_align = inner.nodes[id].a.clamp(0, 2) as u8;
    let loose = c.loosened();
    let mut w = 0.0f32;
    let mut h = 0.0f32;
    let mut sizes = Vec::with_capacity(children.len());
    let mut volatile = false;
    for &ch in &children {
        let s = layout_node(inner, ch, loose, host);
        w = w.max(s.w);
        h = h.max(s.h);
        sizes.push(s);
        volatile |= inner.nodes[ch].volatile;
    }
    let size = c.constrain(Size::new(w, h));
    for (i, &ch) in children.iter().enumerate() {
        let a = inner.nodes[ch].modifier.align.unwrap_or(box_align);
        let f = align_factor(a);
        inner.nodes[ch].offset = (origin.0 + (size.w - sizes[i].w) * f, origin.1 + (size.h - sizes[i].h) * f);
    }
    inner.nodes[id].volatile = volatile;
    size
}

fn layout_custom<H: MeasureHost>(inner: &mut Inner, id: NodeId, c: Constraints, origin: (f32, f32), host: &mut H) -> Size {
    let children = inner.layout_children(id);
    let handler = inner.nodes[id].handler;
    let n = children.len();
    let res = match host.measure_custom(inner, id, handler, &children, c) {
        Ok(r) => r,
        Err(e) => {
            host.record_error(e);
            CustomResult::zero(n)
        }
    };
    let mut s = res.size;
    if !s.w.is_finite() || s.w < 0.0 {
        s.w = 0.0;
    }
    if !s.h.is_finite() || s.h < 0.0 {
        s.h = 0.0;
    }
    let size = c.constrain(s);
    for (i, &ch) in children.iter().enumerate() {
        if !res.measured.get(i).copied().unwrap_or(false) {
            layout_node(inner, ch, Constraints::tight(0.0, 0.0), host);
        }
        let (x, y) = res.placed.get(i).copied().flatten().unwrap_or((0.0, 0.0));
        let x = if x.is_finite() { x } else { 0.0 };
        let y = if y.is_finite() { y } else { 0.0 };
        inner.nodes[ch].offset = (origin.0 + x, origin.1 + y);
    }
    inner.nodes[id].volatile = true;
    size
}

/// Absolute rects for every node: offsets are relative to the nearest layout container, and
/// the children of a SCROLL node are shifted up by its scroll offset. Cheap enough to re-run
/// on every scroll (no measuring).
pub fn assign_abs(inner: &mut Inner) {
    let root = inner.root;
    let mut stack: Vec<(NodeId, (f32, f32), f32)> = vec![(root, (0.0, 0.0), 0.0)];
    while let Some((id, base, scroll)) = stack.pop() {
        let Some(node) = inner.nodes.get_mut(id) else { continue };
        let (child_base, child_scroll) = if node.kind == Kind::Popup {
            // a popup hangs off the window, not off the container it was written in
            node.abs = Rect::new(node.popup_at.0, node.popup_at.1, node.size.w, node.size.h);
            (node.popup_at, 0.0)
        } else if node.is_group() {
            node.abs = Rect::new(base.0, base.1, 0.0, 0.0);
            (base, scroll)
        } else {
            let x = base.0 + node.offset.0;
            let y = base.1 + node.offset.1 - scroll;
            node.abs = Rect::new(x, y, node.size.w, node.size.h);
            ((x, y), if node.kind == Kind::Scroll { node.scroll } else { 0.0 })
        };
        if id == root {
            node.abs = Rect::new(0.0, 0.0, node.size.w, node.size.h);
        }
        for &c in node.children.iter().rev() {
            stack.push((c, child_base, child_scroll));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::commit;
    use crate::text::TextSystem;

    fn core() -> Inner {
        Inner::new(800.0, 600.0, 1.0, TextSystem::monospace_only())
    }

    const END: [i32; 8] = [0, 0, -1, 0, -1, 0, 0, 0];
    fn column(m: i32, arrangement: i32) -> [i32; 8] {
        [3, 0, -1, m, -1, arrangement, 0, 0]
    }
    fn row(m: i32) -> [i32; 8] {
        [4, 0, -1, m, -1, 0, 0, 0]
    }
    fn boxk(m: i32, align: i32) -> [i32; 8] {
        [5, 0, -1, m, -1, align, 0, 0]
    }
    fn text(idx: i32, m: i32) -> [i32; 8] {
        [6, 0, idx, m, -1, 0, 0, 0]
    }
    fn spacer(m: i32) -> [i32; 8] {
        [10, 0, -1, m, -1, 0, 0, 0]
    }
    fn layout_rec(handler: i32) -> [i32; 8] {
        [12, 0, -1, 0, handler, 0, 0, 0]
    }
    fn recs(list: &[[i32; 8]]) -> Vec<i32> {
        list.iter().flatten().copied().collect()
    }

    fn run(inner: &mut Inner, ints: &[i32], strs: &[&str], mods: &[(i64, Vec<f64>)]) {
        let n = (ints.len() / 8) as i64;
        commit(inner, ints, strs, &[(0, 0, n)], mods, &[]).unwrap();
        let mut host = NoHost::default();
        layout_tree(inner, &mut host);
        assert!(host.error.is_none(), "{:?}", host.error);
    }

    fn rect_of(inner: &Inner, path: &[usize]) -> Rect {
        let mut id = inner.root;
        for &i in path {
            id = inner.layout_children(id)[i];
        }
        inner.nodes[id].abs
    }

    #[test]
    fn column_with_padding_and_weights() {
        let mut inner = core();
        // mod 1: padding 10; mod 2: weight 1; mod 3: weight 3; mod 4: fill max size
        let mods = vec![
            (1, vec![1.0, 10.0, 10.0, 10.0, 10.0]),
            (2, vec![7.0, 1.0]),
            (3, vec![7.0, 3.0]),
            (4, vec![4.0, 5.0]),
        ];
        let ints = recs(&[column(4, 0), text(0, 1), spacer(2), spacer(3), END]);
        run(&mut inner, &ints, &["hello"], &mods);
        let col = rect_of(&inner, &[0]);
        assert_eq!(col, Rect::new(0.0, 0.0, 800.0, 600.0));
        let t = rect_of(&inner, &[0, 0]);
        assert_eq!(t, Rect::new(0.0, 0.0, 42.0 + 20.0, 18.0 + 20.0)); // 5 chars * 8.4 = 42
        let s1 = rect_of(&inner, &[0, 1]);
        let s2 = rect_of(&inner, &[0, 2]);
        let remaining = 600.0 - 38.0;
        assert_eq!(s1.y, 38.0);
        assert!((s1.h - remaining / 4.0).abs() < 0.01, "{:?}", s1);
        assert!((s2.h - remaining * 3.0 / 4.0).abs() < 0.01, "{:?}", s2);
        assert!((s2.y + s2.h - 600.0).abs() < 0.01);
        // width of a weighted spacer without fill is 0 (loose cross axis)
        assert_eq!(s1.w, 0.0);
    }

    #[test]
    fn row_arrangements_and_align() {
        let mut inner = core();
        let mods = vec![
            (1, vec![2.0, 100.0, 3.0, 20.0]),          // 100x20
            (2, vec![2.0, 100.0, 3.0, 40.0, 8.0, 2.0]), // 100x40 align end
            (3, vec![4.0]),                             // fill max width
        ];
        // space-between
        let ints = recs(&[[4, 0, -1, 3, -1, 3, 0, 0], spacer(1), spacer(2), END]);
        run(&mut inner, &ints, &[], &mods);
        let r = rect_of(&inner, &[0]);
        assert_eq!(r, Rect::new(0.0, 0.0, 800.0, 40.0));
        assert_eq!(rect_of(&inner, &[0, 0]), Rect::new(0.0, 0.0, 100.0, 20.0));
        assert_eq!(rect_of(&inner, &[0, 1]), Rect::new(700.0, 0.0, 100.0, 40.0));
        // center
        let ints = recs(&[[4, 0, -1, 3, -1, 1, 0, 0], spacer(1), spacer(2), END]);
        run(&mut inner, &ints, &[], &[]);
        assert_eq!(rect_of(&inner, &[0, 0]).x, 300.0);
        assert_eq!(rect_of(&inner, &[0, 1]).x, 400.0);
        // space-evenly
        let ints = recs(&[[4, 0, -1, 3, -1, 4, 0, 0], spacer(1), spacer(2), END]);
        run(&mut inner, &ints, &[], &[]);
        assert_eq!(rect_of(&inner, &[0, 0]).x, 200.0);
        assert_eq!(rect_of(&inner, &[0, 1]).x, 500.0);
        // a row without fill wraps its content
        let ints = recs(&[row(0), spacer(1), spacer(2), END]);
        run(&mut inner, &ints, &[], &[]);
        assert_eq!(rect_of(&inner, &[0]), Rect::new(0.0, 0.0, 200.0, 40.0));
    }

    #[test]
    fn box_alignment_and_modifier_order() {
        let mut inner = core();
        let mods = vec![
            (1, vec![4.0, 5.0]),                                    // fill max size
            (2, vec![2.0, 50.0, 3.0, 50.0]),                        // 50x50
            (3, vec![2.0, 50.0, 3.0, 50.0, 8.0, 0.0]),              // 50x50 align start
            (4, vec![1.0, 16.0, 16.0, 16.0, 16.0, 2.0, 100.0]),     // padding then width: 132 wide
            (5, vec![2.0, 100.0, 1.0, 16.0, 16.0, 16.0, 16.0]),     // width then padding: 100 wide
            (6, vec![6.0, 4294901760.0, 1.0, 5.0, 5.0, 5.0, 5.0, 6.0, 4278190335.0]), // bg red, pad 5, bg blue
        ];
        let ints = recs(&[boxk(1, 1), spacer(2), spacer(3), END]);
        run(&mut inner, &ints, &[], &mods);
        assert_eq!(rect_of(&inner, &[0, 0]), Rect::new(375.0, 275.0, 50.0, 50.0));
        assert_eq!(rect_of(&inner, &[0, 1]), Rect::new(0.0, 0.0, 50.0, 50.0));
        let ints = recs(&[boxk(1, 2), spacer(2), END]);
        run(&mut inner, &ints, &[], &[]);
        assert_eq!(rect_of(&inner, &[0, 0]), Rect::new(750.0, 550.0, 50.0, 50.0));
        // modifier order
        let ints = recs(&[column(0, 0), spacer(4), spacer(5), spacer(6), END]);
        run(&mut inner, &ints, &[], &[]);
        assert_eq!(rect_of(&inner, &[0, 0]).w, 132.0);
        assert_eq!(rect_of(&inner, &[0, 1]).w, 100.0);
        let id = inner.layout_children(inner.layout_children(inner.root)[0])[2];
        let layers = &inner.nodes[id].layers;
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0], Layer::Background { rect: Rect::new(0.0, 0.0, 10.0, 10.0), argb: 0xFFFF0000, corners: Corners::NONE });
        assert_eq!(layers[1], Layer::Background { rect: Rect::new(5.0, 5.0, 0.0, 0.0), argb: 0xFF0000FF, corners: Corners::NONE });
    }

    #[test]
    fn text_wraps_and_sizes_with_monospace_fallback() {
        let mut inner = core();
        let mods = vec![(1, vec![2.0, 50.0])];
        let ints = recs(&[column(0, 0), text(0, 0), text(0, 1), END]);
        run(&mut inner, &ints, &["0123456789"], &mods);
        assert_eq!(rect_of(&inner, &[0, 0]), Rect::new(0.0, 0.0, 84.0, 18.0));
        assert_eq!(rect_of(&inner, &[0, 1]), Rect::new(0.0, 18.0, 50.0, 36.0));
    }

    #[test]
    fn size_cache_skips_clean_subtrees_and_commit_invalidates() {
        let mut inner = core();
        let ints = recs(&[column(0, 0), text(0, 0), text(1, 0), END]);
        run(&mut inner, &ints, &["a", "b"], &[]);
        assert!(!inner.nodes[inner.root].dirty);
        let before = inner.text.cache_len();
        // same content again: nothing is dirty, no new measurement
        run(&mut inner, &ints, &["a", "b"], &[]);
        assert_eq!(inner.text.cache_len(), before);
        // change the second text: only that node and its ancestors are dirty
        let ints = recs(&[column(0, 0), text(0, 0), text(2, 0), END]);
        commit(&mut inner, &ints, &["a", "b", "ccc"], &[(0, 0, 4)], &[], &[]).unwrap();
        let col = inner.layout_children(inner.root)[0];
        let kids = inner.layout_children(col);
        assert!(inner.nodes[inner.root].dirty && inner.nodes[col].dirty && inner.nodes[kids[1]].dirty);
        assert!(!inner.nodes[kids[0]].dirty);
        layout_tree(&mut inner, &mut NoHost::default());
        assert_eq!(rect_of(&inner, &[0, 1]).w, 26.0);
    }

    #[test]
    fn invalidation_passes_through_moved_scope_groups() {
        // regression: a group moved between containers used to keep a stale dirty flag that
        // stopped later invalidations from reaching the container above it
        let mut inner = core();
        let scope = |id: i32| [1, 0, -1, 0, -1, id, 0, 0];
        let ints = recs(&[column(0, 0), scope(1), text(0, 0), END, END]);
        run(&mut inner, &ints, &["a"], &[]);
        assert_eq!(rect_of(&inner, &[0, 0]).w, 9.0);
        // the column becomes a box: the group moves into the new container
        let ints = recs(&[boxk(0, 0), [2, 0, -1, 0, -1, 1, 0, 0], END]);
        run(&mut inner, &ints, &["a"], &[]);
        assert_eq!(rect_of(&inner, &[0, 0]).w, 9.0);
        // now only scope 1 re-runs with a longer text: the box and root must relayout
        let ints = recs(&[text(0, 0)]);
        commit(&mut inner, &ints, &["abcd"], &[(1, 0, 1)], &[], &[]).unwrap();
        assert!(inner.nodes[inner.root].dirty);
        layout_tree(&mut inner, &mut NoHost::default());
        assert_eq!(rect_of(&inner, &[0, 0]).w, 34.0);
        assert_eq!(rect_of(&inner, &[0]).w, 34.0);
    }

    fn scroll(m: i32) -> [i32; 8] {
        [13, 0, -1, m, -1, 0, 0, 0]
    }

    #[test]
    fn scroll_container_offsets_children_and_clamps() {
        let mut inner = core();
        // mod 1: 50x50 spacer; mod 2: width 200 height 120 (the viewport); mod 3: 50x50 + reveal
        let mods = vec![(1, vec![2.0, 50.0, 3.0, 50.0]), (2, vec![2.0, 200.0, 3.0, 120.0]), (3, vec![2.0, 50.0, 3.0, 50.0, 14.0])];
        let ints = recs(&[column(0, 0), scroll(2), spacer(1), spacer(1), spacer(1), spacer(1), END, text(0, 0), END]);
        run(&mut inner, &ints, &["x"], &mods);
        let sc = inner.layout_children(inner.layout_children(inner.root)[0])[0];
        assert_eq!(inner.nodes[sc].size, Size::new(200.0, 120.0));
        assert_eq!(inner.nodes[sc].content_len, 200.0);
        assert_eq!(inner.nodes[sc].scroll_limit(), 80.0);
        assert_eq!(rect_of(&inner, &[0, 0, 2]), Rect::new(0.0, 100.0, 50.0, 50.0));
        // the text after the scroll sits right below the viewport, not below the content
        assert_eq!(rect_of(&inner, &[0, 1]).y, 120.0);
        // scrolling moves the children up, and is clamped to the limit
        inner.nodes[sc].scroll = 500.0;
        layout_tree(&mut inner, &mut NoHost::default());
        assert_eq!(inner.nodes[sc].scroll, 500.0, "a clean tree is not re-laid out");
        inner.nodes[sc].scroll = 30.0;
        assign_abs(&mut inner);
        assert_eq!(rect_of(&inner, &[0, 0, 2]).y, 70.0);
        assert_eq!(rect_of(&inner, &[0, 0]).y, 0.0, "the viewport itself does not move");
        // a child gaining `reveal` scrolls the container so it is visible
        let ints = recs(&[column(0, 0), scroll(2), spacer(1), spacer(1), spacer(1), spacer(3), END, text(0, 0), END]);
        commit(&mut inner, &ints, &["x"], &[(0, 0, 9)], &[], &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        assert_eq!(inner.nodes[sc].scroll, 80.0);
        assert_eq!(rect_of(&inner, &[0, 0, 3]), Rect::new(0.0, 70.0, 50.0, 50.0));
        // revealing the first child scrolls back up
        let ints = recs(&[column(0, 0), scroll(2), spacer(3), spacer(1), spacer(1), spacer(1), END, text(0, 0), END]);
        commit(&mut inner, &ints, &["x"], &[(0, 0, 9)], &[], &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        assert_eq!(inner.nodes[sc].scroll, 0.0);
    }

    #[test]
    fn decorated_layers_record_corners_border_shadow_and_hover() {
        let mut inner = core();
        // shadow, rounded (8 top only), background, border, hover + clickable, padding 4, size
        let mods = vec![(1, vec![13.0, 3.0, 2164260864.0, 10.0, 8.0, 8.0, 0.0, 0.0, 6.0, 4294967295.0, 11.0, 1.0, 4278190080.0, 12.0, 4278255360.0, 9.0, 3.0, 1.0, 4.0, 4.0, 4.0, 4.0, 2.0, 40.0, 3.0, 20.0])];
        let ints = recs(&[column(0, 0), spacer(1), END]);
        run(&mut inner, &ints, &[], &mods);
        let id = inner.layout_children(inner.layout_children(inner.root)[0])[0];
        let layers = &inner.nodes[id].layers;
        assert_eq!(layers.len(), 4);
        let rect = Rect::new(0.0, 0.0, 48.0, 28.0);
        let top = Corners { tl: 8.0, tr: 8.0, br: 0.0, bl: 0.0 };
        assert_eq!(layers[0], Layer::Shadow { rect, corners: Corners::NONE, elevation: 3.0, argb: 0x81000000 });
        assert_eq!(layers[1], Layer::Background { rect, argb: 0xFFFFFFFF, corners: top });
        assert_eq!(layers[2], Layer::Border { rect, argb: 0xFF000000, width: 1.0, corners: top });
        assert_eq!(layers[3], Layer::Click { rect, handler: 3, hover: Some(0xFF00FF00), corners: top });
    }

    #[test]
    fn a_scrolling_container_reserves_the_scrollbar_gutter() {
        let mut inner = core();
        // mod 1: the 200x100 viewport; mod 2: a row that fills the width, 60 tall
        let mods = vec![(1, vec![2.0, 200.0, 3.0, 100.0]), (2, vec![4.0, 3.0, 60.0])];
        let short = recs(&[scroll(1), spacer(2), END]);
        run(&mut inner, &short, &[], &mods);
        assert_eq!(rect_of(&inner, &[0, 0]).w, 200.0, "nothing to scroll: the child keeps the full width");
        let tall = recs(&[scroll(1), spacer(2), spacer(2), END]);
        run(&mut inner, &tall, &[], &[]);
        let sc = inner.layout_children(inner.root)[0];
        assert_eq!(inner.nodes[sc].size, Size::new(200.0, 100.0));
        assert_eq!(inner.nodes[sc].content_len, 120.0);
        assert_eq!(rect_of(&inner, &[0, 0]).w, 200.0 - SCROLLBAR_GUTTER, "the content makes room for the thumb");
        assert_eq!(rect_of(&inner, &[0, 1]).y, 60.0);
    }

    #[test]
    fn a_row_aligns_its_children_on_the_cross_axis() {
        let mut inner = core();
        // mod 1: 50x50; mod 2: 50x10; mod 3: 50x10 aligned to the end itself
        let mods = vec![
            (1, vec![2.0, 50.0, 3.0, 50.0]),
            (2, vec![2.0, 50.0, 3.0, 10.0]),
            (3, vec![2.0, 50.0, 3.0, 10.0, 8.0, 2.0]),
        ];
        // a row with `c` = 1 centres its children
        let ints = recs(&[[4, 0, -1, 0, -1, 0, 0, 1], spacer(1), spacer(2), spacer(3), END]);
        run(&mut inner, &ints, &[], &mods);
        assert_eq!(rect_of(&inner, &[0, 1]).y, 20.0, "centred");
        assert_eq!(rect_of(&inner, &[0, 2]).y, 40.0, "the child's own align wins");
        // the default is still the start
        let ints = recs(&[row(0), spacer(1), spacer(2), END]);
        run(&mut inner, &ints, &[], &[]);
        assert_eq!(rect_of(&inner, &[0, 1]).y, 0.0);
        // and a column centres horizontally
        let ints = recs(&[[3, 0, -1, 0, -1, 0, 0, 1], spacer(1), spacer(2), END]);
        run(&mut inner, &ints, &[], &[]);
        assert_eq!(rect_of(&inner, &[0, 0]).x, 0.0);
        assert_eq!(rect_of(&inner, &[0, 1]).x, 0.0, "both are 50 wide, so centring moves nothing");
    }

    /// A custom layout that stacks children diagonally and claims 300x300.
    struct Diagonal {
        calls: usize,
    }

    impl MeasureHost for Diagonal {
        type Error = String;
        fn measure_custom(&mut self, inner: &mut Inner, _: NodeId, handler: i32, children: &[NodeId], c: Constraints) -> Result<CustomResult, String> {
            self.calls += 1;
            if handler == 99 {
                return Err("boom".to_string());
            }
            let mut res = CustomResult::zero(children.len());
            for (i, &ch) in children.iter().enumerate() {
                let _ = layout_node(inner, ch, c.loosened(), self);
                res.measured[i] = true;
                res.placed[i] = Some((10.0 * i as f32, 10.0 * i as f32));
            }
            res.size = Size::new(300.0, 300.0);
            Ok(res)
        }
        fn record_error(&mut self, _: String) {}
    }

    #[test]
    fn custom_layout_places_children_and_is_never_cached() {
        let mut inner = core();
        let mods = vec![(1, vec![2.0, 50.0, 3.0, 50.0])];
        let ints = recs(&[column(0, 0), layout_rec(7), spacer(1), spacer(1), END, text(0, 0), END]);
        commit(&mut inner, &ints, &["x"], &[(0, 0, 7)], &mods, &[]).unwrap();
        let mut host = Diagonal { calls: 0 };
        layout_tree(&mut inner, &mut host);
        assert_eq!(host.calls, 1);
        assert_eq!(rect_of(&inner, &[0, 0]), Rect::new(0.0, 0.0, 300.0, 300.0));
        assert_eq!(rect_of(&inner, &[0, 0, 1]), Rect::new(10.0, 10.0, 50.0, 50.0));
        assert_eq!(rect_of(&inner, &[0, 1]).y, 300.0);
        // a second layout without a commit re-invokes the custom policy but nothing else
        layout_tree(&mut inner, &mut host);
        assert_eq!(host.calls, 2);
        // a failing callback leaves the node zero-sized and the pass completes
        let ints = recs(&[column(0, 0), layout_rec(99), spacer(1), END, text(0, 0), END]);
        commit(&mut inner, &ints, &["x"], &[(0, 0, 6)], &[], &[]).unwrap();
        layout_tree(&mut inner, &mut host);
        assert_eq!(rect_of(&inner, &[0, 0]).h, 0.0);
        assert_eq!(rect_of(&inner, &[0, 1]).y, 0.0);
        // and NoHost reports the missing callback
        let mut no = NoHost::default();
        layout_tree(&mut inner, &mut no);
        assert!(no.error.is_some());
    }
}
