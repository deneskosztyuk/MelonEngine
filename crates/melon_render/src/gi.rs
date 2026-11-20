//! Dynamic diffuse global illumination (DDGI) with software voxel tracing.
//!
//! Per frame (`render_gi`, after the sun shadow pass): clear the voxel
//! volume, voxelise every instance in the GI volume (three orthographic
//! passes, X, Y and Z, writing radiance from the fragment stage), then the
//! probe update (one compute workgroup per probe: trace rays through the
//! voxels, blend irradiance and distance moments into the atlases with
//! hysteresis), then copy the new atlases over the sampled ones. The
//! mesh shader reads the atlases from group 3. See AGENTS.md.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, UVec3, Vec2, Vec3};

use crate::{Mesh, SceneRenderer};

// ponytail: fixed volume around the origin (the sun shadow box's footprint);
// it should follow the camera (with probe scrolling) once scenes outgrow it.
/// Min corner of the GI volume (world, metres). Its floor sits half a metre
/// below y = 0, so the lowest probe row is 0.75 m above ground.
pub const GI_ORIGIN: Vec3 = Vec3::new(-crate::SHADOW_RADIUS, -0.5, -crate::SHADOW_RADIUS);
/// Probes along x, y, z; probe i sits at `GI_ORIGIN + (i + 0.5) * PROBE_SPACING`.
pub const PROBE_COUNTS: UVec3 = UVec3::new(16, 8, 16);
/// Metres between probes.
pub const PROBE_SPACING: f32 = 2.5;
/// Voxels along x, y, z over the volume (40 x 20 x 40 m).
pub const VOXEL_COUNTS: UVec3 = UVec3::new(128, 64, 128);
/// Voxel edge in metres. Walls under 0.35 m (a voxel plus the 1 cm inward
/// push of each face, see mesh.wgsl `fs_voxel`) leak light.
pub const VOXEL_SIZE: f32 = 2.0 * crate::SHADOW_RADIUS / 128.0;
/// Rays per probe per frame (gi_common.wgsl `GI_RAYS`, the trace workgroup size).
pub const GI_RAYS: u32 = 64;
/// Interior texels per side of a probe's irradiance and depth tiles (each
/// tile has a 1-texel border more).
pub const IRRADIANCE_TEXELS: u32 = 8;
pub const DEPTH_TEXELS: u32 = 16;
/// Weight of last frame's probe values once converged.
pub const GI_HYSTERESIS: f32 = 0.97;
/// Format of both probe atlases.
const ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// GI volume size in metres.
pub fn volume_size() -> Vec3 {
    PROBE_COUNTS.as_vec3() * PROBE_SPACING
}

/// The GI settings as the shaders see them (gi_common.wgsl `GiUniform`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct GiUniform {
    origin: [f32; 3],
    intensity: f32,
    probe_counts: [u32; 3],
    gi_only: u32,
    voxel_counts: [u32; 3],
    voxel_size: f32,
    spacing: f32,
    hysteresis: f32,
    max_depth: f32,
    _pad: f32,
    ray_rotation: [[f32; 4]; 4],
}

/// Ray `i` of `n` on the unit sphere, spherical Fibonacci (mirrors
/// gi.wgsl `ray_direction` before the rotation).
pub fn spherical_fibonacci(i: u32, n: u32) -> Vec3 {
    let golden_ratio = (1.0 + 5f32.sqrt()) / 2.0;
    let phi = std::f32::consts::TAU * (i as f32 / golden_ratio).fract();
    let cos_theta = 1.0 - (2.0 * i as f32 + 1.0) / n as f32;
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    Vec3::new(phi.cos() * sin_theta, phi.sin() * sin_theta, cos_theta)
}

fn sign_not_zero(v: Vec2) -> Vec2 {
    Vec2::new(
        if v.x >= 0.0 { 1.0 } else { -1.0 },
        if v.y >= 0.0 { 1.0 } else { -1.0 },
    )
}

/// Unit direction to the octahedral square [-1, 1]^2 (gi_common.wgsl).
pub fn oct_encode(n: Vec3) -> Vec2 {
    let p = Vec2::new(n.x, n.y) / (n.x.abs() + n.y.abs() + n.z.abs());
    if n.z < 0.0 {
        (Vec2::ONE - Vec2::new(p.y.abs(), p.x.abs())) * sign_not_zero(p)
    } else {
        p
    }
}

