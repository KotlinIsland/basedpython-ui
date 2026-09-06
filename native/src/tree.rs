//! The retained element / render tree: one arena of nodes, scope groups (transparent for
//! layout), the interned modifier and style tables, focus, and the paint target.

use std::collections::HashMap;
use std::sync::Arc;

use slotmap::SlotMap;

use crate::text::{TextKey, TextSystem};
use crate::types::*;

slotmap::new_key_type! { pub struct NodeId; }

impl NodeId {
    /// An opaque integer for Python (`node_id` in `find_text` / `node_rect` / `canvas_requests`).
    pub fn to_ffi(self) -> u64 {
        slotmap::Key::data(&self).as_ffi()
    }
    pub fn from_ffi(v: u64) -> NodeId {
        NodeId::from(slotmap::KeyData::from_ffi(v))
    }
}

/// An element with its render state. Scope groups are nodes too (kind `Scope`).
pub struct Node {
    pub kind: Kind,
    pub key: Key,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Scope id for `Kind::Scope` groups (0 = root), unused otherwise.
    pub scope_id: i32,
    /// TEXT text, BUTTON label, TEXTFIELD value.
    pub text: Option<Arc<str>>,
    /// TEXTFIELD placeholder.
    pub placeholder: Option<Arc<str>>,
    pub modifier_id: u32,
    pub modifier: Arc<Modifier>,
    pub handler: i32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    /// Style for TEXT (`a`) / BUTTON (`b`), resolved at commit.
    pub style: Style,
    pub canvas: Option<Box<Vec<CanvasCmd>>>,
    pub canvas_pending: bool,
    /// SCROLL: the current offset of the content (logical pixels, >= 0) and its full length.
    pub scroll: f32,
    pub content_len: f32,
    /// Commit serial of the last reconciliation that reused this node (see `commit.rs`).
    pub mark: u32,
    /// Commit serial of the reconciliation frame currently running on this container.
    pub frame_serial: u32,

    // ---- render state (owned by layout / paint) ----
    /// Needs (re)layout. Invariant: dirty(child) implies dirty(parent).
    pub dirty: bool,
    /// The subtree contains a LAYOUT node, so it is never served from the size cache.
    pub volatile: bool,
    pub cache: Option<(Constraints, Size)>,
    pub size: Size,
    /// Offset of the node origin relative to the origin of the enclosing *layout* container
    /// (scope groups are transparent).
    pub offset: (f32, f32),
    /// Absolute rect in logical pixels, assigned after layout.
    pub abs: Rect,
    /// Origin and size of the innermost modifier layer (the content), relative to the node origin.
    pub content_origin: (f32, f32),
    pub content_size: Size,
    /// Modifier layers, outermost first.
    pub layers: Vec<Layer>,
    /// The text measurement used by the last layout (paint looks it up in the text cache).
    pub text_key: Option<TextKey>,
}

impl Node {
    pub fn new(kind: Kind, modifier: Arc<Modifier>) -> Node {
        Node {
            kind,
            key: Key::None,
            parent: None,
            children: Vec::new(),
            scope_id: -1,
            text: None,
            placeholder: None,
            modifier_id: 0,
            modifier,
            handler: -1,
            a: 0,
            b: 0,
            c: 0,
            style: Style::DEFAULT,
            canvas: None,
            canvas_pending: false,
            scroll: 0.0,
            content_len: 0.0,
            mark: 0,
            frame_serial: 0,
            dirty: true,
            volatile: false,
            cache: None,
            size: Size::ZERO,
            offset: (0.0, 0.0),
            abs: Rect::default(),
            content_origin: (0.0, 0.0),
            content_size: Size::ZERO,
            layers: Vec::new(),
            text_key: None,
        }
    }

    pub fn is_group(&self) -> bool {
        self.kind == Kind::Scope
    }

    pub fn text_str(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }

