use bevy_ecs::world::World;
use ::cef::ImplBrowser as _;
use bevy_render::render_graph::{Node, NodeRunError, RenderGraphContext, RenderLabel};
use bevy_render::renderer::RenderContext;

use crate::browser::backend::cef::bootstrap::{ForeignOsrIndexResource, GpuResource};
use crate::browser::backend::osr::foreign_index;
use crate::browser::renderer::vmux_render::plugin::{VmuxOsrRedrawToken, VmuxWindowsStoreResource};

/// Label for the OSR→swapchain composite node in [`super::plugin::VmuxOsrRenderGraphState::graph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, RenderLabel)]
pub struct OsrCompositeGraphLabel;

pub struct OsrCompositeNode;

impl Node for OsrCompositeNode {
    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        _render_context: &mut RenderContext,
        world: &World,
    ) -> Result<(), NodeRunError> {
        let Some(token) = world.get_resource::<VmuxOsrRedrawToken>() else {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "OSR composite: missing VmuxOsrRedrawToken (skip)"
            );
            return Ok(());
        };
        let window_id = token.0;

        let gpu = world.resource::<GpuResource>().0.clone();
        let index = world.resource::<ForeignOsrIndexResource>().0.clone();
        let store = world.resource::<VmuxWindowsStoreResource>().0.clone();

        let mut guard = store.lock().unwrap_or_else(|e| {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "OSR composite: windows_store mutex poisoned, using inner map"
            );
            e.into_inner()
        });
        let Some(entry) = guard.get_mut(&window_id) else {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "OSR composite: WindowId {:?} not in windows_store (race or stale redraw; skip)",
                window_id
            );
            return Ok(());
        };
        let browser_id = entry.browser.identifier();

        let painted = foreign_index::with_bind_group(index.as_ref(), browser_id, |bg| {
            entry.surface.composite_osr_frame(&*gpu, Some(bg));
        })
        .is_some();

        if !painted {
            entry.surface.composite_osr_frame(&*gpu, None);
        }
        entry.surface.window.request_redraw();
        Ok(())
    }
}
