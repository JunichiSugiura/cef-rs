use std::sync::Arc;

use bevy_ecs::prelude::Mut;
use bevy_ecs::world::World;
use bevy_render::render_graph::{RenderGraphContext, SlotValue};
use bevy_render::renderer::RenderContext;
use winit::window::WindowId;

use crate::browser::event_loop::RuntimeState;
use crate::browser::renderer::vmux_render::extract::extract_osr_browser_ids;
use crate::browser::renderer::vmux_render::plugin::{DummyWgpuBevy, VmuxOsrRedrawToken, VmuxOsrRenderGraphState};

/// `bevy_render::RenderContext::finish` expects Bevy’s global task pools (normally from `DefaultPlugins`).
fn ensure_bevy_render_task_pools() {
    use bevy_tasks::{
        AsyncComputeTaskPool, ComputeTaskPool, IoTaskPool, TaskPoolBuilder,
    };
    ComputeTaskPool::get_or_init(|| {
        TaskPoolBuilder::new()
            .thread_name("vmux Compute".to_string())
            .build()
    });
    AsyncComputeTaskPool::get_or_init(|| {
        TaskPoolBuilder::new()
            .thread_name("vmux AsyncCompute".to_string())
            .build()
    });
    IoTaskPool::get_or_init(|| {
        TaskPoolBuilder::new()
            .thread_name("vmux Io".to_string())
            .build()
    });
}

/// Run extract → insert redraw token → execute the single OSR composite [`bevy_render::render_graph::Node`], then submit the **dummy** wgpu 23 encoder (OSR work uses [`GpuResource`] / vmux wgpu 28 inside the node).
pub fn run_osr_composite_for_window(world: &mut World, window_id: WindowId) {
    ensure_bevy_render_task_pools();
    extract_osr_browser_ids(world);
    world.insert_resource(VmuxOsrRedrawToken(window_id));
    let (device, queue, adapter_info_plain) = world.resource_scope(|world, mut state: Mut<VmuxOsrRenderGraphState>| {
        state.graph.update(world);
        let d = state.ensure_dummy_wgpu_bevy();
        (
            d.device.clone(),
            d.queue.clone(),
            d.adapter_info_plain.clone(),
        )
    });
    let mut render_context = RenderContext::new(device, adapter_info_plain, None);
    if let Err(e) = run_single_osr_node(&*world, &mut render_context) {
        bevy_log::error!(
            target: "vmux",
            pid = std::process::id(),
            "OSR composite node failed (skipping frame): {e:?}"
        );
        drop(render_context);
        world.remove_resource::<VmuxOsrRedrawToken>();
        return;
    }
    let (command_buffers, _, _diag) = render_context.finish();
    queue.submit(command_buffers);
    world.remove_resource::<VmuxOsrRedrawToken>();
}

fn run_single_osr_node<'a>(
    world: &'a World,
    render_context: &mut RenderContext<'a>,
) -> Result<(), bevy_render::render_graph::NodeRunError> {
    let graph = &world.resource::<VmuxOsrRenderGraphState>().graph;
    let node_state = graph
        .get_node_state(super::node::OsrCompositeGraphLabel)
        .expect("vmux: OsrCompositeGraphLabel node");
    let inputs: &[SlotValue] = &[];
    let mut outputs: Vec<Option<SlotValue>> = vec![None; node_state.output_slots.len()];
    let mut ctx = RenderGraphContext::new(graph, node_state, inputs, &mut outputs);
    node_state.node.run(&mut ctx, render_context, world)?;
    Ok(())
}

impl VmuxOsrRenderGraphState {
    pub(crate) fn ensure_dummy_wgpu_bevy(&mut self) -> &DummyWgpuBevy {
        self.dummy_wgpu.get_or_init(|| {
            pollster::block_on(async {
                init_dummy_wgpu_bevy_async().await.unwrap_or_else(|e| {
                    bevy_log::error!(
                        target: "vmux",
                        pid = std::process::id(),
                        "FATAL: bevy_render dummy wgpu 23 init failed: {e}"
                    );
                    eprintln!("vmux: FATAL dummy wgpu (bevy_render): {e}");
                    std::process::exit(1);
                })
            })
        })
    }
}

