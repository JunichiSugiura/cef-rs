//! Off-screen rendering: winit + wgpu + CEF windowless mode.

mod app;
pub mod color;
mod vim_scroll;
pub mod bootstrap;
pub mod demo_pages;
pub mod gpu;
pub mod hub;
pub mod input;
mod render;
pub mod shutdown;
pub mod titles;
mod window_registry;

pub use app::VmuxOsrApp;
pub use window_registry::{show_all_windows, track_window};
