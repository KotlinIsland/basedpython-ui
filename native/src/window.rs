//! The Python `Window`: a winit 0.30 `ApplicationHandler` loop on the calling (main) thread,
//! blitting the core's RGBA buffer through softbuffer and turning winit events into the
//! protocol's event tuples. The GIL is released while the loop waits; callbacks re-attach.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Icon, Theme, Window as WinitWindow, WindowId};

use crate::input::{
    self, Event, EV_CLOSE, EV_KEY_CHORD, EV_POINTER_DOWN, EV_POINTER_MOVE, EV_POINTER_UP, EV_RESIZE, EV_SECONDARY_DOWN,
    EV_SECONDARY_UP, EV_THEME, WHEEL_LINE,
};
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

/// The application's icon, as a png. The window system wants it in two places: the window
/// itself (windows, x11) and the application (the dock, on macos).
struct AppIcon {
    png: Vec<u8>,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl AppIcon {
    fn decode(png: Vec<u8>) -> Result<AppIcon, String> {
        let decoder = png::Decoder::new(std::io::Cursor::new(png.as_slice()));
        let mut reader = decoder.read_info().map_err(|e| format!("the icon is not a readable png: {}", e))?;
        let mut buffer = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut buffer).map_err(|e| format!("the icon could not be decoded: {}", e))?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return Err("the icon must be an 8-bit rgba png".to_string());
        }
        buffer.truncate(info.buffer_size());
        Ok(AppIcon { png, rgba: buffer, width: info.width, height: info.height })
    }

    fn window_icon(&self) -> Option<Icon> {
        Icon::from_rgba(self.rgba.clone(), self.width, self.height).ok()
    }
}

/// The dock icon and the name in the menu bar are the *application's*, not a window's, so
/// winit's per-window icon (a no-op here) is not what shows: AppKit is asked directly, once
/// the event loop is running on the main thread. Without a bundle the name would otherwise
/// be the interpreter's.
#[cfg(target_os = "macos")]
fn set_application_identity(name: &str, png: Option<&[u8]>) {
    use objc2::ClassType;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData, NSProcessInfo, NSString};

    let Some(mtm) = MainThreadMarker::new() else { return };
    // SAFETY: the process name and the shared application's icon, set on the main thread —
    // what these methods are for, and nothing escapes here. without a bundle the dock would
    // otherwise show the interpreter's name and a blank document, since that is literally
    // what is running; the icon has to be set again once the application is up, because
    // launching replaces whatever was set before it
    unsafe {
        NSProcessInfo::processInfo().setProcessName(&NSString::from_str(name));
        set_dock_name(name);
        if let Some(png) = png {
            let data = NSData::with_bytes(png);
            if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
                NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image));
            }
        }
    }
}

