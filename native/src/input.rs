//! Hit testing, focus, the text field edit buffer, and the event tuples of the protocol.

use crate::tree::{Caret, Focus, Inner, NodeId, Selection};
use crate::types::*;

pub const EV_POINTER_DOWN: i32 = 1;
pub const EV_POINTER_UP: i32 = 2;
pub const EV_POINTER_MOVE: i32 = 3;
pub const EV_KEY_TEXT: i32 = 4;
pub const EV_RESIZE: i32 = 5;
pub const EV_CLOSE: i32 = 6;
/// A key chord for the application (`text` is e.g. "cmd+x", "shift+up", "pagedown").
pub const EV_KEY_CHORD: i32 = 7;
/// The system appearance the window follows (`text` is "light" or "dark"); sent when the
/// window opens and whenever it changes.
pub const EV_THEME: i32 = 8;
/// The secondary (right) button, down and up, over a node carrying an `on_secondary`
/// modifier; `handler` is that handler.
pub const EV_SECONDARY_DOWN: i32 = 9;
pub const EV_SECONDARY_UP: i32 = 10;
/// The pointer entered (`text` = "1") or left (`text` = "") a node carrying a `hoverable`
/// modifier; `handler` is that handler, `x` / `y` where the pointer is.
pub const EV_HOVER: i32 = 11;
/// A drag on a node carrying a `draggable` modifier: `text` is "start", "move" or "end",
/// `x` / `y` where the pointer is, `handler` that node's drag handler.
pub const EV_DRAG: i32 = 12;
/// A press outside a `dismiss` layer: the layer's handler, with nothing to say.
pub const EV_DISMISS: i32 = 13;
/// A drag passing over, or let go on, a `drop_target`: "over", "leave" or "drop".
pub const EV_DROP: i32 = 14;
/// A scroll container moved: `x` is its offset and `y` the height of its viewport, so a
/// list can compose only the rows that are in it.
pub const EV_SCROLLED: i32 = 15;
/// How far the pointer must travel with the button down before a press becomes a drag. Under
/// it the press is a click, so a row can be both dragged and clicked.
pub const DRAG_THRESHOLD: f32 = 4.0;
/// A press landed inside a `focus_region`: the keyboard belongs to that part now.
pub const EV_FOCUS: i32 = 16;

/// Lines of a mouse-wheel tick, in logical pixels.
pub const WHEEL_LINE: f32 = 40.0;

/// `(kind, x, y, handler_idx, text)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub kind: i32,
    pub x: f32,
    pub y: f32,
    pub handler: i32,
    pub text: String,
}

impl Event {
    pub fn new(kind: i32, x: f32, y: f32, handler: i32, text: String) -> Event {
        Event { kind, x, y, handler, text }
    }
    pub fn tuple(&self) -> (i32, f64, f64, i32, String) {
        (self.kind, self.x as f64, self.y as f64, self.handler, self.text.clone())
    }
}

/// Which handler a hit test is looking for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Which {
    /// Buttons, checkboxes, text fields and `clickable` layers.
    Primary,
    /// `on_secondary` layers only — the right button falls through everything else.
    Secondary,
    /// `hoverable` layers only.
    Hover,
    /// `draggable` layers only.
    Drag,
    /// `selectable` layers only.
    Select,
    /// `drop_target` layers only — where something being dragged may be let go.
    Drop,
    /// `focus_region` layers only — the part of the window a press gives the keyboard to.
    Focus,
}

/// The result of a hit test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// Handler index of the interactive node or clickable layer hit, or -1.
    pub handler: i32,
    /// The text field hit (it consumes the pointer and takes focus).
    pub focus: Option<NodeId>,
    pub node: Option<NodeId>,
}

impl Hit {
    pub const NONE: Hit = Hit { handler: -1, focus: None, node: None };
}

/// Topmost interactive thing under `(x, y)`: later siblings are on top, children before their
/// parent; enabled buttons, checkboxes and clickable layers report their handler, disabled
/// buttons and text fields consume the hit without one.
pub fn hit_test(inner: &Inner, x: f32, y: f32) -> Hit {
    hit_for(inner, x, y, Which::Primary)
}

/// The topmost thing under `(x, y)` that answers to `which`. Popups are tested before the
/// rest of the tree, because that is the order they are painted in.
pub fn hit_for(inner: &Inner, x: f32, y: f32, which: Which) -> Hit {
    for popup in inner.live_popups().into_iter().rev() {
        let Some(node) = inner.nodes.get(popup) else { continue };
        if !node.abs.contains(x, y) {
            continue;
        }
        if let Some(h) = hit_children(inner, popup, x, y, which) {
            return h;
        }
        // nothing in the popup answers here, so the pointer carries on down: a tooltip is
        // not something to click, and a menu keeps its own sheet behind it to catch the
        // clicks that dismiss it
    }
    hit_children(inner, inner.root, x, y, which).unwrap_or(Hit::NONE)
}

fn hit_children(inner: &Inner, id: NodeId, x: f32, y: f32, which: Which) -> Option<Hit> {
    let node = inner.nodes.get(id)?;
    for &c in node.children.iter().rev() {
        let Some(child) = inner.nodes.get(c) else { continue };
        if child.kind == Kind::Popup {
            continue; // reached through `hit_for`, ahead of everything else
        }
        if child.is_group() {
            if let Some(h) = hit_children(inner, c, x, y, which) {
                return Some(h);
            }
            continue;
        }
        if !child.abs.contains(x, y) {
            continue;
        }
        // a container that clips what it draws clips what it can be pressed on too:
        // painting already stopped at the viewport, and a thing you cannot see is not a
        // thing you can click
        let clips = child.kind == Kind::Scroll || child.modifier.clip;
        if clips {
            let content = Rect::new(
                child.abs.x + child.content_origin.0,
                child.abs.y + child.content_origin.1,
                child.content_size.w,
                child.content_size.h,
            );
            if !content.contains(x, y) {
                continue;
            }
        }
        if let Some(h) = hit_children(inner, c, x, y, which) {
            return Some(h);
        }
        let content = Rect::new(
            child.abs.x + child.content_origin.0,
            child.abs.y + child.content_origin.1,
            child.content_size.w,
            child.content_size.h,
        );
        if which == Which::Primary {
            match child.kind {
                Kind::Button if content.contains(x, y) => {
                    return Some(Hit { handler: if child.a != 0 { child.handler } else { -1 }, focus: None, node: Some(c) });
                }
                Kind::Checkbox if content.contains(x, y) => {
                    return Some(Hit { handler: child.handler, focus: None, node: Some(c) });
                }
                Kind::TextField if content.contains(x, y) => {
                    return Some(Hit { handler: -1, focus: Some(c), node: Some(c) });
                }
                _ => {}
            }
        }
        let lx = x - child.abs.x;
        let ly = y - child.abs.y;
        for layer in child.layers.iter().rev() {
            let found = match (which, layer) {
                (Which::Primary, Layer::Click { rect, handler, .. }) => Some((rect, *handler)),
                (Which::Secondary, Layer::Secondary { rect, handler }) => Some((rect, *handler)),
                (Which::Hover, Layer::Hoverable { rect, handler }) => Some((rect, *handler)),
                (Which::Drag, Layer::Drag { rect, handler }) => Some((rect, *handler)),
                (Which::Select, Layer::Select { rect, .. }) => Some((rect, 0)),
                (Which::Drop, Layer::Drop { rect, handler }) => Some((rect, *handler)),
                (Which::Focus, Layer::Focus { rect, handler }) => Some((rect, *handler)),
                _ => None,
            };
            if let Some((rect, handler)) = found {
                if rect.contains(lx, ly) {
                    return Some(Hit { handler, focus: None, node: Some(c) });
                }
            }
        }
    }
    None
}

/// What the pointer should look like at `(x, y)`: the innermost node under it that asked for
/// a cursor, and 0 (the platform's own) when nothing did. Popups are consulted first, the way
/// they are hit first.
pub fn cursor_at(inner: &Inner, x: f32, y: f32) -> u32 {
    for popup in inner.live_popups().into_iter().rev() {
        let Some(node) = inner.nodes.get(popup) else { continue };
        if !node.abs.contains(x, y) {
            continue;
        }
        let found = cursor_in(inner, popup, x, y);
        if found != 0 {
            return found;
        }
    }
    cursor_in(inner, inner.root, x, y)
}

fn cursor_in(inner: &Inner, id: NodeId, x: f32, y: f32) -> u32 {
    let Some(node) = inner.nodes.get(id) else { return 0 };
    for &c in node.children.iter().rev() {
        let Some(child) = inner.nodes.get(c) else { continue };
        if child.kind == Kind::Popup {
            continue;
        }
        let inside = child.is_group() || child.abs.contains(x, y);
        if !inside {
            continue;
        }
        let found = cursor_in(inner, c, x, y);
        if found != 0 {
            return found;
        }
        if !child.is_group() && child.modifier.cursor != 0 {
            return child.modifier.cursor;
        }
    }
    0
}

