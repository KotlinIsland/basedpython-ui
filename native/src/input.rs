//! Hit testing, focus, the text field edit buffer, and the event tuples of the protocol.

use crate::tree::{Focus, Inner, NodeId};
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

/// A pointer event (kinds 1, 2, 3, or 9 / 10 for the right button). Pointer down moves focus
/// to the text field hit, or clears it; every kind records what is under the pointer, for the
/// hover paint and for the enter / leave events `take_hover_events` hands back.
pub fn pointer(inner: &mut Inner, kind: i32, x: f32, y: f32) -> Event {
    let secondary = kind == EV_SECONDARY_DOWN || kind == EV_SECONDARY_UP;
    // a drag owns the pointer while it lasts, so the press that started it, the moves and
    // the release are the drag's rather than anything else's
    let dragging = !secondary && update_drag(inner, kind, x, y);
    let hit = hit_for(inner, x, y, if secondary { Which::Secondary } else { Which::Primary });
    if kind == EV_POINTER_DOWN && !dragging {
        match hit.focus {
            Some(field) => focus_field(inner, field),
            None => inner.focus = None,
        }
    }
    if !secondary {
        inner.hover_node = hit.node;
    }
    update_hover_target(inner, x, y);
    Event::new(kind, x, y, if dragging { -1 } else { hit.handler }, String::new())
}

/// Start, continue or end a drag. A press over a `draggable` layer takes the pointer: until
/// it comes up every move belongs to that node, wherever it has moved to — which is what
/// lets a splitter follow the pointer past its own few pixels.
fn update_drag(inner: &mut Inner, kind: i32, x: f32, y: f32) -> bool {
    match kind {
        EV_POINTER_DOWN => {
            let hit = hit_for(inner, x, y, Which::Drag);
            match (hit.node, hit.handler >= 0) {
                (Some(node), true) => {
                    inner.drag_node = Some(node);
                    inner.pending_events.push(Event::new(EV_DRAG, x, y, hit.handler, "start".to_string()));
                    true
                }
                _ => false,
            }
        }
        EV_POINTER_MOVE => match dragging_handler(inner) {
            Some(handler) => {
                inner.pending_events.push(Event::new(EV_DRAG, x, y, handler, "move".to_string()));
                true
            }
            None => false,
        },
        EV_POINTER_UP => {
            let handler = dragging_handler(inner);
            let dragging = inner.drag_node.is_some();
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
    let target = hit_for(inner, x, y, Which::Hover).node;
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
    if !dy.is_finite() || dy == 0.0 {
        return false;
    }
    let root = inner.root;
    let Some(target) = innermost_scrollable(inner, root, x, y, dy) else { return false };
    let node = &mut inner.nodes[target];
    let new = (node.scroll + dy).clamp(0.0, node.scroll_limit());
    if new == node.scroll {
        return false;
    }
    node.scroll = new;
    crate::layout::assign_abs(inner);
    inner.hover_node = hit_test(inner, x, y).node;
    update_hover_target(inner, x, y);
    true
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
    inner.focus = Some(Focus { node: field, buffer, caret });
    inner.mark_dirty(field);
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
    let at = byte_index(&f.buffer, f.caret);
    f.buffer.insert_str(at, &insert);
    f.caret += insert.chars().count();
    edited(inner)
}

/// Editing keys. Names: Backspace, Delete, Left, Right, Home, End, Escape, Tab, Enter.
pub fn key_named(inner: &mut Inner, name: &str) -> Option<Event> {
    if inner.focus.is_none() {
        let text = match name {
            "Enter" => "\n",
            "Tab" => "\t",
            "Escape" => "\x1b",
            "Backspace" => "\x08",
            "Delete" => "\x7f",
            _ => return None,
        };
        return Some(Event::new(EV_KEY_TEXT, 0.0, 0.0, -1, text.to_string()));
    }
    match name {
        "Backspace" => {
            let f = inner.focus.as_mut()?;
            if f.caret == 0 {
                return None;
            }
            let start = byte_index(&f.buffer, f.caret - 1);
            let end = byte_index(&f.buffer, f.caret);
            f.buffer.replace_range(start..end, "");
            f.caret -= 1;
            edited(inner)
        }
        "Delete" => {
            let f = inner.focus.as_mut()?;
            let len = f.buffer.chars().count();
            if f.caret >= len {
                return None;
            }
            let start = byte_index(&f.buffer, f.caret);
            let end = byte_index(&f.buffer, f.caret + 1);
            f.buffer.replace_range(start..end, "");
            edited(inner)
        }
        "Left" => {
            let f = inner.focus.as_mut()?;
            f.caret = f.caret.saturating_sub(1);
            None
        }
        "Right" => {
            let f = inner.focus.as_mut()?;
            f.caret = (f.caret + 1).min(f.buffer.chars().count());
            None
        }
        "Home" => {
            inner.focus.as_mut()?.caret = 0;
            None
        }
        "End" => {
            let f = inner.focus.as_mut()?;
            f.caret = f.buffer.chars().count();
            None
        }
        "Escape" => {
            if let Some(f) = inner.focus.take() {
                inner.mark_dirty(f.node);
            }
            None
        }
        "Tab" => {
            focus_next_field(inner);
            None
        }
        _ => None,
    }
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

        // pressing the strip starts a drag rather than a click
        let down = pointer(&mut inner, EV_POINTER_DOWN, 10.0, 100.0);
        assert_eq!(down.handler, -1, "the press belongs to the drag, not to a click");
        let events = take_events(&mut inner);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].kind, events[0].handler, events[0].text.as_str()), (EV_DRAG, 7, "start"));

        // the pointer moves far away, over the other strip: the drag still owns it
        pointer(&mut inner, EV_POINTER_MOVE, 150.0, 40.0);
        let events = take_events(&mut inner);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].handler, events[0].text.as_str(), events[0].x), (7, "move", 150.0));

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
    fn chords_and_modifier_text() {
        assert_eq!(chord(false, false, false, true, "x"), "cmd+x");
        assert_eq!(chord(true, true, true, true, "up"), "ctrl+alt+shift+cmd+up");
        assert_eq!(chord(false, false, false, false, "enter"), "enter");
        assert_eq!(modifier_text(false, false, true, true), "shift+cmd");
        assert_eq!(modifier_text(false, false, false, false), "");
    }

    #[test]
    fn focus_and_editing() {
        let mut inner = build();
        let col = inner.layout_children(inner.root)[0];
        let kids = inner.layout_children(col);
        let fr = inner.nodes[kids[2]].abs;
        assert!(key_text(&mut inner, "x").is_some_and(|e| e.handler == -1 && e.text == "x"));
        let down = pointer(&mut inner, EV_POINTER_DOWN, fr.x + 5.0, fr.y + 5.0);
        assert_eq!(down.handler, -1);
        assert_eq!(focused_handler(&inner), 3);
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