    /// The content rect (inside the modifier layers) in absolute coordinates; valid after layout.
    pub fn content_rect(&self) -> Rect {
        Rect::new(self.abs.x + self.content_origin.0, self.abs.y + self.content_origin.1, self.content_size.w, self.content_size.h)
    }

    /// How far the content of a SCROLL node can be scrolled.
    pub fn scroll_limit(&self) -> f32 {
        (self.content_len - self.content_size.h).max(0.0)
    }
}

/// The text field the core currently edits.
#[derive(Clone, Debug)]
pub struct Focus {
    pub node: NodeId,
    pub buffer: String,
    /// Caret as a char index into `buffer`.
    pub caret: usize,
}

/// Everything the core retains. Owned by the Python `Core` object (behind a mutex).
pub struct Inner {
    pub nodes: SlotMap<NodeId, Node>,
    pub root: NodeId,
    pub scopes: HashMap<i32, NodeId>,
    pub modifiers: HashMap<u32, Arc<Modifier>>,
    pub empty_modifier: Arc<Modifier>,
    /// Shared `""` so empty texts never allocate a key.
    pub empty_text: Arc<str>,
    pub styles: HashMap<u32, Style>,
    pub text: TextSystem,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub pixmap: Option<tiny_skia::Pixmap>,
    pub focus: Option<Focus>,
    /// Canvas nodes whose draw block must run again (flag `canvas_pending` on the node too).
    pub canvas_pending: Vec<NodeId>,
    /// Nodes whose modifier gained `reveal` this commit: layout scrolls them into view.
    pub reveal_pending: Vec<NodeId>,
    /// Handler index under the pointer as of the last pointer event, or -1 (paint reads it).
    pub hover_handler: i32,
    /// commit, layout, paint durations of the last calls, in milliseconds.
    pub timings: [f64; 3],
    pub commit_serial: u32,
    /// Bumped by every commit; text cache entries older than this may be swept.
    pub layout_epoch: u64,
}

impl Inner {
    pub fn new(width: f32, height: f32, scale: f32, text: TextSystem) -> Inner {
        let empty_modifier = Arc::new(Modifier::default());
        let mut nodes = SlotMap::with_key();
        let mut root_node = Node::new(Kind::Scope, empty_modifier.clone());
        root_node.scope_id = 0;
        let root = nodes.insert(root_node);
        let mut scopes = HashMap::new();
        scopes.insert(0, root);
        let mut modifiers = HashMap::new();
        modifiers.insert(0, empty_modifier.clone());
        let mut styles = HashMap::new();
        styles.insert(0, Style::DEFAULT);
        let mut inner = Inner {
            nodes,
            root,
            scopes,
            modifiers,
            empty_modifier,
            empty_text: Arc::from(""),
            styles,
            text,
            width: 0.0,
            height: 0.0,
            scale: 1.0,
            pixmap: None,
            focus: None,
            canvas_pending: Vec::new(),
            reveal_pending: Vec::new(),
            hover_handler: -1,
            timings: [0.0; 3],
            commit_serial: 0,
            layout_epoch: 0,
        };
        inner.resize(width, height, scale);
        inner
    }

    /// A cheap, empty stand-in used while the real state is handed to a Python callback.
    pub fn placeholder() -> Inner {
        Inner::new(0.0, 0.0, 1.0, TextSystem::placeholder())
    }

    pub fn resize(&mut self, width: f32, height: f32, scale: f32) {
        let width = if width.is_finite() { width.max(0.0) } else { 0.0 };
        let height = if height.is_finite() { height.max(0.0) } else { 0.0 };
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        if self.width != width || self.height != height || self.scale != scale {
            self.width = width;
            self.height = height;
            self.scale = scale;
            self.pixmap = None;
            self.mark_dirty(self.root);
        }
    }

    /// Physical size of the paint target.
    pub fn pixel_size(&self) -> (u32, u32) {
        let pw = (self.width * self.scale).round().max(0.0) as u32;
        let ph = (self.height * self.scale).round().max(0.0) as u32;
        (pw, ph)
    }

