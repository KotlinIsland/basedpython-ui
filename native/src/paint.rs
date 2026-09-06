//! Painting: walk the tree into a tiny-skia pixmap (premultiplied RGBA8, physical pixels).
//!
//! Every node is painted inside a clip rect: the window at the root, narrowed by each SCROLL
//! container (to its content rect) and by each node carrying a `clip` modifier (to its own
//! rect). Plain rectangles are clipped arithmetically; paths (rounded corners, circles, lines)
//! go through a tiny-skia mask built once per distinct clip; glyphs are clipped per pixel.
//!
//! M1 repaints everything each frame. TODO(M6): damage rects / repaint boundaries with per
//! container display lists; TODO: a GPU rasteriser behind the same walk.

use tiny_skia::{Color, FillRule, Mask, Paint, Path, PathBuilder, Pixmap, Shader, Stroke, Transform};

use crate::text::TextKey;
use crate::tree::{Inner, NodeId};
use crate::types::*;

pub const WINDOW_BACKGROUND: u32 = 0xFFFF_FFFF;
const BUTTON_FILL: u32 = 0xFFE4_E4E7;
const BUTTON_FILL_HOVER: u32 = 0xFFD4_D4D8;
const BUTTON_FILL_DISABLED: u32 = 0xFFF4_F4F5;
const BUTTON_BORDER: u32 = 0xFFA1_A1AA;
const BUTTON_RADIUS: f32 = 6.0;
const TEXT_DISABLED: u32 = 0xFF9A_9A9A;
const FIELD_BORDER: u32 = 0xFFA1_A1AA;
const FIELD_BORDER_FOCUS: u32 = 0xFF3B_82F6;
const FIELD_RADIUS: f32 = 6.0;
const PLACEHOLDER: u32 = 0xFF9C_A3AF;
const ACCENT: u32 = 0xFF3B_82F6;
const SCROLLBAR: u32 = 0x5A00_0000;
const SCROLLBAR_WIDTH: f32 = 4.0;
const SCROLLBAR_INSET: f32 = 2.0;
const SCROLLBAR_MIN: f32 = 16.0;

/// The paint pass state: the target and a cache of clip masks (one per distinct clip rect).
struct Painter {
    pixmap: Pixmap,
    scale: f32,
    masks: Vec<(Rect, Mask)>,
}

