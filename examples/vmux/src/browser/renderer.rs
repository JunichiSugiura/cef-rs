//! Off-screen rendering: winit + wgpu + CEF windowless mode.

pub mod osr_host;
pub mod gpu;
pub mod input;
pub mod vmux_render;

pub use osr_host::state::OsrHostState;
pub use crate::browser::active_browser::ActiveBrowserId;
pub use crate::browser::backend::cef::bootstrap as bootstrap;
pub use crate::window::{show_all_windows, track_window, WindowRegistryPlugin};
