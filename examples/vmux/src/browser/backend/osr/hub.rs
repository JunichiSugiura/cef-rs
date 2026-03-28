use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use ::cef::Browser;
use winit::dpi::LogicalSize;
use winit::window::WindowId;

use crate::browser::renderer::gpu::device::WindowSurface;

/// One vmux window: winit + wgpu surface and its CEF browser (after `on_after_created`).
pub struct WindowEntry {
    pub surface: WindowSurface,
    pub browser: Browser,
    pub size: Arc<Mutex<LogicalSize<f32>>>,
}

/// Queued in the main loop before async `browser_host_create_browser`; matched in `on_after_created`.
pub struct PendingCefWindow {
    pub surface: WindowSurface,
    pub logical: LogicalSize<f32>,
    /// Initial navigation URL passed to `browser_host_create_browser`.
    pub url: String,
    /// When set, after the first load completes we navigate to this URL (empty-config → default URL).
    pub deferred_url: Option<String>,
}

/// Shared between [`OsrHostState`](crate::browser::renderer::osr_host::state::OsrHostState) (enqueue window + pump) and `BrowserHandler::on_after_created` (attach browser).
#[derive(Clone)]
pub struct CefAttach {
    pub shell_fifo: Arc<Mutex<VecDeque<PendingCefWindow>>>,
    pub windows_store: Arc<Mutex<HashMap<WindowId, WindowEntry>>>,
    /// Winit shells created for OSR that are not yet paired in `on_after_created` (spawn → +1, attach → -1).
    /// Prevents `on_before_close` from shutting down when the only live browser closes while others
    /// are still queued or in flight.
    pub unpaired_cef_shells: Arc<AtomicUsize>,
    /// `browser_id` → URL to load after the initial blank load finishes (empty startup only).
    pub deferred_url_after_blank: Arc<Mutex<HashMap<i32, String>>>,
}

impl CefAttach {
    pub fn new() -> Self {
        Self {
            shell_fifo: Arc::new(Mutex::new(VecDeque::new())),
            windows_store: Arc::new(Mutex::new(HashMap::new())),
            unpaired_cef_shells: Arc::new(AtomicUsize::new(0)),
            deferred_url_after_blank: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
