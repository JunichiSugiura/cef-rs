//! Windowless rendering: wgpu surfaces and presentation (hub + OSR handler live in `backend::osr`).
//!
//! ## Bevy `bevy_render` and `SharedGpu` (spike)
//!
//! - **Branch A (single `wgpu::Device` shared with Bevy):** `bevy_render` 0.15 is built on `wgpu` 23;
//!   vmux and CEF use workspace `wgpu` 28 (`SharedGpu`). Types do not unify,
//!   so `RenderDevice::from(Device)` cannot wrap vmux’s device without aligning the entire workspace on one
//!   `wgpu` major (or upgrading Bevy).
//! - **Branch B (current):** One GPU context for CEF/OSR (`SharedGpu`, `wgpu` 28). Composite encode/present runs
//!   on that device. A minimal `wgpu` 23 stack exists only so `bevy_render::render_graph::Node::run` receives a
//!   valid `RenderContext`; the OSR node does not use it for the CEF→swapchain path.

pub mod device;
pub mod color;
