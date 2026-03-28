//! Bevy custom runner + winit-backed main-thread event loop for vmux (`crate::browser::event_loop`).
//!
//! The winit [`UserEvent`](user_events::UserEvent) payload is split into [`AppEvent`](user_events::AppEvent),
//! [`CefEvent`](user_events::CefEvent), and [`VimiumEvent`](user_events::VimiumEvent).
//! [`AppUserEvent`] is a type alias for [`UserEvent`](user_events::UserEvent) (back-compat).

mod foreign_callbacks;
mod systems;
mod user_events;
mod winit_runner;

pub use crate::browser::browser_entity::{
    BrowserDespawnEvent, BrowserSpawnEvent, BrowserEntities, BrowserId, BrowserWindowId,
    CefBrowserHandle, CefBrowserHandles, CefBrowserHandlesInner, OsrPaintBindGroup, OsrViewLogicalSize,
};
pub use crate::browser::handler_runtime::{
    apply_close_all_browsers_requests_system, BrowserCloseGuardsInner, BrowserCloseGuardsResource,
    BrowserLifecycleInner, BrowserLifecycleResource, RequestCloseAllBrowsersEvent,
};

pub use foreign_callbacks::{
    foreign_browser_cef_attach, foreign_browser_cef_handles, foreign_browser_close_guards,
    foreign_browser_lifecycle, foreign_device_scale_factor, foreign_gpu, foreign_osr_index,
    register_browser_runtime_for_foreign_callbacks, register_cef_attach_for_foreign_callbacks,
    register_gpu_only_for_foreign_callbacks, register_gpu_runtime_for_foreign_callbacks,
    register_osr_index_and_scale_for_foreign_callbacks, register_winit_proxy_for_foreign_callbacks,
    request_quit, schedule_cef_work, send_user_event, try_foreign_gpu, try_foreign_osr_index,
};
pub use systems::{
    apply_pending_set_active_browser_system, drain_signal_quit_to_request_quit_system,
    EditableFocusQueues, emit_app_exit_on_shutdown_signal, handle_app_exit_for_graceful_shutdown,
    process_editable_focus_probe_queue_system, process_editable_vimium_replay_queue_system,
    AppExitRequested, WinitPlugin,
};
// Used as `crate::browser::event_loop::…` from other modules; not referenced inside this file.
#[allow(unused_imports)]
pub(crate) use systems::with_osr_host_runtime_and_vimium;
pub use user_events::{
    ingest_link_hints_nav_invalidate_events_system, AppEvent, AppUserEvent, CefEvent, CefPumpDeadline,
    LinkHintFeedEvent, LinkHintsNavPending, RuntimeState, ShutdownFlag, SignalQuitFlag, UserEvent,
    VimiumEvent, VimiumKeyReplayEvent,
};
pub use winit_runner::{build_event_loop, run_winit, WinitAppRunnerState};
#[allow(unused_imports)]
pub(crate) use winit_runner::flush_winit_runner_pending_callbacks;
