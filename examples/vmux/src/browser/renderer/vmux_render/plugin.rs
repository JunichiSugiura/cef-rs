use std::sync::{Arc, Mutex};

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::Resource;
use bevy_render::render_graph::RenderGraph;
use winit::window::WindowId;

use crate::browser::backend::osr::hub::WindowEntry;
use crate::browser::renderer::vmux_render::node::{OsrCompositeGraphLabel, OsrCompositeNode};

use super::extract::ExtractedOsrBrowserIds;

/// Cloned from [`crate::browser::backend::osr::hub::CefAttach::windows_store`] at startup so `bevy_render` graph nodes can lock the same map as dispatch (under `&World`).
#[derive(Resource, Clone)]
pub struct VmuxWindowsStoreResource(
    pub Arc<Mutex<std::collections::HashMap<WindowId, WindowEntry>>>,
);

/// Per-frame token: which window is being composited (inserted immediately before running the graph).
#[derive(Resource, Clone, Copy)]
pub struct VmuxOsrRedrawToken(pub WindowId);

/// Holds the retained [`RenderGraph`] and lazily initialized wgpu 23 handles for a minimal [`bevy_render::renderer::RenderContext`].
#[derive(Resource)]
pub struct VmuxOsrRenderGraphState {
    pub graph: RenderGraph,
    pub(crate) dummy_wgpu: std::sync::OnceLock<DummyWgpuBevy>,
}

pub(crate) struct DummyWgpuBevy {
    pub device: bevy_render::renderer::RenderDevice,
    pub queue: bevy_render::renderer::RenderQueue,
    /// Plain `wgpu` 23 info for [`bevy_render::renderer::RenderContext::new`].
    pub adapter_info_plain: wgpu_bevy::AdapterInfo,
}

impl Default for VmuxOsrRenderGraphState {
    fn default() -> Self {
        let mut graph = RenderGraph::default();
        graph.add_node(OsrCompositeGraphLabel, OsrCompositeNode);
        Self {
            graph,
            dummy_wgpu: std::sync::OnceLock::new(),
        }
    }
}

pub struct VmuxRenderPlugin;

impl Plugin for VmuxRenderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VmuxOsrRenderGraphState>()
            .init_resource::<ExtractedOsrBrowserIds>();
    }
}
