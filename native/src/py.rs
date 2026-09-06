//! The Python `Core` class: the boundary. Every method catches panics (→ `RuntimeError`, the
//! core is poisoned), converts malformed input to `ValueError`, and never holds a Python object
//! per node. The state lives in a mutex-guarded slot so the `Layout` callback path can lend it
//! back to `measure_child` / `place_child` while Python runs.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use pyo3::buffer::PyBuffer;
use pyo3::exceptions::{PyIndexError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList, PyString, PyTuple};

use crate::commit;
use crate::input;
use crate::layout::{self, CustomResult, MeasureHost};
use crate::paint;
use crate::text::TextSystem;
use crate::tree::{Inner, NodeId};
use crate::types::*;

struct CustomFrame {
    children: Vec<NodeId>,
    measured: Vec<bool>,
    placed: Vec<Option<(f32, f32)>>,
}

struct Slot {
    /// `None` only while a layout pass owns the state on the Rust stack.
    inner: Option<Box<Inner>>,
    frames: Vec<CustomFrame>,
    measure: Option<Py<PyAny>>,
    /// Nesting depth of Layout callbacks currently running in Python.
    depth: u32,
    /// First exception raised by a Layout callback in the current pass.
    layout_error: Option<PyErr>,
}

#[pyclass(name = "Core", module = "basedpython_ui._native")]
pub struct Core {
    slot: Mutex<Slot>,
    poisoned: AtomicBool,
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn finite(v: f64, what: &str) -> PyResult<f32> {
    if v.is_finite() {
        Ok(v as f32)
    } else {
        Err(PyValueError::new_err(format!("{} must be finite, got {}", what, v)))
    }
}

fn constraints_from(min_w: f64, max_w: f64, min_h: f64, max_h: f64) -> PyResult<Constraints> {
    let lo = |v: f64, what: &str| -> PyResult<f32> {
        let f = finite(v, what)?;
        if f < 0.0 {
            return Err(PyValueError::new_err(format!("{} must be >= 0, got {}", what, v)));
        }
        Ok(f)
    };
    let hi = |v: f64, what: &str| -> PyResult<f32> {
        if v.is_nan() || v < 0.0 {
            return Err(PyValueError::new_err(format!("{} must be >= 0 or inf, got {}", what, v)));
        }
        Ok(v as f32)
    };
    let c = Constraints { min_w: lo(min_w, "min_w")?, max_w: hi(max_w, "max_w")?, min_h: lo(min_h, "min_h")?, max_h: hi(max_h, "max_h")? };
    if c.min_w > c.max_w || c.min_h > c.max_h {
        return Err(PyValueError::new_err("min constraints must not exceed max constraints"));
    }
    Ok(c)
}

/// A contiguous buffer (`array('i')` / `array('d')`, memoryview, …) copied once into a Vec.
/// An empty buffer is accepted even though its data pointer is null (pyo3 would reject it).
fn buffer_to_vec<T: pyo3::buffer::Element + Copy>(py: Python<'_>, obj: &Bound<'_, PyAny>, what: &str, ty: &str) -> PyResult<Vec<T>> {
    if let Ok(0) = obj.len() {
        return Ok(Vec::new());
    }
    let buf = PyBuffer::<T>::get(obj)
        .map_err(|e| PyTypeError::new_err(format!("{} must be a contiguous buffer of {} (e.g. array('{}')): {}", what, ty, if ty == "i32" { "i" } else { "d" }, e)))?;
    buf.to_vec(py)
}

/// `strs` as `&str`s: a list or tuple of `str`.
fn extract_strs<'py>(strs: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyString>>> {
    let items: Vec<Bound<'py, PyAny>> = if let Ok(list) = strs.cast::<PyList>() {
        list.iter().collect()
    } else if let Ok(tuple) = strs.cast::<PyTuple>() {
        tuple.iter().collect()
    } else {
        return Err(PyTypeError::new_err("strs must be a list or tuple of str"));
    };
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            item.cast_into::<PyString>()
                .map_err(|_| PyTypeError::new_err(format!("strs[{}] is not a str", i)))
        })
        .collect()
}

impl Core {
    pub fn with_inner(inner: Inner) -> Core {
        Core {
            slot: Mutex::new(Slot { inner: Some(Box::new(inner)), frames: Vec::new(), measure: None, depth: 0, layout_error: None }),
            poisoned: AtomicBool::new(false),
        }
    }