/// A pointer event (kinds 1, 2, 3, or 9 / 10 for the right button). Pointer down moves focus
/// to the text field hit, or clears it; every kind records what is under the pointer, for the
/// hover paint and for the enter / leave events `take_hover_events` hands back.
pub fn pointer(inner: &mut Inner, kind: i32, x: f32, y: f32) -> Event {
    let secondary = kind == EV_SECONDARY_DOWN || kind == EV_SECONDARY_UP;
    // a drag owns the pointer while it lasts, so the press that started it, the moves and
    // the release are the drag's rather than anything else's
    let dragging = !secondary && update_drag(inner, kind, x, y);
    let selecting = !secondary && !dragging && update_selection(inner, kind, x, y);
    if kind == EV_POINTER_DOWN || kind == EV_SECONDARY_DOWN {
        dismiss_outside(inner, x, y);
    }
    let hit = hit_for(inner, x, y, if secondary { Which::Secondary } else { Which::Primary });
    if kind == EV_POINTER_DOWN {
        // whichever part of the window the press landed in now owns the keyboard
        let region = hit_for(inner, x, y, Which::Focus).handler;
        if region >= 0 && region != inner.focus_region {
            inner.focus_region = region;
            inner.pending_events.push(Event::new(EV_FOCUS, x, y, region, String::new()));
        }
    }
    if kind == EV_POINTER_DOWN && !dragging && !selecting {
        match hit.focus {
            Some(field) => focus_field_at(inner, field, x, y),
            None => inner.focus = None,
        }
    }
    if !secondary {
        inner.hover_node = hit.node;
        // what the pointer is holding down, so the thing pressed can say so. a press that
        // wanders off the node it started on stops showing, the way a button does
        match kind {
            EV_POINTER_DOWN => inner.pressed_node = hit.node,
            EV_POINTER_UP => inner.pressed_node = None,
            EV_POINTER_MOVE if inner.pressed_node.is_some() && inner.pressed_node != hit.node => inner.pressed_node = None,
            _ => {}
        }
    }
    update_hover_target(inner, x, y);
    Event::new(kind, x, y, if dragging || selecting { -1 } else { hit.handler }, String::new())
}

/// Whether a point is over a live popup. What a popup does not answer falls through to the
/// tree below it, which is right for a click — a tooltip must not eat one — but wrong for a
/// sweep or a drag: pressing a menu item that happens to sit over selectable text is not the
/// start of a selection.
fn over_popup(inner: &Inner, x: f32, y: f32) -> bool {
    inner
        .live_popups()
        .into_iter()
        .any(|p| inner.nodes.get(p).map(|n| n.abs.contains(x, y)).unwrap_or(false))
}

/// Tell every `dismiss` layer that does not contain the point about the press. The press is
/// not taken: it goes on to whatever it landed on, so a click that closes a menu still does
/// what it would have done.
fn dismiss_outside(inner: &mut Inner, x: f32, y: f32) {
    let mut handlers = Vec::new();
    for id in inner.all_nodes() {
        let Some(node) = inner.nodes.get(id) else { continue };
        for layer in node.layers.iter() {
            if let Layer::Dismiss { rect, handler } = layer {
                if *handler >= 0 && !rect.translated(node.abs.x, node.abs.y).contains(x, y) {
                    handlers.push(*handler);
                }
            }
        }
    }
    for handler in handlers {
        inner.pending_events.push(Event::new(EV_DISMISS, x, y, handler, String::new()));
    }
}

/// The hoverable under the pointer inside a live popup, ignoring the tree below it.
fn hover_in_popups(inner: &Inner, x: f32, y: f32) -> Option<NodeId> {
    for popup in inner.live_popups().into_iter().rev() {
        let Some(node) = inner.nodes.get(popup) else { continue };
        if !node.abs.contains(x, y) {
            continue;
        }
        if let Some(hit) = hit_children(inner, popup, x, y, Which::Hover) {
            return hit.node;
        }
    }
    None
}

/// Start, extend or finish a text selection. A press inside a `selectable` node puts the
/// caret where it landed and takes the pointer, so dragging sweeps text the way it does
/// anywhere else; a press outside one clears whatever was selected.
fn update_selection(inner: &mut Inner, kind: i32, x: f32, y: f32) -> bool {
    match kind {
        EV_POINTER_DOWN => {
            if over_popup(inner, x, y) {
                inner.selection = None;
                inner.selecting = false;
                return false;
            }
            // a press on something that can be clicked is that click, not the start of a
            // sweep: a control inside selectable text has to be reachable, and text inside a
            // control is not what anybody is trying to copy
            if hit_for(inner, x, y, Which::Primary).handler >= 0 {
                inner.selection = None;
                inner.selecting = false;
                return false;
            }
            let hit = hit_for(inner, x, y, Which::Select);
            let Some(node) = hit.node else {
                inner.selection = None;
                inner.selecting = false;
                return false;
            };
            let argb = inner
                .nodes
                .get(node)
                .and_then(|n| n.layers.iter().rev().find_map(|l| match l {
                    Layer::Select { argb, .. } => Some(*argb),
                    _ => None,
                }))
                .unwrap_or(0x4000_0000);
            match caret_at(inner, node, x, y) {
                Some(caret) => {
                    inner.selection = Some(Selection { anchor: caret, focus: caret, root: node, argb });
                    inner.selecting = true;
                    true
                }
                None => {
                    inner.selection = None;
                    inner.selecting = false;
                    false
                }
            }
        }
        EV_POINTER_MOVE if inner.selecting => {
            let Some(mut selection) = inner.selection else { return false };
            if let Some(caret) = caret_at(inner, selection.root, x, y) {
                selection.focus = caret;
                inner.selection = Some(selection);
            }
            true
        }
        EV_POINTER_UP if inner.selecting => {
            inner.selecting = false;
            // a press that selected nothing is a click, not an empty selection
            if inner.selection.map(|s| s.anchor == s.focus) == Some(true) {
                inner.selection = None;
            }
            true
        }
        _ => false,
    }
}

/// The caret nearest `(x, y)` among the text nodes under `root`: the one the point is inside,
/// else the nearest by line, so a sweep past the end of the text still selects to there.
fn caret_at(inner: &mut Inner, root: NodeId, x: f32, y: f32) -> Option<Caret> {
    let nodes = inner.text_nodes(root);
    if nodes.is_empty() {
        return None;
    }
    let mut chosen = nodes[0];
    let mut best = f32::INFINITY;
    for &node in &nodes {
        let Some(n) = inner.nodes.get(node) else { continue };
        let rect = n.abs;
        let distance = if y < rect.y {
            rect.y - y
        } else if y > rect.y + rect.h {
            y - (rect.y + rect.h)
        } else {
            0.0
        };
        if distance < best || (distance == best && rect.y <= y) {
            best = distance;
            chosen = node;
        }
        if distance == 0.0 && rect.contains(x, y) {
            chosen = node;
            break;
        }
    }
    let (key, origin) = {
        let n = inner.nodes.get(chosen)?;
        (n.text_key.clone()?, (n.abs.x + n.content_origin.0, n.abs.y + n.content_origin.1))
    };
    let index = inner.text.index_at(&key, x - origin.0, y - origin.1);
    Some(Caret { node: chosen, index })
}

/// The selection in reading order: `(first caret, last caret)`, whichever way it was dragged.
pub fn ordered_selection(inner: &Inner) -> Option<(Caret, Caret)> {
    let selection = inner.selection?;
    let nodes = inner.text_nodes(selection.root);
    let anchor_at = nodes.iter().position(|&n| n == selection.anchor.node)?;
    let focus_at = nodes.iter().position(|&n| n == selection.focus.node)?;
    if (focus_at, selection.focus.index) < (anchor_at, selection.anchor.index) {
        Some((selection.focus, selection.anchor))
    } else {
        Some((selection.anchor, selection.focus))
    }
}

/// What is selected in one text node: the byte range of it that lies inside the selection.
pub fn selected_range(inner: &Inner, node: NodeId) -> Option<(usize, usize)> {
    let (first, last) = ordered_selection(inner)?;
    let selection = inner.selection?;
    let nodes = inner.text_nodes(selection.root);
    let at = nodes.iter().position(|&n| n == node)?;
    let first_at = nodes.iter().position(|&n| n == first.node)?;
    let last_at = nodes.iter().position(|&n| n == last.node)?;
    if at < first_at || at > last_at {
        return None;
    }
    let text = inner.nodes.get(node)?.text_str();
    let from = if at == first_at { first.index } else { 0 };
    let to = if at == last_at { last.index } else { text.len() };
    if from >= to {
        None
    } else {
        Some((from.min(text.len()), to.min(text.len())))
    }
}

