//! Text shaping, measurement and glyph rasterisation with cosmic-text.
//!
//! One `FontSystem` (the system font database) is shared by every core in the process behind a
//! mutex: loading it costs hundreds of milliseconds and it is only ever borrowed briefly. Each
//! core owns its own shaped-text cache and glyph image cache.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight, Wrap};

use crate::types::{Size, Style};

/// Width factor of the monospace estimate used when no font can shape the text.
pub const FALLBACK_ADVANCE: f32 = 0.6;
/// Line height as a multiple of the font size.
pub const LINE_HEIGHT_FACTOR: f32 = 1.25;

static GLOBAL_FONTS: OnceLock<Mutex<FontSystem>> = OnceLock::new();

fn global_fonts() -> MutexGuard<'static, FontSystem> {
    let m = GLOBAL_FONTS.get_or_init(|| Mutex::new(system_font_system()));
    // A panic while shaping in another core must not take every core down with it.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The system font database with sensible generic-family defaults per platform.
fn system_font_system() -> FontSystem {
    let mut db = cosmic_text::fontdb::Database::new();
    db.load_system_fonts();
    if cfg!(target_os = "macos") {
        db.set_sans_serif_family("Helvetica");
        db.set_serif_family("Times");
        db.set_monospace_family("Menlo");
    } else if cfg!(target_os = "windows") {
        db.set_sans_serif_family("Segoe UI");
        db.set_serif_family("Times New Roman");
        db.set_monospace_family("Consolas");
    } else {
        db.set_sans_serif_family("DejaVu Sans");
        db.set_serif_family("DejaVu Serif");
        db.set_monospace_family("DejaVu Sans Mono");
    }
    FontSystem::new_with_locale_and_db("en-US".to_string(), db)
}

/// Cache key of a shaped text: the text, its style and the width it was wrapped at.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TextKey {
    pub text: Arc<str>,
    pub style: Style,
    /// `f32::to_bits` of the max width (`INFINITY` = unbounded).
    pub max_w_bits: u32,
}

impl TextKey {
    pub fn new(text: Arc<str>, style: Style, max_w: f32) -> TextKey {
        let max_w = if max_w.is_finite() { max_w.max(0.0) } else { f32::INFINITY };
        TextKey { text, style, max_w_bits: max_w.to_bits() }
    }
    pub fn max_w(&self) -> f32 {
        f32::from_bits(self.max_w_bits)
    }
}

pub struct TextEntry {
    pub buffer: Buffer,
    pub size: Size,
    /// True when cosmic-text produced no glyphs for a non-blank text (no usable font); the size
    /// is then the monospace estimate and paint draws nothing.
    pub fallback: bool,
    pub used: u64,
}

pub struct TextSystem {
    /// A private font system (tests); `None` uses the shared system one.
    own_fonts: Option<Box<FontSystem>>,
    swash: SwashCache,
    cache: HashMap<TextKey, TextEntry>,
    epoch: u64,
    sweep_threshold: usize,
}

enum Fonts<'a> {
    Own(&'a mut FontSystem),
    Global(MutexGuard<'static, FontSystem>),
}

impl std::ops::Deref for Fonts<'_> {
    type Target = FontSystem;
    fn deref(&self) -> &FontSystem {
        match self {
            Fonts::Own(f) => f,
            Fonts::Global(g) => g,
        }
    }
}

impl std::ops::DerefMut for Fonts<'_> {
    fn deref_mut(&mut self) -> &mut FontSystem {
        match self {
            Fonts::Own(f) => f,
            Fonts::Global(g) => g,
        }
    }
}

fn fonts_of(own: &mut Option<Box<FontSystem>>) -> Fonts<'_> {
    match own {
        Some(f) => Fonts::Own(f),
        None => Fonts::Global(global_fonts()),
    }
}

impl TextSystem {
    /// Uses the shared system font database, loading it now (once per process, ~0.1–0.5 s)
    /// rather than inside the first layout.
    pub fn system() -> TextSystem {
        drop(global_fonts());
        TextSystem::with(None)
    }