impl Painter {
    fn full_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.pixmap.width() as f32 / self.scale, self.pixmap.height() as f32 / self.scale)
    }

    /// The mask for a clip rect that is smaller than the whole target; `None` means unclipped.
    fn mask_for(&mut self, clip: &Rect) -> Option<usize> {
        let full = self.full_rect();
        if clip.x <= full.x && clip.y <= full.y && clip.x + clip.w >= full.x + full.w && clip.y + clip.h >= full.y + full.h {
            return None;
        }
        if let Some(i) = self.masks.iter().position(|(r, _)| r == clip) {
            return Some(i);
        }
        let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height())?;
        if let Some(rect) = skia_rect(*clip, self.scale) {
            mask.fill_path(&PathBuilder::from_rect(rect), FillRule::Winding, false, Transform::identity());
        }
        self.masks.push((*clip, mask));
        Some(self.masks.len() - 1)
    }

    fn clip_px(&self, clip: &Rect) -> (i32, i32, i32, i32) {
        let s = self.scale;
        ((clip.x * s).floor() as i32, (clip.y * s).floor() as i32, ((clip.x + clip.w) * s).ceil() as i32, ((clip.y + clip.h) * s).ceil() as i32)
    }

    fn fill_path(&mut self, path: &Path, argb: u32, clip: &Rect) {
        if argb >> 24 == 0 {
            return;
        }
        let mask = self.mask_for(clip);
        let paint = solid(argb);
        match mask {
            Some(i) => {
                let (masks, pixmap) = (&self.masks, &mut self.pixmap);
                pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), Some(&masks[i].1));
            }
            None => self.pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None),
        }
    }

    fn stroke_path(&mut self, path: &Path, argb: u32, width: f32, clip: &Rect) {
        if argb >> 24 == 0 {
            return;
        }
        let mask = self.mask_for(clip);
        let paint = solid(argb);
        let stroke = Stroke { width: (width * self.scale).max(0.5), ..Stroke::default() };
        match mask {
            Some(i) => {
                let (masks, pixmap) = (&self.masks, &mut self.pixmap);
                pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), Some(&masks[i].1));
            }
            None => self.pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), None),
        }
    }

    /// A filled rectangle, square or rounded, inside `clip`.
    fn fill_rect(&mut self, r: Rect, argb: u32, radius: f32, clip: &Rect) {
        if argb >> 24 == 0 {
            return;
        }
        if radius <= 0.0 {
            let visible = r.intersect(clip);
            if visible.is_empty() {
                return;
            }
            if let Some(rect) = skia_rect(visible, self.scale) {
                self.pixmap.fill_rect(rect, &solid(argb), Transform::identity(), None);
            }
            return;
        }
        if !r.intersects(clip) {
            return;
        }
        if let Some(path) = rounded_path(r, radius, self.scale) {
            self.fill_path(&path, argb, clip);
        }
    }

    /// A rectangle outline of `width`, drawn inside the rect.
    fn stroke_rect(&mut self, r: Rect, argb: u32, width: f32, radius: f32, clip: &Rect) {
        if argb >> 24 == 0 || !r.intersects(clip) {
            return;
        }
        let inset = width / 2.0;
        let inner = Rect::new(r.x + inset, r.y + inset, (r.w - width).max(0.0), (r.h - width).max(0.0));
        let path = if radius > 0.0 {
            rounded_path(inner, (radius - inset).max(0.0), self.scale)
        } else {
            skia_rect(inner, self.scale).map(PathBuilder::from_rect)
        };
        if let Some(path) = path {
            self.stroke_path(&path, argb, width, clip);
        }
    }

    fn fill_circle(&mut self, c: (f32, f32), r: f32, argb: u32, clip: &Rect) {
        let Some(path) = PathBuilder::from_circle(c.0 * self.scale, c.1 * self.scale, r * self.scale) else { return };
        self.fill_path(&path, argb, clip);
    }

    fn stroke_line(&mut self, a: (f32, f32), b: (f32, f32), argb: u32, width: f32, clip: &Rect) {
        let mut pb = PathBuilder::new();
        pb.move_to(a.0 * self.scale, a.1 * self.scale);
        pb.line_to(b.0 * self.scale, b.1 * self.scale);
        let Some(path) = pb.finish() else { return };
        self.stroke_path(&path, argb, width, clip);
    }

    /// A soft shadow: a few stacked translucent rounded rects, each a little larger and
    /// lower than the last, so the edge fades instead of stepping.
    fn shadow(&mut self, r: Rect, radius: f32, elevation: f32, argb: u32, clip: &Rect) {
        let (a, red, green, blue) = argb_channels(argb);
        if a == 0 || elevation <= 0.0 {
            return;
        }
        let steps = elevation.ceil().clamp(1.0, 8.0) as u32;
        let alpha_each = ((a as f32) / (steps as f32)).max(1.0);
        for i in 0..steps {
            let spread = elevation * (i as f32 + 1.0) / steps as f32;
            let rect = Rect::new(r.x - spread, r.y - spread + elevation * 0.6, r.w + 2.0 * spread, r.h + 2.0 * spread);
            let alpha = (alpha_each * (1.0 - i as f32 / steps as f32 * 0.5)).round().clamp(1.0, 255.0) as u32;
            let colour = (alpha << 24) | ((red as u32) << 16) | ((green as u32) << 8) | blue as u32;
            self.fill_rect(rect, colour, radius + spread, clip);
        }
    }

    fn draw_text(&mut self, inner: &mut Inner, key: &TextKey, origin: (f32, f32), argb: u32, clip: &Rect) {
        let px = self.clip_px(clip);
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        inner.text.draw(key, origin, self.scale, argb, self.pixmap.data_mut(), w, h, Some(px));
    }
}

