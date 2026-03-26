//! Off-screen rendering: winit + wgpu + CEF windowless mode.

mod app;
pub(crate) mod cef_pump;
pub mod color;
mod vim_modes;
mod vim_scroll;
mod vim_state;
pub mod bootstrap;
pub mod demo_pages;
pub mod gpu;
pub mod hub;
pub mod input;
pub mod event_loop;
mod render;
pub mod shutdown;
pub mod titles;
mod window_registry;

pub use app::VmuxOsrApp;
pub use window_registry::{show_all_windows, track_window};