/// Octahedral square [-1, 1]^2 to unit direction (gi_common.wgsl).
pub fn oct_decode(e: Vec2) -> Vec3 {
    let z = 1.0 - e.x.abs() - e.y.abs();
    let xy = if z < 0.0 {
        (Vec2::ONE - Vec2::new(e.y.abs(), e.x.abs())) * sign_not_zero(e)
    } else {
        e
    };
    Vec3::new(xy.x, xy.y, z).normalize()
}

/// Probe grid cell of probe `index` (x fastest, then y, then z).
pub fn probe_cell(index: u32) -> UVec3 {
    let c = PROBE_COUNTS;
    UVec3::new(index % c.x, (index / c.x) % c.y, index / (c.x * c.y))
}

/// Index of probe `cell`.
pub fn probe_index(cell: UVec3) -> u32 {
    cell.x + PROBE_COUNTS.x * (cell.y + PROBE_COUNTS.y * cell.z)
}

/// World position of probe `cell`.
pub fn probe_position(cell: UVec3) -> Vec3 {
    GI_ORIGIN + (cell.as_vec3() + 0.5) * PROBE_SPACING
}

/// The probe cell containing world point `p` (clamped to the grid).
pub fn probe_cell_at(p: Vec3) -> UVec3 {
    ((p - GI_ORIGIN) / PROBE_SPACING)
        .floor()
        .clamp(Vec3::ZERO, (PROBE_COUNTS - 1).as_vec3())
        .as_uvec3()
}

/// Size in texels of an atlas with `n` interior texels per tile side:
/// tiles laid out as (x + X * y, z).
fn atlas_size(n: u32) -> (u32, u32) {
    (
        PROBE_COUNTS.x * PROBE_COUNTS.y * (n + 2),
        PROBE_COUNTS.z * (n + 2),
    )
}

/// Blend weight of last frame's probe values after `updates` updates: a
/// running mean at first (the first update overwrites), then `GI_HYSTERESIS`.
fn hysteresis(updates: u32) -> f32 {
    (updates as f32 / (updates as f32 + 1.0)).min(GI_HYSTERESIS)
}

/// A deterministic pseudo-random rotation for update `frame` (splitmix64
/// bits into Shoemake's uniform quaternion).
fn ray_rotation(frame: u32) -> Mat4 {
    let mut state = u64::from(frame);
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    };
    let (u1, u2, u3) = (next(), next(), next());
    let tau = std::f32::consts::TAU;
    let (a, b) = ((1.0 - u1).sqrt(), u1.sqrt());
    Mat4::from_quat(
        Quat::from_xyzw(
            a * (tau * u2).sin(),
            a * (tau * u2).cos(),
            b * (tau * u3).sin(),
            b * (tau * u3).cos(),
        )
        .normalize(),
    )
}

/// Group 3 layout entries 1 to 4 (irradiance atlas, depth atlas, sampler,
/// `GiUniform`); entry 0 is the AO texture.
pub(crate) fn lighting_layout_entries() -> [wgpu::BindGroupLayoutEntry; 4] {
    let atlas = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    [
        atlas(1),
        atlas(2),
        wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 4,
            // The voxel passes project with it in the vertex stage.
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
    ]
}

/// Frame resources the GI passes read (from `SceneRenderer::new`).
pub(crate) struct GiInputs<'a> {
    pub light: &'a wgpu::Buffer,
    pub environment: &'a wgpu::Buffer,
    pub shadow_view: &'a wgpu::TextureView,
    pub shadow_sampler: &'a wgpu::Sampler,
    pub model_layout: &'a wgpu::BindGroupLayout,
    pub material_layout: &'a wgpu::BindGroupLayout,
    pub lighting_layout: &'a wgpu::BindGroupLayout,
}

/// One probe atlas: the texture the mesh shader samples and the storage
/// texture the update writes, copied over it after each update.
struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    scratch: wgpu::Texture,
}