/// Paint the whole tree into `inner.pixmap` (allocated to the physical size on demand).
pub fn paint(inner: &mut Inner) {
    let (pw, ph) = inner.pixel_size();
    if pw == 0 || ph == 0 {
        inner.pixmap = None;
        return;
    }
    let needs_alloc = inner.pixmap.as_ref().map(|p| (p.width(), p.height())) != Some((pw, ph));
    if needs_alloc {
        inner.pixmap = Pixmap::new(pw, ph);
    }
    let Some(mut pixmap) = inner.pixmap.take() else { return };
    pixmap.fill(to_color(WINDOW_BACKGROUND));
    let mut painter = Painter { pixmap, scale: inner.scale, masks: Vec::new() };
    let clip = Rect::new(0.0, 0.0, inner.width, inner.height);
    let root = inner.root;
    paint_children(inner, root, &mut painter, &clip);
    inner.pixmap = Some(painter.pixmap);
}

fn paint_children(inner: &mut Inner, id: NodeId, painter: &mut Painter, clip: &Rect) {
    let n = inner.nodes.get(id).map(|n| n.children.len()).unwrap_or(0);
    for i in 0..n {
        let Some(c) = inner.nodes.get(id).and_then(|n| n.children.get(i).copied()) else { break };
        paint_node(inner, c, painter, clip);
    }
}

