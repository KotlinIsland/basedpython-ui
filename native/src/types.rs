//! Plain value types shared by every module. Nothing here touches Python.

use std::sync::Arc;

/// Element kinds. The numbers are the record kinds of the protocol
/// (`END` = 0 and `SCOPE_REF` = 2 are record kinds but not element kinds).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    Scope,
    Column,
    Row,
    Box,
    Text,
    Button,
    TextField,
    Checkbox,
    Spacer,
    Canvas,
    Layout,
    Scroll,
}

pub const REC_END: i32 = 0;
pub const REC_SCOPE: i32 = 1;
pub const REC_SCOPE_REF: i32 = 2;

impl Kind {
    pub fn from_record(kind: i32) -> Option<Kind> {
        Some(match kind {
            1 => Kind::Scope,
            3 => Kind::Column,
            4 => Kind::Row,
            5 => Kind::Box,
            6 => Kind::Text,
            7 => Kind::Button,
            8 => Kind::TextField,
            9 => Kind::Checkbox,
            10 => Kind::Spacer,
            11 => Kind::Canvas,
            12 => Kind::Layout,
            13 => Kind::Scroll,
            _ => return None,
        })
    }

    /// Kinds whose records are followed by children until `END`.
    pub fn has_children(self) -> bool {
        matches!(
            self,
            Kind::Scope | Kind::Column | Kind::Row | Kind::Box | Kind::Layout | Kind::Scroll
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Scope => "Scope",
            Kind::Column => "Column",
            Kind::Row => "Row",
            Kind::Box => "Box",
            Kind::Text => "Text",
            Kind::Button => "Button",
            Kind::TextField => "TextField",
            Kind::Checkbox => "Checkbox",
            Kind::Spacer => "Spacer",
            Kind::Canvas => "Canvas",
            Kind::Layout => "Layout",
            Kind::Scroll => "Scroll",
        }
    }
}

/// A reconciliation key. Keyed children match by `(key, kind)`.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub enum Key {
    None,
    Int(i32),
    Str(Arc<str>),
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const ZERO: Size = Size { w: 0.0, h: 0.0 };
    pub fn new(w: f32, h: f32) -> Size {
        Size { w, h }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn translated(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    /// The overlap of two rects; empty (zero-sized) when they do not meet.
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = (self.x + self.w).min(other.x + other.w);
        let b = (self.y + self.h).min(other.y + other.h);
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    pub fn intersects(&self, other: &Rect) -> bool {
        !self.intersect(other).is_empty()
    }
}

/// Layout constraints; `max_*` may be `f32::INFINITY`. Never NaN (inputs are sanitised).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Constraints {
    pub min_w: f32,
    pub max_w: f32,
    pub min_h: f32,
    pub max_h: f32,
}

impl Constraints {
    pub fn tight(w: f32, h: f32) -> Constraints {
        Constraints { min_w: w, max_w: w, min_h: h, max_h: h }
    }
    pub fn loose(max_w: f32, max_h: f32) -> Constraints {
        Constraints { min_w: 0.0, max_w, min_h: 0.0, max_h }
    }
    /// Clamp a size into these constraints.
    pub fn constrain(&self, s: Size) -> Size {
        Size {
            w: clamp(s.w, self.min_w, self.max_w),
            h: clamp(s.h, self.min_h, self.max_h),
        }
    }
    /// Shrink by an inset on each axis (padding); never below zero, infinity stays infinite.
    pub fn deflate(&self, dw: f32, dh: f32) -> Constraints {
        Constraints {
            min_w: (self.min_w - dw).max(0.0),
            max_w: (self.max_w - dw).max(0.0),
            min_h: (self.min_h - dh).max(0.0),
            max_h: (self.max_h - dh).max(0.0),
        }
    }
    pub fn loosened(&self) -> Constraints {
        Constraints { min_w: 0.0, max_w: self.max_w, min_h: 0.0, max_h: self.max_h }
    }
}

/// `v.clamp(lo, hi)` that tolerates `lo > hi` (returns `lo`, like Compose's coerceIn order).
pub fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi.max(lo)
    } else {
        v
    }
}

/// One modifier operation. Values are already validated (finite, non-negative where required).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ModOp {
    Padding { l: f32, t: f32, r: f32, b: f32 },
    Width(f32),
    Height(f32),
    FillMaxWidth,
    FillMaxHeight,
    Background(u32),
    Weight(f32),
    Align(u8),
    Clickable(i32),
    /// Corner radius for every background, border, shadow and hover layer after it in the chain.
    Rounded(f32),
    Border { width: f32, argb: u32 },
    /// Colour painted over the node's clickable layer while the pointer is over it.
    Hover(u32),
    /// A soft drop shadow under the layer's rect; `elevation` is its spread in logical pixels.
    Shadow { elevation: f32, argb: u32 },
    /// Ask the nearest scroll container to bring the node into view when this op appears.
    Reveal,
    /// Clip the node's own painting and its children to the node's rect.
    Clip,
}