    /// Mark a node and its ancestors dirty, stopping at the first already-dirty ancestor
    /// (the invariant dirty(child) ⇒ dirty(parent) among elements makes that sufficient).
    /// Scope groups are transparent: layout never visits them, so their flag is never cleared
    /// and must not stop the walk. The root group is the one group whose flag layout reads.
    pub fn mark_dirty(&mut self, mut id: NodeId) {
        let root = self.root;
        loop {
            let Some(node) = self.nodes.get_mut(id) else { return };
            if node.is_group() && id != root {
                match node.parent {
                    Some(p) => {
                        id = p;
                        continue;
                    }
                    None => return,
                }
            }
            let was_dirty = node.dirty;
            node.dirty = true;
            node.cache = None;
            if was_dirty {
                return;
            }
            match node.parent {
                Some(p) => id = p,
                None => return,
            }
        }
    }

    /// Element count, excluding scope groups.
    pub fn node_count(&self) -> usize {
        self.nodes.values().filter(|n| !n.is_group()).count()
    }

    /// Children as layout sees them: scope groups flattened in place.
    pub fn layout_children(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.collect_layout_children(id, &mut out);
        out
    }

    fn collect_layout_children(&self, id: NodeId, out: &mut Vec<NodeId>) {
        let Some(node) = self.nodes.get(id) else { return };
        for &c in &node.children {
            match self.nodes.get(c) {
                Some(child) if child.is_group() => self.collect_layout_children(c, out),
                Some(_) => out.push(c),
                None => {}
            }
        }
    }

