use std::sync::Arc;
use wgpu::util::DeviceExt;
use wgpu::Backends;
use winit::dpi::PhysicalSize;
use winit::window::Window;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub tex_coords: [f32; 2],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

pub struct Geometry {
    pub vertex_buffer: wgpu::Buffer,
    pub vertex_count: u32,
}

impl Geometry {
    pub fn new(device: &wgpu::Device) -> Self {
        let x = -1.0;
        let y = 1.0;
        let width = 2.0;
        let height = 2.0;
        let z = 1.0;
        let vertices = [
            Vertex {
                position: [x, y, z],
                tex_coords: [0.0, 0.0],
            },
            Vertex {
                position: [x + width, y, z],
                tex_coords: [1.0, 0.0],
            },
            Vertex {
                position: [x, y - height, z],
                tex_coords: [0.0, 1.0],
            },
            Vertex {
                position: [x + width, y - height, z],
                tex_coords: [1.0, 1.0],
            },
        ];
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vmux-osr quad"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Self {
            vertex_buffer,
            vertex_count: vertices.len() as u32,
        }
    }
}

pub struct SharedGpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub texture_bind_group_layout: Arc<wgpu::BindGroupLayout>,
    pub surface_format: wgpu::TextureFormat,
}

impl SharedGpu {
    pub async fn new_headless() -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            #[cfg(target_os = "windows")]
            backends: Backends::from_comma_list("dx12"),
            #[cfg(target_os = "macos")]
            backends: Backends::from_comma_list("metal"),
            #[cfg(target_os = "linux")]
            backends: Backends::from_comma_list("vulkan"),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("vmux-osr: request_adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits {
                    max_non_sampler_bindings: 2048,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .expect("vmux-osr: request_device");
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("vmux-osr cef texture"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        Self {
            instance,
            adapter,
            device,
            queue,
            texture_bind_group_layout: Arc::new(texture_bind_group_layout),
            surface_format: wgpu::TextureFormat::Bgra8Unorm,
        }
    }

    pub fn create_pipeline(&self, surface_format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        let shader = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vmux-osr shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vmux-osr pipeline layout"),
                bind_group_layouts: &[&self.texture_bind_group_layout],
                immediate_size: 0,
            });
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vmux-osr pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Vertex::desc()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        // We always render the quad as opaque (shader outputs alpha=1).
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Cw,
                    cull_mode: Some(wgpu::Face::Back),
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
    }
}

pub struct WindowSurface {
    pub window: Arc<Window>,
    pub surface: wgpu::Surface<'static>,
    pub pipeline: wgpu::RenderPipeline,
    pub quad: Geometry,
    pub configured_size: PhysicalSize<u32>,
    pub surface_format: wgpu::TextureFormat,
    pub alpha_mode: wgpu::CompositeAlphaMode,
}

impl WindowSurface {
    pub fn new(gpu: &SharedGpu, window: Arc<Window>) -> Self {
        let surface = gpu
            .instance
            .create_surface(window.clone())
            .expect("vmux-osr: create_surface");
        let caps = surface.get_capabilities(&gpu.adapter);
        let surface_format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .or_else(|| caps.formats.first().copied())
            .expect("vmux-osr: surface has no supported formats");
        // Prefer an opaque swapchain if supported. If the swapchain is translucent and the
        // sampled content carries alpha < 1 (common with accelerated OSR textures), the whole
        // window will look like a grey/dim overlay due to compositor blending.
        let alpha_mode = caps
            .alpha_modes
            .iter()
            .copied()
            .find(|m| *m == wgpu::CompositeAlphaMode::Opaque)
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);

        if cfg!(debug_assertions) && std::env::var_os("VMUX_OSR_DEBUG_GPU").is_some() {
            println!(
                "[vmux-osr gpu pid={}] surface_format={:?} (srgb={}) alpha_mode={:?} caps_formats={:?} caps_alpha_modes={:?}",
                std::process::id(),
                surface_format,
                surface_format.is_srgb(),
                alpha_mode,
                caps.formats,
                caps.alpha_modes
            );
        }
        let pipeline = gpu.create_pipeline(surface_format);
        let quad = Geometry::new(&gpu.device);
        let size = window.inner_size();
        let mut s = Self {
            window,
            surface,
            pipeline,
            quad,
            configured_size: size,
            surface_format,
            alpha_mode,
        };
        s.configure(gpu);
        s
    }

    pub fn configure(&mut self, gpu: &SharedGpu) {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.surface_format,
            view_formats: vec![self.surface_format],
            alpha_mode: self.alpha_mode,
            width: self.configured_size.width.max(1),
            height: self.configured_size.height.max(1),
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::AutoVsync,
        };
        self.surface.configure(&gpu.device, &config);
    }

    pub fn resize(&mut self, gpu: &SharedGpu, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.configured_size = new_size;
            self.configure(gpu);
        }
    }

    pub fn render(&mut self, gpu: &SharedGpu, bind_group: Option<&wgpu::BindGroup>) {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => return,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("vmux-osr surface"),
            ..Default::default()
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vmux-osr encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vmux-osr pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });
            // Only draw the textured quad once we have a texture bind-group from CEF.
            // Otherwise we just clear the surface. The pipeline expects bind group 0,
            // so drawing without it is a wgpu validation error.
            if let Some(bg) = bind_group {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, Some(bg), &[]);
                pass.set_vertex_buffer(0, self.quad.vertex_buffer.slice(..));
                pass.draw(0..self.quad.vertex_count, 0..1);
            }
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        frame.present();
    }
}