    /// Uses a private font system; an empty database gives the deterministic monospace estimate.
    pub fn with_fonts(fonts: FontSystem) -> TextSystem {
        TextSystem::with(Some(Box::new(fonts)))
    }

    /// No fonts at all: every measurement is the monospace estimate (unit tests).
    pub fn monospace_only() -> TextSystem {
        let db = cosmic_text::fontdb::Database::new();
        TextSystem::with_fonts(FontSystem::new_with_locale_and_db("en-US".to_string(), db))
    }

    /// The stand-in used while the real state is lent to a Python callback; never shapes.
    pub fn placeholder() -> TextSystem {
        TextSystem::with(None)
    }

    fn with(own_fonts: Option<Box<FontSystem>>) -> TextSystem {
        TextSystem {
            own_fonts,
            swash: SwashCache::new(),
            cache: HashMap::new(),
            epoch: 0,
            sweep_threshold: 1024,
        }
    }

    pub fn line_height(style: Style) -> f32 {
        (style.size() * LINE_HEIGHT_FACTOR).ceil()
    }

    /// Measure (shaping and caching on first use).
    pub fn measure(&mut self, key: &TextKey) -> Size {
        self.entry(key).size
    }

    /// The shaped entry for a key, shaping it if needed.
    pub fn entry(&mut self, key: &TextKey) -> &TextEntry {
        let epoch = self.epoch;
        if !self.cache.contains_key(key) {
            let entry = shape(&mut fonts_of(&mut self.own_fonts), key, epoch);
            self.cache.insert(key.clone(), entry);
        }
        let e = self.cache.get_mut(key).expect("just inserted");
        e.used = epoch;
        e
    }

