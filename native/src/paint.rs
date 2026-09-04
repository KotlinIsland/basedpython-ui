//! Painting: walk the tree into a tiny-skia pixmap (premultiplied RGBA8, physical pixels).
//!
//! M1 repaints everything each frame. TODO(M6): damage rects / repaint boundaries with per
//! container display lists; TODO: a GPU rasteriser behind the same walk.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Shader, Stroke, Transform};

use crate::text::TextKey;
use crate::tree::{Inner, NodeId};
use crate::types::*;

pub const WINDOW_BACKGROUND: u32 = 0xFFFF_FFFF;
const BUTTON_FILL: u32 = 0xFFE4_E4E7;
const BUTTON_FILL_DISABLED: u32 = 0xFFF4_F4F5;
const BUTTON_BORDER: u32 = 0xFFA1_A1AA;
const TEXT_DISABLED: u32 = 0xFF9A_9A9A;
const FIELD_BORDER: u32 = 0xFFA1_A1AA;
const FIELD_BORDER_FOCUS: u32 = 0xFF3B_82F6;
const PLACEHOLDER: u32 = 0xFF9C_A3AF;
const ACCENT: u32 = 0xFF3B_82F6;

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
    let scale = inner.scale;
    let root = inner.root;
    paint_children(inner, root, &mut pixmap, scale);
    inner.pixmap = Some(pixmap);
}

fn paint_children(inner: &mut Inner, id: NodeId, pixmap: &mut Pixmap, scale: f32) {
    let n = inner.nodes.get(id).map(|n| n.children.len()).unwrap_or(0);
    for i in 0..n {
        let Some(c) = inner.nodes.get(id).and_then(|n| n.children.get(i).copied()) else { break };
        paint_node(inner, c, pixmap, scale);
    }
}

