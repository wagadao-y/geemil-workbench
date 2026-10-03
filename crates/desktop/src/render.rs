use eframe::{egui, egui_wgpu::wgpu};
use geemil_core::{Camera, Sample};
use glam::{DMat4, DVec3};
use std::ops::Range;
use wgpu::util::DeviceExt;

// The point pass writes linear RGB to an sRGB target, so the hardware encodes
// it and the EDL pass reads linear RGB back with textureLoad.
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
// egui-wgpu (0.36) samples native textures as plain Rgba8Unorm holding sRGB
// code values, so edl.wgsl encodes its linear result before writing here.
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// sRGB colour of points an exclusion would remove.
const HIGHLIGHT: [u8; 4] = [255, 48, 48, 255];
/// Per-pixel view depth for EDL; 0 marks background.
const VIEW_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

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
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdlUniform {
    radius: f32,
    strength: f32,
    padding: [f32; 2],
}
/// Eye-Dome Lighting. A strength of 0 shows the plain colours.
#[derive(Clone, Copy)]
pub struct Edl {
    /// Neighbour distance in physical pixels.
    pub radius: f32,
    pub strength: f32,
}
/// Size-dependent render targets, recreated when the viewport resizes.
struct Targets {
    size: [u32; 2],
    scene: wgpu::TextureView,
    view_depth: wgpu::TextureView,
    depth: wgpu::TextureView,
    output: wgpu::TextureView,
    edl_bind: wgpu::BindGroup,
}
/// Instances drawn with one transform: a range of uploaded points and the
/// world-space motion to apply to them, e.g. a transform being previewed.
pub struct Segment {
    pub range: Range<u32>,
    pub motion: DMat4,
}
pub struct PointRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    /// One `Uniform` per segment, `uniform_stride` bytes apart, selected with
    /// a dynamic offset.
    uniform: wgpu::Buffer,
    uniform_stride: u64,
    uniform_slots: usize,
    layout: wgpu::BindGroupLayout,
    bind: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    count: u32,
    capacity: usize,
    edl_pipeline: wgpu::RenderPipeline,
    edl_layout: wgpu::BindGroupLayout,
    edl_uniform: wgpu::Buffer,
    targets: Option<Targets>,
    id: Option<egui::TextureId>,
    origin: DVec3,
    uploaded: (u64, u64),
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
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                },
                count: None,
            }],
        });
        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let uniform_stride = UNIFORM_SIZE.div_ceil(alignment) * alignment;
        let (uniform, bind) = uniform_slots(&device, &layout, uniform_stride, 1);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("point splats"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x4],
                })],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(SCENE_FORMAT.into()), Some(VIEW_DEPTH_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let (edl_pipeline, edl_layout) = edl_pipeline(&device);
        let edl_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("edl"),
            size: std::mem::size_of::<EdlUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
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
            uniform_stride,
            uniform_slots: 1,
            layout,
            bind,
            vertices,
            count: 0,
            capacity: 28,
            edl_pipeline,
            edl_layout,
            edl_uniform,
            targets: None,
            id: None,
            origin: DVec3::ZERO,
            uploaded: (0, 0),
        }
    }
    /// Uploads `points` unless the same generation and highlight revision are
    /// resident. `highlight` marks points to draw in `HIGHLIGHT`, with a revision
    /// that changes whenever the marks do; revision 0 means no highlight.
    pub fn upload(
        &mut self,
        points: &[Sample],
        origin: DVec3,
        generation: u64,
        highlight: Option<(&[bool], u64)>,
    ) {
        let key = (generation, highlight.map_or(0, |(_, revision)| revision));
        if key == self.uploaded && origin == self.origin {
            return;
        }
        self.origin = origin;
        self.uploaded = key;
        let marks = highlight.map(|(marks, _)| marks);
        let vertices: Vec<_> = points
            .iter()
            .enumerate()
            .map(|(i, p)| Vertex {
                position: (DVec3::from(p.position) - origin).as_vec3().to_array(),
                // These are sRGB code values; points.wgsl decodes RGB before
                // writing to SCENE_FORMAT. Alpha remains a linear coverage value.
                color: if marks.is_some_and(|m| m.get(i) == Some(&true)) {
                    HIGHLIGHT
                } else {
                    p.color
                }
                .map(|c| c as f32 / 255.),
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
    /// Draws the uploaded points; `segments` moves ranges of them, and `None`
    /// draws everything where it was uploaded.
    pub fn draw(
        &mut self,
        rs: &eframe::egui_wgpu::RenderState,
        camera: &Camera,
        size: [u32; 2],
        point_size: f32,
        edl: Edl,
        segments: Option<&[Segment]>,
    ) -> egui::TextureId {
        let limit = self.device.limits().max_texture_dimension_2d;
        let size = size.map(|v| v.clamp(1, limit));
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            let targets = self.create_targets(size);
            if let Some(id) = self.id {
                rs.renderer.write().update_egui_texture_from_wgpu_texture(
                    &self.device,
                    &targets.output,
                    wgpu::FilterMode::Nearest,
                    id,
                );
            } else {
                self.id = Some(rs.renderer.write().register_native_texture(
                    &self.device,
                    &targets.output,
                    wgpu::FilterMode::Nearest,
                ));
            }
            self.targets = Some(targets);
        }
        let targets = self.targets.as_ref().unwrap();
        let all = [Segment {
            range: 0..self.count,
            motion: DMat4::IDENTITY,
        }];
        let segments: Vec<_> = segments
            .unwrap_or(&all)
            .iter()
            .map(|s| {
                (
                    s.range.start.min(self.count)..s.range.end.min(self.count),
                    s,
                )
            })
            .filter(|(range, _)| !range.is_empty())
            .collect();
        if segments.len() > self.uniform_slots {
            self.uniform_slots = segments.len().next_power_of_two();
            (self.uniform, self.bind) = uniform_slots(
                &self.device,
                &self.layout,
                self.uniform_stride,
                self.uniform_slots,
            );
        }
        // Vertices are relative to `origin`; compose in f64 so large
        // coordinates keep their precision before narrowing to f32.
        let view = camera.relative_matrix() * DMat4::from_translation(-DVec3::from(camera.target));
        let mut uniforms = vec![0u8; segments.len() * self.uniform_stride as usize];
        for ((_, segment), slot) in segments
            .iter()
            .zip(uniforms.chunks_mut(self.uniform_stride as usize))
        {
            let matrix = view * segment.motion * DMat4::from_translation(self.origin);
            let uniform = Uniform {
                matrix: matrix.as_mat4().to_cols_array_2d(),
                viewport: [size[0] as f32, size[1] as f32],
                size: point_size,
                padding: 0.,
            };
            slot[..UNIFORM_SIZE as usize].copy_from_slice(bytemuck::bytes_of(&uniform));
        }
        if !uniforms.is_empty() {
            self.queue.write_buffer(&self.uniform, 0, &uniforms);
        }
        let edl = EdlUniform {
            radius: edl.radius,
            strength: edl.strength,
            padding: [0.; 2],
        };
        self.queue
            .write_buffer(&self.edl_uniform, 0, bytemuck::bytes_of(&edl));
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("point splats"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &targets.scene,
                        depth_slice: None,
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
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &targets.view_depth,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            let vertex = std::mem::size_of::<Vertex>() as u64;
            for (i, (range, _)) in segments.iter().enumerate() {
                pass.set_bind_group(0, &self.bind, &[(i as u64 * self.uniform_stride) as u32]);
                pass.set_vertex_buffer(
                    0,
                    self.vertices
                        .slice(range.start as u64 * vertex..range.end as u64 * vertex),
                );
                pass.draw(0..6, 0..range.len() as u32);
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("eye-dome lighting"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.output,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.edl_pipeline);
            pass.set_bind_group(0, &targets.edl_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.id.unwrap()
    }
    fn create_targets(&self, size: [u32; 2]) -> Targets {
        let texture = |label, format, usage| {
            self.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let scene = texture("point colors", SCENE_FORMAT, sampled);
        let view_depth = texture("point view depth", VIEW_DEPTH_FORMAT, sampled);
        let depth = texture(
            "point depth",
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let output = texture(
            "point viewport",
            OUTPUT_FORMAT,
            sampled | wgpu::TextureUsages::COPY_SRC,
        );
        let edl_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("edl"),
            layout: &self.edl_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&scene),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view_depth),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.edl_uniform.as_entire_binding(),
                },
            ],
        });
        Targets {
            size,
            scene,
            view_depth,
            depth,
            output,
            edl_bind,
        }
    }
}

const UNIFORM_SIZE: u64 = std::mem::size_of::<Uniform>() as u64;

fn uniform_slots(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    stride: u64,
    slots: usize,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("point segments"),
        size: stride * slots as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("point segments"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: 0,
                size: wgpu::BufferSize::new(UNIFORM_SIZE),
            }),
        }],
    });
    (buffer, bind)
}

fn edl_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("eye-dome lighting"),
        source: wgpu::ShaderSource::Wgsl(include_str!("edl.wgsl").into()),
    });
    // Both inputs are read with textureLoad, so no sampler or filtering is needed.
    let texture = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("edl"),
        entries: &[
            texture(0),
            texture(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("edl"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("eye-dome lighting"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment"),
            compilation_options: Default::default(),
            targets: &[Some(OUTPUT_FORMAT.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, layout)
}
