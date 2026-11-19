//! Overlay layer: solid 3D editor shapes (gizmos, later icons and debug
//! shapes) drawn after tonemapping, in exact display colours.

use std::f32::consts::TAU;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

/// Unit meshes every overlay draw is built from (scaled and placed by
/// `OverlayDraw::transform`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverlayShape {
    /// Radius 1, from y = 0 to y = 1.
    Cylinder,
    /// Base radius 1 at y = 0, tip at y = 1.
    Cone,
    /// -0.5..0.5 on every axis.
    Cube,
    /// Radius 1.
    Sphere,
    /// Major radius 1 in the XZ plane, tube radius 0.03.
    Torus,
    /// 0..1 in X and Z at y = 0.
    Quad,
    /// (0,0,0), (1,0,0), (0,0,1).
    Triangle,
}

impl OverlayShape {
    pub const ALL: [OverlayShape; 7] = [
        OverlayShape::Cylinder,
        OverlayShape::Cone,
        OverlayShape::Cube,
        OverlayShape::Sphere,
        OverlayShape::Torus,
        OverlayShape::Quad,
        OverlayShape::Triangle,
    ];
}

/// Which depth an overlay draw is tested against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayDepth {
    /// Over the scene (hides only behind other on-top shapes): gizmos.
    OnTop,
    /// Hidden behind scene geometry: icons, debug shapes.
    SceneTested,
}

/// One shape to draw this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayDraw {
    pub shape: OverlayShape,
    /// Unit shape to world.
    pub transform: Mat4,
    /// Display (sRGB) colour, straight alpha; never tonemapped.
    pub color: [f32; 4],
    pub depth: OverlayDepth,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct OverlayVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

const SEGMENTS: u16 = 24;
/// Tube radius of the unit torus (thin rings at any ring size).
pub const TORUS_TUBE: f32 = 0.03;

fn vertex(position: Vec3, normal: Vec3) -> OverlayVertex {
    OverlayVertex {
        position: position.to_array(),
        normal: normal.normalize().to_array(),
    }
}

/// Two triangles over a grid of `rings` x `SEGMENTS + 1` vertices.
fn grid_indices(rings: u16, first: u16, out: &mut Vec<u16>) {
    let row = SEGMENTS + 1;
    for r in 0..rings - 1 {
        for s in 0..SEGMENTS {
            let a = first + r * row + s;
            let b = a + row;
            out.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
    }
}

/// A flat disc at height `y` facing `normal_y` (+1 or -1).
fn disc(y: f32, normal_y: f32, vertices: &mut Vec<OverlayVertex>, indices: &mut Vec<u16>) {
    let centre = vertices.len() as u16;
    let n = Vec3::new(0.0, normal_y, 0.0);
    vertices.push(vertex(Vec3::new(0.0, y, 0.0), n));
    for s in 0..=SEGMENTS {
        let a = TAU * s as f32 / SEGMENTS as f32;
        vertices.push(vertex(Vec3::new(a.cos(), y, a.sin()), n));
    }
    for s in 0..SEGMENTS {
        indices.extend([centre, centre + 1 + s, centre + 2 + s]);
    }
}

/// Vertices and triangle indices of a unit shape. Faces are drawn without
/// culling, so winding doesn't matter.
pub(crate) fn shape_mesh(shape: OverlayShape) -> (Vec<OverlayVertex>, Vec<u16>) {
    let mut v = Vec::new();
    let mut i = Vec::new();
    let around = |s: u16| {
        let a = TAU * s as f32 / SEGMENTS as f32;
        (a.cos(), a.sin())
    };
    match shape {
        OverlayShape::Cylinder => {
            for y in [0.0, 1.0] {
                for s in 0..=SEGMENTS {
                    let (c, sn) = around(s);
                    v.push(vertex(Vec3::new(c, y, sn), Vec3::new(c, 0.0, sn)));
                }
            }
            grid_indices(2, 0, &mut i);
            disc(0.0, -1.0, &mut v, &mut i);
            disc(1.0, 1.0, &mut v, &mut i);
        }
        OverlayShape::Cone => {
            for y in [0.0, 1.0] {
                for s in 0..=SEGMENTS {
                    let (c, sn) = around(s);
                    let r = 1.0 - y;
                    v.push(vertex(Vec3::new(c * r, y, sn * r), Vec3::new(c, 1.0, sn)));
                }
            }
            grid_indices(2, 0, &mut i);
            disc(0.0, -1.0, &mut v, &mut i);
        }
        OverlayShape::Cube => {
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                for sign in [1.0, -1.0] {
                    let n = axis * sign;
                    let (u, w) = (
                        n.any_orthonormal_vector(),
                        n.cross(n.any_orthonormal_vector()),
                    );
                    let first = v.len() as u16;
                    for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                        v.push(vertex((n + u * a + w * b) * 0.5, n));
                    }
                    i.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
                }
            }
        }
        OverlayShape::Sphere => {
            let rings = 13;
            for r in 0..rings {
                let theta = std::f32::consts::PI * r as f32 / (rings - 1) as f32;
                for s in 0..=SEGMENTS {
                    let (c, sn) = around(s);
                    let p = Vec3::new(theta.sin() * c, theta.cos(), theta.sin() * sn);
                    // Poles: any horizontal tangent is fine for a unit normal.
                    v.push(vertex(p, if p.length() > 0.0 { p } else { Vec3::Y }));
                }
            }
            grid_indices(rings, 0, &mut i);
        }
        OverlayShape::Torus => {
            let tube = SEGMENTS / 2;
            for t in 0..=tube {
                let b = TAU * t as f32 / tube as f32;
                for s in 0..=SEGMENTS {
                    let (c, sn) = around(s);
                    let ring = Vec3::new(c, 0.0, sn);
                    let n = ring * b.cos() + Vec3::Y * b.sin();
                    v.push(vertex(ring + n * TORUS_TUBE, n));
                }
            }
            grid_indices(tube + 1, 0, &mut i);
        }
        OverlayShape::Quad => {
            for (x, z) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                v.push(vertex(Vec3::new(x, 0.0, z), Vec3::Y));
            }
            i.extend([0, 1, 2, 0, 2, 3]);
        }
        OverlayShape::Triangle => {
            for (x, z) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)] {
                v.push(vertex(Vec3::new(x, 0.0, z), Vec3::Y));
            }
            i.extend([0, 1, 2]);
        }
    }
    (v, i)
}

