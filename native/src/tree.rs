//! The retained element / render tree: one arena of nodes, scope groups (transparent for
//! layout), the interned modifier and style tables, focus, and the paint target.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slotmap::SlotMap;

use crate::input::Event;
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
    /// SCROLL, sideways: the same pair for a container that also pans horizontally, which is
    /// what gives a line wider than the viewport somewhere to go.
    pub scroll_x: f32,
    pub content_width: f32,
    /// POPUP: where its content sits in the window, after clamping it into view.
    pub popup_at: (f32, f32),
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
            scroll_x: 0.0,
            content_width: 0.0,
            content_len: 0.0,
            popup_at: (0.0, 0.0),
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

    /// The handler of this node's outermost layer of a kind, as of the last layout.
    pub fn layer_handler(&self, which: crate::input::Which) -> i32 {
        use crate::input::Which;
        for layer in self.layers.iter().rev() {
            match (which, layer) {
                (Which::Hover, Layer::Hoverable { handler, .. }) => return *handler,
                (Which::Primary, Layer::Click { handler, .. }) => return *handler,
                (Which::Secondary, Layer::Secondary { handler, .. }) => return *handler,
                (Which::Drag, Layer::Drag { handler, .. }) => return *handler,
                _ => {}
            }
        }
        -1
    }

    /// The content rect (inside the modifier layers) in absolute coordinates; valid after layout.
    pub fn content_rect(&self) -> Rect {
        Rect::new(self.abs.x + self.content_origin.0, self.abs.y + self.content_origin.1, self.content_size.w, self.content_size.h)
    }

    /// How far the content of a SCROLL node can be scrolled.
    pub fn scroll_limit(&self) -> f32 {
        (self.content_len - self.content_size.h).max(0.0)
    }

    /// The same sideways, for a container laid out at its content's natural width.
    pub fn scroll_limit_x(&self) -> f32 {
        (self.content_width - self.content_size.w).max(0.0)
    }
}

/// One end of a text selection: a text node and a byte index into its text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub node: NodeId,
    pub index: usize,
}

/// A selection dragged out with the pointer. `anchor` is where the press landed and `focus`
/// where the pointer is now, so either may come first in the tree.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub anchor: Caret,
    pub focus: Caret,
    /// The `selectable` node the selection belongs to; a selection never leaves it.
    pub root: NodeId,
    pub argb: u32,
}

/// The text field the core currently edits.
#[derive(Clone, Debug)]
pub struct Focus {
    pub node: NodeId,
    pub buffer: String,
    /// Caret as a char index into `buffer`.
    pub caret: usize,
    /// The other end of the selection, when the caret has been shifted or dragged away from
    /// somewhere. `None` is a plain caret with nothing selected.
    pub anchor: Option<usize>,
    /// What the field has scrolled by to keep the caret in sight, and what it is easing
    /// toward. Text longer than the box would otherwise be typed off the end of it.
    pub scroll: (f32, f32),
    pub scroll_to: (f32, f32),
    /// Where the caret is drawn. It slides to where it belongs rather than jumping, so a
    /// long move reads as a move; `None` until the first paint places it.
    pub drawn: Option<(f32, f32)>,
    /// When the caret last moved. The blink holds solid for a moment afterwards, so it is
    /// never invisible in the middle of typing.
    pub moved_at: Instant,
}

impl Focus {
    pub fn new(node: NodeId, buffer: String, caret: usize) -> Focus {
        Focus {
            node,
            buffer,
            caret,
            anchor: None,
            scroll: (0.0, 0.0),
            scroll_to: (0.0, 0.0),
            drawn: None,
            moved_at: Instant::now(),
        }
    }

    /// The selection in reading order, when there is one and it is not empty.
    pub fn range(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        let (a, b) = (anchor.min(self.caret), anchor.max(self.caret));
        if a == b { None } else { Some((a, b)) }
    }

    /// The caret moved: the blink restarts solid, so typing never blinks out under you.
    pub fn touch(&mut self) {
        self.moved_at = Instant::now();
    }
}

/// How long one blink takes, and how much of it the caret spends fading in or out. A caret
/// that snaps between on and off flickers; one that fades reads as a pulse.
pub const CARET_PERIOD: f32 = 1.06;
pub const CARET_FADE: f32 = 0.16;
/// How long the caret stays solid after it moves.
pub const CARET_SOLID: f32 = 0.5;

/// How opaque the caret is, `seconds` after it last moved.
pub fn caret_alpha(seconds: f32) -> f32 {
    if seconds < CARET_SOLID {
        return 1.0;
    }
    let phase = ((seconds - CARET_SOLID) % CARET_PERIOD) / CARET_PERIOD;
    let fade = CARET_FADE / CARET_PERIOD;
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    if phase < 0.5 - fade {
        1.0
    } else if phase < 0.5 {
        smooth((0.5 - phase) / fade)
    } else if phase < 1.0 - fade {
        0.0
    } else {
        smooth((phase - (1.0 - fade)) / fade)
    }
}

