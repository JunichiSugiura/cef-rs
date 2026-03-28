//! `OnceLock` handles for CEF callbacks and other code that cannot hold `&World` / `Res<…>`.
//!
//! After [`crate::browser::backend::cef::bootstrap::startup_browser_runtime_system`], the same `Arc`s
//! are inserted as [`crate::browser::backend::cef::bootstrap::GpuResource`],
//! [`crate::browser::backend::cef::bootstrap::ForeignOsrIndexResource`], etc. Prefer those in Bevy `Update`
//! systems; keep `foreign_*` for non-Bevy paths (render handlers, `events::*` bridging CEF threads).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use crate::browser::backend::osr::foreign_index::ForeignOsrIndex;
use crate::browser::backend::osr::hub::CefAttach;
use crate::browser::browser_entity::CefBrowserHandlesInner;
use crate::browser::handler_runtime::{
    BrowserCloseGuardsInner, BrowserLifecycleInner,
};
use crate::browser::renderer::gpu::device::SharedGpu;

use super::user_events::{AppEvent, UserEvent};

static WINIT_PROXY_FOR_FOREIGN: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
/// Set when AppKit quit fires before [`register_winit_proxy_for_foreign_callbacks`]; drained in
/// [`super::winit_runner::run_winit`].
static PENDING_REQUEST_QUIT: AtomicBool = AtomicBool::new(false);
static GPU_FOR_FOREIGN: OnceLock<Arc<SharedGpu>> = OnceLock::new();
static FOREIGN_OSR_INDEX: OnceLock<Arc<ForeignOsrIndex>> = OnceLock::new();
static DEVICE_SCALE_FACTOR_FOR_FOREIGN: OnceLock<Arc<Mutex<f32>>> = OnceLock::new();
static BROWSER_CEF_ATTACH_FOR_FOREIGN: OnceLock<Option<CefAttach>> = OnceLock::new();
static BROWSER_LIFECYCLE_FOR_FOREIGN: OnceLock<Arc<Mutex<BrowserLifecycleInner>>> = OnceLock::new();
static BROWSER_CEF_HANDLES_FOR_FOREIGN: OnceLock<Arc<Mutex<CefBrowserHandlesInner>>> = OnceLock::new();
static BROWSER_CLOSE_GUARDS_FOR_FOREIGN: OnceLock<Arc<Mutex<BrowserCloseGuardsInner>>> = OnceLock::new();

/// Register the winit proxy for CEF / AppKit code paths that cannot access Bevy [`bevy_ecs::world::World`].
pub fn register_winit_proxy_for_foreign_callbacks(proxy: EventLoopProxy<UserEvent>) {
    let _ = WINIT_PROXY_FOR_FOREIGN.set(proxy);
}

pub fn schedule_cef_work(delay_ms: i64) {
    let Some(proxy) = WINIT_PROXY_FOR_FOREIGN.get() else {
        return;
    };
    let deadline = Instant::now() + Duration::from_millis(delay_ms.max(0) as u64);
    let _ = proxy.send_event(UserEvent::App(AppEvent::ScheduleCefPump { deadline }));
}

pub fn request_quit() {
    if let Some(proxy) = WINIT_PROXY_FOR_FOREIGN.get() {
        let ok = proxy
            .send_event(UserEvent::App(AppEvent::RequestQuit))
            .is_ok();
        crate::lifecycle_trace::record_runtime_event(&format!(
            "request_quit proxy_send ok={ok}"
        ));
    } else {
        PENDING_REQUEST_QUIT.store(true, Ordering::Release);
        crate::lifecycle_trace::record_runtime_event(
            "request_quit no_proxy set_pending_request_quit",
        );
    }
}

/// Returns true if a quit was pending and should become [`bevy_app::AppExit`] this runner turn.
pub(crate) fn take_pending_request_quit() -> bool {
    PENDING_REQUEST_QUIT.swap(false, Ordering::AcqRel)
}

pub fn send_user_event(event: UserEvent) {
    let Some(proxy) = WINIT_PROXY_FOR_FOREIGN.get() else {
        return;
    };
    let _ = proxy.send_event(event);
}