    /// Whether `node` is `ancestor` or one of its descendants.
    pub fn is_descendant_or_self(&self, mut node: NodeId, ancestor: NodeId) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.nodes.get(node).and_then(|n| n.parent) {
                Some(p) => node = p,
                None => return false,
            }
        }
    }

    /// Remove a subtree. Returns the scope ids of the groups it contained. Children whose
    /// parent pointer no longer points at the removed node were moved elsewhere and are kept.
    pub fn dispose(&mut self, id: NodeId, disposed_scopes: &mut Vec<i32>) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let Some(node) = self.nodes.remove(n) else { continue };
            if node.is_group() {
                disposed_scopes.push(node.scope_id);
                if self.scopes.get(&node.scope_id) == Some(&n) {
                    self.scopes.remove(&node.scope_id);
                }
            }
            if self.focus.as_ref().map(|f| f.node) == Some(n) {
                self.focus = None;
            }
            if node.canvas_pending {
                self.canvas_pending.retain(|&c| c != n);
            }
            self.reveal_pending.retain(|&c| c != n);
            for c in node.children {
                if self.nodes.get(c).map(|cn| cn.parent) == Some(Some(n)) {
                    stack.push(c);
                }
            }
        }
    }

    pub fn node_rect(&self, id: NodeId) -> Option<Rect> {
        let node = self.nodes.get(id)?;
        if node.is_group() {
            return None;
        }
        Some(node.abs)
    }

    /// First node in tree order whose text (TEXT, BUTTON label, TEXTFIELD value) equals `text`;
    /// then the first TEXTFIELD whose placeholder equals it.
    pub fn find_text(&self, text: &str) -> Option<(NodeId, Rect)> {
        let mut placeholder_hit = None;
        let mut stack = vec![self.root];
        let mut order = Vec::new();
        // iterative pre-order
        while let Some(n) = stack.pop() {
            order.push(n);
            if let Some(node) = self.nodes.get(n) {
                for &c in node.children.iter().rev() {
                    stack.push(c);
                }
            }
        }
        for n in order {
            let Some(node) = self.nodes.get(n) else { continue };
            match node.kind {
                Kind::Text | Kind::Button | Kind::TextField => {
                    if node.text_str() == text {
                        return Some((n, node.abs));
                    }
                    if node.kind == Kind::TextField
                        && placeholder_hit.is_none()
                        && node.placeholder.as_deref() == Some(text)
                    {
                        placeholder_hit = Some((n, node.abs));
                    }
                }
                _ => {}
            }
        }
        placeholder_hit
    }

    /// Nodes in pre-order.
    fn preorder(&self) -> Vec<NodeId> {
        let mut stack = vec![self.root];
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(n) = stack.pop() {
            order.push(n);
            if let Some(node) = self.nodes.get(n) {
                for &c in node.children.iter().rev() {
                    stack.push(c);
                }
            }
        }
        order
    }

    /// Every visible text in tree order: TEXT texts, BUTTON labels, TEXTFIELD values (or the
    /// placeholder when the value is empty).
    pub fn all_text(&self) -> Vec<String> {
        let mut out = Vec::new();
        for n in self.preorder() {
            let Some(node) = self.nodes.get(n) else { continue };
            match node.kind {
                Kind::Text | Kind::Button => out.push(node.text_str().to_string()),
                Kind::TextField => {
                    let value = node.text_str();
                    if value.is_empty() {
                        if let Some(p) = &node.placeholder {
                            out.push(p.to_string());
                        }
                    } else {
                        out.push(value.to_string());
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Handler index of the first TEXTFIELD whose placeholder equals `placeholder`, or -1.
    pub fn field_handler(&self, placeholder: &str) -> i32 {
        for n in self.preorder() {
            if let Some(node) = self.nodes.get(n) {
                if node.kind == Kind::TextField && node.placeholder.as_deref() == Some(placeholder) {
                    return node.handler;
                }
            }
        }
        -1
    }

    /// The indented tree for debugging and tests.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_node(self.root, 0, &mut out);
        out
    }

    fn dump_node(&self, id: NodeId, depth: usize, out: &mut String) {
        use std::fmt::Write;
        let Some(node) = self.nodes.get(id) else { return };
        for _ in 0..depth {
            out.push_str("  ");
        }
        if node.is_group() {
            if node.scope_id == 0 {
                let _ = writeln!(out, "Root [{}x{}]", fmt(self.width), fmt(self.height));
            } else {
                let _ = writeln!(out, "Scope {}", node.scope_id);
            }
        } else {
            let _ = write!(out, "{}", node.kind.name());
            if let Some(t) = &node.text {
                let _ = write!(out, " {:?}", &**t);
            }
            if let Key::Int(k) = node.key {
                let _ = write!(out, " key={}", k);
            } else if let Key::Str(k) = &node.key {
                let _ = write!(out, " key={:?}", &**k);
            }
            let r = node.abs;
            let _ = write!(out, " [{},{} {}x{}]", fmt(r.x), fmt(r.y), fmt(r.w), fmt(r.h));
            if node.modifier_id != 0 {
                let _ = write!(out, " mod={}", node.modifier_id);
            }
            if node.handler >= 0 {
                let _ = write!(out, " handler={}", node.handler);
            }
            if node.modifier.clip {
                out.push_str(" clip");
            }
            match node.kind {
                Kind::Column | Kind::Row => {
                    let _ = write!(out, " arrangement={}", node.a);
                }
                Kind::Box => {
                    let _ = write!(out, " alignment={}", node.a);
                }
                Kind::Scroll => {
                    let _ = write!(out, " scroll={} content={}", fmt(node.scroll), fmt(node.content_len));
                }
                Kind::Button => {
                    if node.a == 0 {
                        out.push_str(" disabled");
                    }
                }
                Kind::Checkbox => {
                    let _ = write!(out, " checked={}", node.a != 0);
                }
                Kind::TextField => {
                    if let Some(p) = &node.placeholder {
                        let _ = write!(out, " placeholder={:?}", &**p);
                    }
                    if self.focus.as_ref().map(|f| f.node) == Some(id) {
                        out.push_str(" focused");
                    }
                }
                _ => {}
            }
            out.push('\n');
        }
        for &c in &node.children {
            self.dump_node(c, depth + 1, out);
        }
    }
}

fn fmt(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{:.1}", v)
    }
}