/// Everything the selection covers, one text node per line.
pub fn selected_text(inner: &Inner) -> String {
    let Some(selection) = inner.selection else { return String::new() };
    let mut parts = Vec::new();
    for node in inner.text_nodes(selection.root) {
        if let Some((from, to)) = selected_range(inner, node) {
            if let Some(n) = inner.nodes.get(node) {
                parts.push(n.text_str().get(from..to).unwrap_or("").to_string());
            }
        }
    }
    parts.join("\n")
}

/// Start, continue or end a drag. A press over a `draggable` layer takes the pointer: until
/// it comes up every move belongs to that node, wherever it has moved to — which is what
/// lets a splitter follow the pointer past its own few pixels.
fn update_drag(inner: &mut Inner, kind: i32, x: f32, y: f32) -> bool {
    match kind {
        EV_POINTER_DOWN => {
            if over_popup(inner, x, y) {
                return false;
            }
            let hit = hit_for(inner, x, y, Which::Drag);
            match (hit.node, hit.handler >= 0) {
                (Some(node), true) => {
                    // armed, not started: a press that never travels is a click, and a row
                    // that can be dragged is usually a row that can be clicked as well
                    inner.drag_armed = Some((node, x, y));
                    false
                }
                _ => false,
            }
        }
        EV_POINTER_MOVE if inner.drag_node.is_none() => {
            let Some((node, ox, oy)) = inner.drag_armed else { return false };
            if (x - ox).abs() < DRAG_THRESHOLD && (y - oy).abs() < DRAG_THRESHOLD {
                return false;
            }
            inner.drag_armed = None;
            inner.drag_node = Some(node);
            let Some(handler) = dragging_handler(inner) else { return false };
            inner.pending_events.push(Event::new(EV_DRAG, ox, oy, handler, "start".to_string()));
            inner.pending_events.push(Event::new(EV_DRAG, x, y, handler, "move".to_string()));
            announce_drop(inner, x, y);
            true
        }
        EV_POINTER_MOVE => match dragging_handler(inner) {
            Some(handler) => {
                inner.pending_events.push(Event::new(EV_DRAG, x, y, handler, "move".to_string()));
                announce_drop(inner, x, y);
                true
            }
            None => false,
        },
        EV_POINTER_UP => {
            inner.drag_armed = None;
            let handler = dragging_handler(inner);
            let dragging = inner.drag_node.is_some();
            if dragging {
                // where it was let go, which is the whole point of a drag that moves a thing
                let onto = hit_for(inner, x, y, Which::Drop).handler;
                if onto >= 0 {
                    inner.pending_events.push(Event::new(EV_DROP, x, y, onto, "drop".to_string()));
                } else if inner.drop_node >= 0 {
                    inner.pending_events.push(Event::new(EV_DROP, x, y, inner.drop_node, "leave".to_string()));
                }
            }
            inner.drop_node = -1;
            if let Some(handler) = handler {
                inner.pending_events.push(Event::new(EV_DRAG, x, y, handler, "end".to_string()));
            }
            inner.drag_node = None;
            dragging
        }
        _ => false,
    }
}

fn dragging_handler(inner: &Inner) -> Option<i32> {
    let node = inner.drag_node?;
    let handler = inner.nodes.get(node)?.layer_handler(Which::Drag);
    if handler >= 0 {
        Some(handler)
    } else {
        None
    }
}

/// The `hoverable` handler of a node as of the last layout, or -1.
fn hover_handler_of(inner: &Inner, node: Option<NodeId>) -> i32 {
    node.and_then(|n| inner.nodes.get(n)).map(|n| n.layer_handler(Which::Hover)).unwrap_or(-1)
}

/// The handler the pointer is over, for the python side (`hovered_handler`).
pub fn hovered_handler(inner: &Inner) -> i32 {
    inner.hover_node.and_then(|n| inner.nodes.get(n)).map(|n| n.layer_handler(Which::Primary)).unwrap_or(-1)
}

/// The `hoverable` handler the pointer is over, or -1.
pub fn hover_target(inner: &Inner) -> i32 {
    hover_handler_of(inner, inner.hover_target_node)
}

/// Recompute what the pointer hovers, queueing a leave and an enter when it changes. The
/// handler each event carries is read now rather than remembered, so a node whose scope
/// recomposed while the pointer sat on it is still told when the pointer leaves.
pub fn update_hover_target(inner: &mut Inner, x: f32, y: f32) {
    // a popup covers what is under it: a menu over a row must not leave the row explaining
    // itself, so hover only reaches into the popup the pointer is actually over
    let target = if over_popup(inner, x, y) {
        hover_in_popups(inner, x, y)
    } else {
        hit_for(inner, x, y, Which::Hover).node
    };
    if target == inner.hover_target_node {
        return;
    }
    let leaving = hover_handler_of(inner, inner.hover_target_node);
    inner.hover_target_node = target;
    let entering = hover_handler_of(inner, target);
    if leaving >= 0 {
        inner.pending_events.push(Event::new(EV_HOVER, x, y, leaving, String::new()));
    }
    if entering >= 0 {
        inner.pending_events.push(Event::new(EV_HOVER, x, y, entering, "1".to_string()));
    }
}

/// The hover and drag events queued since the last call.
pub fn take_events(inner: &mut Inner) -> Vec<Event> {
    std::mem::take(&mut inner.pending_events)
}

/// The pointer left the window: nothing is hovered any more.
pub fn pointer_left(inner: &mut Inner) {
    inner.hover_node = None;
    let leaving = hover_handler_of(inner, inner.hover_target_node);
    inner.hover_target_node = None;
    if leaving >= 0 {
        inner.pending_events.push(Event::new(EV_HOVER, 0.0, 0.0, leaving, String::new()));
    }
}

/// Scroll the innermost scrollable container under `(x, y)` by `dy` logical pixels
/// (positive = content moves up). Returns whether anything moved. Absolute rects and the
/// hovered handler are brought up to date, so a repaint is all that is needed afterwards.
pub fn scroll(inner: &mut Inner, x: f32, y: f32, dy: f32) -> bool {
    scroll_by(inner, x, y, 0.0, dy)
}

/// The same with a sideways component, for a container laid out at its content's own width.
pub fn scroll_by(inner: &mut Inner, x: f32, y: f32, dx: f32, dy: f32) -> bool {
    let dx = if dx.is_finite() { dx } else { 0.0 };
    let dy = if dy.is_finite() { dy } else { 0.0 };
    if dx == 0.0 && dy == 0.0 {
        return false;
    }
    let root = inner.root;
    let mut moved = false;
    if dy != 0.0 {
        if let Some(target) = innermost_scrollable(inner, root, x, y, dy) {
            let node = &mut inner.nodes[target];
            let new = (node.scroll + dy).clamp(0.0, node.scroll_limit());
            if new != node.scroll {
                node.scroll = new;
                moved = true;
            }
        }
    }
    if dx != 0.0 {
        if let Some(target) = innermost_pannable(inner, root, x, y, dx) {
            let node = &mut inner.nodes[target];
            let new = (node.scroll_x + dx).clamp(0.0, node.scroll_limit_x());
            if new != node.scroll_x {
                node.scroll_x = new;
                moved = true;
            }
        }
    }
    if !moved {
        return false;
    }
    announce_scroll(inner, root, x, y);
    crate::layout::assign_abs(inner);
    inner.hover_node = hit_test(inner, x, y).node;
    update_hover_target(inner, x, y);
    true
}

/// Tell a scroll container that asked how far it has moved and how tall its viewport is —
/// which is everything a list needs to draw only the rows that are in view.
pub fn announce_scroll(inner: &mut Inner, root: NodeId, x: f32, y: f32) {
    let mut found: Vec<(i32, f32, f32)> = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let Some(node) = inner.nodes.get(id) else { continue };
        if node.kind == Kind::Scroll && node.handler >= 0 {
            found.push((node.handler, node.scroll, node.content_size.h));
        }
        stack.extend(node.children.iter().copied());
    }
    for (handler, offset, height) in found {
        inner.pending_events.push(Event::new(EV_SCROLLED, offset, height, handler, String::new()));
    }
}

/// What a drag is over, so the thing it would land on can say so before it lands.
fn announce_drop(inner: &mut Inner, x: f32, y: f32) {
    let over = hit_for(inner, x, y, Which::Drop).handler;
    if over == inner.drop_node {
        return;
    }
    if inner.drop_node >= 0 {
        inner.pending_events.push(Event::new(EV_DROP, x, y, inner.drop_node, "leave".to_string()));
    }
    if over >= 0 {
        inner.pending_events.push(Event::new(EV_DROP, x, y, over, "over".to_string()));
    }
    inner.drop_node = over;
}

