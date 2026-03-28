//! CEF thread identity for vmux.
//!
//! [`ThreadId::UI`] is a **fixed** CEF constant — not discovered at runtime — so it is safe to keep
//! in a Bevy [`Resource`] ([`CefThreadContext`]).
//!
//! Whether the **current OS thread** is the CEF UI thread (`currently_on`) **must not** be stored in
//! a global cached `bool`: the same `Resource` is visible from the winit main thread, worker
//! threads, and the CEF UI thread, and each has a different answer. Always call [`on_cef_ui_thread`]
//! or [`CefThreadContext::on_cef_ui_thread`] at the point of use.
//!
//! Per-browser “thread” metadata is usually unnecessary: OSR browsers are always driven from the CEF
//! UI thread for callbacks; use this module + events instead of duplicating `ThreadId::UI` literals.

use bevy_ecs::prelude::Resource;
use ::cef::{ThreadId, currently_on};

/// Well-known CEF thread ids for systems that already have `Res`/`ResMut` (e.g. `Update`).
#[derive(Resource, Clone, Copy)]
pub struct CefThreadContext {
    /// Browser / UI work must be scheduled with [`cef::post_task`] targeting this id when not
    /// already on this thread.
    pub ui: ThreadId,
}

impl Default for CefThreadContext {
    fn default() -> Self {
        Self { ui: ThreadId::UI }
    }
}

impl CefThreadContext {
    /// `true` if the **current** thread is CEF’s UI thread. See module docs — do not cache the result.
    #[inline]
    pub fn on_cef_ui_thread(&self) -> bool {
        on_cef_ui_thread()
    }
}

/// CEF UI thread id (constant). For code without `World` access (CEF callbacks, `post_task` targets).
#[inline]
pub fn cef_ui_thread_id() -> ThreadId {
    ThreadId::UI
}

/// Whether the **current OS thread** is CEF’s UI thread.
#[inline]
pub fn on_cef_ui_thread() -> bool {
    currently_on(ThreadId::UI) != 0
}
