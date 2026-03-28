//! Winit window + OSR host state for windowless CEF views (queues, attach, client holder).

pub mod state;
pub mod lifecycle;
pub mod quit_feedback;
pub mod window_effect;
pub mod titles;
#[cfg(target_os = "macos")]
pub mod macos_lifecycle;
