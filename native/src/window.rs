//! The Python `Window`: a winit 0.30 `ApplicationHandler` loop on the calling (main) thread,
//! blitting the core's RGBA buffer through softbuffer and turning winit events into the
//! protocol's event tuples. The GIL is released while the loop waits; callbacks re-attach.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window as WinitWindow, WindowId};

use crate::input::{self, Event, EV_CLOSE, EV_KEY_CHORD, EV_POINTER_DOWN, EV_POINTER_MOVE, EV_POINTER_UP, EV_RESIZE, WHEEL_LINE};
use crate::py::Core;
use crate::text::TextSystem;
use crate::tree::Inner;

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Wake,
    Close,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Event loops are created in `run()` on the thread that calls it; this only records which
    /// window ids already ran so `run` cannot be called twice for the same window.
    static RAN: RefCell<HashMap<u64, ()>> = RefCell::new(HashMap::new());
}

#[pyclass(name = "Window", module = "basedpython_ui._native")]
pub struct Window {
    id: u64,
    title: String,
    width: f64,
    height: f64,
    proxy: Mutex<Option<EventLoopProxy<UserEvent>>>,
}

#[pymethods]
impl Window {
    #[new]
    fn new(title: String, width: f64, height: f64) -> PyResult<Window> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(PyValueError::new_err("width and height must be positive"));
        }
        Ok(Window { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), title, width, height, proxy: Mutex::new(None) })
    }

    /// Run the event loop until the window closes. `on_events(events)` receives pending input
    /// before a frame; `on_frame(core)` runs when a redraw is due and returns True to request
    /// another frame. A Python exception in either ends the loop and is re-raised here.
    fn run(&self, py: Python<'_>, on_frame: Bound<'_, PyAny>, on_events: Bound<'_, PyAny>) -> PyResult<()> {
        if !on_frame.is_callable() || !on_events.is_callable() {
            return Err(PyTypeError::new_err("on_frame and on_events must be callable"));
        }
        let already = RAN.with(|r| r.borrow_mut().insert(self.id, ()).is_some());
        if already {
            return Err(PyRuntimeError::new_err("Window.run was already called for this window"));
        }
        let on_frame = on_frame.unbind();
        let on_events = on_events.unbind();
        let outcome = py.detach(|| {
            catch_unwind(AssertUnwindSafe(|| {
                let event_loop = EventLoop::<UserEvent>::with_user_event()
                    .build()
                    .map_err(|e| PyRuntimeError::new_err(format!("cannot create the event loop (one per process, main thread only): {}", e)))?;
                if let Ok(mut p) = self.proxy.lock() {
                    *p = Some(event_loop.create_proxy());
                }
                let mut app = App::new(self.title.clone(), self.width, self.height, on_frame, on_events);
                let run = event_loop.run_app(&mut app);
                if let Ok(mut p) = self.proxy.lock() {
                    *p = None;
                }
                if let Some(e) = app.error.take() {
                    return Err(e);
                }
                run.map_err(|e| PyRuntimeError::new_err(format!("event loop failed: {}", e)))
            }))
        });
        match outcome {
            Ok(r) => r,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                Err(PyRuntimeError::new_err(format!("the window loop panicked: {}", msg)))
            }
        }
    }

    /// Thread-safe wake-up: schedules a frame. A no-op before `run` starts or after it ends.
    fn request_frame(&self) {
        if let Ok(p) = self.proxy.lock() {
            if let Some(proxy) = p.as_ref() {
                let _ = proxy.send_event(UserEvent::Wake);
            }
        }
    }

    /// Thread-safe: ends the loop. A no-op before `run` starts.
    fn close(&self) {
        if let Ok(p) = self.proxy.lock() {
            if let Some(proxy) = p.as_ref() {
                let _ = proxy.send_event(UserEvent::Close);
            }
        }
    }
}

struct App {
    title: String,
    width: f64,
    height: f64,
    on_frame: Py<PyAny>,
    on_events: Py<PyAny>,
    window: Option<Arc<WinitWindow>>,
    surface: Option<softbuffer::Surface<Arc<WinitWindow>, Arc<WinitWindow>>>,
    core: Option<Py<Core>>,
    pending: Vec<Event>,
    cursor: (f32, f32),
    modifiers: ModifiersState,
    hover_handler: i32,
    error: Option<PyErr>,
}