fn paint_node(inner: &mut Inner, id: NodeId, painter: &mut Painter, clip: &Rect) {
    let Some(node) = inner.nodes.get(id) else { return };
    if node.is_group() {
        paint_children(inner, id, painter, clip);
        return;
    }
    let kind = node.kind;
    let abs = node.abs;
    let clips_children = node.modifier.clip;
    let own_clip = if clips_children { abs.intersect(clip) } else { *clip };
    // culling: a node outside the clip has nothing visible, and neither do its children
    // (a scroll container or a clipping node cuts them off; anything else can still overflow
    // under a custom layout, so those children are walked)
    if !abs.intersects(clip) {
        if kind != Kind::Scroll && !clips_children {
            paint_children(inner, id, painter, clip);
        }
        return;
    }
    let content = node.content_rect();
    let layer_count = node.layers.len();
    let hovered = inner.hover_handler;
    for i in 0..layer_count {
        match inner.nodes[id].layers.get(i).copied() {
            Some(Layer::Shadow { rect, radius, elevation, argb }) => painter.shadow(rect.translated(abs.x, abs.y), radius, elevation, argb, &own_clip),
            Some(Layer::Background { rect, argb, radius }) => painter.fill_rect(rect.translated(abs.x, abs.y), argb, radius, &own_clip),
            Some(Layer::Border { rect, argb, width, radius }) => painter.stroke_rect(rect.translated(abs.x, abs.y), argb, width, radius, &own_clip),
            Some(Layer::Click { rect, handler, hover: Some(argb), radius }) if handler >= 0 && handler == hovered => {
                painter.fill_rect(rect.translated(abs.x, abs.y), argb, radius, &own_clip)
            }
            _ => {}
        }
    }
    match kind {
        Kind::Text => {
            let (key, argb, nowrap) = {
                let n = &inner.nodes[id];
                (n.text_key.clone(), n.style.argb, n.style.nowrap)
            };
            if let Some(key) = key {
                // a single-line text is cut off at its own edge rather than spilling over
                let text_clip = if nowrap { content.intersect(&own_clip) } else { own_clip };
                painter.draw_text(inner, &key, (content.x, content.y), argb, &text_clip);
            }
        }
        Kind::Button => {
            let (enabled, key, argb, handler) = {
                let n = &inner.nodes[id];
                (n.a != 0, n.text_key.clone(), n.style.argb, n.handler)
            };
            let fill = if !enabled {
                BUTTON_FILL_DISABLED
            } else if handler >= 0 && handler == hovered {
                BUTTON_FILL_HOVER
            } else {
                BUTTON_FILL
            };
            painter.fill_rect(content, fill, BUTTON_RADIUS, &own_clip);
            painter.stroke_rect(content, BUTTON_BORDER, 1.0, BUTTON_RADIUS, &own_clip);
            if let Some(key) = key {
                let s = inner.text.measure(&key);
                let x = content.x + (content.w - s.w) / 2.0;
                let y = content.y + (content.h - s.h) / 2.0;
                painter.draw_text(inner, &key, (x, y), if enabled { argb } else { TEXT_DISABLED }, &own_clip);
            }
        }
        Kind::TextField => {
            let focused = inner.focus.as_ref().filter(|f| f.node == id).cloned();
            let (key, is_placeholder) = {
                let n = &inner.nodes[id];
                let placeholder = focused.is_none() && n.text.as_deref().map(|t| t.is_empty()).unwrap_or(true);
                (n.text_key.clone(), placeholder)
            };
            painter.fill_rect(content, 0xFFFF_FFFF, FIELD_RADIUS, &own_clip);
            let (border, width) = if focused.is_some() { (FIELD_BORDER_FOCUS, 2.0) } else { (FIELD_BORDER, 1.0) };
            painter.stroke_rect(content, border, width, FIELD_RADIUS, &own_clip);
            let text_origin = (content.x + crate::layout::FIELD_PAD_X, content.y + crate::layout::FIELD_PAD_Y);
            let text_clip = content.intersect(&own_clip);
            if let Some(key) = key {
                let argb = if is_placeholder { PLACEHOLDER } else { Style::DEFAULT.argb };
                painter.draw_text(inner, &key, text_origin, argb, &text_clip);
            }
            if let Some(f) = focused {
                let prefix: String = f.buffer.chars().take(f.caret).collect();
                let w = if prefix.is_empty() {
                    0.0
                } else {
                    inner.text.measure(&TextKey::new(prefix.into(), Style::DEFAULT, f32::INFINITY)).w
                };
                let line = crate::text::TextSystem::line_height(Style::DEFAULT);
                painter.fill_rect(Rect::new(text_origin.0 + w, text_origin.1, 1.0, line), Style::DEFAULT.argb, 0.0, &text_clip);
            }
        }
        Kind::Checkbox => {
            let checked = inner.nodes[id].a != 0;
            let b = Rect::new(content.x, content.y, crate::layout::CHECKBOX_SIZE, crate::layout::CHECKBOX_SIZE);
            if checked {
                painter.fill_rect(b, ACCENT, 4.0, &own_clip);
                let (x, y, s) = (b.x, b.y, b.w);
                painter.stroke_line((x + s * 0.22, y + s * 0.52), (x + s * 0.42, y + s * 0.72), 0xFFFF_FFFF, 2.0, &own_clip);
                painter.stroke_line((x + s * 0.42, y + s * 0.72), (x + s * 0.78, y + s * 0.3), 0xFFFF_FFFF, 2.0, &own_clip);
            } else {
                painter.fill_rect(b, 0xFFFF_FFFF, 4.0, &own_clip);
                painter.stroke_rect(b, BUTTON_BORDER, 1.0, 4.0, &own_clip);
            }
        }
        Kind::Canvas => {
            // take the commands out while drawing so the text system can be borrowed mutably
            let cmds = inner.nodes[id].canvas.take();
            if let Some(cmds) = cmds {
                for cmd in cmds.iter() {
                    paint_canvas_cmd(inner, painter, cmd, (content.x, content.y), &own_clip);
                }
                if let Some(n) = inner.nodes.get_mut(id) {
                    if n.canvas.is_none() {
                        n.canvas = Some(cmds);
                    }
                }
            }
        }
        _ => {}
    }
    if kind == Kind::Scroll {
        let child_clip = content.intersect(&own_clip);
        if !child_clip.is_empty() {
            paint_children(inner, id, painter, &child_clip);
        }
        paint_scrollbar(inner, id, painter, &own_clip);
    } else {
        paint_children(inner, id, painter, &own_clip);
    }
}