/// An interned modifier chain. `weight` / `align` / `hover` / `reveal` / `clip` are read by
/// the parent container or by paint (last op of that kind wins); the other ops are applied in
/// order by the node itself.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Modifier {
    pub ops: Vec<ModOp>,
    pub weight: f32,
    pub align: Option<u8>,
    pub hover: Option<u32>,
    pub reveal: bool,
    pub clip: bool,
}

impl Modifier {
    /// Parse the protocol's flat `[op, args…]` runs.
    pub fn parse(raw: &[f64]) -> Result<Modifier, String> {
        let mut ops = Vec::new();
        let mut i = 0;
        let mut weight = 0.0f32;
        let mut align = None;
        let mut hover = None;
        let mut reveal = false;
        let mut clip = false;
        while i < raw.len() {
            let op = raw[i];
            let take = |n: usize| -> Result<&[f64], String> {
                if i + 1 + n > raw.len() {
                    Err(format!("modifier op {} at {} needs {} args", op, i, n))
                } else {
                    Ok(&raw[i + 1..i + 1 + n])
                }
            };
            match op {
                x if x == 1.0 => {
                    let a = take(4)?;
                    ops.push(ModOp::Padding {
                        l: non_negative(a[0], "padding")?,
                        t: non_negative(a[1], "padding")?,
                        r: non_negative(a[2], "padding")?,
                        b: non_negative(a[3], "padding")?,
                    });
                    i += 5;
                }
                x if x == 2.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Width(non_negative(a[0], "width")?));
                    i += 2;
                }
                x if x == 3.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Height(non_negative(a[0], "height")?));
                    i += 2;
                }
                x if x == 4.0 => {
                    ops.push(ModOp::FillMaxWidth);
                    i += 1;
                }
                x if x == 5.0 => {
                    ops.push(ModOp::FillMaxHeight);
                    i += 1;
                }
                x if x == 6.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Background(argb_from_f64(a[0])?));
                    i += 2;
                }
                x if x == 7.0 => {
                    let a = take(1)?;
                    let w = finite(a[0], "weight")?;
                    if w <= 0.0 {
                        return Err(format!("weight must be > 0, got {}", w));
                    }
                    weight = w;
                    ops.push(ModOp::Weight(w));
                    i += 2;
                }
                x if x == 8.0 => {
                    let a = take(1)?;
                    let v = finite(a[0], "align")?;
                    if !(v == 0.0 || v == 1.0 || v == 2.0) {
                        return Err(format!("align must be 0, 1 or 2, got {}", v));
                    }
                    align = Some(v as u8);
                    ops.push(ModOp::Align(v as u8));
                    i += 2;
                }
                x if x == 9.0 => {
                    let a = take(1)?;
                    let h = finite(a[0], "clickable")?;
                    if h < 0.0 || h > i32::MAX as f32 || h.fract() != 0.0 {
                        return Err(format!("clickable handler index must be a non-negative int, got {}", h));
                    }
                    ops.push(ModOp::Clickable(h as i32));
                    i += 2;
                }
                x if x == 10.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Rounded(non_negative(a[0], "rounded")?));
                    i += 2;
                }
                x if x == 11.0 => {
                    let a = take(2)?;
                    ops.push(ModOp::Border { width: non_negative(a[0], "border width")?, argb: argb_from_f64(a[1])? });
                    i += 3;
                }
                x if x == 12.0 => {
                    let a = take(1)?;
                    let argb = argb_from_f64(a[0])?;
                    hover = Some(argb);
                    ops.push(ModOp::Hover(argb));
                    i += 2;
                }
                x if x == 13.0 => {
                    let a = take(2)?;
                    ops.push(ModOp::Shadow { elevation: non_negative(a[0], "shadow elevation")?, argb: argb_from_f64(a[1])? });
                    i += 3;
                }
                x if x == 14.0 => {
                    reveal = true;
                    ops.push(ModOp::Reveal);
                    i += 1;
                }
                x if x == 15.0 => {
                    clip = true;
                    ops.push(ModOp::Clip);
                    i += 1;
                }
                _ => return Err(format!("unknown modifier op {} at {}", op, i)),
            }
        }
        Ok(Modifier { ops, weight, align, hover, reveal, clip })
    }
}

fn finite(v: f64, what: &str) -> Result<f32, String> {
    if v.is_finite() {
        Ok(v as f32)
    } else {
        Err(format!("{} must be finite, got {}", what, v))
    }
}

fn non_negative(v: f64, what: &str) -> Result<f32, String> {
    let f = finite(v, what)?;
    if f < 0.0 {
        Err(format!("{} must be >= 0, got {}", what, v))
    } else {
        Ok(f)
    }
}

/// Colours cross the boundary as the f64 of a u32 `0xAARRGGBB`.
pub fn argb_from_f64(v: f64) -> Result<u32, String> {
    if !v.is_finite() || v < 0.0 || v > u32::MAX as f64 || v.fract() != 0.0 {
        return Err(format!("colour must be an integer 0..=0xFFFFFFFF as float, got {}", v));
    }
    Ok(v as u32)
}

