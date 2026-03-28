//! CEF integration (backend).

pub mod bootstrap;

pub use bootstrap::{CefPlugin, CefStartupState};
pub use crate::browser::{BrowserClient, BrowserHandler};
pub use crate::browser::event_loop::{
    register_browser_runtime_for_foreign_callbacks, register_gpu_runtime_for_foreign_callbacks,
    register_cef_attach_for_foreign_callbacks, request_quit, schedule_cef_work, send_user_event,
    try_foreign_osr_index,
};

pub mod pump;
pub mod threads;
