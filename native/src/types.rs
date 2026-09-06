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
    Image,
    Layout,
    Scroll,
    Popup,
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
            15 => Kind::Image,
            12 => Kind::Layout,
            13 => Kind::Scroll,
            14 => Kind::Popup,
            _ => return None,
        })
    }

    /// Kinds whose records are followed by children until `END`.
    pub fn has_children(self) -> bool {
        matches!(
            self,
            Kind::Scope | Kind::Column | Kind::Row | Kind::Box | Kind::Layout | Kind::Scroll | Kind::Popup
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
            Kind::Image => "Image",
            Kind::Layout => "Layout",
            Kind::Scroll => "Scroll",
            Kind::Popup => "Popup",
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

/// Corner radii, clockwise from the top left. Uniform ones come from `Corners::uniform`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Corners {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl Corners {
    pub const NONE: Corners = Corners { tl: 0.0, tr: 0.0, br: 0.0, bl: 0.0 };

    pub const fn uniform(r: f32) -> Corners {
        Corners { tl: r, tr: r, br: r, bl: r }
    }

    pub fn is_zero(&self) -> bool {
        self.tl <= 0.0 && self.tr <= 0.0 && self.br <= 0.0 && self.bl <= 0.0
    }

    /// Each radius moved by `delta` (negative shrinks), never below zero: a border stroked
    /// inside a rounded rect follows the same curve one half-width in.
    pub fn adjust(&self, delta: f32) -> Corners {
        let one = |r: f32| if r <= 0.0 { 0.0 } else { (r + delta).max(0.0) };
        Corners { tl: one(self.tl), tr: one(self.tr), br: one(self.br), bl: one(self.bl) }
    }

    /// Clamped so no pair of radii on a side exceeds it (the css rule).
    pub fn fit(&self, w: f32, h: f32) -> Corners {
        let mut c = *self;
        let scale = [
            (c.tl + c.tr, w),
            (c.bl + c.br, w),
            (c.tl + c.bl, h),
            (c.tr + c.br, h),
        ]
        .iter()
        .filter(|(sum, limit)| *sum > *limit && *sum > 0.0)
        .map(|(sum, limit)| limit / sum)
        .fold(1.0f32, f32::min);
        if scale < 1.0 {
            c = Corners { tl: c.tl * scale, tr: c.tr * scale, br: c.br * scale, bl: c.bl * scale };
        }
        c
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
    /// Corner radii for every background, border, shadow and hover layer after it in the chain.
    Rounded(Corners),
    Border { width: f32, argb: u32 },
    /// Colour painted over the node's clickable layer while the pointer is over it.
    Hover(u32),
    /// A soft drop shadow under the layer's rect; `elevation` is its spread in logical pixels.
    Shadow { elevation: f32, argb: u32 },
    /// Ask the nearest scroll container to bring the node into view when this op appears.
    Reveal,
    /// Clip the node's own painting and its children to the node's rect.
    Clip,
    /// A handler for the secondary (right) button.
    Secondary(i32),
    /// A handler told when the pointer enters and leaves the layer.
    Hoverable(i32),
    /// The colour of a scroll container's thumb.
    Scrollbar(u32),
    /// A handler told when the pointer is pressed on the layer and dragged, wherever it
    /// then goes: the core holds the pointer for the duration.
    Drag(i32),
    /// A handler told when the pointer is pressed outside this node, without taking the
    /// press: what a menu closes on.
    Dismiss(i32),
    /// The text under this layer can be selected with the pointer; the colour is the
    /// highlight's.
    Selectable(u32),
    /// What the pointer looks like over this node: 0 the platform's own, 1 a hand, 2 a text
    /// bar, 3 a column-resize arrow, 4 a grabbing hand.
    Cursor(u32),
    /// A `TEXTFIELD` that holds more than one line: it wraps, it grows, and Enter puts a
    /// line break in rather than doing nothing.
    Multiline,
    /// A handler told what a drag is over and where it was let go: without it a drag knows
    /// where the pointer is and nothing about what is under it.
    DropTarget(i32),
    /// A handler told when a press lands inside this node, so a key can mean one thing in
    /// one part of a window and something else in another.
    FocusRegion(i32),
    /// A bar along one edge of this node — 0 leading, 1 top, 2 trailing, 3 bottom — inside
    /// its rect. What a row that is selected, or a notice that is a warning, is marked with.
    Rule { side: u8, width: f32, argb: u32 },
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
    /// Colour washed over the node's clickable layer while the pointer is held down on it.
    /// Without it a button gives no sign at all that a click landed.
    pub pressed: Option<u32>,
    pub scrollbar: Option<u32>,
    pub reveal: bool,
    pub clip: bool,
    /// What the pointer looks like over this node; 0 is the platform's own.
    pub cursor: u32,
    /// A text field that takes more than one line.
    pub multiline: bool,
    /// Left out of a sweep of the pointer: a line number beside a diff is not part of the
    /// text, and copying it with the text is not what anybody meant.
    pub unselectable: bool,
    /// Space a `Row` or `Column` puts between its children, so a container spaces itself
    /// instead of every caller placing a spacer by hand.
    pub gap: f32,
    /// Runs of this node's own text painted with a colour behind them, as byte offsets into
    /// it: what says *these* words of the line are the ones that changed.
    pub marks: Vec<(usize, usize, u32)>,
    /// Runs of this node's own text drawn in a colour of their own, as byte offsets: what a
    /// line of code coloured by what its words are is made of.
    pub tints: Vec<(usize, usize, u32)>,
}

impl Modifier {
    /// Parse the protocol's flat `[op, args…]` runs.
    pub fn parse(raw: &[f64]) -> Result<Modifier, String> {
        let mut ops = Vec::new();
        let mut i = 0;
        let mut weight = 0.0f32;
        let mut align = None;
        let mut hover = None;
        let mut scrollbar = None;
        let mut reveal = false;
        let mut clip = false;
        let mut cursor = 0u32;
        let mut gap = 0.0f32;
        let mut pressed = None;
        let mut unselectable = false;
        let mut multiline = false;
        let mut marks: Vec<(usize, usize, u32)> = Vec::new();
        let mut tints: Vec<(usize, usize, u32)> = Vec::new();
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
                    ops.push(ModOp::Clickable(handler_index(a[0], "clickable")?));
                    i += 2;
                }
                x if x == 10.0 => {
                    let a = take(4)?;
                    ops.push(ModOp::Rounded(Corners {
                        tl: non_negative(a[0], "rounded")?,
                        tr: non_negative(a[1], "rounded")?,
                        br: non_negative(a[2], "rounded")?,
                        bl: non_negative(a[3], "rounded")?,
                    }));
                    i += 5;
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
                x if x == 16.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Secondary(handler_index(a[0], "secondary")?));
                    i += 2;
                }
                x if x == 17.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Hoverable(handler_index(a[0], "hoverable")?));
                    i += 2;
                }
                x if x == 18.0 => {
                    let a = take(1)?;
                    let argb = argb_from_f64(a[0])?;
                    scrollbar = Some(argb);
                    ops.push(ModOp::Scrollbar(argb));
                    i += 2;
                }
                x if x == 19.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Drag(handler_index(a[0], "draggable")?));
                    i += 2;
                }
                x if x == 20.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Selectable(argb_from_f64(a[0])?));
                    i += 2;
                }
                x if x == 23.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::Dismiss(handler_index(a[0], "dismiss")?));
                    i += 2;
                }
                x if x == 22.0 => {
                    multiline = true;
                    ops.push(ModOp::Multiline);
                    i += 1;
                }
                x if x == 24.0 => {
                    let a = take(1)?;
                    gap = finite(a[0], "gap")?.max(0.0);
                    i += 2;
                }
                x if x == 25.0 => {
                    let a = take(1)?;
                    pressed = Some(argb_from_f64(a[0])?);
                    i += 2;
                }
                x if x == 26.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::DropTarget(handler_index(a[0], "drop_target")?));
                    i += 2;
                }
                x if x == 27.0 => {
                    let a = take(1)?;
                    ops.push(ModOp::FocusRegion(handler_index(a[0], "focus_region")?));
                    i += 2;
                }
                x if x == 28.0 => {
                    unselectable = true;
                    i += 1;
                }
                x if x == 29.0 => {
                    let a = take(3)?;
                    let from = non_negative(a[0], "mark")? as usize;
                    let to = non_negative(a[1], "mark")? as usize;
                    if to > from {
                        marks.push((from, to, argb_from_f64(a[2])?));
                    }
                    i += 4;
                }
                x if x == 31.0 => {
                    let a = take(3)?;
                    let side = finite(a[0], "rule")?;
                    if !(0.0..=3.0).contains(&side) || side.fract() != 0.0 {
                        return Err(format!("rule side must be 0..=3, got {}", side));
                    }
                    ops.push(ModOp::Rule {
                        side: side as u8,
                        width: non_negative(a[1], "rule")?,
                        argb: argb_from_f64(a[2])?,
                    });
                    i += 4;
                }
                x if x == 30.0 => {
                    let a = take(3)?;
                    let from = non_negative(a[0], "tint")? as usize;
                    let to = non_negative(a[1], "tint")? as usize;
                    if to > from {
                        tints.push((from, to, argb_from_f64(a[2])?));
                    }
                    i += 4;
                }
                x if x == 21.0 => {
                    let a = take(1)?;
                    let kind = finite(a[0], "cursor")?;
                    if kind < 0.0 || kind > 4.0 || kind.fract() != 0.0 {
                        return Err(format!("cursor must be 0..=4, got {}", kind));
                    }
                    cursor = kind as u32;
                    ops.push(ModOp::Cursor(cursor));
                    i += 2;
                }
                _ => return Err(format!("unknown modifier op {} at {}", op, i)),
            }
        }
        Ok(Modifier { ops, weight, align, hover, pressed, scrollbar, reveal, clip, cursor, multiline, unselectable, gap, marks, tints })
    }
}

