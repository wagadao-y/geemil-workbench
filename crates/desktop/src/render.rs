use eframe::{egui, egui_wgpu::wgpu};
use geemil_core::{Camera, Sample};
use glam::{DMat4, DVec3};
use std::{collections::HashMap, sync::Arc};
use wgpu::util::DeviceExt;

// The point pass writes linear RGB to an sRGB target, so the hardware encodes
// it and the EDL pass reads linear RGB back with textureLoad.
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
// egui-wgpu (0.36) samples native textures as plain Rgba8Unorm holding sRGB
// code values, so edl.wgsl encodes its linear result before writing here.
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Points uploaded per frame at most; the rest follow in later frames so a
/// large view does not stall one.
const UPLOAD_PER_FRAME: usize = 2_000_000;
/// Per-pixel view depth for EDL; 0 marks background.
const VIEW_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    /// Relative to the node's origin.
    position: [f32; 3],
    /// sRGB code values; points.wgsl decodes RGB before writing to
    /// SCENE_FORMAT. Alpha remains a linear coverage value.
    color: [u8; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    matrix: [[f32; 4]; 4],
    viewport: [f32; 2],
    size: f32,
    padding: f32,
    tint: [f32; 4],
    /// View depth of a vertex for EDL as `dot(depth, (position, 1))`.
    depth: [f32; 4],
    /// Maps a vertex into box coordinates; points inside `[-1, 1]` are
    /// highlighted when `box_highlight_on` is 1.
    highlight_box: [[f32; 4]; 4],
    box_highlight_on: f32,
    /// 1 to colour by height with `ramp`.
    ramp_on: f32,
    padding2: [f32; 2],
    /// Height scaled to 0..1 over the ramp as `dot(ramp, (position, 1))`.
    ramp: [f32; 4],
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
/// How to draw the uploaded points.
pub struct DrawOptions<'a> {
    /// Splat diameter in physical pixels.
    pub point_size: f32,
    pub edl: Edl,
    /// Maps the project frame into a box whose interior points are highlighted.
    pub highlight_box: Option<DMat4>,
    /// Colour by height from the first to the second value (project Z).
    pub height_ramp: Option<[f64; 2]>,
    /// Changes whenever the nodes' marks do.
    pub marks_revision: u64,
    pub nodes: &'a [DrawNode<'a>],
}
/// A display octree node to draw: its points in scan coordinates, placed in
/// the project frame by `world`.
pub struct DrawNode<'a> {
    pub samples: &'a Arc<[Sample]>,
    pub world: DMat4,
    /// sRGB colour (0..1) mixed into the points, by the fourth component.
    pub tint: [f32; 4],
    /// Points to highlight, e.g. those a move would take.
    pub marks: Option<&'a [bool]>,
}
/// A node's points on the GPU. It keeps their samples, so a new node never
/// reuses the address that identifies this one.
struct GpuNode {
    _samples: Arc<[Sample]>,
    origin: DVec3,
    vertices: wgpu::Buffer,
    /// One u32 per point; bit 0 highlights it.
    flags: wgpu::Buffer,
    count: u32,
    marked: bool,
    marks_revision: u64,
    used: u64,
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
    /// Resident nodes by the address of their samples.
    nodes: HashMap<usize, GpuNode>,
    frame: u64,
    resident: usize,
    /// Points kept resident at most, besides those drawn this frame.
    limit: usize,
    /// Whether nodes were left for later frames to upload.
    pending: bool,
    edl_pipeline: wgpu::RenderPipeline,
    edl_layout: wgpu::BindGroupLayout,
    edl_uniform: wgpu::Buffer,
    targets: Option<Targets>,
    id: Option<egui::TextureId>,
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
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![0=>Float32x3,1=>Unorm8x4],
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 4,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![2=>Uint32],
                    }),
                ],
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
        Self {
            device,
            queue,
            pipeline,
            uniform,
            uniform_stride,
            uniform_slots: 1,
            layout,
            bind,
            nodes: HashMap::new(),
            frame: 0,
            resident: 0,
            limit: 0,
            pending: false,
            edl_pipeline,
            edl_layout,
            edl_uniform,
            targets: None,
            id: None,
        }
    }
    /// Keeps about `points` resident, like Potree's point load limit.
    pub fn set_point_limit(&mut self, points: usize) {
        self.limit = points;
    }
    /// Whether some nodes still wait for upload in a later frame.
    pub fn pending(&self) -> bool {
        self.pending
    }
    /// Uploads a node unless resident, and brings its marks up to date.
    /// Returns false when it has to wait for a later frame.
    fn resident(&mut self, node: &DrawNode, revision: u64, uploaded: &mut usize) -> bool {
        let key = Arc::as_ptr(node.samples) as *const () as usize;
        if !self.nodes.contains_key(&key) {
            if *uploaded > 0 && *uploaded + node.samples.len() > UPLOAD_PER_FRAME {
                self.pending = true;
                return false;
            }
            *uploaded += node.samples.len();
            let origin = DVec3::from(node.samples[0].position);
            let vertices: Vec<_> = node
                .samples
                .iter()
                .map(|p| Vertex {
                    position: (DVec3::from(p.position) - origin).as_vec3().to_array(),
                    color: p.color,
                })
                .collect();
            let create = |label, contents: &[u8]| {
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(label),
                        contents,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    })
            };
            let gpu = GpuNode {
                _samples: node.samples.clone(),
                origin,
                vertices: create("node points", bytemuck::cast_slice(&vertices)),
                flags: create("node flags", &vec![0u8; node.samples.len() * 4]),
                count: node.samples.len() as u32,
                marked: false,
                marks_revision: 0,
                used: 0,
            };
            self.resident += node.samples.len();
            self.nodes.insert(key, gpu);
        }
        let gpu = self.nodes.get_mut(&key).unwrap();
        gpu.used = self.frame;
        if gpu.marks_revision != revision {
            gpu.marks_revision = revision;
            let marks = node.marks.filter(|m| m.iter().any(|m| *m));
            if marks.is_some() || gpu.marked {
                let flags: Vec<u32> = (0..gpu.count as usize)
                    .map(|i| marks.and_then(|m| m.get(i)).is_some_and(|m| *m) as u32)
                    .collect();
                self.queue
                    .write_buffer(&gpu.flags, 0, bytemuck::cast_slice(&flags));
                gpu.marked = marks.is_some();
            }
        }
        true
    }
    /// Drops the least recently drawn nodes beyond the limit.
    fn evict(&mut self) {
        if self.resident <= self.limit {
            return;
        }
        let mut old: Vec<_> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.used < self.frame)
            .map(|(k, n)| (n.used, *k))
            .collect();
        old.sort_unstable();
        for (_, key) in old {
            if self.resident <= self.limit {
                break;
            }
            let node = self.nodes.remove(&key).unwrap();
            self.resident -= node.count as usize;
        }
    }
    /// Draws the nodes, uploading those not resident yet.
    pub fn draw(
        &mut self,
        rs: &eframe::egui_wgpu::RenderState,
        camera: &Camera,
        size: [u32; 2],
        options: &DrawOptions,
    ) -> egui::TextureId {
        let DrawOptions {
            point_size,
            edl,
            highlight_box,
            height_ramp,
            marks_revision,
            nodes,
        } = *options;
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
        self.frame += 1;
        self.pending = false;
        let mut uploaded = 0;
        let segments: Vec<_> = nodes
            .iter()
            .filter(|n| !n.samples.is_empty())
            .filter(|n| self.resident(n, marks_revision, &mut uploaded))
            .map(|n| (Arc::as_ptr(n.samples) as *const () as usize, n))
            .collect();
        self.evict();
        let targets = self.targets.as_ref().unwrap();
        if segments.len() > self.uniform_slots {
            self.uniform_slots = segments.len().next_power_of_two();
            (self.uniform, self.bind) = uniform_slots(
                &self.device,
                &self.layout,
                self.uniform_stride,
                self.uniform_slots,
            );
        }
        // Vertices are relative to their node's origin; compose in f64 so
        // large coordinates keep their precision before narrowing to f32.
        let view = camera.relative_matrix() * DMat4::from_translation(-DVec3::from(camera.target));
        // Parallel projection sees behind the eye; measure EDL depth from a
        // plane as far behind the eye as the target is in front, so it stays
        // positive and EDL shades like perspective does at the target.
        let mut depth_row = camera.depth_row();
        if camera.ortho {
            depth_row.w += camera.distance;
        }
        let mut uniforms = vec![0u8; segments.len() * self.uniform_stride as usize];
        for ((key, segment), slot) in segments
            .iter()
            .zip(uniforms.chunks_mut(self.uniform_stride as usize))
        {
            let to_world = segment.world * DMat4::from_translation(self.nodes[key].origin);
            let matrix = view * to_world;
            let depth = to_world.transpose() * depth_row;
            let uniform = Uniform {
                matrix: matrix.as_mat4().to_cols_array_2d(),
                viewport: [size[0] as f32, size[1] as f32],
                size: point_size,
                padding: 0.,
                tint: segment.tint,
                depth: depth.as_vec4().to_array(),
                highlight_box: highlight_box
                    .map_or(DMat4::IDENTITY, |c| c * to_world)
                    .as_mat4()
                    .to_cols_array_2d(),
                box_highlight_on: highlight_box.is_some() as u32 as f32,
                ramp_on: height_ramp.is_some() as u32 as f32,
                padding2: [0.; 2],
                ramp: height_ramp
                    .map_or(glam::DVec4::ZERO, |[low, high]| {
                        let z = to_world.row(2) - glam::DVec4::new(0., 0., 0., low);
                        z / (high - low).max(1e-9)
                    })
                    .as_vec4()
                    .to_array(),
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
            for (i, (key, _)) in segments.iter().enumerate() {
                let node = &self.nodes[key];
                pass.set_bind_group(0, &self.bind, &[(i as u64 * self.uniform_stride) as u32]);
                pass.set_vertex_buffer(0, node.vertices.slice(..));
                pass.set_vertex_buffer(1, node.flags.slice(..));
                pass.draw(0..6, 0..node.count);
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
