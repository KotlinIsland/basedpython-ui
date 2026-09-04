//! Hit testing, focus, the text field edit buffer, and the event tuples of the protocol.

use crate::tree::{Focus, Inner, NodeId};
use crate::types::*;

pub const EV_POINTER_DOWN: i32 = 1;
pub const EV_POINTER_UP: i32 = 2;
pub const EV_POINTER_MOVE: i32 = 3;
pub const EV_KEY_TEXT: i32 = 4;
pub const EV_RESIZE: i32 = 5;
pub const EV_CLOSE: i32 = 6;

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
    let root = inner.root;
    hit_children(inner, root, x, y).unwrap_or(Hit::NONE)
}

fn hit_children(inner: &Inner, id: NodeId, x: f32, y: f32) -> Option<Hit> {
    let node = inner.nodes.get(id)?;
    for &c in node.children.iter().rev() {
        let Some(child) = inner.nodes.get(c) else { continue };
        if child.is_group() {
            if let Some(h) = hit_children(inner, c, x, y) {
                return Some(h);
            }
            continue;
        }
        if !child.abs.contains(x, y) {
            continue;
        }
        if let Some(h) = hit_children(inner, c, x, y) {
            return Some(h);
        }
        let content = Rect::new(
            child.abs.x + child.content_origin.0,
            child.abs.y + child.content_origin.1,
            child.content_size.w,
            child.content_size.h,
        );
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
        let lx = x - child.abs.x;
        let ly = y - child.abs.y;
        for layer in child.layers.iter().rev() {
            if let Layer::Click { rect, handler } = layer {
                if rect.contains(lx, ly) {
                    return Some(Hit { handler: *handler, focus: None, node: Some(c) });
                }
            }
        }
    }
    None
}

/// A pointer event (kinds 1, 2, 3). Pointer down moves focus to the text field hit, or clears it.
pub fn pointer(inner: &mut Inner, kind: i32, x: f32, y: f32) -> Event {
    let hit = hit_test(inner, x, y);
    if kind == EV_POINTER_DOWN {
        match hit.focus {
            Some(field) => focus_field(inner, field),
            None => inner.focus = None,
        }
    }
    Event::new(kind, x, y, hit.handler, String::new())
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
