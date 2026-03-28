//! `bevy_render`-shaped OSR composite: [`RenderGraph`] + extract + flush on redraw (see [`super::gpu`](crate::browser::renderer::gpu)).

pub mod extract;
pub mod node;
pub mod plugin;
pub mod runner;

pub use extract::ExtractedOsrBrowserIds;
pub use node::OsrCompositeGraphLabel;
pub use plugin::{VmuxOsrRedrawToken, VmuxOsrRenderGraphState, VmuxRenderPlugin, VmuxWindowsStoreResource};
pub use runner::run_osr_composite_for_window;