/// The deepest SCROLL node under the point that can still move sideways.
fn innermost_pannable(inner: &Inner, id: NodeId, x: f32, y: f32, dx: f32) -> Option<NodeId> {
    let node = inner.nodes.get(id)?;
    for &c in node.children.iter().rev() {
        let Some(child) = inner.nodes.get(c) else { continue };
        if child.is_group() {
            if let Some(found) = innermost_pannable(inner, c, x, y, dx) {
                return Some(found);
            }
            continue;
        }
        if !child.abs.contains(x, y) {
            continue;
        }
        if let Some(found) = innermost_pannable(inner, c, x, y, dx) {
            return Some(found);
        }
        if child.kind == Kind::Scroll {
            let limit = child.scroll_limit_x();
            let can_move = if dx > 0.0 { child.scroll_x < limit } else { child.scroll_x > 0.0 };
            if limit > 0.0 && can_move {
                return Some(c);
            }
        }
    }
    None
}

/// The deepest SCROLL node containing the point that can still move in the direction of `dy`;
/// an inner container that has reached its end hands the wheel to the one around it.
fn innermost_scrollable(inner: &Inner, id: NodeId, x: f32, y: f32, dy: f32) -> Option<NodeId> {
    let node = inner.nodes.get(id)?;
    for &c in node.children.iter().rev() {
        let Some(child) = inner.nodes.get(c) else { continue };
        if child.is_group() {
            if let Some(found) = innermost_scrollable(inner, c, x, y, dy) {
                return Some(found);
            }
            continue;
        }
        if !child.abs.contains(x, y) {
            continue;
        }
        if let Some(found) = innermost_scrollable(inner, c, x, y, dy) {
            return Some(found);
        }
        if child.kind == Kind::Scroll {
            let limit = child.scroll_limit();
            let can_move = if dy > 0.0 { child.scroll < limit } else { child.scroll > 0.0 };
            if limit > 0.0 && can_move {
                return Some(c);
            }
        }
    }
    None
}