/// A handler index carried by a modifier op: a non-negative whole number.
fn handler_index(v: f64, what: &str) -> Result<i32, String> {
    let h = finite(v, what)?;
    if h < 0.0 || h > i32::MAX as f32 || h.fract() != 0.0 {
        return Err(format!("{} handler index must be a non-negative int, got {}", what, h));
    }
    Ok(h as i32)
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
/// `corners` are the radii in effect at that point of the chain (all zero = square).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Layer {
    Shadow { rect: Rect, corners: Corners, elevation: f32, argb: u32 },
    Background { rect: Rect, argb: u32, corners: Corners },
    Border { rect: Rect, argb: u32, width: f32, corners: Corners },
    Click { rect: Rect, handler: i32, hover: Option<u32>, pressed: Option<u32>, corners: Corners },
    /// Where something being dragged may be let go.
    Drop { rect: Rect, handler: i32 },
    /// A part of the window a press gives the keyboard to.
    Focus { rect: Rect, handler: i32 },
    /// The right button over this rect.
    Secondary { rect: Rect, handler: i32 },
    /// Told when the pointer enters and leaves this rect.
    Hoverable { rect: Rect, handler: i32 },
    /// Told when the pointer is pressed here and dragged.
    Drag { rect: Rect, handler: i32 },
    /// The text inside here can be selected; `argb` is the highlight's colour.
    Select { rect: Rect, argb: u32 },
    /// Told when the pointer is pressed anywhere but inside this rect. The press itself goes
    /// on to whatever it landed on, so a menu can close and the click still count.
    Dismiss { rect: Rect, handler: i32 },
    /// A bar along one edge, inside the rect: 0 leading, 1 top, 2 trailing, 3 bottom.
    Rule { rect: Rect, side: u8, width: f32, argb: u32 },
}