/// Colour format of the overlay layer (exact display colours).
pub(crate) const OVERLAY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Per-draw instance data (model matrix, normal matrix, colour).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct OverlayInstance {
    model: [[f32; 4]; 4],
    normal: [[f32; 4]; 3],
    color: [f32; 4],
}

impl OverlayInstance {
    fn new(draw: &OverlayDraw) -> Self {
        let m3 = glam::Mat3::from_mat4(draw.transform);
        let normal = if m3.determinant().abs() > 1e-12 {
            m3.inverse().transpose()
        } else {
            glam::Mat3::IDENTITY
        };
        Self {
            model: draw.transform.to_cols_array_2d(),
            normal: [
                normal.x_axis.extend(0.0).to_array(),
                normal.y_axis.extend(0.0).to_array(),
                normal.z_axis.extend(0.0).to_array(),
            ],
            color: draw.color,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct OverlayCamera {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
}

struct ShapeMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

/// The colour targets one viewport's overlay renders into.
pub(crate) struct OverlayTargets {
    msaa_view: wgpu::TextureView,
    resolve_view: wgpu::TextureView,
}

impl OverlayTargets {
    pub(crate) fn new(
        device: &wgpu::Device,
        size: winit::dpi::PhysicalSize<u32>,
        samples: u32,
    ) -> Self {
        let texture = |label, sample_count, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: OVERLAY_FORMAT,
                usage,
                view_formats: &[],
            })
        };
        let msaa = texture(
            "overlay msaa",
            samples,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let resolve = texture(
            "overlay resolve",
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        Self {
            msaa_view: msaa.create_view(&wgpu::TextureViewDescriptor::default()),
            resolve_view: resolve.create_view(&wgpu::TextureViewDescriptor::default()),
        }
    }
}

/// GPU side of the overlay layer: shape meshes, pipelines and the passes that
/// draw `OverlayDraw`s and composite them onto a viewport's display image.
pub(crate) struct OverlayRenderer {
    meshes: Vec<ShapeMesh>,
    opaque: wgpu::RenderPipeline,
    blended: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    composite: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
}

const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
    2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
    6 => Float32x4, 7 => Float32x4, 8 => Float32x4, 9 => Float32x4,
];

impl OverlayRenderer {
    pub(crate) fn new(
        device: &wgpu::Device,
        display_format: wgpu::TextureFormat,
        samples: u32,
    ) -> Self {
        use wgpu::util::DeviceExt;
        let meshes = OverlayShape::ALL
            .iter()
            .map(|&shape| {
                let (vertices, indices) = shape_mesh(shape);
                ShapeMesh {
                    vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("overlay shape vertices"),
                        contents: bytemuck::cast_slice(&vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("overlay shape indices"),
                        contents: bytemuck::cast_slice(&indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                    count: indices.len() as u32,
                }
            })
            .collect();

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay camera"),
            size: std::mem::size_of::<OverlayCamera>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay camera"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let shader = device
            .create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/overlay.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });
        let pipeline = |label, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[
                        wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<OverlayVertex>()
                                as wgpu::BufferAddress,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &wgpu::vertex_attr_array![
                                0 => Float32x3,
                                1 => Float32x3,
                            ],
                        },
                        wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<OverlayInstance>()
                                as wgpu::BufferAddress,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &INSTANCE_ATTRIBUTES,
                        },
                    ],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: OVERLAY_FORMAT,
                        // Colour straight-alpha over, alpha accumulates: the
                        // target ends up premultiplied for the composite.
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::OVER,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = pipeline("overlay opaque pipeline", true);
        let blended = pipeline("overlay blended pipeline", false);

        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay composite layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let composite_shader = device.create_shader_module(wgpu::include_wgsl!(
            "../../../assets/shaders/composite.wgsl"
        ));
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay composite pipeline layout"),
                bind_group_layouts: &[Some(&composite_layout)],
                immediate_size: 0,
            });
        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay composite pipeline"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: display_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let instance_capacity = 64;
        let instances = Self::instance_buffer(device, instance_capacity);
        Self {
            meshes,
            opaque,
            blended,
            camera_buffer,
            camera_bind_group,
            instances,
            instance_capacity,
            composite,
            composite_layout,
        }
    }

    fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay instances"),
            size: (capacity * std::mem::size_of::<OverlayInstance>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Draw `draws` over the scene already in `display` (the viewport's
    /// tonemapped image). `depth` is the scene's MSAA depth; the on-top layer
    /// clears it, so nothing may read it afterwards.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        targets: &OverlayTargets,
        depth: &wgpu::TextureView,
        display: &wgpu::TextureView,
        view_proj: Mat4,
        eye: Vec3,
        draws: &[OverlayDraw],
    ) {
        if draws.is_empty() {
            return;
        }
        // Scene-tested, then on-top; in each, opaque before blended so
        // see-through shapes never hide opaque ones behind them.
        let mut ordered: Vec<&OverlayDraw> = draws.iter().collect();
        ordered.sort_by_key(|d| (d.depth == OverlayDepth::OnTop, d.color[3] < 1.0));
        let instances: Vec<OverlayInstance> =
            ordered.iter().map(|d| OverlayInstance::new(d)).collect();
        if instances.len() > self.instance_capacity {
            self.instance_capacity = instances.len().next_power_of_two();
            self.instances = Self::instance_buffer(device, self.instance_capacity);
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        let camera = OverlayCamera {
            view_proj: view_proj.to_cols_array_2d(),
            eye: eye.extend(1.0).to_array(),
        };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[camera]));

        let tested = ordered
            .iter()
            .filter(|d| d.depth == OverlayDepth::SceneTested)
            .count();
        for (on_top, range) in [(false, 0..tested), (true, tested..ordered.len())] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(if on_top {
                    "overlay on-top pass"
                } else {
                    "overlay scene-tested pass"
                }),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa_view,
                    depth_slice: None,
                    resolve_target: on_top.then_some(&targets.resolve_view),
                    ops: wgpu::Operations {
                        load: if on_top {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: if on_top {
                            wgpu::LoadOp::Clear(1.0)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for i in range {
                let draw = ordered[i];
                let shape = OverlayShape::ALL
                    .iter()
                    .position(|s| *s == draw.shape)
                    .unwrap_or(0);
                let mesh = &self.meshes[shape];
                pass.set_pipeline(if draw.color[3] < 1.0 {
                    &self.blended
                } else {
                    &self.opaque
                });
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint16);
                let instance = i as u32;
                pass.draw_indexed(0..mesh.count, 0, instance..instance + 1);
            }
        }

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay composite"),
            layout: &self.composite_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&targets.resolve_view),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("overlay composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: display,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.composite);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn bounds(shape: OverlayShape) -> (Vec3, Vec3) {
        let (vertices, _) = shape_mesh(shape);
        vertices.iter().fold(
            (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
            |(lo, hi), v| {
                let p = Vec3::from_array(v.position);
                (lo.min(p), hi.max(p))
            },
        )
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-3
    }

    #[test]
    fn shape_bounds_match_unit_spaces() {
        let expect = [
            (
                OverlayShape::Cylinder,
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 1.0, 1.0),
            ),
            (
                OverlayShape::Cone,
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 1.0, 1.0),
            ),
            (OverlayShape::Cube, Vec3::splat(-0.5), Vec3::splat(0.5)),
            (OverlayShape::Sphere, Vec3::splat(-1.0), Vec3::splat(1.0)),
            (
                OverlayShape::Torus,
                Vec3::new(-1.03, -0.03, -1.03),
                Vec3::new(1.03, 0.03, 1.03),
            ),
            (OverlayShape::Quad, Vec3::ZERO, Vec3::new(1.0, 0.0, 1.0)),
            (OverlayShape::Triangle, Vec3::ZERO, Vec3::new(1.0, 0.0, 1.0)),
        ];
        for (shape, lo, hi) in expect {
            let (min, max) = bounds(shape);
            assert!(
                close(min, lo) && close(max, hi),
                "{shape:?}: {min} .. {max}"
            );
        }
    }

    #[test]
    fn normals_are_unit_length() {
        for shape in OverlayShape::ALL {
            for v in shape_mesh(shape).0 {
                let n = Vec3::from_array(v.normal).length();
                assert!((n - 1.0).abs() < 1e-4, "{shape:?} normal length {n}");
            }
        }
    }

    #[test]
    fn indices_are_in_range() {
        for shape in OverlayShape::ALL {
            let (vertices, indices) = shape_mesh(shape);
            assert_eq!(indices.len() % 3, 0, "{shape:?}");
            assert!(!indices.is_empty());
            assert!(
                indices.iter().all(|&i| (i as usize) < vertices.len()),
                "{shape:?}"
            );
        }
    }
}