    fn check(&self) -> PyResult<()> {
        if self.poisoned.load(Ordering::Acquire) {
            Err(PyRuntimeError::new_err("the native core is poisoned by an earlier internal error; create a new Core"))
        } else {
            Ok(())
        }
    }

    fn lock(&self) -> PyResult<MutexGuard<'_, Slot>> {
        self.slot.lock().map_err(|_| {
            self.poisoned.store(true, Ordering::Release);
            PyRuntimeError::new_err("the native core is poisoned (its lock was poisoned)")
        })
    }

    fn poison(&self, payload: Box<dyn Any + Send>) -> PyErr {
        self.poisoned.store(true, Ordering::Release);
        PyRuntimeError::new_err(format!(
            "basedpython_ui native core panicked: {}; the core is poisoned and every later call raises",
            panic_message(&payload)
        ))
    }

    /// Run `f` on the state. `mutating` operations are refused inside a Layout callback.
    pub fn with_state<R>(&self, mutating: bool, f: impl FnOnce(&mut Inner) -> PyResult<R>) -> PyResult<R> {
        self.check()?;
        let mut slot = self.lock()?;
        if mutating && slot.depth > 0 {
            return Err(PyRuntimeError::new_err("this Core method is not allowed inside a Layout measure callback"));
        }
        let inner = slot
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("the core state is busy in a layout pass (re-entrant call)"))?;
        match catch_unwind(AssertUnwindSafe(|| f(inner))) {
            Ok(r) => r,
            Err(payload) => Err(self.poison(payload)),
        }
    }

    fn take_for_layout(&self, measure: Option<Py<PyAny>>) -> PyResult<Box<Inner>> {
        let mut slot = self.lock()?;
        if slot.depth > 0 {
            return Err(PyRuntimeError::new_err("Core.layout() is not allowed inside a Layout measure callback"));
        }
        let inner = slot
            .inner
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("the core state is busy in a layout pass (re-entrant call)"))?;
        slot.measure = measure;
        slot.layout_error = None;
        slot.frames.clear();
        Ok(inner)
    }
}

/// The Python side of `MeasureHost`: hands the state back to the slot around the callback.
struct PyHost<'a, 'py> {
    core: &'a Core,
    py: Python<'py>,
}

impl MeasureHost for PyHost<'_, '_> {
    type Error = PyErr;

    fn measure_custom(&mut self, inner: &mut Inner, _node: NodeId, handler: i32, children: &[NodeId], c: Constraints) -> Result<CustomResult, PyErr> {
        let n = children.len();
        let measure = {
            let slot = self.core.lock()?;
            match &slot.measure {
                Some(m) => m.clone_ref(self.py),
                None => {
                    return Err(PyRuntimeError::new_err(
                        "layout callback required: the tree contains a Layout node but Core.layout() was called without a measure callable",
                    ))
                }
            }
        };
        let real = Box::new(std::mem::replace(inner, Inner::placeholder()));
        {
            let mut slot = self.core.lock()?;
            slot.inner = Some(real);
            slot.frames.push(CustomFrame { children: children.to_vec(), measured: vec![false; n], placed: vec![None; n] });
            slot.depth += 1;
        }
        let result = measure.call1(
            self.py,
            (handler, n, c.min_w as f64, c.max_w as f64, c.min_h as f64, c.max_h as f64),
        );
        let (real, frame) = {
            let mut slot = self.core.lock()?;
            slot.depth = slot.depth.saturating_sub(1);
            (slot.inner.take(), slot.frames.pop())
        };
        match real {
            Some(b) => *inner = *b,
            None => panic!("internal: core state was lost during a Layout callback"),
        }
        let Some(frame) = frame else { panic!("internal: layout frame stack corrupted") };
        let value = result?;
        let (w, h): (f64, f64) = value
            .extract(self.py)
            .map_err(|_| PyTypeError::new_err("the Layout measure callback must return (width, height)"))?;
        if !w.is_finite() || !h.is_finite() || w < 0.0 || h < 0.0 {
            return Err(PyValueError::new_err(format!("the Layout measure callback returned an invalid size ({}, {})", w, h)));
        }
        Ok(CustomResult { size: Size::new(w as f32, h as f32), measured: frame.measured, placed: frame.placed })
    }