/// A text style (interned). `size_bits` keeps the struct hashable; use `size()`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Style {
    pub size_bits: u32,
    pub argb: u32,
    pub bold: bool,
    /// Shape with the monospace family.
    pub mono: bool,
    /// Never wrap: one line, clipped to the node's rect when it is wider.
    pub nowrap: bool,
}

pub const STYLE_BOLD: i64 = 1;
pub const STYLE_MONO: i64 = 2;
pub const STYLE_NOWRAP: i64 = 4;

impl Style {
    pub const DEFAULT: Style = Style { size_bits: 0x4160_0000, argb: 0xFF20_2020, bold: false, mono: false, nowrap: false }; // 14.0

    pub fn new(size: f32, argb: u32, bold: bool) -> Style {
        Style { size_bits: size.to_bits(), argb, bold, mono: false, nowrap: false }
    }
    /// From the protocol's flag word: bit 0 bold, bit 1 monospace, bit 2 no-wrap.
    pub fn with_flags(size: f32, argb: u32, flags: i64) -> Style {
        Style {
            size_bits: size.to_bits(),
            argb,
            bold: flags & STYLE_BOLD != 0,
            mono: flags & STYLE_MONO != 0,
            nowrap: flags & STYLE_NOWRAP != 0,
        }
    }
    pub fn size(&self) -> f32 {
        f32::from_bits(self.size_bits)
    }
}

/// A rectangle produced by a modifier layer during layout, relative to the node origin.
/// `radius` is the corner radius in effect at that point of the chain (0 = square).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Layer {
    Shadow { rect: Rect, radius: f32, elevation: f32, argb: u32 },
    Background { rect: Rect, argb: u32, radius: f32 },
    Border { rect: Rect, argb: u32, width: f32, radius: f32 },
    Click { rect: Rect, handler: i32, hover: Option<u32>, radius: f32 },
}

/// A retained canvas draw command in the canvas's own coordinates.
#[derive(Clone, PartialEq, Debug)]
pub enum CanvasCmd {
    Rect { x: f32, y: f32, w: f32, h: f32, argb: u32 },
    Circle { cx: f32, cy: f32, r: f32, argb: u32 },
    Line { x1: f32, y1: f32, x2: f32, y2: f32, argb: u32, stroke: f32 },
    Text { x: f32, y: f32, text: Arc<str>, style: Style },
}

/// Malformed input from Python: becomes a `ValueError`. The tree is untouched when raised.
#[derive(Debug, Clone, PartialEq)]
pub struct InputError(pub String);

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InputError {}

/// Split `0xAARRGGBB` into channels.
pub fn argb_channels(argb: u32) -> (u8, u8, u8, u8) {
    (
        (argb >> 24) as u8,
        (argb >> 16) as u8,
        (argb >> 8) as u8,
        argb as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_parse_round_trip() {
        let m = Modifier::parse(&[1.0, 1.0, 2.0, 3.0, 4.0, 7.0, 2.0, 8.0, 1.0, 4.0]).unwrap();
        assert_eq!(m.ops.len(), 4);
        assert_eq!(m.weight, 2.0);
        assert_eq!(m.align, Some(1));
        assert!(Modifier::parse(&[1.0, 1.0]).is_err());
        assert!(Modifier::parse(&[42.0]).is_err());
        assert!(Modifier::parse(&[7.0, -1.0]).is_err());
        assert!(Modifier::parse(&[6.0, f64::NAN]).is_err());
        let m = Modifier::parse(&[10.0, 8.0, 11.0, 1.0, 4278190080.0, 12.0, 4278190335.0, 13.0, 4.0, 1090519040.0, 14.0, 15.0]).unwrap();
        assert_eq!(m.ops.len(), 6);
        assert_eq!(m.hover, Some(0xFF0000FF));
        assert!(m.reveal && m.clip);
        assert!(Modifier::parse(&[10.0, -1.0]).is_err());
        assert!(Modifier::parse(&[11.0, 1.0]).is_err());
    }

    #[test]
    fn style_flags_and_rect_math() {
        let s = Style::with_flags(12.0, 0xFF000000, STYLE_BOLD | STYLE_MONO | STYLE_NOWRAP);
        assert!(s.bold && s.mono && s.nowrap);
        assert_eq!(Style::with_flags(14.0, 0xFF202020, 0), Style::DEFAULT);
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersect(&b), Rect::new(5.0, 5.0, 5.0, 5.0));
        assert!(a.intersect(&Rect::new(20.0, 20.0, 1.0, 1.0)).is_empty());
        assert!(a.intersects(&b));
    }

    #[test]
    fn constraints_math() {
        let c = Constraints::loose(100.0, f32::INFINITY);
        let d = c.deflate(20.0, 20.0);
        assert_eq!(d.max_w, 80.0);
        assert!(d.max_h.is_infinite());
        assert_eq!(c.constrain(Size::new(150.0, 5.0)), Size::new(100.0, 5.0));
        assert_eq!(clamp(5.0, 10.0, 8.0), 10.0);
    }
}