impl App {
    fn new(title: String, width: f64, height: f64, on_frame: Py<PyAny>, on_events: Py<PyAny>) -> App {
        App {
            title,
            width,
            height,
            on_frame,
            on_events,
            window: None,
            surface: None,
            core: None,
            pending: Vec::new(),
            cursor: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            hover_handler: -1,
            error: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: PyErr) {
        if self.error.is_none() {
            self.error = Some(err);
        }
        event_loop.exit();
    }

    fn with_core<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> PyResult<R> {
        let Some(core) = &self.core else { return Err(PyRuntimeError::new_err("no core yet")) };
        Python::attach(|py| core.bind(py).borrow().with_state(true, |inner| Ok(f(inner))))
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn push(&mut self, ev: Event) {
        // only the latest size matters: collapse a pending resize into this one
        if ev.kind == EV_RESIZE {
            if let Some(last) = self.pending.last_mut() {
                if last.kind == EV_RESIZE {
                    *last = ev;
                    return;
                }
            }
        }
        self.pending.push(ev);
    }

    fn resize(&mut self, event_loop: &ActiveEventLoop, size: PhysicalSize<u32>) {
        let Some(window) = self.window.clone() else { return };
        let scale = window.scale_factor().max(0.01);
        let lw = size.width as f64 / scale;
        let lh = size.height as f64 / scale;
        if let Err(e) = self.with_core(|inner| inner.resize(lw as f32, lh as f32, scale as f32)) {
            self.fail(event_loop, e);
            return;
        }
        if let (Some(surface), Some(w), Some(h)) = (self.surface.as_mut(), NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
            let _ = surface.resize(w, h);
        }
        self.push(Event::new(EV_RESIZE, lw as f32, lh as f32, -1, String::new()));
        self.request_redraw();
    }

    fn logical(&self, p: PhysicalPosition<f64>) -> (f32, f32) {
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0).max(0.01);
        ((p.x / scale) as f32, (p.y / scale) as f32)
    }

    fn modifier_text(&self) -> String {
        let m = self.modifiers;
        input::modifier_text(m.control_key(), m.alt_key(), m.shift_key(), m.super_key())
    }

    /// The lowercase chord name of a key, when it is one the application can bind.
    fn chord_key(key: &Key) -> Option<String> {
        Some(match key {
            Key::Named(n) => match n {
                NamedKey::ArrowUp => "up".to_string(),
                NamedKey::ArrowDown => "down".to_string(),
                NamedKey::ArrowLeft => "left".to_string(),
                NamedKey::ArrowRight => "right".to_string(),
                NamedKey::PageUp => "pageup".to_string(),
                NamedKey::PageDown => "pagedown".to_string(),
                NamedKey::Home => "home".to_string(),
                NamedKey::End => "end".to_string(),
                NamedKey::Enter => "enter".to_string(),
                NamedKey::Escape => "escape".to_string(),
                NamedKey::Tab => "tab".to_string(),
                NamedKey::Backspace => "backspace".to_string(),
                NamedKey::Delete => "delete".to_string(),
                NamedKey::Space => "space".to_string(),
                NamedKey::F1 => "f1".to_string(),
                NamedKey::F2 => "f2".to_string(),
                NamedKey::F3 => "f3".to_string(),
                NamedKey::F4 => "f4".to_string(),
                NamedKey::F5 => "f5".to_string(),
                NamedKey::F6 => "f6".to_string(),
                NamedKey::F7 => "f7".to_string(),
                NamedKey::F8 => "f8".to_string(),
                NamedKey::F9 => "f9".to_string(),
                NamedKey::F10 => "f10".to_string(),
                NamedKey::F11 => "f11".to_string(),
                NamedKey::F12 => "f12".to_string(),
                _ => return None,
            },
            Key::Character(c) => {
                let t = c.to_string();
                if t == " " {
                    "space".to_string()
                } else {
                    t.to_lowercase()
                }
            }
            _ => return None,
        })
    }

    fn keyboard(&mut self, event_loop: &ActiveEventLoop, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let m = self.modifiers;
        let chorded = m.super_key() || m.control_key() || m.alt_key();
        // keys the text field never uses go to the application as chords, with or without
        // modifiers; everything else is a chord only while a command modifier is held
        let navigation = matches!(
            &event.logical_key,
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::PageUp | NamedKey::PageDown)
                | Key::Named(NamedKey::F1 | NamedKey::F2 | NamedKey::F3 | NamedKey::F4 | NamedKey::F5 | NamedKey::F6)
                | Key::Named(NamedKey::F7 | NamedKey::F8 | NamedKey::F9 | NamedKey::F10 | NamedKey::F11 | NamedKey::F12)
        );
        if chorded || navigation {
            if let Some(key) = Self::chord_key(&event.logical_key) {
                let text = input::chord(m.control_key(), m.alt_key(), m.shift_key(), m.super_key(), &key);
                self.push(Event::new(EV_KEY_CHORD, 0.0, 0.0, -1, text));
                self.request_redraw();
            }
            return;
        }
        let named = match &event.logical_key {
            Key::Named(NamedKey::Backspace) => Some("Backspace"),
            Key::Named(NamedKey::Delete) => Some("Delete"),
            Key::Named(NamedKey::ArrowLeft) => Some("Left"),
            Key::Named(NamedKey::ArrowRight) => Some("Right"),
            Key::Named(NamedKey::Home) => Some("Home"),
            Key::Named(NamedKey::End) => Some("End"),
            Key::Named(NamedKey::Escape) => Some("Escape"),
            Key::Named(NamedKey::Tab) => Some("Tab"),
            Key::Named(NamedKey::Enter) => Some("Enter"),
            _ => None,
        };
        let result = if let Some(name) = named {
            self.with_core(|inner| input::key_named(inner, name))
        } else if let Some(text) = event.text.as_ref() {
            let text = text.to_string();
            self.with_core(move |inner| input::key_text(inner, &text))
        } else {
            Ok(None)
        };
        match result {
            Ok(Some(ev)) => {
                self.push(ev);
                self.request_redraw();
            }
            Ok(None) => {
                // caret moves and focus changes need a repaint but no python event
                self.request_redraw();
            }
            Err(e) => self.fail(event_loop, e),
        }
    }