/// The name the dock's tooltip and the menu bar show is launch services' display name for the
/// running application, and neither `NSProcessInfo`'s process name nor the main bundle's info
/// dictionary reaches it: without a bundle it stays the name of the executable, which is the
/// interpreter. The call that does reach it is private, so it is looked up at run time — a
/// macOS that no longer exports it leaves the name as it was rather than refusing to start.
#[cfg(target_os = "macos")]
fn set_dock_name(name: &str) {
    use objc2_foundation::NSString;
    use std::ffi::{c_char, c_int, c_void};

    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    const RTLD_LAZY: c_int = 1;
    /// launch services' word for the session this process is already in
    const CURRENT_SESSION: c_int = -2;

    type CurrentAsn = unsafe extern "C" fn() -> *const c_void;
    type SetItem = unsafe extern "C" fn(c_int, *const c_void, *const c_void, *const c_void, *mut *const c_void) -> c_int;

    // SAFETY: three symbols of core services, which appkit has already brought into the
    // process, called with the signatures launch services declares for them. each lookup is
    // checked for null, the key is read as the `CFStringRef` variable it is, and the name is
    // an `NSString` — toll-free bridged, and alive for the length of the call
    unsafe {
        let services = dlopen(c"/System/Library/Frameworks/CoreServices.framework/CoreServices".as_ptr(), RTLD_LAZY);
        if services.is_null() {
            return;
        }
        let asn = dlsym(services, c"_LSGetCurrentApplicationASN".as_ptr());
        let set = dlsym(services, c"_LSSetApplicationInformationItem".as_ptr());
        let key = dlsym(services, c"_kLSDisplayNameKey".as_ptr());
        if asn.is_null() || set.is_null() || key.is_null() {
            return;
        }
        let current_asn: CurrentAsn = std::mem::transmute(asn);
        let set_item: SetItem = std::mem::transmute(set);
        let display_name = *(key as *const *const c_void);
        let value = NSString::from_str(name);
        let _ = set_item(
            CURRENT_SESSION,
            current_asn(),
            display_name,
            &*value as *const NSString as *const c_void,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn set_application_identity(_name: &str, _png: Option<&[u8]>) {}

#[pyclass(name = "Window", module = "basedpython_ui._native")]
pub struct Window {
    id: u64,
    /// what the application is called — the dock, the menu bar — where the title is what
    /// this one window is showing
    name: String,
    title: String,
    width: f64,
    height: f64,
    icon: Option<AppIcon>,
    proxy: Mutex<Option<EventLoopProxy<UserEvent>>>,
    /// windows python asked for while the loop was running, waiting to be made
    wanted: Arc<Mutex<Vec<Wanted>>>,
}

/// A window asked for from a handler: it is made on the loop's next turn, in this process,
/// so every window of an application shares one event loop and one dock icon.
struct Wanted {
    title: String,
    width: f64,
    height: f64,
    on_frame: Py<PyAny>,
    on_events: Py<PyAny>,
}

#[pymethods]
impl Window {
    #[new]
    #[pyo3(signature = (title, width, height, icon = None, name = None))]
    fn new(title: String, width: f64, height: f64, icon: Option<Vec<u8>>, name: Option<String>) -> PyResult<Window> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(PyValueError::new_err("width and height must be positive"));
        }
        let icon = match icon {
            Some(png) => Some(AppIcon::decode(png).map_err(PyValueError::new_err)?),
            None => None,
        };
        Ok(Window {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            name: name.unwrap_or_else(|| title.clone()),
            title,
            width,
            height,
            icon,
            proxy: Mutex::new(None),
            wanted: Arc::new(Mutex::new(Vec::new())),
        })
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
                // before the loop, so the name is in place as the application registers
                set_application_identity(&self.name, None);
                let mut app = App::new(
                    self.name.clone(),
                    self.title.clone(),
                    self.width,
                    self.height,
                    self.icon.as_ref().and_then(|i| i.window_icon()),
                    self.icon.as_ref().map(|i| i.png.clone()),
                    on_frame,
                    on_events,
                    self.wanted.clone(),
                );
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

    /// Open another window of this same application, composed by its own callbacks. It is
    /// made on the loop's next turn; closing the last window is what ends the loop.
    fn open(&self, title: String, width: f64, height: f64, on_frame: Bound<'_, PyAny>, on_events: Bound<'_, PyAny>) -> PyResult<()> {
        if !on_frame.is_callable() || !on_events.is_callable() {
            return Err(PyTypeError::new_err("on_frame and on_events must be callable"));
        }
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(PyValueError::new_err("width and height must be positive"));
        }
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.push(Wanted { title, width, height, on_frame: on_frame.unbind(), on_events: on_events.unbind() });
        }
        self.request_frame();
        Ok(())
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

fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Dark => "dark",
        Theme::Light => "light",
    }
}

/// One window and everything that belongs to it: its surface, its core, the events waiting
/// to reach python, and the callbacks that compose it. An application is a set of these.
struct Pane {
    window: Arc<WinitWindow>,
    surface: softbuffer::Surface<Arc<WinitWindow>, Arc<WinitWindow>>,
    core: Py<Core>,
    on_frame: Py<PyAny>,
    on_events: Py<PyAny>,
    pending: Vec<Event>,
    cursor: (f32, f32),
    hover_handler: i32,
    cursor_kind: u32,
    /// When this pane next has something to redraw of its own — a caret blinking, a field
    /// still scrolling. `None` while nothing is moving, which is what lets the loop sleep.
    next_frame: Option<Instant>,
}

impl Pane {
    fn with_core<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> PyResult<R> {
        Python::attach(|py| self.core.bind(py).borrow().with_state(true, |inner| Ok(f(inner))))
    }