async fn init_dummy_wgpu_bevy_async() -> Result<DummyWgpuBevy, String> {
    use bevy_render::renderer::{RenderDevice, RenderQueue, WgpuWrapper};
    use wgpu_bevy::Backends;

    // Match [`SharedGpu::new_headless`] — `Backends::all()` can pick a bad backend on macOS and
    // `request_adapter` then fails or misbehaves next to the Metal OSR instance.
    let backends = {
        #[cfg(target_os = "windows")]
        {
            Backends::DX12
        }
        #[cfg(target_os = "macos")]
        {
            Backends::METAL
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Backends::VULKAN
        }
        #[cfg(not(any(
            target_os = "windows",
            target_os = "macos",
            unix
        )))]
        {
            Backends::all()
        }
    };

    let instance = wgpu_bevy::Instance::new(wgpu_bevy::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let adapter = instance
        .request_adapter(&wgpu_bevy::RequestAdapterOptions::default())
        .await
        .ok_or_else(|| {
            "dummy wgpu 23: request_adapter returned None (try GPU / Metal availability)".to_string()
        })?;
    let adapter_info_plain = adapter.get_info();
    let (device, queue) = adapter
        .request_device(
            &wgpu_bevy::DeviceDescriptor {
                label: Some("vmux bevy_render dummy"),
                required_features: wgpu_bevy::Features::empty(),
                required_limits: wgpu_bevy::Limits::default(),
                memory_hints: wgpu_bevy::MemoryHints::default(),
            },
            None,
        )
        .await
        .map_err(|e| format!("dummy wgpu 23: request_device failed: {e}"))?;
    Ok(DummyWgpuBevy {
        device: RenderDevice::from(device),
        queue: RenderQueue(Arc::new(WgpuWrapper::new(queue))),
        adapter_info_plain,
    })
}

/// Pending shell: same as inline path in [`crate::window::dispatch`] before CEF attaches.
pub fn composite_pending_surface(world: &mut World, window_id: WindowId) {
    use crate::browser::backend::cef::bootstrap::GpuResource;

    let gpu = world.resource::<GpuResource>().0.clone();
    let mut rt = world.resource_mut::<RuntimeState>();
    if let Some(p) = rt
        .pending_browser_hosts
        .iter_mut()
        .find(|p| p.surface.window.id() == window_id)
    {
        p.surface.composite_osr_frame(&*gpu, None);
        p.surface.window.request_redraw();
    }
}

/// Drains [`RuntimeState::vmux_osr_redraw_queue`] after OSR window dispatches (see `WinitPlugin` chain).
pub fn vmux_osr_flush_redraw_queue_system(world: &mut World) {
    use crate::browser::backend::cef::bootstrap::GpuResource;

    // macOS: Bevy `GpuResource` is inserted in `winit_runner_resumed` after the first window; an
    // early `RedrawRequested` before that would otherwise panic in `composite_pending_surface`.
    let qsize = world
        .get_resource::<crate::browser::event_loop::RuntimeState>()
        .map(|rt| rt.vmux_osr_redraw_queue.len())
        .unwrap_or(0);
    if world.get_resource::<GpuResource>().is_none() {
        if qsize > 0 {
            crate::lifecycle_trace::record_runtime_event(&format!(
                "vmux_osr_flush_skipped_no_gpu redraw_queue_len={qsize}"
            ));
        }
        return;
    }

    let pending: Vec<WindowId> = {
        let mut rt = world.resource_mut::<RuntimeState>();
        rt.vmux_osr_redraw_queue.drain(..).collect()
    };
    for wid in pending {
        flush_osr_redraw_for_window(world, wid);
    }
}

/// Full redraw flush: attached window uses the render graph; pending host uses [`composite_pending_surface`].
pub fn flush_osr_redraw_for_window(world: &mut World, window_id: WindowId) {
    use super::plugin::VmuxWindowsStoreResource;

    let attached = world
        .resource::<VmuxWindowsStoreResource>()
        .0
        .lock()
        .map(|g| g.contains_key(&window_id))
        .unwrap_or(false);
    if attached {
        run_osr_composite_for_window(world, window_id);
    } else {
        composite_pending_surface(world, window_id);
    }
}
