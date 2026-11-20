//! GI backend: screen-space diffuse GI (SSGI) at half resolution, per view.
//!
//! Per frame (`Ssgi::render`, after the shared depth/normal prepass, before
//! the 3D pass), two fullscreen passes over the view's half-resolution
//! textures (ssgi.wgsl):
//! 1. `fs_trace`: a few cosine-weighted rays per pixel (rotated per pixel
//!    and frame) march the prepass eye distances. A hit reads last frame's
//!    resolved HDR colour (the view's HDR target, before this frame's 3D
//!    pass overwrites it) at the hit's position under last frame's
//!    `view_proj`; a miss, or a hit off last frame's screen, reads the
//!    hemisphere sky light (`sky_light`, as GI off). The mean blends into
//!    the reprojected history (rejected on disocclusion by eye distance and
//!    normal), ping-ponged with the pixel's normal and eye distance.
//! 2. `fs_denoise`: a depth- and normal-aware 5x5 blur into the output the
//!    mesh shader upsamples (gi_ssgi.wgsl) in place of the hemisphere
//!    diffuse. A camera cut, a new view (resize) or a frame without SSGI
//!    resets the history. See AGENTS.md.

use std::cell::Cell;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, UVec2, Vec2, Vec3};

/// Texture format of the history, geometry and output textures.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// An eye jump beyond this (metres per frame) is a camera cut.
const CUT_DISTANCE: f32 = 2.0;
/// A view direction turn beyond this (cosine; 30 degrees) is a camera cut.
const CUT_COS: f32 = 0.866;

/// The SSGI settings of one view as the shaders see them (ssgi.wgsl and
/// gi_ssgi.wgsl `SsgiUniform`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SsgiUniform {
    /// World -> clip of this view's last frame (reprojection).
    prev_view_proj: [[f32; 4]; 4],
    /// Eye of this view's last frame (history eye distances).
    prev_eye: [f32; 3],
    /// Frames with SSGI on this view (noise rotation).
    frame: u32,
    /// 0: no usable last frame (cut, new view, SSGI just on).
    history_valid: u32,
    intensity: f32,
    /// 1 = the mesh shader outputs indirect diffuse only (debug).
    gi_only: u32,
    _pad: u32,
}

/// Size of the SSGI textures for a `width` x `height` view: half, rounded
/// up (every full-resolution pixel has a texel; never 0).
pub fn half_size(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(2).max(1), height.div_ceil(2).max(1))
}

/// Screen uv (0..1, y down) of world point `p` under `view_proj`, `None`
/// behind the camera or off screen (ssgi.wgsl `screen_uv`).
pub fn reproject(view_proj: Mat4, p: Vec3) -> Option<Vec2> {
    let clip = view_proj * p.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let uv = Vec2::new(clip.x, clip.y) / clip.w * Vec2::new(0.5, -0.5) + 0.5;
    (uv.cmpge(Vec2::ZERO).all() && uv.cmplt(Vec2::ONE).all()).then_some(uv)
}

/// The half-resolution texel whose sample (full pixel 2i, centre 2i + 0.5)
/// is nearest screen `uv` of a `full` sized view (ssgi.wgsl `fs_trace`).
pub fn half_texel(uv: Vec2, full: (u32, u32)) -> UVec2 {
    let f = uv * Vec2::new(full.0 as f32, full.1 as f32);
    let (w, h) = half_size(full.0, full.1);
    (f * 0.5 + 0.25)
        .floor()
        .as_uvec2()
        .min(UVec2::new(w - 1, h - 1))
}

/// Whether the camera jumped between two frames (eye moved more than
/// `CUT_DISTANCE`, or the view direction turned more than 30 degrees): the
/// history is then thrown away instead of reprojected.
pub fn is_camera_cut(prev_view_proj: Mat4, prev_eye: Vec3, view_proj: Mat4, eye: Vec3) -> bool {
    let forward = |m: Mat4| {
        let inv = m.inverse();
        (inv.project_point3(Vec3::Z) - inv.project_point3(Vec3::ZERO)).normalize()
    };
    prev_eye.distance(eye) > CUT_DISTANCE
        || forward(prev_view_proj).dot(forward(view_proj)) < CUT_COS
}

