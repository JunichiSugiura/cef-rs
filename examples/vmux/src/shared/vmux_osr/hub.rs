use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use cef::Browser;
use winit::dpi::LogicalSize;
use winit::window::WindowId;

use super::gpu::WindowSurface;

/// One vmux window: winit + wgpu surface and its CEF browser (after `on_after_created`).
pub struct WindowEntry {
    pub surface: WindowSurface,
    pub browser: Browser,
    pub size: Arc<Mutex<LogicalSize<f32>>>,
}

/// Queued in the main loop before async `browser_host_create_browser`; matched in `on_after_created`.
pub struct PendingOsrWindow {
    pub surface: WindowSurface,
    pub logical: LogicalSize<f32>,
    pub url: String,
}

/// Shared between `VmuxOsrApp` (enqueue shell + pump) and `VmuxHandler::on_after_created` (attach browser).
#[derive(Clone)]
pub struct VmuxOsrAttach {
    pub shell_fifo: Arc<Mutex<VecDeque<PendingOsrWindow>>>,
    pub windows_store: Arc<Mutex<HashMap<WindowId, WindowEntry>>>,
    /// Winit shells created for OSR that are not yet paired in `on_after_created` (spawn → +1, attach → -1).
    /// Prevents `on_before_close` from shutting down when the only live browser closes while others
    /// are still queued or in flight.
    pub unpaired_osr_shells: Arc<AtomicUsize>,
}

impl VmuxOsrAttach {
    pub fn new() -> Self {
        Self {
            shell_fifo: Arc::new(Mutex::new(VecDeque::new())),
            windows_store: Arc::new(Mutex::new(HashMap::new())),
            unpaired_osr_shells: Arc::new(AtomicUsize::new(0)),
        }
    }
}

pub struct TabPaintSlot {
    pub size: Arc<Mutex<LogicalSize<f32>>>,
    pub bind_group: Mutex<Option<wgpu::BindGroup>>,
}

pub struct VmuxOsrHub {
    tabs: Mutex<HashMap<i32, TabPaintSlot>>,
    browser_to_window: Mutex<HashMap<i32, WindowId>>,
}

impl VmuxOsrHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tabs: Mutex::new(HashMap::new()),
            browser_to_window: Mutex::new(HashMap::new()),
        })
    }

    pub fn register_browser(
        &self,
        browser_id: i32,
        window_id: WindowId,
        logical_size: LogicalSize<f32>,
    ) -> Arc<Mutex<LogicalSize<f32>>> {
        let size = Arc::new(Mutex::new(logical_size));
        self.tabs.lock().unwrap().insert(
            browser_id,
            TabPaintSlot {
                size: size.clone(),
                bind_group: Mutex::new(None),
            },
        );
        self.browser_to_window
            .lock()
            .unwrap()
            .insert(browser_id, window_id);
        size
    }

    pub fn unregister_browser(&self, browser_id: i32) {
        self.tabs.lock().unwrap().remove(&browser_id);
        self.browser_to_window.lock().unwrap().remove(&browser_id);
    }

    pub fn window_id_for_browser(&self, browser_id: i32) -> Option<WindowId> {
        self.browser_to_window
            .lock()
            .ok()
            .and_then(|m| m.get(&browser_id).copied())
    }

    pub fn with_tab<R>(&self, browser_id: i32, f: impl FnOnce(&TabPaintSlot) -> R) -> Option<R> {
        let map = self.tabs.lock().ok()?;
        let slot = map.get(&browser_id)?;
        Some(f(slot))
    }

    pub fn replace_bind_group(&self, browser_id: i32, bind_group: wgpu::BindGroup) {
        if let Ok(map) = self.tabs.lock() {
            if let Some(slot) = map.get(&browser_id) {
                if let Ok(mut g) = slot.bind_group.lock() {
                    *g = Some(bind_group);
                }
            }
        }
    }

    pub fn with_bind_group<R>(&self, browser_id: i32, f: impl FnOnce(&wgpu::BindGroup) -> R) -> Option<R> {
        let map = self.tabs.lock().ok()?;
        let slot = map.get(&browser_id)?;
        let g = slot.bind_group.lock().ok()?;
        g.as_ref().map(f)
    }
}