pub fn register_gpu_runtime_for_foreign_callbacks(
    gpu: Arc<SharedGpu>,
    osr_index: Arc<ForeignOsrIndex>,
    device_scale_factor: Arc<Mutex<f32>>,
) {
    let _ = GPU_FOR_FOREIGN.set(gpu);
    let _ = FOREIGN_OSR_INDEX.set(osr_index);
    let _ = DEVICE_SCALE_FACTOR_FOR_FOREIGN.set(device_scale_factor);
}

/// OSR index + scale only (macOS: before first `NSWindow` + wgpu surface exist).
pub fn register_osr_index_and_scale_for_foreign_callbacks(
    osr_index: Arc<ForeignOsrIndex>,
    device_scale_factor: Arc<Mutex<f32>>,
) {
    let _ = FOREIGN_OSR_INDEX.set(osr_index);
    let _ = DEVICE_SCALE_FACTOR_FOR_FOREIGN.set(device_scale_factor);
}

/// Call after [`register_osr_index_and_scale_for_foreign_callbacks`] when the real [`SharedGpu`] exists.
pub fn register_gpu_only_for_foreign_callbacks(gpu: Arc<SharedGpu>) {
    let _ = GPU_FOR_FOREIGN.set(gpu);
}

fn exit_foreign_missing(what: &str) -> ! {
    eprintln!("vmux FATAL: {what} (foreign OnceLock empty — wrong init order or helper process)");
    std::process::exit(79);
}

pub fn foreign_gpu() -> Arc<SharedGpu> {
    GPU_FOR_FOREIGN
        .get()
        .cloned()
        .unwrap_or_else(|| exit_foreign_missing("GPU not registered for foreign callbacks"))
}

pub fn try_foreign_gpu() -> Option<Arc<SharedGpu>> {
    GPU_FOR_FOREIGN.get().cloned()
}

pub fn foreign_osr_index() -> Arc<ForeignOsrIndex> {
    FOREIGN_OSR_INDEX
        .get()
        .cloned()
        .unwrap_or_else(|| exit_foreign_missing("foreign OSR index not registered"))
}

pub fn try_foreign_osr_index() -> Option<Arc<ForeignOsrIndex>> {
    FOREIGN_OSR_INDEX.get().cloned()
}

pub fn foreign_device_scale_factor() -> Arc<Mutex<f32>> {
    DEVICE_SCALE_FACTOR_FOR_FOREIGN
        .get()
        .cloned()
        .unwrap_or_else(|| exit_foreign_missing("device_scale_factor not registered"))
}

pub fn register_browser_runtime_for_foreign_callbacks(
    cef_handles: Arc<Mutex<CefBrowserHandlesInner>>,
    lifecycle: Arc<Mutex<BrowserLifecycleInner>>,
    close_guards: Arc<Mutex<BrowserCloseGuardsInner>>,
) {
    let _ = BROWSER_CEF_HANDLES_FOR_FOREIGN.set(cef_handles);
    let _ = BROWSER_LIFECYCLE_FOR_FOREIGN.set(lifecycle);
    let _ = BROWSER_CLOSE_GUARDS_FOR_FOREIGN.set(close_guards);
}

pub fn register_cef_attach_for_foreign_callbacks(cef_attach: Option<CefAttach>) {
    let _ = BROWSER_CEF_ATTACH_FOR_FOREIGN.set(cef_attach);
}

pub fn foreign_browser_cef_attach() -> Option<CefAttach> {
    BROWSER_CEF_ATTACH_FOR_FOREIGN.get().cloned().flatten()
}

pub fn foreign_browser_lifecycle() -> Arc<Mutex<BrowserLifecycleInner>> {
    BROWSER_LIFECYCLE_FOR_FOREIGN
        .get()
        .cloned()
        .unwrap_or_else(|| exit_foreign_missing("browser lifecycle not registered"))
}

pub fn foreign_browser_cef_handles() -> Arc<Mutex<CefBrowserHandlesInner>> {
    BROWSER_CEF_HANDLES_FOR_FOREIGN
        .get()
        .cloned()
        .unwrap_or_else(|| exit_foreign_missing("browser CEF handles not registered"))
}

pub fn foreign_browser_close_guards() -> Arc<Mutex<BrowserCloseGuardsInner>> {
    BROWSER_CLOSE_GUARDS_FOR_FOREIGN
        .get()
        .cloned()
        .unwrap_or_else(|| exit_foreign_missing("browser close guards not registered"))
}