/// A thin thumb at the right edge of a scroll container whose content overflows it.
fn paint_scrollbar(inner: &Inner, id: NodeId, painter: &mut Painter, clip: &Rect) {
    let node = &inner.nodes[id];
    let limit = node.scroll_limit();
    if limit <= 0.0 {
        return;
    }
    let viewport = node.content_rect();
    let track = (viewport.h - 2.0 * SCROLLBAR_INSET).max(0.0);
    if track <= 0.0 {
        return;
    }
    let thumb_h = (track * viewport.h / node.content_len).clamp(SCROLLBAR_MIN.min(track), track);
    let thumb_y = viewport.y + SCROLLBAR_INSET + (track - thumb_h) * (node.scroll / limit);
    let rect = Rect::new(viewport.x + viewport.w - SCROLLBAR_WIDTH - SCROLLBAR_INSET, thumb_y, SCROLLBAR_WIDTH, thumb_h);
    painter.fill_rect(rect, SCROLLBAR, SCROLLBAR_WIDTH / 2.0, clip);
}

fn paint_canvas_cmd(inner: &mut Inner, painter: &mut Painter, cmd: &CanvasCmd, origin: (f32, f32), clip: &Rect) {
    match cmd {
        CanvasCmd::Rect { x, y, w, h, argb } => painter.fill_rect(Rect::new(origin.0 + x, origin.1 + y, *w, *h), *argb, 0.0, clip),
        CanvasCmd::Circle { cx, cy, r, argb } => painter.fill_circle((origin.0 + cx, origin.1 + cy), *r, *argb, clip),
        CanvasCmd::Line { x1, y1, x2, y2, argb, stroke } => {
            painter.stroke_line((origin.0 + x1, origin.1 + y1), (origin.0 + x2, origin.1 + y2), *argb, *stroke, clip)
        }
        CanvasCmd::Text { x, y, text, style } => {
            let key = TextKey::new(text.clone(), *style, f32::INFINITY);
            painter.draw_text(inner, &key, (origin.0 + x, origin.1 + y), style.argb, clip);
        }
    }
}

fn to_color(argb: u32) -> Color {
    let (a, r, g, b) = argb_channels(argb);
    Color::from_rgba8(r, g, b, a)
}

fn solid(argb: u32) -> Paint<'static> {
    Paint { shader: Shader::SolidColor(to_color(argb)), anti_alias: true, ..Paint::default() }
}

fn skia_rect(r: Rect, scale: f32) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_xywh(r.x * scale, r.y * scale, r.w * scale, r.h * scale)
}