impl Atlas {
    fn new(device: &wgpu::Device, label: &str, n: u32) -> Self {
        let (width, height) = atlas_size(n);
        let texture = |usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: ATLAS_FORMAT,
                usage,
                view_formats: &[],
            })
        };
        let atlas = texture(wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        let scratch = texture(wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC);
        Self {
            view: atlas.create_view(&wgpu::TextureViewDescriptor::default()),
            texture: atlas,
            scratch,
        }
    }
}

/// The GI world data and passes, shared by every view.
pub(crate) struct Gi {
    uniform: wgpu::Buffer,
    irradiance: Atlas,
    depth: Atlas,
    sampler: wgpu::Sampler,
    /// 3 u32 per voxel: fixed-point radiance + 1 (0 = empty), atomicMax.
    voxels: wgpu::Buffer,
    voxel_pipeline: wgpu::RenderPipeline,
    /// Voxel pass group 0 per axis (X, Y, Z).
    voxel_groups: [wgpu::BindGroup; 3],
    /// Colour attachment the voxel passes need but never write.
    voxel_target: wgpu::TextureView,
    /// One workgroup per probe: trace its rays, blend both atlas tiles.
    update_pipeline: wgpu::ComputePipeline,
    update_group: wgpu::BindGroup,
    /// Probe updates since GI was last off (hysteresis and ray rotation).
    updates: u32,
    /// GI was on in the last `update_gi`: the mesh draws use the GI pipeline.
    pub(crate) on: bool,
    gi_only: bool,
}

impl Gi {
    pub(crate) fn new(device: &wgpu::Device, inputs: &GiInputs) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gi uniform buffer"),
            size: std::mem::size_of::<GiUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let irradiance = Atlas::new(device, "gi irradiance atlas", IRRADIANCE_TEXELS);
        let depth = Atlas::new(device, "gi depth atlas", DEPTH_TEXELS);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("gi atlas sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let voxel_count = VOXEL_COUNTS.x * VOXEL_COUNTS.y * VOXEL_COUNTS.z;
        let voxels = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gi voxel buffer"),
            size: u64::from(voxel_count) * 3 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty,
            count: None,
        };
        let buffer = |ty| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let uniform_ty = buffer(wgpu::BufferBindingType::Uniform);
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let voxel_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gi voxel bind group layout"),
            entries: &[
                entry(1, fragment, uniform_ty),
                entry(
                    2,
                    fragment,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    3,
                    fragment,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                ),
                entry(
                    5,
                    fragment,
                    buffer(wgpu::BufferBindingType::Storage { read_only: false }),
                ),
                entry(6, wgpu::ShaderStages::VERTEX, uniform_ty),
            ],
        });
        let voxel_groups = [0u32, 1, 2].map(|axis| {
            let axis_buffer = wgpu::util::DeviceExt::create_buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("gi voxel axis buffer"),
                    contents: bytemuck::bytes_of(&[axis, 0, 0, 0]),
                    usage: wgpu::BufferUsages::UNIFORM,
                },
            );
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("gi voxel bind group"),
                layout: &voxel_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: inputs.light.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(inputs.shadow_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(inputs.shadow_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: voxels.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: axis_buffer.as_entire_binding(),
                    },
                ],
            })
        });
        let voxel_target = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("gi voxel pass target"),
                size: wgpu::Extent3d {
                    width: VOXEL_COUNTS.max_element(),
                    height: VOXEL_COUNTS.max_element(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let voxel_pipeline = build_voxel_pipeline(
            device,
            &[
                &voxel_layout,
                inputs.model_layout,
                inputs.material_layout,
                inputs.lighting_layout,
            ],
        );

        let compute = wgpu::ShaderStages::COMPUTE;
        let read_texture = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let write_texture = wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: ATLAS_FORMAT,
            view_dimension: wgpu::TextureViewDimension::D2,
        };
        let update_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gi update bind group layout"),
            entries: &[
                entry(0, compute, uniform_ty),
                entry(1, compute, uniform_ty),
                entry(2, compute, uniform_ty),
                entry(
                    3,
                    compute,
                    buffer(wgpu::BufferBindingType::Storage { read_only: true }),
                ),
                entry(4, compute, read_texture),
                entry(5, compute, read_texture),
                entry(6, compute, write_texture),
                entry(7, compute, write_texture),
            ],
        });
        let irradiance_scratch = irradiance.scratch.create_view(&Default::default());
        let depth_scratch = depth.scratch.create_view(&Default::default());
        let update_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gi update bind group"),
            layout: &update_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: inputs.light.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: inputs.environment.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: voxels.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&irradiance.view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&depth.view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&irradiance_scratch),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&depth_scratch),
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gi.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("../../../assets/shaders/gi_common.wgsl"),
                    include_str!("../../../assets/shaders/gi.wgsl")
                )
                .into(),
            ),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gi update pipeline layout"),
            bind_group_layouts: &[Some(&update_layout)],
            immediate_size: 0,
        });
        let update_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("melon_render gi probe update pipeline"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("cs_update"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            update_pipeline,
            uniform,
            irradiance,
            depth,
            sampler,
            voxels,
            voxel_pipeline,
            voxel_groups,
            voxel_target,
            update_group,
            updates: 0,
            on: false,
            gi_only: false,
        }
    }

    /// Group 3 entries 1 to 4 for a lighting bind group.
    pub(crate) fn lighting_entries(&self) -> [wgpu::BindGroupEntry<'_>; 4] {
        [
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&self.irradiance.view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&self.depth.view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: self.uniform.as_entire_binding(),
            },
        ]
    }
}