/// Move `current` a fraction of the way to `target`, framerate-independently: `rate` is how
/// many e-foldings a second, so the same motion comes out of a slow frame and a fast one.
pub fn approach(current: f32, target: f32, dt: f32, rate: f32) -> f32 {
    if (target - current).abs() < 0.2 {
        return target;
    }
    current + (target - current) * (1.0 - (-rate * dt).exp())
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
    /// Popups, in creation order: laid out against the window, painted last, hit first.
    pub popups: Vec<NodeId>,
    /// Nodes whose modifier gained `reveal` this commit: layout scrolls them into view.
    pub reveal_pending: Vec<NodeId>,
    /// What the pointer is over, as of the last pointer event. Nodes rather than handler
    /// indices: a handler index belongs to one composition, and the node outlives it, so a
    /// scope that recomposes while the pointer rests on it keeps its hover.
    pub hover_node: Option<NodeId>,
    pub hover_target_node: Option<NodeId>,
    /// The node a drag started on, while the pointer is still down: until it comes up, every
    /// move belongs to that node however far the pointer travels.
    pub drag_node: Option<NodeId>,
    /// What the pointer is currently holding down, so a press shows on the thing pressed.
    pub pressed_node: Option<NodeId>,
    /// The `drop_target` handler a drag is currently over, or -1.
    pub drop_node: i32,
    /// A press on a draggable layer that has not travelled far enough to be a drag yet.
    pub drag_armed: Option<(NodeId, f32, f32)>,
    /// The `focus_region` handler a press last landed in, or -1.
    pub focus_region: i32,
    /// The text the pointer has selected, and whether it is still being dragged out.
    pub selection: Option<Selection>,
    pub selecting: bool,
    /// The enter / leave and drag events not yet taken.
    pub pending_events: Vec<Event>,
    /// commit, layout, paint durations of the last calls, in milliseconds.
    pub timings: [f64; 3],
    pub commit_serial: u32,
    /// Bumped by every commit; text cache entries older than this may be swept.
    pub layout_epoch: u64,
    /// When the last paint ran, so an animation moves by elapsed time rather than by frame.
    pub painted_at: Instant,
    /// How long the last frame took, which is what an animation moves by.
    pub frame_dt: f32,
    /// Set by paint while the caret or a field's scroll is still on its way.
    pub settling: bool,
}

impl Inner {
    /// How long until the next frame would look different: a caret easing into place or a
    /// field still scrolling wants one straight away; a caret sitting still wants one only
    /// when its blink is next about to change. `None` when nothing is moving at all, which
    /// is what lets the event loop go back to sleep.
    pub fn next_frame_in(&self) -> Option<Duration> {
        let f = self.focus.as_ref()?;
        if self.settling {
            return Some(Duration::from_millis(8));
        }
        let seconds = f.moved_at.elapsed().as_secs_f32();
        if seconds < CARET_SOLID {
            return Some(Duration::from_secs_f32(CARET_SOLID - seconds));
        }
        // inside a fade every frame counts; on a plateau only the moment it ends does
        let phase = ((seconds - CARET_SOLID) % CARET_PERIOD) / CARET_PERIOD;
        let fade = CARET_FADE / CARET_PERIOD;
        let until = if phase < 0.5 - fade {
            0.5 - fade - phase
        } else if phase >= 0.5 && phase < 1.0 - fade {
            1.0 - fade - phase
        } else {
            return Some(Duration::from_millis(8));
        };
        Some(Duration::from_secs_f32(until * CARET_PERIOD))
    }

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
            popups: Vec::new(),
            reveal_pending: Vec::new(),
            hover_node: None,
            hover_target_node: None,
            drag_node: None,
            pressed_node: None,
            drop_node: -1,
            drag_armed: None,
            focus_region: -1,
            selection: None,
            selecting: false,
            pending_events: Vec::new(),
            timings: [0.0; 3],
            commit_serial: 0,
            layout_epoch: 0,
            painted_at: Instant::now(),
            frame_dt: 0.0,
            settling: false,
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

    /// Children as layout sees them: scope groups flattened in place, popups left out —
    /// they are laid out against the window rather than by the container they sit in.
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
                Some(child) if child.kind == Kind::Popup => {}
                Some(_) => out.push(c),
                None => {}
            }
        }
    }

    /// The popups still in the tree, in creation order.
    pub fn live_popups(&self) -> Vec<NodeId> {
        self.popups.iter().copied().filter(|&p| self.nodes.contains_key(p)).collect()
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
            if self.hover_node == Some(n) {
                self.hover_node = None;
            }
            if self.hover_target_node == Some(n) {
                self.hover_target_node = None;
            }
            if self.drag_node == Some(n) {
                self.drag_node = None;
            }
            if self.selection.map(|s| s.anchor.node == n || s.focus.node == n || s.root == n) == Some(true) {
                self.selection = None;
                self.selecting = false;
            }
            self.reveal_pending.retain(|&c| c != n);
            if node.kind == Kind::Popup {
                self.popups.retain(|&c| c != n);
            }
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

    /// The text nodes under `id`, in the order they read.
    pub fn text_nodes(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        let mut order = Vec::new();
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.get(n) {
                // a subtree left out of a sweep takes its children with it
                if node.modifier.unselectable && n != id {
                    continue;
                }
                order.push(n);
                for &c in node.children.iter().rev() {
                    stack.push(c);
                }
            } else {
                order.push(n);
            }
        }
        for n in order {
            if self.nodes.get(n).map(|node| node.kind == Kind::Text) == Some(true) {
                out.push(n);
            }
        }
        out
    }

    /// Nodes in pre-order.
    /// Every node, including the ones inside popups: what a whole-tree sweep walks.
    pub fn all_nodes(&self) -> Vec<NodeId> {
        self.preorder()
    }

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
                    if node.c != 0 {
                        let _ = write!(out, " alignment={}", node.c);
                    }
                }
                Kind::Box => {
                    let _ = write!(out, " alignment={}", node.a);
                }
                Kind::Scroll => {
                    let _ = write!(out, " scroll={} content={}", fmt(node.scroll), fmt(node.content_len));
                }
                Kind::Popup => {
                    let _ = write!(out, " at={},{}", fmt(node.popup_at.0), fmt(node.popup_at.1));
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
