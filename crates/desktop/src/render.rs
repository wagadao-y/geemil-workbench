use eframe::{egui, egui_wgpu::wgpu};
use geemil_core::{Camera, Sample};
use glam::DVec3;
use wgpu::util::DeviceExt;

// egui-wgpu expects sampled images to yield linear RGB. The target encodes
// linear shader output to sRGB bytes, and texture sampling decodes them again.
const VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    matrix: [[f32; 4]; 4],
    viewport: [f32; 2],
    size: f32,
    padding: f32,
}
pub struct PointRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    count: u32,
    capacity: usize,
    texture: Option<wgpu::Texture>,
    depth: Option<wgpu::Texture>,
    size: [u32; 2],
    id: Option<egui::TextureId>,
    origin: DVec3,
    uploaded_generation: u64,
}
impl PointRenderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("point splats"),
            source: wgpu::ShaderSource::Wgsl(include_str!("points.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("point splats"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x4],
                }],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: VIEW_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 28,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue,
            pipeline,
            uniform,
            bind,
            vertices,
            count: 0,
            capacity: 28,
            texture: None,
            depth: None,
            size: [0, 0],
            id: None,
            origin: DVec3::ZERO,
            uploaded_generation: 0,
        }
    }
    pub fn upload(&mut self, points: &[Sample], origin: DVec3, generation: u64) {
        if generation == self.uploaded_generation && origin == self.origin {
            return;
        }
        self.origin = origin;
        self.uploaded_generation = generation;
        let vertices: Vec<_> = points
            .iter()
            .map(|p| Vertex {
                position: (DVec3::from(p.position) - origin).as_vec3().to_array(),
                // These are sRGB code values; points.wgsl decodes RGB before
                // writing to VIEW_FORMAT. Alpha remains a linear coverage value.
                color: p.color.map(|c| c as f32 / 255.),
            })
            .collect();
        self.count = vertices.len() as u32;
        if !vertices.is_empty() {
            let bytes = bytemuck::cast_slice(&vertices);
            if bytes.len() > self.capacity {
                self.capacity = bytes.len();
                self.vertices = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("resident point samples"),
                        contents: bytes,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    });
            } else {
                self.queue.write_buffer(&self.vertices, 0, bytes);
            }
        }
    }
    pub fn draw(
        &mut self,
        rs: &eframe::egui_wgpu::RenderState,
        camera: &Camera,
        size: [u32; 2],
        point_size: f32,
    ) -> egui::TextureId {
        let limit = self.device.limits().max_texture_dimension_2d;
        let size = size.map(|v| v.clamp(1, limit));
        if size != self.size {
            self.size = size;
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("point viewport"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: VIEW_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            if let Some(id) = self.id {
                rs.renderer.write().update_egui_texture_from_wgpu_texture(
                    &self.device,
                    &view,
                    wgpu::FilterMode::Nearest,
                    id,
                );
            } else {
                self.id = Some(rs.renderer.write().register_native_texture(
                    &self.device,
                    &view,
                    wgpu::FilterMode::Nearest,
                ));
            }
            self.texture = Some(texture);
            self.depth = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("point depth"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            }));
        }
        let matrix = camera.relative_matrix()
            * glam::DMat4::from_translation(self.origin - DVec3::from(camera.target));
        let uniform = Uniform {
            matrix: matrix.as_mat4().to_cols_array_2d(),
            viewport: [size[0] as f32, size[1] as f32],
            size: point_size,
            padding: 0.,
        };
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));
        let view = self
            .texture
            .as_ref()
            .unwrap()
            .create_view(&Default::default());
        let depth = self
            .depth
            .as_ref()
            .unwrap()
            .create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("point viewport"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.025,
                            g: 0.035,
                            b: 0.05,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.draw(0..6, 0..self.count);
        }
        self.queue.submit([encoder.finish()]);
        self.id.unwrap()
    }
}