/// The voxel pipeline: mesh.wgsl `vs_voxel` / `fs_voxel`, no culling, no
/// depth, one sample. `layouts`: voxel group 0, model, material, lighting.
fn build_voxel_pipeline(
    device: &wgpu::Device,
    layouts: &[&wgpu::BindGroupLayout; 4],
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mesh.wgsl (voxel)"),
        source: wgpu::ShaderSource::Wgsl(crate::MESH_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gi voxel pipeline layout"),
        bind_group_layouts: &layouts.map(Some),
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("melon_render gi voxel pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_voxel"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<crate::Vertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3,  // position
                    1 => Float32x3,  // normal
                    2 => Float32x2,  // uv
                    3 => Float32x4,  // tangent + handedness
                ],
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_voxel"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl SceneRenderer {
    /// Debug: the mesh shader outputs only the indirect diffuse term.
    pub fn set_gi_only(&mut self, gi_only: bool) {
        self.gi.gi_only = gi_only;
    }

    /// Writes the GI uniform for this frame. `intensity` 0 turns GI off and
    /// restarts convergence for the next frame it is on.
    pub(crate) fn update_gi(&mut self, intensity: f32) {
        let gi = &mut self.gi;
        let on = intensity > 0.0;
        gi.on = on;
        if !on {
            gi.updates = 0;
        }
        let uniform = GiUniform {
            origin: GI_ORIGIN.to_array(),
            intensity: if on { intensity } else { 0.0 },
            probe_counts: PROBE_COUNTS.to_array(),
            gi_only: u32::from(gi.gi_only),
            voxel_counts: VOXEL_COUNTS.to_array(),
            voxel_size: VOXEL_SIZE,
            spacing: PROBE_SPACING,
            hysteresis: hysteresis(gi.updates),
            max_depth: 1.5 * PROBE_SPACING * 3f32.sqrt(),
            _pad: 0.0,
            ray_rotation: ray_rotation(gi.updates).to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&gi.uniform, 0, bytemuck::bytes_of(&uniform));
        if on {
            gi.updates += 1;
        }
    }

    /// Voxelises `draw_calls` (the instances in the GI volume) and updates
    /// the probes. Call after `update_gi`, `update_instances` and the
    /// shadow pass, before the 3D pass.
    pub(crate) fn render_gi(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        draw_calls: &[(&Mesh, &wgpu::BindGroup, std::ops::Range<u32>)],
    ) {
        let gi = &self.gi;
        encoder.clear_buffer(&gi.voxels, 0, None);
        // Viewport (one pixel per voxel) of the projections along X, Y, Z.
        let c = VOXEL_COUNTS;
        let viewports = [(c.z, c.y), (c.x, c.z), (c.x, c.y)];
        for (group, (width, height)) in gi.voxel_groups.iter().zip(viewports) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("melon_render gi voxel pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gi.voxel_target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
            pass.set_pipeline(&gi.voxel_pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.set_bind_group(1, &self.instance_bind_group, &[]);
            pass.set_bind_group(3, &self.unoccluded_lighting, &[]);
            for (mesh, material, instances) in draw_calls {
                pass.set_bind_group(2, *material, &[]);
                pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
            }
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("melon_render gi probe update"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&gi.update_pipeline);
            pass.set_bind_group(0, &gi.update_group, &[]);
            pass.dispatch_workgroups(PROBE_COUNTS.x * PROBE_COUNTS.y * PROBE_COUNTS.z, 1, 1);
        }
        for atlas in [&gi.irradiance, &gi.depth] {
            encoder.copy_texture_to_texture(
                atlas.scratch.as_image_copy(),
                atlas.texture.as_image_copy(),
                atlas.texture.size(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spherical_fibonacci_is_unit_and_balanced() {
        let mut mean = Vec3::ZERO;
        for i in 0..GI_RAYS {
            let d = spherical_fibonacci(i, GI_RAYS);
            assert!((d.length() - 1.0).abs() < 1e-5, "ray {i}: {d}");
            mean += d / GI_RAYS as f32;
        }
        assert!(mean.length() < 0.02, "mean {mean}");
        // Every octant gets rays (roughly uniform, no clumping).
        for octant in 0..8 {
            let hits = (0..GI_RAYS)
                .map(|i| spherical_fibonacci(i, GI_RAYS))
                .filter(|d| {
                    (d.x >= 0.0) == (octant & 1 == 0)
                        && (d.y >= 0.0) == (octant & 2 == 0)
                        && (d.z >= 0.0) == (octant & 4 == 0)
                })
                .count();
            assert!((4..=12).contains(&hits), "octant {octant}: {hits} rays");
        }
    }

    #[test]
    fn octahedral_round_trip() {
        for i in 0..500 {
            let d = spherical_fibonacci(i, 500);
            let e = oct_encode(d);
            assert!(e.abs().max_element() <= 1.0 + 1e-6, "{e}");
            let back = oct_decode(e);
            assert!((back - d).length() < 1e-4, "{d} -> {e} -> {back}");
        }
        for d in [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
        ] {
            assert!((oct_decode(oct_encode(d)) - d).length() < 1e-5, "{d}");
        }
    }

    #[test]
    fn probe_index_cell_and_position_round_trip() {
        let count = PROBE_COUNTS.x * PROBE_COUNTS.y * PROBE_COUNTS.z;
        assert_eq!(count, 2048);
        for index in 0..count {
            let cell = probe_cell(index);
            assert_eq!(probe_index(cell), index);
            assert_eq!(probe_cell_at(probe_position(cell)), cell);
        }
        assert_eq!(probe_position(UVec3::ZERO), GI_ORIGIN + 0.5 * PROBE_SPACING);
        // The probe grid and the voxel grid span the same volume.
        assert_eq!(volume_size(), VOXEL_COUNTS.as_vec3() * VOXEL_SIZE);
        assert_eq!(volume_size(), Vec3::new(40.0, 20.0, 40.0));
    }

    #[test]
    fn gi_uniform_is_128_bytes_16_aligned() {
        assert_eq!(std::mem::size_of::<GiUniform>(), 128);
        assert_eq!(std::mem::size_of::<GiUniform>() % 16, 0);
        assert_eq!(std::mem::offset_of!(GiUniform, ray_rotation), 64);
    }

    #[test]
    fn hysteresis_starts_as_running_mean() {
        assert_eq!(hysteresis(0), 0.0);
        assert_eq!(hysteresis(1), 0.5);
        assert_eq!(hysteresis(1000), GI_HYSTERESIS);
    }

    #[test]
    fn ray_rotation_is_a_deterministic_rotation() {
        let (a, b) = (ray_rotation(7), ray_rotation(8));
        assert_eq!(a, ray_rotation(7));
        assert_ne!(a, b);
        let x = a.transform_vector3(Vec3::X);
        assert!((x.length() - 1.0).abs() < 1e-5);
        assert!((a.determinant() - 1.0).abs() < 1e-4);
    }
}
