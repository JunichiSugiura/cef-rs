//! Plain OSR tab index for CEF / winit paths that cannot hold `Res` / `&World`.
//! Updated from Bevy `apply_*` systems when browsers attach or detach; read from
//! [`crate::browser::event_loop::foreign_osr_index`] in `RenderHandler` and shell code.
//!
//! Tab identity for app logic lives in ECS ([`crate::browser::browser_entity`]); this structure
//! only holds what synchronous CEF callbacks need (sizes, bind groups, id→window).
//!
//! **Spike note:** cef-rs does not expose per-`Browser` user-data hooks usable from `RenderHandler`
//! to stash an `Arc` of tab GPU state (unlike e.g. V8 `set_user_data`). So we keep this minimal
//! mutex-backed index (plan “B”) rather than a global id map hidden behind `CefHub` methods.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use winit::dpi::LogicalSize;
use winit::window::WindowId;

pub struct TabPaintSlot {
    pub size: Arc<Mutex<LogicalSize<f32>>>,
    /// Same `Arc` as [`crate::browser::browser_entity::OsrPaintBindGroup`] for this tab.
    pub bind_group: Arc<Mutex<Option<wgpu::BindGroup>>>,
}

/// Handles created in [`register_tab`], shared with Bevy [`crate::browser::browser_entity::BrowserBundle`].
pub struct OsrTabShared {
    pub view_logical_size: Arc<Mutex<LogicalSize<f32>>>,
    pub paint_bind_group: Arc<Mutex<Option<wgpu::BindGroup>>>,
}

pub struct ForeignOsrIndex {
    pub tabs: Mutex<HashMap<i32, TabPaintSlot>>,
    pub browser_to_window: Mutex<HashMap<i32, WindowId>>,
    pub paint_redraw_throttle: Arc<Mutex<HashMap<i32, Instant>>>,
    /// Until [`register_tab`] runs, CEF may call `view_rect` / `screen_info` with no tab entry.
    /// `examples/osr` avoids this with a RefCell size on the handler; vmux stages the pending shell’s
    /// logical size here from [`crate::browser::renderer::osr_host::OsrHostState::finish_next_pending_browser_if_any`]
    /// until attach (single-window MVP).
    pub pre_attach_view_logical: Mutex<Option<LogicalSize<f32>>>,
}

impl Default for ForeignOsrIndex {
    fn default() -> Self {
        Self {
            tabs: Mutex::new(HashMap::new()),
            browser_to_window: Mutex::new(HashMap::new()),
            paint_redraw_throttle: Arc::new(Mutex::new(HashMap::new())),
            pre_attach_view_logical: Mutex::new(None),
        }
    }
}

pub fn register_tab(
    index: &ForeignOsrIndex,
    browser_id: i32,
    window_id: WindowId,
    logical_size: LogicalSize<f32>,
) -> OsrTabShared {
    let view_logical_size = Arc::new(Mutex::new(logical_size));
    let paint_bind_group = Arc::new(Mutex::new(None));
    index
        .tabs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
        browser_id,
        TabPaintSlot {
            size: view_logical_size.clone(),
            bind_group: paint_bind_group.clone(),
        },
    );
    index
        .browser_to_window
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(browser_id, window_id);
    if let Ok(mut p) = index.pre_attach_view_logical.lock() {
        p.take();
    }
    OsrTabShared {
        view_logical_size,
        paint_bind_group,
    }
}

pub fn unregister_tab(index: &ForeignOsrIndex, browser_id: i32) {
    index
        .tabs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&browser_id);
    index
        .browser_to_window
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&browser_id);
    reset_paint_redraw_throttle(index, browser_id);
}

pub fn window_id_for_browser(index: &ForeignOsrIndex, browser_id: i32) -> Option<WindowId> {
    index
        .browser_to_window
        .lock()
        .ok()
        .and_then(|m| m.get(&browser_id).copied())
}

pub fn browser_window_pairs(index: &ForeignOsrIndex) -> Vec<(i32, WindowId)> {
    index
        .browser_to_window
        .lock()
        .map(|m| m.iter().map(|(&browser_id, &window_id)| (browser_id, window_id)).collect())
        .unwrap_or_default()
}

pub fn with_tab<R>(
    index: &ForeignOsrIndex,
    browser_id: i32,
    f: impl FnOnce(&TabPaintSlot) -> R,
) -> Option<R> {
    let map = index.tabs.lock().ok()?;
    let slot = map.get(&browser_id)?;
    Some(f(slot))
}

pub fn replace_bind_group(
    index: &ForeignOsrIndex,
    browser_id: i32,
    bind_group: wgpu::BindGroup,
) {
    if let Ok(map) = index.tabs.lock() {
        if let Some(slot) = map.get(&browser_id) {
            if let Ok(mut g) = slot.bind_group.lock() {
                *g = Some(bind_group);
            }
        }
    }
}

pub fn with_bind_group<R>(
    index: &ForeignOsrIndex,
    browser_id: i32,
    f: impl FnOnce(&wgpu::BindGroup) -> R,
) -> Option<R> {
    let map = index.tabs.lock().ok()?;
    let slot = map.get(&browser_id)?;
    let g = slot.bind_group.lock().ok()?;
    g.as_ref().map(f)
}

pub fn reset_paint_redraw_throttle(index: &ForeignOsrIndex, browser_id: i32) {
    let _ = index
        .paint_redraw_throttle
        .lock()
        .ok()
        .map(|mut m| m.remove(&browser_id));
}