    fn deliver_events(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if self.pending.is_empty() {
            return true;
        }
        let events: Vec<(i32, f64, f64, i32, String)> = self.pending.drain(..).map(|e| e.tuple()).collect();
        let r = Python::attach(|py| self.on_events.call1(py, (events,)).map(|_| ()));
        if let Err(e) = r {
            self.fail(event_loop, e);
            return false;
        }
        true
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        if self.core.is_none() {
            return;
        }
        if !self.deliver_events(event_loop) {
            return;
        }
        let again = Python::attach(|py| -> PyResult<bool> {
            let core = self.core.as_ref().map(|c| c.clone_ref(py)).ok_or_else(|| PyRuntimeError::new_err("no core"))?;
            self.on_frame.call1(py, (core,))?.is_truthy(py)
        });
        let again = match again {
            Ok(a) => a,
            Err(e) => {
                self.fail(event_loop, e);
                return;
            }
        };
        if let Err(e) = self.blit() {
            self.fail(event_loop, e);
            return;
        }
        if again {
            self.request_redraw();
        }
    }

    fn blit(&mut self) -> PyResult<()> {
        let Some(window) = self.window.clone() else { return Ok(()) };
        let Some(surface) = self.surface.as_mut() else { return Ok(()) };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return Ok(()) };
        surface.resize(w, h).map_err(|e| PyRuntimeError::new_err(format!("surface resize failed: {}", e)))?;
        let mut buffer = surface.buffer_mut().map_err(|e| PyRuntimeError::new_err(format!("surface buffer failed: {}", e)))?;
        let core = self.core.as_ref().ok_or_else(|| PyRuntimeError::new_err("no core"))?;
        Python::attach(|py| {
            core.bind(py).borrow().with_state(false, |inner| {
                let Some(pixmap) = inner.pixmap.as_ref() else {
                    buffer.fill(0);
                    return Ok(());
                };
                let (pw, ph) = (pixmap.width() as usize, pixmap.height() as usize);
                let (sw, sh) = (size.width as usize, size.height as usize);
                let data = pixmap.data();
                let cw = pw.min(sw);
                for y in 0..ph.min(sh) {
                    let src = &data[y * pw * 4..(y * pw + cw) * 4];
                    let dst = &mut buffer[y * sw..y * sw + cw];
                    for (d, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                        *d = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | (px[2] as u32);
                    }
                }
                Ok(())
            })
        })?;
        window.pre_present_notify();
        buffer.present().map_err(|e| PyRuntimeError::new_err(format!("present failed: {}", e)))?;
        Ok(())
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        event_loop.set_control_flow(ControlFlow::Wait);
        let attrs = WinitWindow::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(LogicalSize::new(self.width, self.height));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.fail(event_loop, PyRuntimeError::new_err(format!("cannot create the window: {}", e)));
                return;
            }
        };
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                self.fail(event_loop, PyRuntimeError::new_err(format!("softbuffer context: {}", e)));
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.fail(event_loop, PyRuntimeError::new_err(format!("softbuffer surface: {}", e)));
                return;
            }
        };
        let scale = window.scale_factor().max(0.01);
        let size = window.inner_size();
        let lw = size.width as f64 / scale;
        let lh = size.height as f64 / scale;
        let core = Python::attach(|py| {
            let inner = Inner::new(lw as f32, lh as f32, scale as f32, TextSystem::system());
            Py::new(py, Core::with_inner(inner))
        });
        let core = match core {
            Ok(c) => c,
            Err(e) => {
                self.fail(event_loop, e);
                return;
            }
        };
        self.window = Some(window.clone());
        self.surface = Some(surface);
        self.core = Some(core);
        self.push(Event::new(EV_RESIZE, lw as f32, lh as f32, -1, String::new()));
        window.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => self.request_redraw(),
            UserEvent::Close => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.push(Event::new(EV_CLOSE, 0.0, 0.0, -1, String::new()));
                self.deliver_events(event_loop);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.resize(event_loop, size),
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = self.window.clone() {
                    self.resize(event_loop, w.inner_size());
                }
            }
            WindowEvent::CursorLeft { .. } => {
                let changed = self.hover_handler != -1;
                self.hover_handler = -1;
                if let Err(e) = self.with_core(input::pointer_left) {
                    self.fail(event_loop, e);
                    return;
                }
                if changed {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -lines * WHEEL_LINE,
                    MouseScrollDelta::PixelDelta(p) => {
                        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0).max(0.01);
                        -(p.y / scale) as f32
                    }
                };
                let (x, y) = self.cursor;
                match self.with_core(|inner| input::scroll(inner, x, y, dy)) {
                    Ok(moved) => {
                        if moved {
                            // the content moved under the pointer: keep the hover state honest
                            if let Ok(ev) = self.with_core(|inner| input::pointer(inner, EV_POINTER_MOVE, x, y)) {
                                self.hover_handler = ev.handler;
                            }
                            self.request_redraw();
                        }
                    }
                    Err(e) => self.fail(event_loop, e),
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = self.logical(position);
                self.cursor = (x, y);
                let mods = self.modifier_text();
                match self.with_core(|inner| {
                    let mut ev = input::pointer(inner, EV_POINTER_MOVE, x, y);
                    ev.text = mods;
                    ev
                }) {
                    Ok(ev) => {
                        let hover_changed = ev.handler != self.hover_handler;
                        self.hover_handler = ev.handler;
                        // coalesce runs of moves: only the latest position matters
                        if let Some(last) = self.pending.last_mut() {
                            if last.kind == EV_POINTER_MOVE {
                                *last = ev;
                                if hover_changed {
                                    self.request_redraw();
                                }
                                return;
                            }
                        }
                        self.push(ev);
                        if hover_changed {
                            self.request_redraw();
                        }
                    }
                    Err(e) => self.fail(event_loop, e),
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                let kind = if state == ElementState::Pressed { EV_POINTER_DOWN } else { EV_POINTER_UP };
                let (x, y) = self.cursor;
                let mods = self.modifier_text();
                match self.with_core(|inner| {
                    let mut ev = input::pointer(inner, kind, x, y);
                    ev.text = mods;
                    ev
                }) {
                    Ok(ev) => {
                        self.push(ev);
                        self.request_redraw();
                    }
                    Err(e) => self.fail(event_loop, e),
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.keyboard(event_loop, event),
            WindowEvent::RedrawRequested => self.frame(event_loop),
            _ => {}
        }
    }
}
