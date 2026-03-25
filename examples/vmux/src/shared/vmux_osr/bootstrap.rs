use std::sync::{Arc, Mutex, OnceLock};

use cef::*;

use super::gpu::SharedGpu;
use super::hub::{VmuxOsrAttach, VmuxOsrHub};
use super::render::{VmuxOsrRenderHandler, VmuxOsrRenderInner};

use crate::shared::vmux_handler::VmuxHandlerClient;

static GPU: OnceLock<Arc<SharedGpu>> = OnceLock::new();
static HUB: OnceLock<Arc<VmuxOsrHub>> = OnceLock::new();
static DEVICE_SCALE_FACTOR: OnceLock<Arc<Mutex<f32>>> = OnceLock::new();

pub fn gpu() -> Arc<SharedGpu> {
    GPU.get().expect("vmux-osr: GPU not initialized").clone()
}

pub fn hub() -> Arc<VmuxOsrHub> {
    HUB.get().expect("vmux-osr: hub not initialized").clone()
}

pub fn device_scale_factor() -> Arc<Mutex<f32>> {
    DEVICE_SCALE_FACTOR
        .get()
        .expect("vmux-osr: device_scale_factor not initialized")
        .clone()
}

pub fn set_device_scale_factor(dsf: f32) {
    if let Ok(mut g) = device_scale_factor().lock() {
        *g = dsf.max(0.5);
    }
}

/// Build headless wgpu device, OSR hub, render handler, and `VmuxHandlerClient`.
///
/// Call this on the **process main thread** after `cef::initialize` returns — not from
/// `BrowserProcessHandler::on_context_initialized`, which can run on a different thread than
/// AppKit’s main thread on macOS and may abort Metal / wgpu during `request_adapter`.
pub fn init_client_with_osr(
    client_cell: &std::cell::RefCell<Option<Client>>,
    osr_attach: VmuxOsrAttach,
) {
    let gpu = pollster::block_on(SharedGpu::new_headless());
    let gpu = Arc::new(gpu);
    let hub = VmuxOsrHub::new();
    let dsf = Arc::new(Mutex::new(1.0f32));
    let render_inner = VmuxOsrRenderInner {
        hub: hub.clone(),
        windows_attach: osr_attach.clone(),
        device: gpu.device.clone(),
        queue: gpu.queue.clone(),
        layout: gpu.texture_bind_group_layout.clone(),
        device_scale_factor: dsf.clone(),
        paint_redraw_throttle: hub.paint_redraw_throttle(),
    };
    let rh = VmuxOsrRenderHandler::build(render_inner);
    let handler = crate::shared::vmux_handler::VmuxHandler::new(Some(osr_attach));
    let client = VmuxHandlerClient::new(handler, rh);
    *client_cell.borrow_mut() = Some(client);
    let _ = GPU.set(gpu);
    let _ = HUB.set(hub);
    let _ = DEVICE_SCALE_FACTOR.set(dsf);
}