/// A retained canvas draw command in the canvas's own coordinates.
#[derive(Clone, PartialEq, Debug)]
pub enum CanvasCmd {
    Rect { x: f32, y: f32, w: f32, h: f32, argb: u32 },
    Circle { cx: f32, cy: f32, r: f32, argb: u32 },
    Line { x1: f32, y1: f32, x2: f32, y2: f32, argb: u32, stroke: f32 },
    /// A quadratic curve from one point to another, bending towards `(cx, cy)`.
    Curve { x1: f32, y1: f32, cx: f32, cy: f32, x2: f32, y2: f32, argb: u32, stroke: f32 },
    Text { x: f32, y: f32, text: Arc<str>, style: Style },
    /// An arbitrary polygon: filled when `stroke` is 0, outlined otherwise. The points are
    /// pairs, in order, and the shape is closed.
    Path { points: Vec<(f32, f32)>, argb: u32, stroke: f32 },
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
        let m = Modifier::parse(&[10.0, 8.0, 8.0, 0.0, 0.0, 11.0, 1.0, 4278190080.0, 12.0, 4278190335.0, 13.0, 4.0, 1090519040.0, 14.0, 15.0]).unwrap();
        assert_eq!(m.ops.len(), 6);
        assert_eq!(m.ops[0], ModOp::Rounded(Corners { tl: 8.0, tr: 8.0, br: 0.0, bl: 0.0 }));
        assert_eq!(m.hover, Some(0xFF0000FF));
        assert!(m.reveal && m.clip);
        assert!(Modifier::parse(&[10.0, 8.0, 8.0, 0.0]).is_err(), "rounded takes four radii");
        assert!(Modifier::parse(&[10.0, -1.0, 0.0, 0.0, 0.0]).is_err());
        assert!(Modifier::parse(&[11.0, 1.0]).is_err());
    }

    #[test]
    fn corner_math() {
        let c = Corners::uniform(10.0);
        assert!(!c.is_zero() && Corners::NONE.is_zero());
        assert_eq!(c.adjust(-1.0), Corners::uniform(9.0));
        // a zero corner stays square however the radii are adjusted
        let mixed = Corners { tl: 4.0, tr: 0.0, br: 0.0, bl: 4.0 };
        assert_eq!(mixed.adjust(-2.0), Corners { tl: 2.0, tr: 0.0, br: 0.0, bl: 2.0 });
        assert_eq!(mixed.adjust(-9.0), Corners { tl: 0.0, tr: 0.0, br: 0.0, bl: 0.0 });
        // radii that do not fit are scaled down together
        assert_eq!(Corners::uniform(10.0).fit(10.0, 100.0), Corners::uniform(5.0));
        assert_eq!(Corners::uniform(4.0).fit(100.0, 100.0), Corners::uniform(4.0));
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