/// The SSGI passes (shared by every view) and the stand-ins for views
/// without SSGI textures.
pub(super) struct Ssgi {
    trace_layout: wgpu::BindGroupLayout,
    denoise_layout: wgpu::BindGroupLayout,
    trace_pipeline: wgpu::RenderPipeline,
    denoise_pipeline: wgpu::RenderPipeline,
    /// Group 3 stand-ins: no SSGI texels (the mesh shader falls back to
    /// the hemisphere) and a zero uniform.
    no_output: wgpu::TextureView,
    no_uniform: wgpu::Buffer,
}

/// One view's SSGI textures (half resolution), uniform and history state.
pub(crate) struct SsgiView {
    uniform: wgpu::Buffer,
    /// History (rgb, a = frames accumulated) and geometry (normal, eye
    /// distance), written alternately: frame parity p writes [p], reads [1 - p].
    history: [wgpu::TextureView; 2],
    geometry: [wgpu::TextureView; 2],
    /// Denoised indirect diffuse (rgb) and the geometry it belongs to
    /// (normal, eye distance; 0 = no surface), read by the mesh shader.
    output: wgpu::TextureView,
    output_geometry: wgpu::TextureView,
    trace_groups: [wgpu::BindGroup; 2],
    denoise_groups: [wgpu::BindGroup; 2],
    /// Camera of this view's last SSGI frame (`None`: no history).
    prev: Cell<Option<(Mat4, Vec3)>>,
    /// Frames with SSGI on this view.
    frame: Cell<u32>,
}

/// The view textures the SSGI passes read.
pub(crate) struct SsgiViewInputs<'a> {
    /// Full-resolution prepass depth and normal + eye distance.
    pub depth: &'a wgpu::TextureView,
    pub normal: &'a wgpu::TextureView,
    /// The view's resolved HDR colour, read before the 3D pass writes it.
    pub hdr: &'a wgpu::TextureView,
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layouts: &[&wgpu::BindGroupLayout; 2],
    entry: &str,
    targets: u32,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ssgi pipeline layout"),
        bind_group_layouts: &layouts.map(Some),
        immediate_size: 0,
    });
    let targets: Vec<_> = (0..targets).map(|_| Some(FORMAT.into())).collect();
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn layout_entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty,
        count: None,
    }
}

const TEXTURE: wgpu::BindingType = wgpu::BindingType::Texture {
    sample_type: wgpu::TextureSampleType::Float { filterable: false },
    view_dimension: wgpu::TextureViewDimension::D2,
    multisampled: false,
};
const UNIFORM: wgpu::BindingType = wgpu::BindingType::Buffer {
    ty: wgpu::BufferBindingType::Uniform,
    has_dynamic_offset: false,
    min_binding_size: None,
};

/// This backend's group 3 layout entries: 6 the output, 7 the uniform, 8
/// the output's geometry.
pub(super) fn layout_entries() -> [wgpu::BindGroupLayoutEntry; 3] {
    [
        layout_entry(6, TEXTURE),
        layout_entry(7, UNIFORM),
        layout_entry(8, TEXTURE),
    ]
}