/// The chord string for a key press with modifiers, in the order `ctrl+alt+shift+cmd+key`.
/// `key` is already the lowercase key name ("x", "up", "enter").
pub fn chord(ctrl: bool, alt: bool, shift: bool, cmd: bool, key: &str) -> String {
    let mut out = String::new();
    for (on, name) in [(ctrl, "ctrl"), (alt, "alt"), (shift, "shift"), (cmd, "cmd")] {
        if on {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(key);
    out
}

/// The modifier prefix for a pointer event ("", "shift", "shift+cmd", …).
pub fn modifier_text(ctrl: bool, alt: bool, shift: bool, cmd: bool) -> String {
    let mut out = chord(ctrl, alt, shift, cmd, "");
    out.pop();
    out
}

fn focus_field(inner: &mut Inner, field: NodeId) {
    if inner.focus.as_ref().map(|f| f.node) == Some(field) {
        return;
    }
    let buffer = inner.nodes.get(field).map(|n| n.text_str().to_string()).unwrap_or_default();
    let caret = buffer.chars().count();
    inner.focus = Some(Focus::new(field, buffer, caret));
    inner.mark_dirty(field);
}

/// Focus a field and put the caret where the press landed, rather than at the end of the
/// text — clicking into the middle of a word and typing at the end of it is nobody's idea of
/// a text box. A press in a field that already has focus moves the caret the same way.
fn focus_field_at(inner: &mut Inner, field: NodeId, x: f32, y: f32) {
    focus_field(inner, field);
    let Some(index) = field_caret_at(inner, field, x, y) else { return };
    if let Some(f) = inner.focus.as_mut().filter(|f| f.node == field) {
        f.caret = index.min(f.buffer.chars().count());
        f.anchor = None;
        f.touch();
    }
    inner.mark_dirty(field);
}

/// The char index a point falls on in a field, allowing for how far the field has scrolled.
fn field_caret_at(inner: &mut Inner, field: NodeId, x: f32, y: f32) -> Option<usize> {
    let (scroll, empty) = inner
        .focus
        .as_ref()
        .filter(|f| f.node == field)
        .map(|f| (f.scroll, f.buffer.is_empty()))
        .unwrap_or(((0.0, 0.0), true));
    if empty {
        return Some(0);
    }
    let (key, origin) = {
        let n = inner.nodes.get(field)?;
        let content = n.content_rect();
        (
            n.text_key.clone()?,
            (content.x + crate::layout::FIELD_PAD_X - scroll.0, content.y + crate::layout::FIELD_PAD_Y - scroll.1),
        )
    };
    // the text system answers in bytes; a caret is a char index
    let byte = inner.text.index_at(&key, x - origin.0, y - origin.1);
    let text = inner.focus.as_ref()?.buffer.clone();
    Some(text.char_indices().take_while(|(i, _)| *i < byte).count())
}

pub fn focused_handler(inner: &Inner) -> i32 {
    inner.focus.as_ref().and_then(|f| inner.nodes.get(f.node)).map(|n| n.handler).unwrap_or(-1)
}

/// Typed text. With a focused field it edits the buffer and emits kind 4 with the new value;
/// without one it emits kind 4 with handler -1 so python can implement shortcuts.
pub fn key_text(inner: &mut Inner, text: &str) -> Option<Event> {
    let Some(f) = inner.focus.as_mut() else {
        if text.is_empty() {
            return None;
        }
        return Some(Event::new(EV_KEY_TEXT, 0.0, 0.0, -1, text.to_string()));
    };
    let insert: String = text.chars().filter(|c| !c.is_control()).collect();
    if insert.is_empty() {
        return None;
    }
    // typing with something selected replaces it, rather than pushing it along
    take_selection(f);
    let at = byte_index(&f.buffer, f.caret);
    f.buffer.insert_str(at, &insert);
    f.caret += insert.chars().count();
    f.touch();
    edited(inner)
}

/// Which modifiers were held with an editing key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, meta: false };
}

/// What a key press did to the focused field. `Ignored` means the field wants nothing to do
/// with it, and the window is free to hand it to the application as a chord.
pub enum Edit {
    Ignored,
    Took(Option<Event>),
}

/// Editing keys with nothing held. Names: Backspace, Delete, Left, Right, Up, Down, Home,
/// End, Escape, Tab, Enter.
pub fn key_named(inner: &mut Inner, name: &str) -> Option<Event> {
    match key_edit(inner, name, Mods::NONE) {
        Edit::Took(event) => event,
        Edit::Ignored => None,
    }
}

/// The same, with modifiers: ⌥ moves and deletes by word, ⌘ goes to the ends of the line,
/// shift keeps the far end of the selection where it was — what the rest of the system does.
pub fn key_edit(inner: &mut Inner, name: &str, mods: Mods) -> Edit {
    if inner.focus.is_none() {
        if mods != Mods::NONE {
            return Edit::Ignored;
        }
        let text = match name {
            "Enter" => "\n",
            "Escape" => "\x1b",
            "Backspace" => "\x08",
            "Delete" => "\x7f",
            _ => return Edit::Ignored,
        };
        return Edit::Took(Some(Event::new(EV_KEY_TEXT, 0.0, 0.0, -1, text.to_string())));
    }
    let by_word = mods.alt;
    let to_edge = mods.meta;
    let multiline = focused_multiline(inner);
    match name {
        "Backspace" | "Delete" => {
            let forward = name == "Delete";
            let f = inner.focus.as_mut().unwrap();
            if take_selection(f) {
                return Edit::Took(edited(inner));
            }
            let len = f.buffer.chars().count();
            let to = if by_word {
                if forward { word_right(&f.buffer, f.caret) } else { word_left(&f.buffer, f.caret) }
            } else if to_edge {
                if forward { line_end(&f.buffer, f.caret, multiline) } else { line_start(&f.buffer, f.caret, multiline) }
            } else if forward {
                (f.caret + 1).min(len)
            } else {
                f.caret.saturating_sub(1)
            };
            if to == f.caret {
                return Edit::Took(None);
            }
            let (a, b) = (to.min(f.caret), to.max(f.caret));
            let (start, end) = (byte_index(&f.buffer, a), byte_index(&f.buffer, b));
            f.buffer.replace_range(start..end, "");
            f.caret = a;
            f.anchor = None;
            f.touch();
            Edit::Took(edited(inner))
        }
        "Left" | "Right" | "Up" | "Down" | "Home" | "End" => {
            // up and down belong to the application unless the field has lines to move through
            if matches!(name, "Up" | "Down") && !multiline && !to_edge {
                return Edit::Ignored;
            }
            let target = caret_after_move(inner, name, by_word, to_edge, multiline);
            let f = inner.focus.as_mut().unwrap();
            let node = f.node;
            if mods.shift {
                // the anchor is where the selection started; it stays while this end moves
                if f.anchor.is_none() {
                    f.anchor = Some(f.caret);
                }
                f.caret = target;
            } else if let Some((a, b)) = f.range() {
                // a plain arrow with a selection collapses it to the end it points at
                f.anchor = None;
                f.caret = if matches!(name, "Left" | "Up" | "Home") { a } else { b };
                if by_word || to_edge || matches!(name, "Up" | "Down" | "Home" | "End") {
                    f.caret = target;
                }
            } else {
                f.anchor = None;
                f.caret = target;
            }
            f.touch();
            inner.mark_dirty(node);
            Edit::Took(None)
        }
        "A" if mods.meta || mods.ctrl => {
            let f = inner.focus.as_mut().unwrap();
            f.anchor = Some(0);
            f.caret = f.buffer.chars().count();
            f.touch();
            let node = f.node;
            inner.mark_dirty(node);
            Edit::Took(None)
        }
        "Escape" => {
            if let Some(f) = inner.focus.take() {
                inner.mark_dirty(f.node);
            }
            Edit::Took(None)
        }
        "Enter" => {
            // a field of one line has nothing to do with Enter; one that takes several
            // gets a line break, which is the only way to type a paragraph
            if !multiline || mods.meta || mods.ctrl {
                return Edit::Ignored;
            }
            let f = inner.focus.as_mut().unwrap();
            take_selection(f);
            let at = byte_index(&f.buffer, f.caret);
            f.buffer.insert(at, '\n');
            f.caret += 1;
            f.touch();
            Edit::Took(edited(inner))
        }
        "Tab" => {
            if mods != Mods::NONE {
                return Edit::Ignored;
            }
            focus_next_field(inner);
            Edit::Took(None)
        }
        _ => Edit::Ignored,
    }
}

fn focused_multiline(inner: &Inner) -> bool {
    inner.focus.as_ref().and_then(|f| inner.nodes.get(f.node)).map(|n| n.modifier.multiline).unwrap_or(false)
}

/// Where an arrow key leaves the caret, before anything is done about the selection.
fn caret_after_move(inner: &Inner, name: &str, by_word: bool, to_edge: bool, multiline: bool) -> usize {
    let f = inner.focus.as_ref().unwrap();
    let len = f.buffer.chars().count();
    match name {
        "Left" if by_word => word_left(&f.buffer, f.caret),
        "Right" if by_word => word_right(&f.buffer, f.caret),
        "Left" if to_edge => line_start(&f.buffer, f.caret, multiline),
        "Right" if to_edge => line_end(&f.buffer, f.caret, multiline),
        "Left" => f.caret.saturating_sub(1),
        "Right" => (f.caret + 1).min(len),
        "Home" => line_start(&f.buffer, f.caret, multiline),
        "End" => line_end(&f.buffer, f.caret, multiline),
        // the whole text, which is what ⌘↑ and ⌘↓ mean everywhere else
        "Up" if to_edge => 0,
        "Down" if to_edge => len,
        "Up" => line_above(&f.buffer, f.caret),
        "Down" => line_below(&f.buffer, f.caret),
        _ => f.caret,
    }
}

/// Cut whatever is selected out of the buffer. True when there was a selection to take.
fn take_selection(f: &mut Focus) -> bool {
    let Some((a, b)) = f.range() else {
        f.anchor = None;
        return false;
    };
    let (start, end) = (byte_index(&f.buffer, a), byte_index(&f.buffer, b));
    f.buffer.replace_range(start..end, "");
    f.caret = a;
    f.anchor = None;
    f.touch();
    true
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The start of the word before the caret: back over any separators, then over the word.
fn word_left(buffer: &str, caret: usize) -> usize {
    let chars: Vec<char> = buffer.chars().collect();
    let mut at = caret.min(chars.len());
    while at > 0 && !is_word(chars[at - 1]) {
        at -= 1;
    }
    while at > 0 && is_word(chars[at - 1]) {
        at -= 1;
    }
    at
}

fn word_right(buffer: &str, caret: usize) -> usize {
    let chars: Vec<char> = buffer.chars().collect();
    let mut at = caret.min(chars.len());
    while at < chars.len() && !is_word(chars[at]) {
        at += 1;
    }
    while at < chars.len() && is_word(chars[at]) {
        at += 1;
    }
    at
}

/// The start of the line the caret is on. A field of one line has one line, so it is 0.
fn line_start(buffer: &str, caret: usize, multiline: bool) -> usize {
    if !multiline {
        return 0;
    }
    let chars: Vec<char> = buffer.chars().collect();
    let mut at = caret.min(chars.len());
    while at > 0 && chars[at - 1] != '\n' {
        at -= 1;
    }
    at
}

fn line_end(buffer: &str, caret: usize, multiline: bool) -> usize {
    let chars: Vec<char> = buffer.chars().collect();
    if !multiline {
        return chars.len();
    }
    let mut at = caret.min(chars.len());
    while at < chars.len() && chars[at] != '\n' {
        at += 1;
    }
    at
}

/// The same column, one line up. These are the writer's own line breaks — a field that also
/// wraps moves a wrapped line at a time only where the two agree, which is close enough to
/// be useful and needs nothing from the shaper.
fn line_above(buffer: &str, caret: usize) -> usize {
    let start = line_start(buffer, caret, true);
    if start == 0 {
        return 0;
    }
    let column = caret - start;
    let previous = line_start(buffer, start - 1, true);
    (previous + column).min(start - 1)
}

fn line_below(buffer: &str, caret: usize) -> usize {
    let len = buffer.chars().count();
    let start = line_start(buffer, caret, true);
    let end = line_end(buffer, caret, true);
    if end >= len {
        return len;
    }
    let column = caret - start;
    let next_end = line_end(buffer, end + 1, true);
    (end + 1 + column).min(next_end)
}

fn edited(inner: &mut Inner) -> Option<Event> {
    let f = inner.focus.as_ref()?;
    let node = f.node;
    let text = f.buffer.clone();
    let handler = inner.nodes.get(node)?.handler;
    inner.mark_dirty(node);
    Some(Event::new(EV_KEY_TEXT, 0.0, 0.0, handler, text))
}

fn byte_index(s: &str, char_index: usize) -> usize {
    s.char_indices().nth(char_index).map(|(i, _)| i).unwrap_or(s.len())
}

fn focus_next_field(inner: &mut Inner) {
    let mut fields = Vec::new();
    let mut stack = vec![inner.root];
    while let Some(n) = stack.pop() {
        let Some(node) = inner.nodes.get(n) else { continue };
        if node.kind == Kind::TextField {
            fields.push(n);
        }
        for &c in node.children.iter().rev() {
            stack.push(c);
        }
    }
    if fields.is_empty() {
        return;
    }
    let current = inner.focus.as_ref().map(|f| f.node);
    let next = match current.and_then(|c| fields.iter().position(|&f| f == c)) {
        Some(i) => fields[(i + 1) % fields.len()],
        None => fields[0],
    };
    if let Some(f) = inner.focus.take() {
        inner.mark_dirty(f.node);
    }
    focus_field(inner, next);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::commit;
    use crate::layout::{layout_tree, NoHost};
    use crate::text::TextSystem;

    fn fragment() -> Vec<i32> {
        [
            [3, 0, -1, 0, -1, 0, 0, 0],   // column
            [7, 0, 0, 0, 1, 1, 0, 0],     // button "ok" handler 1 enabled
            [7, 0, 0, 0, 2, 0, 0, 0],     // button disabled handler 2
            [8, 0, 1, 0, 3, 0, -1, 0],    // text field value strs[1] handler 3
            [10, 0, -1, 1, -1, 0, 0, 0],  // clickable spacer
            [10, 0, -1, 2, -1, 0, 0, 0],  // padded clickable spacer
            [0, 0, -1, 0, -1, 0, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect()
    }

    fn build() -> Inner {
        let mut inner = Inner::new(400.0, 300.0, 1.0, TextSystem::monospace_only());
        // mod 1: clickable handler 5, 100x100; mod 2: padding 10 then clickable 6
        let mods = vec![(1, vec![2.0, 100.0, 3.0, 100.0, 9.0, 5.0]), (2, vec![1.0, 10.0, 10.0, 10.0, 10.0, 9.0, 6.0, 2.0, 50.0, 3.0, 50.0])];
        commit(&mut inner, &fragment(), &["ok", "hi"], &[(0, 0, 7)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        inner
    }

    #[test]
    fn the_cursor_is_the_innermost_one_asked_for() {
        // a column that asks for a hand, holding a spacer that asks for a text bar
        let ints: Vec<i32> = [
            [3, 0, -1, 1, -1, 0, 0, 0],  // column, 200x100, cursor 1
            [10, 0, -1, 2, -1, 0, 0, 0], // spacer, 50x20, cursor 2
            [0, 0, -1, 0, -1, 0, 0, 0],  // end of the column
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        let mods = vec![(1, vec![2.0, 200.0, 3.0, 100.0, 21.0, 1.0]), (2, vec![2.0, 50.0, 3.0, 20.0, 21.0, 2.0])];
        let mut inner = Inner::new(400.0, 300.0, 1.0, TextSystem::monospace_only());
        commit(&mut inner, &ints, &[], &[(0, 0, 3)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        // over the spacer: its own cursor wins over the column's
        assert_eq!(cursor_at(&inner, 10.0, 10.0), 2);
        // elsewhere in the column: the column's
        assert_eq!(cursor_at(&inner, 100.0, 60.0), 1);
        // outside it: whatever the platform draws
        assert_eq!(cursor_at(&inner, 300.0, 200.0), 0);
    }

    #[test]
    fn hit_testing() {
        let inner = build();
        let col = inner.layout_children(inner.root)[0];
        let kids = inner.layout_children(col);
        let r = |i: usize| inner.nodes[kids[i]].abs;
        assert_eq!(hit_test(&inner, r(0).x + 1.0, r(0).y + 1.0).handler, 1);
        assert_eq!(hit_test(&inner, r(1).x + 1.0, r(1).y + 1.0).handler, -1);
        let field = hit_test(&inner, r(2).x + 1.0, r(2).y + 1.0);
        assert_eq!(field.focus, Some(kids[2]));
        assert_eq!(hit_test(&inner, r(3).x + 50.0, r(3).y + 50.0).handler, 5);
        // padding is outside the clickable layer
        assert_eq!(hit_test(&inner, r(4).x + 2.0, r(4).y + 2.0).handler, -1);
        assert_eq!(hit_test(&inner, r(4).x + 20.0, r(4).y + 20.0).handler, 6);
        assert_eq!(hit_test(&inner, 399.0, 299.0), Hit::NONE);
    }

    fn scrolled() -> Inner {
        let mut inner = Inner::new(200.0, 100.0, 1.0, TextSystem::monospace_only());
        // mod 1: 200x100 viewport; mod 2: 200x50 clickable rows (handlers 1..4)
        let mods = vec![
            (1, vec![2.0, 200.0, 3.0, 100.0]),
            (2, vec![2.0, 200.0, 3.0, 50.0, 9.0, 1.0]),
            (3, vec![2.0, 200.0, 3.0, 50.0, 9.0, 2.0]),
            (4, vec![2.0, 200.0, 3.0, 50.0, 9.0, 3.0]),
            (5, vec![2.0, 200.0, 3.0, 50.0, 9.0, 4.0]),
        ];
        let ints: Vec<i32> = [
            [13, 0, -1, 1, -1, 0, 0, 0],
            [10, 0, -1, 2, -1, 0, 0, 0],
            [10, 0, -1, 3, -1, 0, 0, 0],
            [10, 0, -1, 4, -1, 0, 0, 0],
            [10, 0, -1, 5, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        commit(&mut inner, &ints, &[], &[(0, 0, 6)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        inner
    }

    #[test]
    fn wheel_scrolling_moves_content_and_hover() {
        let mut inner = scrolled();
        assert_eq!(hit_test(&inner, 10.0, 75.0).handler, 2);
        assert_eq!(pointer(&mut inner, EV_POINTER_MOVE, 10.0, 75.0).handler, 2);
        assert_eq!(hovered_handler(&inner), 2);
        assert!(scroll(&mut inner, 10.0, 75.0, 60.0));
        assert_eq!(hit_test(&inner, 10.0, 75.0).handler, 3, "row 3 moved under the pointer");
        assert_eq!(hovered_handler(&inner), 3);
        // clamped at the end: 200 content - 100 viewport = 100
        assert!(scroll(&mut inner, 10.0, 75.0, 1000.0));
        let sc = inner.layout_children(inner.root)[0];
        assert_eq!(inner.nodes[sc].scroll, 100.0);
        assert!(!scroll(&mut inner, 10.0, 75.0, 10.0), "nothing left to scroll");
        assert!(scroll(&mut inner, 10.0, 75.0, -100.0));
        assert_eq!(inner.nodes[sc].scroll, 0.0);
        assert!(!scroll(&mut inner, 10.0, 75.0, -1.0));
        assert!(!scroll(&mut inner, 199.0, 99.0, f32::NAN));
        assert!(!scroll(&mut inner, 300.0, 300.0, 10.0), "outside every container");
        pointer_left(&mut inner);
        assert_eq!(hovered_handler(&inner), -1);
    }

    fn popup_tree() -> Inner {
        let mut inner = Inner::new(200.0, 200.0, 1.0, TextSystem::monospace_only());
        // mod 1: 200x200 with a clickable (handler 1) and a secondary handler (2);
        // mod 2: a 60x40 popup body; mod 4: a 60x20 clickable (handler 3) filling its top
        // half, so its bottom half is blank; mod 3: 50x50 hoverable (handler 4)
        let mods = vec![
            (1, vec![2.0, 200.0, 3.0, 200.0, 9.0, 1.0, 16.0, 2.0]),
            (2, vec![2.0, 60.0, 3.0, 40.0]),
            (3, vec![2.0, 50.0, 3.0, 50.0, 17.0, 4.0]),
            (4, vec![2.0, 60.0, 3.0, 20.0, 9.0, 3.0]),
        ];
        let ints: Vec<i32> = [
            [3, 0, -1, 0, -1, 0, 0, 0],       // column
            [10, 0, -1, 3, -1, 0, 0, 0],      // hoverable spacer, 50x50 at the top
            [10, 0, -1, 1, -1, 0, 0, 0],      // the big clickable / right-clickable spacer
            [14, 0, -1, 0, -1, 20, 0, 20],    // a popup at (20, 20)
            [3, 0, -1, 2, -1, 0, 0, 0],       // its body, 60x40
            [10, 0, -1, 4, -1, 0, 0, 0],      // clickable, top half only
            [0, 0, -1, 0, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        commit(&mut inner, &ints, &[], &[(0, 0, 9)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        inner
    }

    #[test]
    fn a_popup_is_hit_before_the_tree_under_it() {
        let mut inner = popup_tree();
        // the popup sits at (20, 20) and is 60x40; inside it, its own handler answers
        assert_eq!(hit_test(&inner, 30.0, 30.0).handler, 3);
        // its blank half is not something to click, so the pointer carries on down to what
        // the popup covers — a tooltip must not eat the click under it
        assert_eq!(hit_test(&inner, 30.0, 55.0).handler, 1);
        // outside it, the tree below answers again
        assert_eq!(hit_test(&inner, 150.0, 150.0).handler, 1);
        // a popup that no longer exists stops being hit
        let ints: Vec<i32> = [[3, 0, -1, 0, -1, 0, 0, 0], [0, 0, -1, 0, -1, 0, 0, 0]].iter().flatten().copied().collect();
        commit(&mut inner, &ints, &[], &[(0, 0, 2)], &[], &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        assert!(inner.live_popups().is_empty());
        assert_eq!(hit_test(&inner, 30.0, 30.0).handler, -1);
    }

    #[test]
    fn the_right_button_reports_its_own_handler() {
        let mut inner = popup_tree();
        // the right button falls through the clickable to the secondary handler
        let down = pointer(&mut inner, EV_SECONDARY_DOWN, 150.0, 150.0);
        assert_eq!((down.kind, down.handler), (EV_SECONDARY_DOWN, 2));
        let up = pointer(&mut inner, EV_SECONDARY_UP, 150.0, 150.0);
        assert_eq!(up.handler, 2);
        // and the left button still reports the clickable
        assert_eq!(pointer(&mut inner, EV_POINTER_DOWN, 150.0, 150.0).handler, 1);
        // a right click where nothing asked for one reports nothing
        assert_eq!(pointer(&mut inner, EV_SECONDARY_DOWN, 30.0, 30.0).handler, -1);
        // the right button leaves the hover paint alone
        assert_eq!(hovered_handler(&inner), 1);
    }

    #[test]
    fn entering_and_leaving_a_hoverable_is_reported_once_each() {
        let mut inner = popup_tree();
        assert!(take_events(&mut inner).is_empty());
        pointer(&mut inner, EV_POINTER_MOVE, 10.0, 10.0);
        let events = take_events(&mut inner);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].kind, events[0].handler, events[0].text.as_str()), (EV_HOVER, 4, "1"));
        assert_eq!(hover_target(&inner), 4);
        // moving within it says nothing more
        pointer(&mut inner, EV_POINTER_MOVE, 12.0, 12.0);
        assert!(take_events(&mut inner).is_empty());
        // moving out reports the leave
        pointer(&mut inner, EV_POINTER_MOVE, 150.0, 150.0);
        let events = take_events(&mut inner);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].handler, events[0].text.as_str()), (4, ""));
        assert_eq!(hover_target(&inner), -1);
        // and leaving the window entirely reports it too
        pointer(&mut inner, EV_POINTER_MOVE, 10.0, 10.0);
        take_events(&mut inner);
        pointer_left(&mut inner);
        let events = take_events(&mut inner);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].handler, events[0].text.as_str()), (4, ""));
    }

    #[test]
    fn a_drag_follows_the_pointer_off_the_node_it_started_on() {
        let mut inner = Inner::new(200.0, 200.0, 1.0, TextSystem::monospace_only());
        // a 20x200 draggable strip (handler 7) beside a 180x200 clickable one (handler 1)
        let mods = vec![
            (1, vec![2.0, 20.0, 3.0, 200.0, 19.0, 7.0]),
            (2, vec![2.0, 180.0, 3.0, 200.0, 9.0, 1.0]),
        ];
        let ints: Vec<i32> = [
            [4, 0, -1, 0, -1, 0, 0, 0],
            [10, 0, -1, 1, -1, 0, 0, 0],
            [10, 0, -1, 2, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        commit(&mut inner, &ints, &[], &[(0, 0, 4)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());

        // pressing the strip arms a drag; until the pointer travels it is still a press
        pointer(&mut inner, EV_POINTER_DOWN, 10.0, 100.0);
        assert!(take_events(&mut inner).is_empty(), "nothing has been dragged yet");

        // the pointer moves far away, over the other strip: the drag starts and owns it
        pointer(&mut inner, EV_POINTER_MOVE, 150.0, 40.0);
        let events = take_events(&mut inner);
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].kind, events[0].handler, events[0].text.as_str()), (EV_DRAG, 7, "start"));
        assert_eq!((events[1].handler, events[1].text.as_str(), events[1].x), (7, "move", 150.0));

        // and releasing ends it, without clicking what is under the pointer
        let up = pointer(&mut inner, EV_POINTER_UP, 150.0, 40.0);
        assert_eq!(up.handler, -1);
        let events = take_events(&mut inner);
        assert_eq!((events[0].handler, events[0].text.as_str()), (7, "end"));
        assert!(inner.drag_node.is_none());

        // afterwards the other strip clicks again
        assert_eq!(pointer(&mut inner, EV_POINTER_DOWN, 150.0, 40.0).handler, 1);
        assert!(take_events(&mut inner).is_empty());
    }

    #[test]
    fn text_is_selected_by_sweeping_the_pointer_over_it() {
        let mut inner = Inner::new(300.0, 200.0, 1.0, TextSystem::monospace_only());
        // a selectable column of three lines
        let mods = vec![(1, vec![20.0, 1073741824.0])];
        let ints: Vec<i32> = [
            [3, 0, -1, 1, -1, 0, 0, 0],
            [6, 0, 0, 0, -1, 0, 0, 0],
            [6, 0, 1, 0, -1, 0, 0, 0],
            [6, 0, 2, 0, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        commit(&mut inner, &ints, &["first line", "second line", "third line"], &[(0, 0, 5)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        let advance = crate::types::Style::DEFAULT.size() * crate::text::FALLBACK_ADVANCE;
        let line = crate::text::TextSystem::line_height(crate::types::Style::DEFAULT);

        // press in the middle of the first line and sweep down into the second
        pointer(&mut inner, EV_POINTER_DOWN, advance * 6.0, line * 0.5);
        assert!(inner.selecting);
        pointer(&mut inner, EV_POINTER_MOVE, advance * 7.0, line * 1.5);
        assert_eq!(selected_text(&inner), "line\nsecond ");
        // and on to the third, which takes the whole second line with it
        pointer(&mut inner, EV_POINTER_MOVE, advance * 5.0, line * 2.5);
        assert_eq!(selected_text(&inner), "line\nsecond line\nthird");
        pointer(&mut inner, EV_POINTER_UP, advance * 5.0, line * 2.5);
        assert!(!inner.selecting && inner.selection.is_some());

        // sweeping backwards reads the same way round
        pointer(&mut inner, EV_POINTER_DOWN, advance * 5.0, line * 2.5);
        pointer(&mut inner, EV_POINTER_MOVE, advance * 6.0, line * 0.5);
        assert_eq!(selected_text(&inner), "line\nsecond line\nthird");

        // a press that goes nowhere is a click, and clears it
        pointer(&mut inner, EV_POINTER_DOWN, advance * 2.0, line * 0.5);
        pointer(&mut inner, EV_POINTER_UP, advance * 2.0, line * 0.5);
        assert_eq!(selected_text(&inner), "");
        // as does a press outside the selectable node
        pointer(&mut inner, EV_POINTER_DOWN, advance * 2.0, line * 0.5);
        pointer(&mut inner, EV_POINTER_MOVE, advance * 8.0, line * 0.5);
        assert!(!selected_text(&inner).is_empty());
        pointer(&mut inner, EV_POINTER_UP, advance * 8.0, line * 0.5);
        pointer(&mut inner, EV_POINTER_DOWN, 290.0, 190.0);
        assert_eq!(selected_text(&inner), "");
    }

    #[test]
    fn a_press_outside_a_dismiss_layer_is_reported_without_being_taken() {
        let mut inner = Inner::new(300.0, 200.0, 1.0, TextSystem::monospace_only());
        // a clickable row under a popup that asks to be told about presses elsewhere
        let mods = vec![
            (1, vec![2.0, 300.0, 3.0, 200.0]),
            (2, vec![2.0, 300.0, 3.0, 40.0, 9.0, 5.0]),
            (3, vec![2.0, 80.0, 3.0, 30.0, 23.0, 7.0]),
        ];
        let ints: Vec<i32> = [
            [3, 0, -1, 1, -1, 0, 0, 0],  // column
            [10, 0, -1, 2, -1, 0, 0, 0], // a clickable row, handler 5
            [14, 0, -1, 0, -1, 0, 0, 0], // popup
            [10, 0, -1, 3, -1, 0, 0, 0], // the menu inside it, dismiss handler 7
            [0, 0, -1, 0, -1, 0, 0, 0],  // end of the popup
            [0, 0, -1, 0, -1, 0, 0, 0],  // end of the column
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        commit(&mut inner, &ints, &[], &[(0, 0, 6)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());

        // a press inside the menu is the menu's: nothing is dismissed
        pointer(&mut inner, EV_POINTER_DOWN, 10.0, 10.0);
        assert!(take_events(&mut inner).is_empty());

        // a press outside it tells the layer, and still lands on the row underneath
        let ev = pointer(&mut inner, EV_POINTER_DOWN, 200.0, 20.0);
        assert_eq!(ev.handler, 5, "the press went to the row, not to the menu");
        let events = take_events(&mut inner);
        assert_eq!(events.len(), 1, "{:?}", events);
        assert_eq!(events[0].kind, EV_DISMISS);
        assert_eq!(events[0].handler, 7);
    }

    #[test]
    fn a_press_on_a_popup_is_the_popups_and_not_a_sweep_of_what_is_under_it() {
        let mut inner = Inner::new(300.0, 200.0, 1.0, TextSystem::monospace_only());
        // selectable text with a popup over it, the way a menu sits over a diff
        let mods = vec![(1, vec![20.0, 1073741824.0]), (2, vec![2.0, 40.0, 3.0, 12.0, 9.0, 7.0])];
        let ints: Vec<i32> = [
            [3, 0, -1, 1, -1, 0, 0, 0],
            [6, 0, 0, 0, -1, 0, 0, 0],
            [6, 0, 1, 0, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
            [14, 0, -1, 0, -1, 0, 0, 0],
            [10, 0, -1, 2, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        commit(&mut inner, &ints, &["first line", "second line"], &[(0, 0, 7)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());

        // a press inside the popup answers the popup's own handler and selects nothing
        let ev = pointer(&mut inner, EV_POINTER_DOWN, 20.0, 6.0);
        assert_eq!(ev.handler, 7);
        assert!(!inner.selecting);
        pointer(&mut inner, EV_POINTER_MOVE, 30.0, 8.0);
        assert_eq!(selected_text(&inner), "");
        pointer(&mut inner, EV_POINTER_UP, 30.0, 8.0);

        // beside the popup, the text still sweeps as it did
        let line = crate::text::TextSystem::line_height(crate::types::Style::DEFAULT);
        let advance = crate::types::Style::DEFAULT.size() * crate::text::FALLBACK_ADVANCE;
        pointer(&mut inner, EV_POINTER_DOWN, advance * 6.0, line * 0.5);
        assert!(inner.selecting);
        pointer(&mut inner, EV_POINTER_MOVE, advance * 4.0, line * 1.5);
        assert!(!selected_text(&inner).is_empty());
    }

    #[test]
    fn chords_and_modifier_text() {
        assert_eq!(chord(false, false, false, true, "x"), "cmd+x");
        assert_eq!(chord(true, true, true, true, "up"), "ctrl+alt+shift+cmd+up");
        assert_eq!(chord(false, false, false, false, "enter"), "enter");
        assert_eq!(modifier_text(false, false, true, true), "shift+cmd");
        assert_eq!(modifier_text(false, false, false, false), "");
    }

    /// A caret that moves by word and by line, and a selection that shift makes and typing
    /// replaces: what every other text box on the machine does.
    #[test]
    fn the_editing_keys_move_by_word_and_select() {
        let mut inner = build();
        let col = inner.layout_children(inner.root)[0];
        let kids = inner.layout_children(col);
        let fr = inner.nodes[kids[2]].abs;
        pointer(&mut inner, EV_POINTER_DOWN, fr.x + fr.w - 2.0, fr.y + 5.0);
        assert_eq!(focused_handler(&inner), 3);
        let alt = Mods { alt: true, ..Mods::NONE };
        let meta = Mods { meta: true, ..Mods::NONE };
        let shift = Mods { shift: true, ..Mods::NONE };

        key_text(&mut inner, " one two three").unwrap();
        assert_eq!(inner.focus.as_ref().unwrap().buffer, "hi one two three");

        // ⌥← is a word back, twice over, and ⌘← is the start of the line
        key_edit(&mut inner, "Left", alt);
        assert_eq!(inner.focus.as_ref().unwrap().caret, 11);
        key_edit(&mut inner, "Left", alt);
        assert_eq!(inner.focus.as_ref().unwrap().caret, 7);
        key_edit(&mut inner, "Left", meta);
        assert_eq!(inner.focus.as_ref().unwrap().caret, 0);
        key_edit(&mut inner, "Right", meta);
        assert_eq!(inner.focus.as_ref().unwrap().caret, 16);

        // shift keeps the far end where it was; a plain arrow lets the selection go
        key_edit(&mut inner, "Left", Mods { alt: true, shift: true, ..Mods::NONE });
        assert_eq!(inner.focus.as_ref().unwrap().range(), Some((11, 16)));
        key_edit(&mut inner, "Right", Mods::NONE);
        assert_eq!(inner.focus.as_ref().unwrap().range(), None);
        assert_eq!(inner.focus.as_ref().unwrap().caret, 16);

        // ⌘A takes the lot, and typing over a selection replaces it
        key_edit(&mut inner, "A", meta);
        assert_eq!(inner.focus.as_ref().unwrap().range(), Some((0, 16)));
        let e = key_text(&mut inner, "x").unwrap();
        assert_eq!(e.text, "x");

        // ⌥⌫ takes the word before the caret
        key_text(&mut inner, " a word").unwrap();
        let e = key_edit_event(&mut inner, "Backspace", alt);
        assert_eq!(e.unwrap().text, "x a ");

        // a chord the field has no use for is left for the window to deal with
        assert!(matches!(key_edit(&mut inner, "R", meta), Edit::Ignored));
        // and up and down belong to the application while the field has one line
        assert!(matches!(key_edit(&mut inner, "Up", Mods::NONE), Edit::Ignored));
        // shift+arrow does not reach an unfocused field at all
        key_edit(&mut inner, "Escape", Mods::NONE);
        assert!(matches!(key_edit(&mut inner, "Left", shift), Edit::Ignored));
    }

    fn key_edit_event(inner: &mut Inner, name: &str, mods: Mods) -> Option<Event> {
        match key_edit(inner, name, mods) {
            Edit::Took(event) => event,
            Edit::Ignored => None,
        }
    }

    /// The caret holds solid while you type and fades afterwards, and the loop is told when
    /// it next has to draw rather than being kept awake for it.
    #[test]
    fn the_caret_blinks_on_its_own_clock() {
        use crate::tree::{caret_alpha, CARET_PERIOD, CARET_SOLID};
        assert_eq!(caret_alpha(0.0), 1.0, "solid the moment it moves");
        assert_eq!(caret_alpha(CARET_SOLID - 0.01), 1.0);
        // half a period after the solid stretch it is out, and a period later back on
        assert_eq!(caret_alpha(CARET_SOLID + CARET_PERIOD * 0.7), 0.0);
        assert_eq!(caret_alpha(CARET_SOLID + CARET_PERIOD * 1.0), 1.0);
        // and it passes through the middle rather than snapping: the fade runs into the
        // half-way point, so half a fade before it the caret is neither on nor off
        use crate::tree::CARET_FADE;
        let edge = caret_alpha(CARET_SOLID + CARET_PERIOD * 0.5 - CARET_FADE * 0.5);
        assert!(edge > 0.0 && edge < 1.0, "it fades: {edge}");

        let mut inner = build();
        assert!(inner.next_frame_in().is_none(), "nothing to draw with no field focused");
        let col = inner.layout_children(inner.root)[0];
        let kids = inner.layout_children(col);
        let fr = inner.nodes[kids[2]].abs;
        pointer(&mut inner, EV_POINTER_DOWN, fr.x + fr.w - 2.0, fr.y + 5.0);
        let wait = inner.next_frame_in().expect("a focused field blinks");
        assert!(wait.as_secs_f32() > 0.05, "and it waits for it rather than spinning: {wait:?}");
    }

    #[test]
    fn focus_and_editing() {
        let mut inner = build();
        let col = inner.layout_children(inner.root)[0];
        let kids = inner.layout_children(col);
        let fr = inner.nodes[kids[2]].abs;
        assert!(key_text(&mut inner, "x").is_some_and(|e| e.handler == -1 && e.text == "x"));
        // a press near the left edge lands before the first letter, which is where the caret
        // goes: a field takes the caret from the pointer, not from the end of its text
        let down = pointer(&mut inner, EV_POINTER_DOWN, fr.x + 1.0, fr.y + 5.0);
        assert_eq!(down.handler, -1);
        assert_eq!(focused_handler(&inner), 3);
        assert_eq!(inner.focus.as_ref().unwrap().caret, 0);
        // and one past the end of the text lands after it
        pointer(&mut inner, EV_POINTER_DOWN, fr.x + fr.w - 2.0, fr.y + 5.0);
        assert_eq!(inner.focus.as_ref().unwrap().caret, 2);
        let e = key_text(&mut inner, "!").unwrap();
        assert_eq!((e.kind, e.handler, e.text.as_str()), (4, 3, "hi!"));
        key_named(&mut inner, "Home");
        let e = key_text(&mut inner, "ab").unwrap();
        assert_eq!(e.text, "abhi!");
        key_named(&mut inner, "Right"); // caret after "abh"
        let e = key_named(&mut inner, "Backspace").unwrap();
        assert_eq!(e.text, "abi!");
        let e = key_named(&mut inner, "Delete").unwrap();
        assert_eq!(e.text, "ab!");
        assert_eq!(key_named(&mut inner, "Delete").unwrap().text, "ab");
        assert!(key_named(&mut inner, "Delete").is_none());
        assert!(key_named(&mut inner, "Right").is_none());
        // python confirms a (transformed) value on commit: the buffer follows python
        commit(&mut inner, &fragment(), &["ok", "typed"], &[(0, 0, 7)], &[], &[]).unwrap();
        assert_eq!(inner.focus.as_ref().unwrap().buffer, "typed");
        assert_eq!(inner.focus.as_ref().unwrap().caret, 2);
        // replacing the field's container disposes it and clears focus
        let ints: Vec<i32> = [[8, 0, 0, 0, 3, 0, -1, 0]].iter().flatten().copied().collect();
        commit(&mut inner, &ints, &["typed"], &[(0, 0, 1)], &[], &[]).unwrap();
        assert!(inner.focus.is_none());
        layout_tree(&mut inner, &mut NoHost::default());
        let fr = inner.nodes[inner.layout_children(inner.root)[0]].abs;
        pointer(&mut inner, EV_POINTER_DOWN, fr.x + 5.0, fr.y + 5.0);
        assert_eq!(focused_handler(&inner), 3);
        // clicking elsewhere blurs
        pointer(&mut inner, EV_POINTER_DOWN, 390.0, 290.0);
        assert!(inner.focus.is_none());
        assert_eq!(key_named(&mut inner, "Enter").unwrap().text, "\n");
    }
}