    fn request_redraw(&self) {
        self.window.request_redraw();
    }

    /// Queue the hover and drag events the last pointer event produced; true when there
    /// were any, which is what makes a drag redraw while the pointer is still down.
    fn push_hover(&mut self) -> bool {
        match self.with_core(input::take_events) {
            Ok(events) => {
                let any = !events.is_empty();
                for ev in events {
                    self.push(ev);
                }
                any
            }
            Err(_) => false,
        }
    }

    /// Point the pointer at whatever is under it. The core says which of a small set of
    /// shapes a node asked for; the platform draws it.
    fn apply_cursor(&mut self, x: f32, y: f32) {
        let Ok(kind) = self.with_core(|inner| input::cursor_at(inner, x, y)) else { return };
        if kind == self.cursor_kind {
            return;
        }
        self.cursor_kind = kind;
        self.window.set_cursor(match kind {
            1 => CursorIcon::Pointer,
            2 => CursorIcon::Text,
            3 => CursorIcon::ColResize,
            4 => CursorIcon::Grabbing,
            _ => CursorIcon::Default,
        });
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

    fn resize(&mut self, size: PhysicalSize<u32>) -> PyResult<()> {
        let scale = self.window.scale_factor().max(0.01);
        let lw = size.width as f64 / scale;
        let lh = size.height as f64 / scale;
        self.with_core(|inner| inner.resize(lw as f32, lh as f32, scale as f32))?;
        if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
            let _ = self.surface.resize(w, h);
        }
        self.push(Event::new(EV_RESIZE, lw as f32, lh as f32, -1, String::new()));
        self.request_redraw();
        Ok(())
    }

    fn logical(&self, p: PhysicalPosition<f64>) -> (f32, f32) {
        let scale = self.window.scale_factor().max(0.01);
        ((p.x / scale) as f32, (p.y / scale) as f32)
    }

    fn deliver_events(&mut self) -> PyResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let events: Vec<(i32, f64, f64, i32, String)> = self.pending.drain(..).map(|e| e.tuple()).collect();
        Python::attach(|py| self.on_events.call1(py, (events,)).map(|_| ()))
    }

    fn frame(&mut self) -> PyResult<()> {
        self.deliver_events()?;
        let again = Python::attach(|py| -> PyResult<bool> {
            let core = self.core.clone_ref(py);
            self.on_frame.call1(py, (core,))?.is_truthy(py)
        })?;
        // what composing and laying out produced — a child asking to be revealed scrolls
        // its container, and the container is told. taken now, it goes out at the top of
        // the next frame against the handler table this one built; left in the core it
        // would wait for the next pointer event and be answered by whatever had taken its
        // index by then
        if self.push_hover() {
            self.request_redraw();
        }
        self.blit()?;
        // what the core says it still has to animate, asked once a frame rather than once a
        // turn of the loop: crossing into python to find out is not free
        self.next_frame = self.with_core(|inner| inner.next_frame_in())?.map(|d| Instant::now() + d);
        if again {
            self.request_redraw();
        }
        Ok(())
    }

    fn blit(&mut self) -> PyResult<()> {
        let size = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return Ok(()) };
        self.surface.resize(w, h).map_err(|e| PyRuntimeError::new_err(format!("surface resize failed: {}", e)))?;
        let mut buffer = self.surface.buffer_mut().map_err(|e| PyRuntimeError::new_err(format!("surface buffer failed: {}", e)))?;
        let core = &self.core;
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
        self.window.pre_present_notify();
        buffer.present().map_err(|e| PyRuntimeError::new_err(format!("present failed: {}", e)))?;
        Ok(())
    }
}

struct App {
    name: String,
    title: String,
    width: f64,
    height: f64,
    icon: Option<Icon>,
    /// the icon as png bytes, which is what the platform's own application icon takes
    icon_png: Option<Vec<u8>>,
    /// the callbacks of the first window, until it is made
    first: Option<(Py<PyAny>, Py<PyAny>)>,
    wanted: Arc<Mutex<Vec<Wanted>>>,
    panes: HashMap<WindowId, Pane>,
    modifiers: ModifiersState,
    error: Option<PyErr>,
}