impl Ssgi {
    /// `frame_layout` is group 0 (camera, light) of the passes.
    pub(super) fn new(device: &wgpu::Device, frame_layout: &wgpu::BindGroupLayout) -> Self {
        let depth = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let trace_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ssgi trace bind group layout"),
            entries: &[
                layout_entry(0, depth),
                layout_entry(1, TEXTURE),
                layout_entry(2, UNIFORM),
                layout_entry(3, TEXTURE),
                layout_entry(4, TEXTURE),
                layout_entry(5, TEXTURE),
            ],
        });
        let denoise_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ssgi denoise bind group layout"),
            entries: &[layout_entry(6, TEXTURE), layout_entry(7, TEXTURE)],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ssgi.wgsl"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("../../../../assets/shaders/gi_common.wgsl"),
                    include_str!("../../../../assets/shaders/ssgi.wgsl")
                )
                .into(),
            ),
        });
        let trace_pipeline = fullscreen_pipeline(
            device,
            &module,
            &[frame_layout, &trace_layout],
            "fs_trace",
            2,
        );
        let denoise_pipeline = fullscreen_pipeline(
            device,
            &module,
            &[frame_layout, &denoise_layout],
            "fs_denoise",
            2,
        );
        let no_output = half_texture(device, "ssgi stand-in output", (1, 1));
        let no_uniform = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("ssgi stand-in uniform"),
                contents: bytemuck::bytes_of(&SsgiUniform::zeroed()),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        Self {
            trace_layout,
            denoise_layout,
            trace_pipeline,
            denoise_pipeline,
            no_output,
            no_uniform,
        }
    }

    /// The textures, uniform and bind groups of one `size` view.
    pub(super) fn create_view(
        &self,
        device: &wgpu::Device,
        size: winit::dpi::PhysicalSize<u32>,
        inputs: &SsgiViewInputs,
    ) -> SsgiView {
        let half = half_size(size.width, size.height);
        let history = [0, 1].map(|_| half_texture(device, "ssgi history", half));
        let geometry = [0, 1].map(|_| half_texture(device, "ssgi geometry", half));
        let output = half_texture(device, "ssgi output", half);
        let output_geometry = half_texture(device, "ssgi output geometry", half);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ssgi uniform buffer"),
            size: std::mem::size_of::<SsgiUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let trace_groups = [0, 1].map(|p| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ssgi trace bind group"),
                layout: &self.trace_layout,
                entries: &[
                    view(0, inputs.depth),
                    view(1, inputs.normal),
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                    view(3, inputs.hdr),
                    view(4, &history[1 - p]),
                    view(5, &geometry[1 - p]),
                ],
            })
        });
        let denoise_groups = [0, 1].map(|p| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ssgi denoise bind group"),
                layout: &self.denoise_layout,
                entries: &[view(6, &history[p]), view(7, &geometry[p])],
            })
        });
        SsgiView {
            uniform,
            history,
            geometry,
            output,
            output_geometry,
            trace_groups,
            denoise_groups,
            prev: Cell::new(None),
            frame: Cell::new(0),
        }
    }

    /// Group 3 entries 6 to 8: `view`'s output, uniform and output
    /// geometry, or stand-ins the mesh shader reads as "no SSGI here" (the
    /// hemisphere).
    pub(super) fn entries<'a>(
        &'a self,
        view: Option<&'a SsgiView>,
    ) -> [wgpu::BindGroupEntry<'a>; 3] {
        let (output, uniform, geometry) = match view {
            Some(v) => (&v.output, &v.uniform, &v.output_geometry),
            None => (&self.no_output, &self.no_uniform, &self.no_output),
        };
        [
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(output),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: wgpu::BindingResource::TextureView(geometry),
            },
        ]
    }

    /// Writes `view`'s uniform for a frame from `view_proj` / `eye`.
    /// `intensity` 0 (SSGI not running) drops the view's history.
    pub(super) fn update(
        &self,
        queue: &wgpu::Queue,
        view: &SsgiView,
        intensity: f32,
        gi_only: bool,
        view_proj: Mat4,
        eye: Vec3,
    ) {
        let on = intensity > 0.0;
        let prev = view.prev.take();
        let valid = on && prev.is_some_and(|(m, e)| !is_camera_cut(m, e, view_proj, eye));
        let (prev_view_proj, prev_eye) = prev.unwrap_or((view_proj, eye));
        let uniform = SsgiUniform {
            prev_view_proj: prev_view_proj.to_cols_array_2d(),
            prev_eye: prev_eye.to_array(),
            frame: view.frame.get(),
            history_valid: u32::from(valid),
            intensity,
            gi_only: u32::from(gi_only),
            _pad: 0,
        };
        queue.write_buffer(&view.uniform, 0, bytemuck::bytes_of(&uniform));
        if on {
            view.prev.set(Some((view_proj, eye)));
            view.frame.set(view.frame.get().wrapping_add(1));
        }
    }

    /// The trace + accumulate and denoise passes of `view`. `frame` is
    /// group 0 (this view's camera). Call after `update` and the prepass.
    pub(super) fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::BindGroup,
        view: &SsgiView,
    ) {
        // `update` counted this frame: its parity picks the history it writes.
        let p = (view.frame.get().wrapping_sub(1) % 2) as usize;
        let passes = [
            (
                "melon_render ssgi trace pass",
                &self.trace_pipeline,
                &view.trace_groups[p],
                vec![&view.history[p], &view.geometry[p]],
            ),
            (
                "melon_render ssgi denoise pass",
                &self.denoise_pipeline,
                &view.denoise_groups[p],
                vec![&view.output, &view.output_geometry],
            ),
        ];
        for (label, pipeline, group, targets) in passes {
            let attachments: Vec<_> = targets
                .into_iter()
                .map(|view| {
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // The fullscreen triangle overwrites every texel.
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &attachments,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, frame, &[]);
            pass.set_bind_group(1, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// A `FORMAT` render target sampled by the SSGI passes or the mesh shader.
fn half_texture(
    device: &wgpu::Device,
    label: &str,
    (width, height): (u32, u32),
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::camera::rh::{proj::directx, view};

    fn camera(eye: Vec3, center: Vec3) -> Mat4 {
        directx::perspective(60f32.to_radians(), 16.0 / 9.0, 0.1, 100.0)
            * view::look_at_mat4(eye, center, Vec3::Y)
    }

    #[test]
    fn ssgi_uniform_is_96_bytes_16_aligned() {
        assert_eq!(std::mem::size_of::<SsgiUniform>(), 96);
        assert_eq!(std::mem::offset_of!(SsgiUniform, prev_eye), 64);
        assert_eq!(std::mem::offset_of!(SsgiUniform, history_valid), 80);
    }

    #[test]
    fn half_size_rounds_odd_sizes_up() {
        assert_eq!(half_size(1280, 720), (640, 360));
        assert_eq!(half_size(1281, 721), (641, 361));
        assert_eq!(half_size(1, 1), (1, 1));
        assert_eq!(half_size(0, 3), (1, 2));
        // The last full pixel's sample texel exists.
        let (w, _) = half_size(1281, 721);
        assert_eq!(half_texel(Vec2::new(0.9999, 0.0), (1281, 721)).x, w - 1);
    }

    #[test]
    fn static_point_keeps_its_texel_and_follows_camera_moves() {
        let full = (1281, 721);
        let a = camera(Vec3::new(0.0, 2.0, 6.0), Vec3::ZERO);
        // The world point behind half texel (100, 50)'s sample (full pixel
        // (200, 100)), at some depth.
        let texel = UVec2::new(100, 50);
        let px = (texel * 2).as_vec2() + 0.5;
        let ndc = Vec2::new(
            px.x / full.0 as f32 * 2.0 - 1.0,
            1.0 - px.y / full.1 as f32 * 2.0,
        );
        let p = a.inverse().project_point3(ndc.extend(0.97));
        // Camera did not move: same texel.
        let uv = reproject(a, p).expect("on screen");
        assert_eq!(half_texel(uv, full), texel);
        // Camera stepped 0.5 m right: the point moves left by
        // 0.5 * proj_x / depth in ndc (half that in uv), y unchanged.
        let b = camera(Vec3::new(0.5, 2.0, 6.0), Vec3::new(0.5, 0.0, 0.0));
        let now = reproject(b, p).expect("on screen");
        let depth = (a * p.extend(1.0)).w;
        let proj_x = directx::perspective(60f32.to_radians(), 16.0 / 9.0, 0.1, 100.0)
            .x_axis
            .x;
        let expected = uv.x - 0.5 * 0.5 * proj_x / depth;
        assert!((now.x - expected).abs() < 1e-4, "{now} vs {expected}");
        assert!((now.y - uv.y).abs() < 1e-4, "{now} vs {uv}");
        // Behind the camera or off screen: nothing.
        assert_eq!(reproject(a, Vec3::new(0.0, 2.0, 20.0)), None);
        assert_eq!(reproject(a, Vec3::new(100.0, 0.0, 0.0)), None);
    }

    #[test]
    fn camera_cut_on_jumps_and_turns_only() {
        let eye = Vec3::new(0.0, 2.0, 6.0);
        let a = camera(eye, Vec3::ZERO);
        let step = Vec3::new(0.1, 0.0, 0.0);
        assert!(!is_camera_cut(a, eye, camera(eye + step, step), eye + step));
        let far = eye + Vec3::new(3.0, 0.0, 0.0);
        assert!(is_camera_cut(
            a,
            eye,
            camera(far, Vec3::new(3.0, 0.0, 0.0)),
            far
        ));
        let turned = camera(eye, eye + Vec3::new(1.0, 0.0, 0.0));
        assert!(is_camera_cut(a, eye, turned, eye));
    }
}