    fn record_error(&mut self, err: PyErr) {
        if let Ok(mut slot) = self.core.slot.lock() {
            if slot.layout_error.is_none() {
                slot.layout_error = Some(err);
            }
        }
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

type EventTuple = (i32, f64, f64, i32, String);

#[pymethods]
impl Core {
    #[new]
    #[pyo3(signature = (width, height, scale = 1.0))]
    fn new(width: f64, height: f64, scale: f64) -> PyResult<Core> {
        let w = finite(width, "width")?;
        let h = finite(height, "height")?;
        let s = finite(scale, "scale")?;
        if w < 0.0 || h < 0.0 {
            return Err(PyValueError::new_err("width and height must be >= 0"));
        }
        if s <= 0.0 {
            return Err(PyValueError::new_err("scale must be > 0"));
        }
        match catch_unwind(|| Inner::new(w, h, s, TextSystem::system())) {
            Ok(inner) => Ok(Core::with_inner(inner)),
            Err(payload) => Err(PyRuntimeError::new_err(format!("failed to create the native core: {}", panic_message(&payload)))),
        }
    }

    /// One frame's fragment. Returns the disposed scope ids. `floats` is accepted for protocol
    /// compatibility and ignored (canvas commands arrive through `set_canvas_commands`).
    #[pyo3(signature = (ints, floats, strs, ranges, new_modifiers, new_styles))]
    fn commit(
        &self,
        py: Python<'_>,
        ints: &Bound<'_, PyAny>,
        floats: &Bound<'_, PyAny>,
        strs: &Bound<'_, PyAny>,
        ranges: Vec<(i64, i64, i64)>,
        new_modifiers: Vec<(i64, Vec<f64>)>,
        new_styles: Vec<(i64, f64, f64, i64)>,
    ) -> PyResult<Vec<i32>> {
        let t0 = Instant::now();
        let _ = floats;
        let ints_vec: Vec<i32> = buffer_to_vec(py, ints, "ints", "i32")?;
        let bound_strs = extract_strs(strs)?;
        let str_refs: Vec<&str> = bound_strs.iter().map(|s| s.to_str()).collect::<PyResult<_>>()?;
        self.with_state(true, |inner| {
            let r = commit::commit(inner, &ints_vec, &str_refs, &ranges, &new_modifiers, &new_styles)
                .map_err(|e| PyValueError::new_err(e.0));
            inner.timings[0] = ms(t0);
            r
        })
    }

    /// Lay the tree out. `measure` is the callback for `Layout` nodes (see the protocol).
    #[pyo3(signature = (measure = None))]
    fn layout(&self, py: Python<'_>, measure: Option<Bound<'_, PyAny>>) -> PyResult<()> {
        self.check()?;
        if let Some(m) = &measure {
            if !m.is_callable() {
                return Err(PyTypeError::new_err("measure must be callable"));
            }
        }
        let t0 = Instant::now();
        let mut inner = self.take_for_layout(measure.map(|m| m.unbind()))?;
        let mut host = PyHost { core: self, py };
        let result = catch_unwind(AssertUnwindSafe(|| layout::layout_tree(&mut *inner, &mut host)));
        inner.timings[1] = ms(t0);
        let pending = {
            let mut slot = self.lock()?;
            slot.inner = Some(inner);
            slot.measure = None;
            slot.frames.clear();
            slot.depth = 0;
            slot.layout_error.take()
        };
        if let Err(payload) = result {
            return Err(self.poison(payload));
        }
        match pending {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Only valid inside a `Layout` measure callback: lay out child `index` under constraints.
    fn measure_child(&self, py: Python<'_>, index: usize, min_w: f64, max_w: f64, min_h: f64, max_h: f64) -> PyResult<(f64, f64)> {
        self.check()?;
        let c = constraints_from(min_w, max_w, min_h, max_h)?;
        let (mut inner, child, had_error) = {
            let mut slot = self.lock()?;
            if slot.depth == 0 {
                return Err(PyRuntimeError::new_err("measure_child is only valid inside a Layout measure callback"));
            }
            let frame = slot.frames.last().ok_or_else(|| PyRuntimeError::new_err("internal: no layout frame"))?;
            let child = *frame
                .children
                .get(index)
                .ok_or_else(|| PyIndexError::new_err(format!("child index {} out of range ({} children)", index, frame.children.len())))?;
            let had_error = slot.layout_error.is_some();
            let inner = slot
                .inner
                .take()
                .ok_or_else(|| PyRuntimeError::new_err("the core state is busy (nested measure_child while one is running)"))?;
            (inner, child, had_error)
        };
        let mut host = PyHost { core: self, py };
        let result = catch_unwind(AssertUnwindSafe(|| layout::layout_node(&mut *inner, child, c, &mut host)));
        let new_error = {
            let mut slot = self.lock()?;
            slot.inner = Some(inner);
            if let Some(frame) = slot.frames.last_mut() {
                if let Some(m) = frame.measured.get_mut(index) {
                    *m = true;
                }
            }
            if !had_error {
                slot.layout_error.as_ref().map(|e| e.clone_ref(py))
            } else {
                None
            }
        };
        match result {
            Ok(size) => match new_error {
                Some(e) => Err(e),
                None => Ok((size.w as f64, size.h as f64)),
            },
            Err(payload) => Err(self.poison(payload)),
        }
    }

    /// Only valid inside a `Layout` measure callback: place child `index` at `(x, y)`.
    fn place_child(&self, index: usize, x: f64, y: f64) -> PyResult<()> {
        self.check()?;
        let x = finite(x, "x")?;
        let y = finite(y, "y")?;
        let mut slot = self.lock()?;
        if slot.depth == 0 {
            return Err(PyRuntimeError::new_err("place_child is only valid inside a Layout measure callback"));
        }
        let frame = slot.frames.last_mut().ok_or_else(|| PyRuntimeError::new_err("internal: no layout frame"))?;
        let n = frame.children.len();
        let p = frame
            .placed
            .get_mut(index)
            .ok_or_else(|| PyIndexError::new_err(format!("child index {} out of range ({} children)", index, n)))?;
        *p = Some((x, y));
        Ok(())
    }

    fn paint(&self) -> PyResult<()> {
        let t0 = Instant::now();
        self.with_state(true, |inner| {
            paint::paint(inner);
            inner.timings[2] = ms(t0);
            Ok(())
        })
    }

    fn resize(&self, width: f64, height: f64, scale: f64) -> PyResult<()> {
        let w = finite(width, "width")?;
        let h = finite(height, "height")?;
        let s = finite(scale, "scale")?;
        if w < 0.0 || h < 0.0 || s <= 0.0 {
            return Err(PyValueError::new_err("width and height must be >= 0 and scale > 0"));
        }
        self.with_state(true, |inner| {
            inner.resize(w, h, s);
            Ok(())
        })
    }

    /// Handler index of the interactive node under `(x, y)`, or -1.
    fn hit_test(&self, x: f64, y: f64) -> PyResult<i32> {
        let x = finite(x, "x")?;
        let y = finite(y, "y")?;
        self.with_state(false, |inner| Ok(input::hit_test(inner, x, y).handler))
    }

    /// `(node_id, x, y, w, h)` of the first node showing `text`, or None.
    fn find_text(&self, text: &str) -> PyResult<Option<(u64, f64, f64, f64, f64)>> {
        self.with_state(false, |inner| {
            Ok(inner.find_text(text).map(|(id, r)| (id.to_ffi(), r.x as f64, r.y as f64, r.w as f64, r.h as f64)))
        })
    }

    /// Every visible text in tree order (TEXT, BUTTON labels, TEXTFIELD values or placeholders).
    fn all_text(&self) -> PyResult<Vec<String>> {
        self.with_state(false, |inner| Ok(inner.all_text()))
    }

    /// Handler index of the first text field with this placeholder, or -1.
    fn field_handler(&self, placeholder: &str) -> PyResult<i32> {
        self.with_state(false, |inner| Ok(inner.field_handler(placeholder)))
    }

    fn node_rect(&self, node_id: u64) -> PyResult<Option<(f64, f64, f64, f64)>> {
        self.with_state(false, |inner| {
            Ok(inner.node_rect(NodeId::from_ffi(node_id)).map(|r| (r.x as f64, r.y as f64, r.w as f64, r.h as f64)))
        })
    }

    fn dump(&self) -> PyResult<String> {
        self.with_state(false, |inner| Ok(inner.dump()))
    }

    /// Number of elements (scope groups excluded).
    fn node_count(&self) -> PyResult<usize> {
        self.with_state(false, |inner| Ok(inner.node_count()))
    }

    /// The paint target as premultiplied RGBA8 (opaque background, so effectively straight),
    /// `pixel_width * pixel_height * 4` bytes; zeros before the first `paint()`.
    fn pixels<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        self.with_state(false, |inner| {
            let (pw, ph) = inner.pixel_size();
            match &inner.pixmap {
                Some(p) if (p.width(), p.height()) == (pw, ph) => Ok(PyBytes::new(py, p.data())),
                _ => Ok(PyBytes::new(py, &vec![0u8; (pw as usize) * (ph as usize) * 4])),
            }
        })
    }

    /// `(commit, layout, paint)` durations of the last calls, in milliseconds.
    fn last_frame_ms(&self) -> PyResult<(f64, f64, f64)> {
        self.with_state(false, |inner| Ok((inner.timings[0], inner.timings[1], inner.timings[2])))
    }

    /// `(width, height, scale)` in logical pixels.
    fn size(&self) -> PyResult<(f64, f64, f64)> {
        self.with_state(false, |inner| Ok((inner.width as f64, inner.height as f64, inner.scale as f64)))
    }

    /// `(pixel_width, pixel_height)` of the paint target.
    fn pixel_size(&self) -> PyResult<(u32, u32)> {
        self.with_state(false, |inner| Ok(inner.pixel_size()))
    }

    /// Canvases whose draw block must run: `[(node_id, handler_idx, width, height), …]`.
    /// The pending list is cleared; call after `layout()`.
    fn canvas_requests(&self) -> PyResult<Vec<(u64, i32, f64, f64)>> {
        self.with_state(true, |inner| {
            let pending = std::mem::take(&mut inner.canvas_pending);
            let mut out = Vec::with_capacity(pending.len());
            for id in pending {
                if let Some(n) = inner.nodes.get_mut(id) {
                    n.canvas_pending = false;
                    if n.kind == Kind::Canvas {
                        out.push((id.to_ffi(), n.handler, n.size.w as f64, n.size.h as f64));
                    }
                }
            }
            Ok(out)
        })
    }

    /// Retain the draw commands of a canvas node until replaced.
    fn set_canvas_commands(&self, py: Python<'_>, node_id: u64, floats: &Bound<'_, PyAny>, strs: &Bound<'_, PyAny>) -> PyResult<()> {
        let floats_vec: Vec<f64> = buffer_to_vec(py, floats, "floats", "f64")?;
        let bound_strs = extract_strs(strs)?;
        let str_refs: Vec<&str> = bound_strs.iter().map(|s| s.to_str()).collect::<PyResult<_>>()?;
        self.with_state(true, |inner| {
            let id = NodeId::from_ffi(node_id);
            match inner.nodes.get(id) {
                Some(n) if n.kind == Kind::Canvas => {}
                Some(n) => return Err(PyValueError::new_err(format!("node {} is a {}, not a Canvas", node_id, n.kind.name()))),
                None => return Err(PyValueError::new_err(format!("node {} does not exist", node_id))),
            }
            let cmds = commit::parse_canvas(inner, &floats_vec, &str_refs).map_err(|e| PyValueError::new_err(e.0))?;
            if let Some(n) = inner.nodes.get_mut(id) {
                n.canvas = Some(Box::new(cmds));
            }
            Ok(())
        })
    }

    /// A pointer event (1 down, 2 up, 3 move, 9 right down, 10 right up) at logical `(x, y)`;
    /// returns the protocol tuple. Down focuses the text field hit (or blurs), and a right
    /// button reports the `on_secondary` handler instead. The `Window` uses this; headless
    /// tests can too.
    fn pointer(&self, kind: i32, x: f64, y: f64) -> PyResult<EventTuple> {
        if !(1..=3).contains(&kind) && !(9..=10).contains(&kind) {
            return Err(PyValueError::new_err("pointer kind must be 1 (down), 2 (up), 3 (move), 9 (right down) or 10 (right up)"));
        }
        let x = finite(x, "x")?;
        let y = finite(y, "y")?;
        self.with_state(true, |inner| Ok(input::pointer(inner, kind, x, y).tuple()))
    }

    /// Typed text; returns the kind-4 event tuple or None when nothing happened.
    fn key_text(&self, text: &str) -> PyResult<Option<EventTuple>> {
        self.with_state(true, |inner| Ok(input::key_text(inner, text).map(|e| e.tuple())))
    }

    /// An editing key by name (Backspace, Delete, Left, Right, Home, End, Escape, Tab, Enter).
    fn key_named(&self, name: &str) -> PyResult<Option<EventTuple>> {
        self.with_state(true, |inner| Ok(input::key_named(inner, name).map(|e| e.tuple())))
    }

    /// The events the pointer produced since the last call: kind 11 hover enters ("1") and
    /// leaves (""), and kind 12 drags ("start" / "move" / "end").
    fn take_events(&self) -> PyResult<Vec<EventTuple>> {
        self.with_state(true, |inner| Ok(input::take_events(inner).iter().map(|e| e.tuple()).collect()))
    }

    /// What the pointer has selected in a `selectable` node, one text per line, or "".
    fn selected_text(&self) -> PyResult<String> {
        self.with_state(false, |inner| Ok(input::selected_text(inner)))
    }

    /// Drop the selection (the app does this when what is shown changes under it).
    fn clear_selection(&self) -> PyResult<()> {
        self.with_state(true, |inner| {
            inner.selection = None;
            inner.selecting = false;
            Ok(())
        })
    }

    /// The `hoverable` handler the pointer is over, or -1.
    fn hover_target(&self) -> PyResult<i32> {
        self.with_state(false, |inner| Ok(input::hover_target(inner)))
    }

    /// What the pointer should look like at a point: 0 the platform's own, 1 a hand, 2 a
    /// text bar, 3 a column-resize arrow, 4 a grabbing hand.
    fn cursor_at(&self, x: f32, y: f32) -> PyResult<u32> {
        self.with_state(false, |inner| Ok(input::cursor_at(inner, x, y)))
    }

    /// Handler index of the focused text field, or -1.
    fn focused_handler(&self) -> PyResult<i32> {
        self.with_state(false, |inner| Ok(input::focused_handler(inner)))
    }

    /// Handler index under the pointer as of the last pointer event, or -1.
    fn hovered_handler(&self) -> PyResult<i32> {
        self.with_state(false, |inner| Ok(input::hovered_handler(inner)))
    }

    /// Scroll the innermost scroll container under `(x, y)` by `dy` logical pixels (positive
    /// moves the content up). Returns whether anything moved; the `Window` uses this for the
    /// mouse wheel and headless tests can too.
    fn scroll(&self, x: f64, y: f64, dy: f64) -> PyResult<bool> {
        let x = finite(x, "x")?;
        let y = finite(y, "y")?;
        let dy = finite(dy, "dy")?;
        self.with_state(true, |inner| Ok(input::scroll(inner, x, y, dy)))
    }

    /// The scroll offset of a SCROLL node (`None` for anything else or a stale id).
    fn scroll_offset(&self, node_id: u64) -> PyResult<Option<f64>> {
        self.with_state(false, |inner| {
            Ok(inner.nodes.get(NodeId::from_ffi(node_id)).filter(|n| n.kind == Kind::Scroll).map(|n| n.scroll as f64))
        })
    }

    /// The first SCROLL node in tree order (`(node_id, scroll, content_length, viewport_height)`),
    /// for tests; `None` when there is none.
    fn first_scroll(&self) -> PyResult<Option<(u64, f64, f64, f64)>> {
        self.with_state(false, |inner| {
            let mut stack = vec![inner.root];
            while let Some(n) = stack.pop() {
                let Some(node) = inner.nodes.get(n) else { continue };
                if node.kind == Kind::Scroll {
                    return Ok(Some((n.to_ffi(), node.scroll as f64, node.content_len as f64, node.content_size.h as f64)));
                }
                for &c in node.children.iter().rev() {
                    stack.push(c);
                }
            }
            Ok(None)
        })
    }

    fn __repr__(&self) -> String {
        match self.slot.lock() {
            Ok(slot) => match &slot.inner {
                Some(i) => format!("<Core {}x{}@{} nodes={}>", i.width, i.height, i.scale, i.node_count()),
                None => "<Core (in layout)>".to_string(),
            },
            Err(_) => "<Core (poisoned)>".to_string(),
        }
    }
}