/// A rounded-rectangle path in physical pixels; the radius is clamped to half the shorter side.
fn rounded_path(r: Rect, radius: f32, scale: f32) -> Option<Path> {
    if r.w <= 0.0 || r.h <= 0.0 {
        return None;
    }
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0) * scale;
    let (x, y, w, h) = (r.x * scale, r.y * scale, r.w * scale, r.h * scale);
    if rad <= 0.0 {
        return skia_rect(r, scale).map(PathBuilder::from_rect);
    }
    // a quarter circle approximated by one cubic per corner (kappa = 0.5523)
    let k = rad * 0.5523;
    let mut pb = PathBuilder::new();
    pb.move_to(x + rad, y);
    pb.line_to(x + w - rad, y);
    pb.cubic_to(x + w - rad + k, y, x + w, y + rad - k, x + w, y + rad);
    pb.line_to(x + w, y + h - rad);
    pb.cubic_to(x + w, y + h - rad + k, x + w - rad + k, y + h, x + w - rad, y + h);
    pb.line_to(x + rad, y + h);
    pb.cubic_to(x + rad - k, y + h, x, y + h - rad + k, x, y + h - rad);
    pb.line_to(x, y + rad);
    pb.cubic_to(x, y + rad - k, x + rad - k, y, x + rad, y);
    pb.close();
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::commit;
    use crate::layout::{layout_tree, NoHost};
    use crate::text::TextSystem;

    fn at(px: &Pixmap, x: usize, y: usize) -> [u8; 4] {
        let i = (y * px.width() as usize + x) * 4;
        let d = &px.data()[i..i + 4];
        [d[0], d[1], d[2], d[3]]
    }

    #[test]
    fn paints_backgrounds_and_buttons() {
        let mut inner = Inner::new(100.0, 50.0, 1.0, TextSystem::monospace_only());
        let mods = vec![(1, vec![2.0, 20.0, 3.0, 20.0, 6.0, 4278190335.0])]; // 20x20 blue
        let ints: Vec<i32> = [[3, 0, -1, 0, -1, 0, 0, 0], [10, 0, -1, 1, -1, 0, 0, 0], [7, 0, 0, 0, 1, 1, 0, 0], [0, 0, -1, 0, -1, 0, 0, 0]]
            .iter()
            .flatten()
            .copied()
            .collect();
        commit(&mut inner, &ints, &["ok"], &[(0, 0, 4)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        paint(&mut inner);
        let px = inner.pixmap.as_ref().unwrap();
        assert_eq!(px.data().len(), 100 * 50 * 4);
        assert_eq!(at(px, 5, 5), [0, 0, 255, 255]);
        assert_eq!(at(px, 90, 45), [255, 255, 255, 255]);
        // the button sits below the spacer: its fill is grey
        let b = at(px, 20, 30);
        assert!(b[0] > 200 && b[0] < 255 && b[3] == 255, "{:?}", b);
        // hovering the button darkens it
        inner.hover_handler = 1;
        paint(&mut inner);
        let h = at(inner.pixmap.as_ref().unwrap(), 20, 30);
        assert!(h[0] < b[0], "{:?} vs {:?}", h, b);
        // a second paint at scale 2 doubles the buffer
        inner.resize(100.0, 50.0, 2.0);
        layout_tree(&mut inner, &mut NoHost::default());
        paint(&mut inner);
        assert_eq!(inner.pixmap.as_ref().unwrap().data().len(), 200 * 100 * 4);
    }

    #[test]
    fn scroll_clips_children_and_rounded_islands_have_transparent_corners() {
        let mut inner = Inner::new(100.0, 100.0, 1.0, TextSystem::monospace_only());
        // mod 1: viewport 60x40 with a red background; mod 2: 60x30 blue child;
        // mod 3: rounded 10 + green background, 40x40
        let mods = vec![
            (1, vec![2.0, 60.0, 3.0, 40.0, 6.0, 4294901760.0]),
            (2, vec![2.0, 60.0, 3.0, 30.0, 6.0, 4278190335.0]),
            (3, vec![2.0, 40.0, 3.0, 40.0, 10.0, 10.0, 6.0, 4278255360.0]),
        ];
        let ints: Vec<i32> = [
            [3, 0, -1, 0, -1, 0, 0, 0],
            [13, 0, -1, 1, -1, 0, 0, 0],
            [10, 0, -1, 2, -1, 0, 0, 0],
            [10, 0, -1, 2, -1, 0, 0, 0],
            [10, 0, -1, 2, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
            [10, 0, -1, 3, -1, 0, 0, 0],
            [0, 0, -1, 0, -1, 0, 0, 0],
        ]
        .iter()
        .flatten()
        .copied()
        .collect();
        commit(&mut inner, &ints, &[], &[(0, 0, 8)], &mods, &[]).unwrap();
        layout_tree(&mut inner, &mut NoHost::default());
        paint(&mut inner);
        let px = inner.pixmap.as_ref().unwrap();
        // inside the viewport: the first blue child
        assert_eq!(at(px, 10, 10), [0, 0, 255, 255]);
        // the second child starts at y=30 and is cut at the viewport's bottom (y=40)
        assert_eq!(at(px, 10, 35), [0, 0, 255, 255]);
        assert_ne!(at(px, 10, 45), [0, 0, 255, 255]);
        // scrollbar thumb at the right edge (content 90 > viewport 40)
        let thumb = at(px, 57, 5);
        assert!(thumb[2] < 255, "thumb darkens the blue: {:?}", thumb);
        // the island below the scroll: green inside, but the very corner pixel keeps the
        // white window background because of the 10px radius
        assert_eq!(at(px, 20, 60), [0, 255, 0, 255]);
        assert_eq!(at(px, 0, 40), [255, 255, 255, 255]);
        // scrolling by 30 brings the third child into view
        let sc = inner.layout_children(inner.layout_children(inner.root)[0])[0];
        inner.nodes[sc].scroll = 30.0;
        crate::layout::assign_abs(&mut inner);
        paint(&mut inner);
        let px = inner.pixmap.as_ref().unwrap();
        assert_eq!(at(px, 10, 35), [0, 0, 255, 255]);
    }
}