    /// Call once per layout pass: ages the cache and evicts stale entries when it grew large.
    pub fn end_layout(&mut self) {
        self.epoch += 1;
        if self.cache.len() > self.sweep_threshold {
            let keep_from = self.epoch.saturating_sub(2);
            self.cache.retain(|_, e| e.used >= keep_from);
            self.sweep_threshold = (self.cache.len() * 2).max(1024);
        }
        if self.swash.image_cache.len() > 16 * 1024 {
            self.swash = SwashCache::new();
        }
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Rasterise the glyphs of a shaped text into a premultiplied RGBA8 buffer.
    /// `origin` is in logical pixels; `scale` maps logical to physical pixels; `clip` is an
    /// optional `(x0, y0, x1, y1)` window in physical pixels (exclusive ends) outside of which
    /// nothing is written.
    pub fn draw(
        &mut self,
        key: &TextKey,
        origin: (f32, f32),
        scale: f32,
        argb: u32,
        target: &mut [u8],
        target_w: u32,
        target_h: u32,
        clip: Option<(i32, i32, i32, i32)>,
    ) {
        let epoch = self.epoch;
        let own = &mut self.own_fonts;
        let swash = &mut self.swash;
        let cache = &mut self.cache;
        if !cache.contains_key(key) {
            let entry = shape(&mut fonts_of(own), key, epoch);
            cache.insert(key.clone(), entry);
        }
        let Some(entry) = cache.get(key) else { return };
        if entry.fallback {
            return;
        }
        let mut fonts = fonts_of(own);
        let (a, r, g, b) = crate::types::argb_channels(argb);
        let (cx0, cy0, cx1, cy1) = match clip {
            Some((x0, y0, x1, y1)) => (x0.max(0), y0.max(0), x1.min(target_w as i32), y1.min(target_h as i32)),
            None => (0, 0, target_w as i32, target_h as i32),
        };
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }
        for run in entry.buffer.layout_runs() {
            for glyph in run.glyphs {
                let phys = glyph.physical((origin.0, origin.1 + run.line_y), scale);
                let Some(image) = swash.get_image(&mut fonts, phys.cache_key) else { continue };
                let x0 = phys.x + image.placement.left;
                let y0 = phys.y - image.placement.top;
                let iw = image.placement.width as i32;
                let ih = image.placement.height as i32;
                match image.content {
                    SwashContent::Mask => {
                        for iy in 0..ih {
                            let py = y0 + iy;
                            if py < cy0 || py >= cy1 {
                                continue;
                            }
                            for ix in 0..iw {
                                let px = x0 + ix;
                                if px < cx0 || px >= cx1 {
                                    continue;
                                }
                                let Some(&m) = image.data.get((iy * iw + ix) as usize) else { continue };
                                if m == 0 {
                                    continue;
                                }
                                let alpha = (m as u32 * a as u32 + 127) / 255;
                                blend(target, target_w, px as u32, py as u32, r, g, b, alpha as u8);
                            }
                        }
                    }
                    SwashContent::Color => {
                        for iy in 0..ih {
                            let py = y0 + iy;
                            if py < cy0 || py >= cy1 {
                                continue;
                            }
                            for ix in 0..iw {
                                let px = x0 + ix;
                                if px < cx0 || px >= cx1 {
                                    continue;
                                }
                                let i = ((iy * iw + ix) * 4) as usize;
                                let Some(px4) = image.data.get(i..i + 4) else { continue };
                                let alpha = (px4[3] as u32 * a as u32 + 127) / 255;
                                if alpha == 0 {
                                    continue;
                                }
                                blend(target, target_w, px as u32, py as u32, px4[0], px4[1], px4[2], alpha as u8);
                            }
                        }
                    }
                    SwashContent::SubpixelMask => {
                        for iy in 0..ih {
                            let py = y0 + iy;
                            if py < cy0 || py >= cy1 {
                                continue;
                            }
                            for ix in 0..iw {
                                let px = x0 + ix;
                                if px < cx0 || px >= cx1 {
                                    continue;
                                }
                                let i = ((iy * iw + ix) * 4) as usize;
                                let Some(px4) = image.data.get(i..i + 4) else { continue };
                                let m = px4[0].max(px4[1]).max(px4[2]) as u32;
                                let alpha = (m * a as u32 + 127) / 255;
                                if alpha == 0 {
                                    continue;
                                }
                                blend(target, target_w, px as u32, py as u32, r, g, b, alpha as u8);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Source-over of a straight colour with coverage `alpha` onto a premultiplied RGBA8 pixel.
#[inline]
fn blend(target: &mut [u8], w: u32, x: u32, y: u32, r: u8, g: u8, b: u8, alpha: u8) {
    let i = ((y * w + x) * 4) as usize;
    let Some(px) = target.get_mut(i..i + 4) else { return };
    let a = alpha as u32;
    let inv = 255 - a;
    let sr = (r as u32 * a + 127) / 255;
    let sg = (g as u32 * a + 127) / 255;
    let sb = (b as u32 * a + 127) / 255;
    px[0] = (sr + (px[0] as u32 * inv + 127) / 255).min(255) as u8;
    px[1] = (sg + (px[1] as u32 * inv + 127) / 255).min(255) as u8;
    px[2] = (sb + (px[2] as u32 * inv + 127) / 255).min(255) as u8;
    px[3] = (a + (px[3] as u32 * inv + 127) / 255).min(255) as u8;
}

fn shape(fonts: &mut FontSystem, key: &TextKey, epoch: u64) -> TextEntry {
    let style = key.style;
    let size = style.size().max(1.0);
    let line_height = TextSystem::line_height(style);
    let max_w = key.max_w();
    let metrics = Metrics::new(size, line_height);
    if fonts.db().is_empty() {
        // cosmic-text panics ("no default font found") with no faces at all; estimate instead.
        return TextEntry {
            buffer: Buffer::new_empty(metrics),
            size: monospace_estimate(&key.text, style, max_w),
            fallback: true,
            used: epoch,
        };
    }
    let mut buffer = Buffer::new(fonts, metrics);
    if style.nowrap {
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
    } else {
        buffer.set_size(if max_w.is_finite() { Some(max_w) } else { None }, None);
    }
    let attrs = Attrs::new()
        .family(if style.mono { Family::Monospace } else { Family::SansSerif })
        .weight(if style.bold { Weight::BOLD } else { Weight::NORMAL });
    buffer.set_text(&key.text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);

    let mut w = 0.0f32;
    let mut lines = 0usize;
    let mut glyphs = 0usize;
    for run in buffer.layout_runs() {
        w = w.max(run.line_w);
        lines += 1;
        glyphs += run.glyphs.len();
    }
    let has_ink = key.text.chars().any(|c| !c.is_whitespace());
    let fallback = has_ink && glyphs == 0;
    let size = if fallback {
        monospace_estimate(&key.text, style, max_w)
    } else {
        Size::new(w.ceil(), (lines.max(1) as f32) * line_height)
    };
    TextEntry { buffer, size, fallback, used: epoch }
}

/// Deterministic estimate for environments without fonts: `0.6 × size` per char,
/// greedy wrapping at whole characters.
pub fn monospace_estimate(text: &str, style: Style, max_w: f32) -> Size {
    let advance = style.size() * FALLBACK_ADVANCE;
    let line_height = TextSystem::line_height(style);
    let per_line = if max_w.is_finite() && advance > 0.0 {
        ((max_w / advance).floor() as usize).max(1)
    } else {
        usize::MAX
    };
    let mut lines = 0usize;
    let mut widest = 0usize;
    for line in text.split('\n') {
        let n = line.chars().count();
        if n == 0 {
            lines += 1;
            continue;
        }
        let wrapped = n.div_ceil(per_line);
        lines += wrapped;
        widest = widest.max(n.min(per_line));
    }
    // round to 1/1000 px first so float noise (14 × 0.6 × 5 = 42.0000017) does not bump the ceil
    let raw = widest as f64 * style.size() as f64 * FALLBACK_ADVANCE as f64;
    let width = ((raw * 1000.0).round() / 1000.0).ceil() as f32;
    Size::new(width, (lines.max(1) as f32) * line_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{STYLE_MONO, STYLE_NOWRAP};

    #[test]
    fn monospace_fallback_is_deterministic() {
        let mut ts = TextSystem::monospace_only();
        let key = TextKey::new(Arc::from("hello"), Style::DEFAULT, f32::INFINITY);
        let s = ts.measure(&key);
        assert_eq!(s.w, (5.0 * 14.0 * FALLBACK_ADVANCE).ceil());
        assert_eq!(s.h, 18.0);
        // wraps at whole characters: 10 chars at 8.4 per char in 50px → 5 per line → 2 lines
        let key = TextKey::new(Arc::from("0123456789"), Style::DEFAULT, 50.0);
        let s = ts.measure(&key);
        assert_eq!(s.h, 36.0);
        assert_eq!(s.w, 42.0);
        // empty text is one line high and zero wide
        let key = TextKey::new(Arc::from(""), Style::DEFAULT, f32::INFINITY);
        let s = ts.measure(&key);
        assert_eq!(s, Size::new(0.0, 18.0));
        assert_eq!(ts.cache_len(), 3);
    }

    #[test]
    fn system_fonts_measure_something() {
        let mut ts = TextSystem::system();
        let key = TextKey::new(Arc::from("hello world"), Style::DEFAULT, f32::INFINITY);
        let s = ts.measure(&key);
        assert!(s.w > 0.0 && s.h >= 14.0, "{:?}", s);
        let bold = TextKey::new(Arc::from("hello world"), Style::new(14.0, 0xFF000000, true), f32::INFINITY);
        let sb = ts.measure(&bold);
        assert!(sb.w >= s.w);
        let mut buf = vec![0u8; 200 * 40 * 4];
        ts.draw(&key, (2.0, 2.0), 1.0, 0xFF000000, &mut buf, 200, 40, None);
        assert!(buf.iter().any(|&b| b != 0), "glyphs were rasterised");
        // a clip window that misses the text draws nothing
        let mut buf = vec![0u8; 200 * 40 * 4];
        ts.draw(&key, (2.0, 2.0), 1.0, 0xFF000000, &mut buf, 200, 40, Some((150, 0, 200, 40)));
        assert!(buf.iter().all(|&b| b == 0));
        // a monospace, no-wrap style shapes on one line
        let mono = TextKey::new(Arc::from("0123456789 0123456789 0123456789"), Style::with_flags(14.0, 0xFF000000, STYLE_MONO | STYLE_NOWRAP), 40.0);
        let sm = ts.measure(&mono);
        assert_eq!(sm.h, 18.0, "{:?}", sm);
        assert!(sm.w > 40.0);
    }
}