fn paint_node(inner: &mut Inner, id: NodeId, pixmap: &mut Pixmap, scale: f32) {
    let Some(node) = inner.nodes.get(id) else { return };
    if node.is_group() {
        paint_children(inner, id, pixmap, scale);
        return;
    }
    let kind = node.kind;
    let abs = node.abs;
    // viewport culling of the node's own drawing (children are still walked: a child may
    // overflow its parent, e.g. under a custom layout)
    let (vw, vh) = (inner.width, inner.height);
    if abs.x >= vw || abs.y >= vh || abs.x + abs.w <= 0.0 || abs.y + abs.h <= 0.0 {
        paint_children(inner, id, pixmap, scale);
        return;
    }
    let content = Rect::new(abs.x + node.content_origin.0, abs.y + node.content_origin.1, node.content_size.w, node.content_size.h);
    let layer_count = node.layers.len();
    for i in 0..layer_count {
        if let Some(Layer::Background { rect, argb }) = inner.nodes[id].layers.get(i).copied() {
            fill_rect(pixmap, rect.translated(abs.x, abs.y), argb, scale);
        }
    }
    match kind {
        Kind::Text => {
            let (key, argb) = {
                let n = &inner.nodes[id];
                (n.text_key.clone(), n.style.argb)
            };
            if let Some(key) = key {
                draw_text(inner, pixmap, &key, (content.x, content.y), scale, argb);
            }
        }
        Kind::Button => {
            let (enabled, key, argb) = {
                let n = &inner.nodes[id];
                (n.a != 0, n.text_key.clone(), n.style.argb)
            };
            fill_rect(pixmap, content, if enabled { BUTTON_FILL } else { BUTTON_FILL_DISABLED }, scale);
            stroke_rect(pixmap, content, BUTTON_BORDER, 1.0, scale);
            if let Some(key) = key {
                let s = inner.text.measure(&key);
                let x = content.x + (content.w - s.w) / 2.0;
                let y = content.y + (content.h - s.h) / 2.0;
                draw_text(inner, pixmap, &key, (x, y), scale, if enabled { argb } else { TEXT_DISABLED });
            }
        }
        Kind::TextField => {
            let focused = inner.focus.as_ref().filter(|f| f.node == id).cloned();
            let (key, is_placeholder) = {
                let n = &inner.nodes[id];
                let placeholder = focused.is_none() && n.text.as_deref().map(|t| t.is_empty()).unwrap_or(true);
                (n.text_key.clone(), placeholder)
            };
            fill_rect(pixmap, content, 0xFFFF_FFFF, scale);
            stroke_rect(pixmap, content, if focused.is_some() { FIELD_BORDER_FOCUS } else { FIELD_BORDER }, if focused.is_some() { 2.0 } else { 1.0 }, scale);
            let text_origin = (content.x + crate::layout::FIELD_PAD_X, content.y + crate::layout::FIELD_PAD_Y);
            if let Some(key) = key {
                let argb = if is_placeholder { PLACEHOLDER } else { Style::DEFAULT.argb };
                draw_text(inner, pixmap, &key, text_origin, scale, argb);
            }
            if let Some(f) = focused {
                let prefix: String = f.buffer.chars().take(f.caret).collect();
                let w = if prefix.is_empty() {
                    0.0
                } else {
                    inner.text.measure(&TextKey::new(prefix.into(), Style::DEFAULT, f32::INFINITY)).w
                };
                let line = crate::text::TextSystem::line_height(Style::DEFAULT);
                fill_rect(pixmap, Rect::new(text_origin.0 + w, text_origin.1, 1.0, line), Style::DEFAULT.argb, scale);
            }
        }
        Kind::Checkbox => {
            let checked = inner.nodes[id].a != 0;
            let b = Rect::new(content.x, content.y, crate::layout::CHECKBOX_SIZE, crate::layout::CHECKBOX_SIZE);
            if checked {
                fill_rect(pixmap, b, ACCENT, scale);
                let (x, y, s) = (b.x, b.y, b.w);
                stroke_line(pixmap, (x + s * 0.22, y + s * 0.52), (x + s * 0.42, y + s * 0.72), 0xFFFF_FFFF, 2.0, scale);
                stroke_line(pixmap, (x + s * 0.42, y + s * 0.72), (x + s * 0.78, y + s * 0.3), 0xFFFF_FFFF, 2.0, scale);
            } else {
                fill_rect(pixmap, b, 0xFFFF_FFFF, scale);
                stroke_rect(pixmap, b, BUTTON_BORDER, 1.0, scale);
            }
        }
        Kind::Canvas => {
            // take the commands out while drawing so the text system can be borrowed mutably
            let cmds = inner.nodes[id].canvas.take();
            if let Some(cmds) = cmds {
                for cmd in cmds.iter() {
                    paint_canvas_cmd(inner, pixmap, cmd, (content.x, content.y), scale);
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
    paint_children(inner, id, pixmap, scale);
}

fn paint_canvas_cmd(inner: &mut Inner, pixmap: &mut Pixmap, cmd: &CanvasCmd, origin: (f32, f32), scale: f32) {
    match cmd {
        CanvasCmd::Rect { x, y, w, h, argb } => fill_rect(pixmap, Rect::new(origin.0 + x, origin.1 + y, *w, *h), *argb, scale),
        CanvasCmd::Circle { cx, cy, r, argb } => fill_circle(pixmap, (origin.0 + cx, origin.1 + cy), *r, *argb, scale),
        CanvasCmd::Line { x1, y1, x2, y2, argb, stroke } => {
            stroke_line(pixmap, (origin.0 + x1, origin.1 + y1), (origin.0 + x2, origin.1 + y2), *argb, *stroke, scale)
        }
        CanvasCmd::Text { x, y, text, style } => {
            let key = TextKey::new(text.clone(), *style, f32::INFINITY);
            draw_text(inner, pixmap, &key, (origin.0 + x, origin.1 + y), scale, style.argb);
        }
    }
}

fn draw_text(inner: &mut Inner, pixmap: &mut Pixmap, key: &TextKey, origin: (f32, f32), scale: f32, argb: u32) {
    let (w, h) = (pixmap.width(), pixmap.height());
    inner.text.draw(key, origin, scale, argb, pixmap.data_mut(), w, h);
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

pub fn fill_rect(pixmap: &mut Pixmap, r: Rect, argb: u32, scale: f32) {
    if argb >> 24 == 0 {
        return;
    }
    let Some(rect) = skia_rect(r, scale) else { return };
    pixmap.fill_rect(rect, &solid(argb), Transform::identity(), None);
}

fn stroke_rect(pixmap: &mut Pixmap, r: Rect, argb: u32, width: f32, scale: f32) {
    let inset = width / 2.0;
    let Some(rect) = skia_rect(Rect::new(r.x + inset, r.y + inset, r.w - width, r.h - width), scale) else { return };
    let path = PathBuilder::from_rect(rect);
    let stroke = Stroke { width: width * scale, ..Stroke::default() };
    pixmap.stroke_path(&path, &solid(argb), &stroke, Transform::identity(), None);
}

fn fill_circle(pixmap: &mut Pixmap, c: (f32, f32), r: f32, argb: u32, scale: f32) {
    let Some(path) = PathBuilder::from_circle(c.0 * scale, c.1 * scale, r * scale) else { return };
    pixmap.fill_path(&path, &solid(argb), FillRule::Winding, Transform::identity(), None);
}

fn stroke_line(pixmap: &mut Pixmap, a: (f32, f32), b: (f32, f32), argb: u32, width: f32, scale: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(a.0 * scale, a.1 * scale);
    pb.line_to(b.0 * scale, b.1 * scale);
    let Some(path) = pb.finish() else { return };
    let stroke = Stroke { width: (width * scale).max(0.5), ..Stroke::default() };
    pixmap.stroke_path(&path, &solid(argb), &stroke, Transform::identity(), None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::commit;
    use crate::layout::{layout_tree, NoHost};
    use crate::text::TextSystem;

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
        let at = |x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            &px.data()[i..i + 4]
        };
        assert_eq!(at(5, 5), &[0, 0, 255, 255]);
        assert_eq!(at(90, 45), &[255, 255, 255, 255]);
        // the button sits below the spacer: its fill is grey
        let b = at(20, 30);
        assert!(b[0] > 200 && b[0] < 255 && b[3] == 255, "{:?}", b);
        // a second paint at scale 2 doubles the buffer
        inner.resize(100.0, 50.0, 2.0);
        layout_tree(&mut inner, &mut NoHost::default());
        paint(&mut inner);
        assert_eq!(inner.pixmap.as_ref().unwrap().data().len(), 200 * 100 * 4);
    }
}