impl App {
    fn new(
        name: String,
        title: String,
        width: f64,
        height: f64,
        icon: Option<Icon>,
        icon_png: Option<Vec<u8>>,
        on_frame: Py<PyAny>,
        on_events: Py<PyAny>,
        wanted: Arc<Mutex<Vec<Wanted>>>,
    ) -> App {
        App {
            name,
            title,
            width,
            height,
            icon,
            icon_png,
            first: Some((on_frame, on_events)),
            wanted,
            panes: HashMap::new(),
            modifiers: ModifiersState::empty(),
            error: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: PyErr) {
        if self.error.is_none() {
            self.error = Some(err);
        }
        event_loop.exit();
    }

    /// Make a window and everything that belongs to it.
    fn open_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        title: String,
        width: f64,
        height: f64,
        on_frame: Py<PyAny>,
        on_events: Py<PyAny>,
    ) -> PyResult<()> {
        let attrs = WinitWindow::default_attributes()
            .with_title(title)
            .with_window_icon(self.icon.clone())
            .with_inner_size(LogicalSize::new(width, height));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| PyRuntimeError::new_err(format!("cannot create the window: {}", e)))?,
        );
        let context = softbuffer::Context::new(window.clone()).map_err(|e| PyRuntimeError::new_err(format!("softbuffer context: {}", e)))?;
        let surface =
            softbuffer::Surface::new(&context, window.clone()).map_err(|e| PyRuntimeError::new_err(format!("softbuffer surface: {}", e)))?;
        let scale = window.scale_factor().max(0.01);
        let size = window.inner_size();
        let lw = size.width as f64 / scale;
        let lh = size.height as f64 / scale;
        let core = Python::attach(|py| {
            let inner = Inner::new(lw as f32, lh as f32, scale as f32, TextSystem::system());
            Py::new(py, Core::with_inner(inner))
        })?;
        let mut pane = Pane {
            window: window.clone(),
            surface,
            core,
            on_frame,
            on_events,
            pending: Vec::new(),
            cursor: (0.0, 0.0),
            hover_handler: -1,
            cursor_kind: 0,
            next_frame: None,
        };
        pane.push(Event::new(EV_RESIZE, lw as f32, lh as f32, -1, String::new()));
        if let Some(theme) = window.theme() {
            pane.push(Event::new(EV_THEME, 0.0, 0.0, -1, theme_name(theme).to_string()));
        }
        window.request_redraw();
        self.panes.insert(window.id(), pane);
        Ok(())
    }

    /// Make the windows python asked for since the last turn.
    fn open_wanted(&mut self, event_loop: &ActiveEventLoop) {
        let batch: Vec<Wanted> = match self.wanted.lock() {
            Ok(mut wanted) => wanted.drain(..).collect(),
            Err(_) => return,
        };
        for one in batch {
            if let Err(e) = self.open_pane(event_loop, one.title, one.width, one.height, one.on_frame, one.on_events) {
                self.fail(event_loop, e);
                return;
            }
        }
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

    fn keyboard(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let m = self.modifiers;
        let chorded = m.super_key() || m.control_key() || m.alt_key();
        // keys the text field never uses go to the application as chords, with or without
        // modifiers; everything else is a chord only while a command modifier is held
        // keys a focused field would use, which reach the application as chords when no
        // field has one — that is what lets a list be walked with the arrows and with tab
        let navigation = matches!(
            &event.logical_key,
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::PageUp | NamedKey::PageDown)
                | Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Home | NamedKey::End)
                | Key::Named(NamedKey::Tab)
                | Key::Named(NamedKey::F1 | NamedKey::F2 | NamedKey::F3 | NamedKey::F4 | NamedKey::F5 | NamedKey::F6)
                | Key::Named(NamedKey::F7 | NamedKey::F8 | NamedKey::F9 | NamedKey::F10 | NamedKey::F11 | NamedKey::F12)
        );
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
        let chord = if chorded || navigation {
            Self::chord_key(&event.logical_key)
                .map(|key| input::chord(m.control_key(), m.alt_key(), m.shift_key(), m.super_key(), &key))
        } else {
            None
        };
        // the name a focused field would know this key by: an editing key, or a bare letter
        // for the chorded ones — `A` with a command modifier selects the text
        let edit_name: Option<String> = named.map(str::to_string).or_else(|| {
            let mut chars = event.text.as_ref()?.chars();
            let c = chars.next()?;
            chars.next().is_none().then(|| c.to_ascii_uppercase().to_string())
        });
        let mods = input::Mods { ctrl: m.control_key(), alt: m.alt_key(), shift: m.shift_key(), meta: m.super_key() };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        // a field being edited takes its own keys first, modifiers and all: while you are
        // typing, ⌥← is a word back and ⌘A is the text, not whatever the window binds them to
        if let Some(name) = edit_name.as_deref() {
            let taken = pane.with_core(|inner| match input::key_edit(inner, name, mods) {
                input::Edit::Took(event) => (true, event),
                input::Edit::Ignored => (false, None),
            });
            match taken {
                Ok((true, event)) => {
                    if let Some(event) = event {
                        pane.push(event);
                    }
                    pane.request_redraw();
                    return;
                }
                Ok((false, _)) => {}
                Err(e) => {
                    self.fail(event_loop, e);
                    return;
                }
            }
        }
        if chorded || navigation {
            if let Some(text) = chord {
                pane.push(Event::new(EV_KEY_CHORD, 0.0, 0.0, -1, text));
                pane.request_redraw();
            }
            return;
        }
        let result = if named.is_some() {
            Ok(None)
        } else if let Some(text) = event.text.as_ref() {
            let text = text.to_string();
            pane.with_core(move |inner| input::key_text(inner, &text))
        } else {
            Ok(None)
        };
        match result {
            Ok(Some(ev)) => {
                pane.push(ev);
                pane.request_redraw();
            }
            // caret moves and focus changes need a repaint but no python event
            Ok(None) => pane.request_redraw(),
            Err(e) => self.fail(event_loop, e),
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some((on_frame, on_events)) = self.first.take() else { return };
        event_loop.set_control_flow(ControlFlow::Wait);
        // the icon belongs to a running application: set before the loop it is dropped when
        // the platform finishes launching, so it is set again here
        let png = self.icon_png.clone();
        set_application_identity(&self.name, png.as_deref());
        let (title, width, height) = (self.title.clone(), self.width, self.height);
        if let Err(e) = self.open_pane(event_loop, title, width, height, on_frame, on_events) {
            self.fail(event_loop, e);
        }
    }

    /// A caret blinks and a field scrolls without anybody touching the keyboard, so the loop
    /// cannot simply wait for the next event. Each pane says when it next has something to
    /// draw; the loop sleeps until the soonest of them, and goes back to waiting outright
    /// once nothing is moving. Redrawing at a fixed rate whether or not anything changed
    /// would keep a whole window repainting for the sake of one blinking line.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let soonest = self.panes.values().filter_map(|pane| pane.next_frame).min();
        match soonest {
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
            return;
        }
        let now = Instant::now();
        for pane in self.panes.values() {
            if pane.next_frame.map(|at| at <= now).unwrap_or(false) {
                pane.request_redraw();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {
                self.open_wanted(event_loop);
                for pane in self.panes.values() {
                    pane.request_redraw();
                }
            }
            UserEvent::Close => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // a window python asked for while a handler ran is made before anything else, so a
        // click that opens one has a window by the time the frame it caused is drawn
        self.open_wanted(event_loop);
        match event {
            WindowEvent::CloseRequested => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.push(Event::new(EV_CLOSE, 0.0, 0.0, -1, String::new()));
                    if let Err(e) = pane.deliver_events() {
                        self.fail(event_loop, e);
                        return;
                    }
                }
                self.panes.remove(&id);
                // the application is its windows: the last one to close ends it
                if self.panes.is_empty() {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                if let Err(e) = pane.resize(size) {
                    self.fail(event_loop, e);
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                let size = pane.window.inner_size();
                if let Err(e) = pane.resize(size) {
                    self.fail(event_loop, e);
                }
            }
            // the window lost focus with a button down: no release will arrive, so the
            // gesture is ended rather than left holding the pointer until the next one
            WindowEvent::Focused(false) => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                let (x, y) = pane.cursor;
                if let Err(e) = pane.with_core(|inner| input::pointer_cancelled(inner, x, y)) {
                    self.fail(event_loop, e);
                    return;
                }
                pane.hover_handler = -1;
                if pane.push_hover() {
                    pane.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                let changed = pane.hover_handler != -1;
                pane.hover_handler = -1;
                if let Err(e) = pane.with_core(input::pointer_left) {
                    self.fail(event_loop, e);
                    return;
                }
                pane.push_hover();
                if changed {
                    pane.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                let scale = pane.window.scale_factor().max(0.01);
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(columns, lines) => (-columns * WHEEL_LINE, -lines * WHEEL_LINE),
                    MouseScrollDelta::PixelDelta(p) => (-(p.x / scale) as f32, -(p.y / scale) as f32),
                };
                let (x, y) = pane.cursor;
                match pane.with_core(|inner| input::scroll_by(inner, x, y, dx, dy)) {
                    Ok(moved) => {
                        if moved {
                            // the content moved under the pointer: keep the hover state honest
                            if let Ok(ev) = pane.with_core(|inner| input::pointer(inner, EV_POINTER_MOVE, x, y)) {
                                pane.hover_handler = ev.handler;
                            }
                            pane.push_hover();
                            pane.request_redraw();
                        }
                    }
                    Err(e) => self.fail(event_loop, e),
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let mods = self.modifier_text();
                let Some(pane) = self.panes.get_mut(&id) else { return };
                let (x, y) = pane.logical(position);
                pane.cursor = (x, y);
                match pane.with_core(|inner| {
                    let mut ev = input::pointer(inner, EV_POINTER_MOVE, x, y);
                    ev.text = mods;
                    ev
                }) {
                    Ok(ev) => {
                        let hover_changed = ev.handler != pane.hover_handler;
                        pane.hover_handler = ev.handler;
                        // coalesce runs of moves: only the latest position matters. the
                        // hover events the move produced are still taken: a pointer that
                        // leaves in one quick sweep must say so, or whatever it left stays
                        // lit until the next click
                        let mut coalesced = false;
                        if let Some(last) = pane.pending.last_mut() {
                            if last.kind == EV_POINTER_MOVE {
                                *last = ev.clone();
                                coalesced = true;
                            }
                        }
                        if !coalesced {
                            pane.push(ev);
                        }
                        // a drag step needs a frame of its own: nothing else will ask for
                        // one, since the handler that moves the layout only runs in it
                        let queued = pane.push_hover();
                        // a sweep that is picking out text changes what is painted without
                        // producing an event of its own, so it asks for the frame itself
                        let sweeping = pane.with_core(|inner| inner.selecting).unwrap_or(false);
                        if hover_changed || queued || sweeping {
                            pane.request_redraw();
                        }
                        pane.apply_cursor(x, y);
                    }
                    Err(e) => self.fail(event_loop, e),
                }
            }
            WindowEvent::MouseInput { state, button, .. } if button == MouseButton::Left || button == MouseButton::Right => {
                let pressed = state == ElementState::Pressed;
                let kind = match (button, pressed) {
                    (MouseButton::Right, true) => EV_SECONDARY_DOWN,
                    (MouseButton::Right, false) => EV_SECONDARY_UP,
                    (_, true) => EV_POINTER_DOWN,
                    (_, false) => EV_POINTER_UP,
                };
                let mods = self.modifier_text();
                let Some(pane) = self.panes.get_mut(&id) else { return };
                let (x, y) = pane.cursor;
                match pane.with_core(|inner| {
                    let mut ev = input::pointer(inner, kind, x, y);
                    ev.text = mods;
                    ev
                }) {
                    Ok(ev) => {
                        pane.push(ev);
                        pane.push_hover();
                        pane.request_redraw();
                    }
                    Err(e) => self.fail(event_loop, e),
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                pane.push(Event::new(EV_THEME, 0.0, 0.0, -1, theme_name(theme).to_string()));
                pane.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.keyboard(event_loop, id, event),
            WindowEvent::RedrawRequested => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                if let Err(e) = pane.frame() {
                    self.fail(event_loop, e);
                }
            }
            _ => {}
        }
    }
}
