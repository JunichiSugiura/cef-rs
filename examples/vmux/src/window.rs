//! Winit [`WindowEvent`] dispatch into CEF OSR and ECS window registry — separate from
//! [`crate::browser::renderer::osr_host`] OSR host state / lifecycle.

pub mod dispatch;
pub mod pending_window_events;
pub mod registry;

pub use registry::{show_all_windows, track_window, WindowComponent, WindowRegistryPlugin};
