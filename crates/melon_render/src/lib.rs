use std::sync::Arc;

mod cascades;
mod extract;
pub mod gi;
mod gltf_materials;
mod ibl;
mod lights;
mod local_shadows;
mod materials;
mod models;
mod overlay;
mod packed_gltf;
mod present;
mod timings;

pub use extract::{FrameData, blends, extract};
pub use gltf_materials::extract_materials;
pub use lights::LocalLight;
pub use overlay::{OverlayDepth, OverlayDraw, OverlayShape, TORUS_TUBE};

/// One node of a glTF file's scene, as the editor spawns it.
#[derive(Clone, Debug, PartialEq)]
pub struct GltfNode {
    pub name: Option<String>,
    /// Index into the returned list; `None` for the scene's root nodes.
    pub parent: Option<usize>,
    pub translation: glam::Vec3,
    pub rotation: glam::Quat,
    pub scale: glam::Vec3,
    /// glTF mesh index, drawn as `<file>#mesh<N>`.
    pub mesh: Option<usize>,
}

/// The node tree of a glTF / GLB file's default scene (or first scene),
/// depth-first, parents before children. Reads no buffers or images.
pub fn gltf_nodes(path: &std::path::Path) -> Result<Vec<GltfNode>> {
    let gltf = gltf::Gltf::open(path)
        .with_context(|| format!("failed to read glTF file: {}", path.display()))?;
    nodes_of(&gltf.document)
}

/// `gltf_nodes` for a file already in memory.
pub fn gltf_nodes_from_slice(bytes: &[u8]) -> Result<Vec<GltfNode>> {
    let gltf = gltf::Gltf::from_slice(bytes).context("failed to read glTF data")?;
    nodes_of(&gltf.document)
}

/// Bounds of a glTF / GLB file's default scene from the POSITION accessors'
/// min / max and the node transforms (no vertex data is read). `None` if
/// the scene has no meshes.
pub fn gltf_bounds(path: &std::path::Path) -> Result<Option<(glam::Vec3, glam::Vec3)>> {
    let gltf = gltf::Gltf::open(path)
        .with_context(|| format!("failed to read glTF file: {}", path.display()))?;
    bounds_of(&gltf.document)
}

/// `gltf_bounds` for a file already in memory.
pub fn gltf_bounds_from_slice(bytes: &[u8]) -> Result<Option<(glam::Vec3, glam::Vec3)>> {
    let gltf = gltf::Gltf::from_slice(bytes).context("failed to read glTF data")?;
    bounds_of(&gltf.document)
}

/// Side of a Content Browser thumbnail, in pixels.
pub const THUMBNAIL_SIZE: u32 = 128;

/// `(view_proj, eye)` for a thumbnail of the box `min..max`: a fixed
/// three-quarter view from +x+y+z, 45 degree field of view, fitted to the
/// box's bounding sphere (DirectX depth, like the rest of the renderer).
pub fn thumbnail_camera(min: glam::Vec3, max: glam::Vec3) -> (glam::Mat4, glam::Vec3) {
    let fov = 45f32.to_radians();
    let centre = (min + max) / 2.0;
    let radius = ((max - min).length() / 2.0).max(0.01);
    let distance = radius / (fov / 2.0).sin() * 1.1;
    let eye = centre + glam::Vec3::new(1.0, 0.8, 1.0).normalize() * distance;
    let view = glam::camera::rh::view::look_at_mat4(eye, centre, glam::Vec3::Y);
    let near = (distance - radius * 1.5).max(0.001);
    let proj =
        glam::camera::rh::proj::directx::perspective(fov, 1.0, near, distance + radius * 1.5);
    (proj * view, eye)
}

fn bounds_of(document: &gltf::Document) -> Result<Option<(glam::Vec3, glam::Vec3)>> {
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .context("glTF file has no scene")?;
    let mut bounds: Option<(glam::Vec3, glam::Vec3)> = None;
    let mut stack: Vec<(gltf::Node, glam::Mat4)> =
        scene.nodes().map(|n| (n, glam::Mat4::IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let world = parent * glam::Mat4::from_cols_array_2d(&node.transform().matrix());
        for primitive in node.mesh().iter().flat_map(|m| m.primitives()) {
            let b = primitive.bounding_box();
            let (lo, hi) = (glam::Vec3::from_array(b.min), glam::Vec3::from_array(b.max));
            for i in 0..8 {
                let corner = glam::Vec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                );
                let p = world.transform_point3(corner);
                bounds = Some(match bounds {
                    Some((min, max)) => (min.min(p), max.max(p)),
                    None => (p, p),
                });
            }
        }
        stack.extend(node.children().map(|c| (c, world)));
    }
    Ok(bounds)
}

fn nodes_of(document: &gltf::Document) -> Result<Vec<GltfNode>> {
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .context("glTF file has no scene")?;
    let mut out = Vec::new();
    let mut stack: Vec<(gltf::Node, Option<usize>)> =
        scene.nodes().map(|n| (n, None)).collect::<Vec<_>>();
    stack.reverse();
    while let Some((node, parent)) = stack.pop() {
        let (t, r, s) = node.transform().decomposed();
        let index = out.len();
        out.push(GltfNode {
            name: node.name().map(str::to_string),
            parent,
            translation: glam::Vec3::from_array(t),
            rotation: glam::Quat::from_array(r),
            scale: glam::Vec3::from_array(s),
            mesh: node.mesh().map(|m| m.index()),
        });
        let children: Vec<_> = node.children().collect();
        stack.extend(children.into_iter().rev().map(|c| (c, Some(index))));
    }
    Ok(out)
}
pub use models::ModelCache;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use materials::sort_back_to_front;
use melon_core::{Handle, Model};
use tracing::instrument;
use wgpu::util::DeviceExt;
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    /// Clip -> world, to rebuild view rays (sky).
    inv_view_proj: [[f32; 4]; 4],
    /// Camera position (fog distance).
    eye: [f32; 3],
    _pad: f32,
}

/// `melon_core::Environment` as the shaders see it (group 0, binding 4).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EnvironmentUniform {
    zenith_color: [f32; 3],
    fog_density: f32,
    horizon_color: [f32; 3],
    _pad: f32,
}

/// Hemisphere samples per pixel in the SSAO pass.
const SSAO_SAMPLES: usize = 16;

/// SSAO kernel and settings (group 1, binding 2 of the SSAO passes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SsaoUniform {
    /// Offsets in the unit +Z hemisphere (xyz; w unused).
    kernel: [[f32; 4]; SSAO_SAMPLES],
    /// Metres.
    radius: f32,
    strength: f32,
    _pad: [f32; 2],
}

/// The SSAO sample offsets: cosine-weighted directions on a golden-angle
/// spiral over the +Z hemisphere, lengths 0.1 to ~0.95 growing
/// quadratically (more samples near the surface), shuffled against the
/// direction order. Fixed, so frames are deterministic.
fn ssao_kernel() -> [[f32; 4]; SSAO_SAMPLES] {
    let golden_angle = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    std::array::from_fn(|i| {
        let t = (i as f32 + 0.5) / SSAO_SAMPLES as f32;
        let (r, z) = (t.sqrt(), (1.0 - t).sqrt());
        let phi = i as f32 * golden_angle;
        // 7 is coprime to 16: a permutation of the length steps.
        let s = (((i * 7) % SSAO_SAMPLES) as f32 + 0.5) / SSAO_SAMPLES as f32;
        let len = 0.1 + 0.9 * s * s;
        [r * phi.cos() * len, r * phi.sin() * len, z * len, 0.0]
    })
}

/// Format of the SSAO textures (1 = unoccluded).
const AO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// Format of the prepass normal target: world normal (xyz), distance to the
/// eye (w).
const PREPASS_NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// One record of the instance storage buffer (group 1), read by the vertex
/// shaders at `instance_index`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceData {
    model: [[f32; 4]; 4],
    normal_matrix: [[f32; 4]; 4],
}

/// Records the instance buffer holds before its first growth.
const INITIAL_INSTANCE_CAPACITY: u64 = 256;

/// Capacity (in records) for `needed` records: `capacity` doubled until it
/// fits.
fn grown_capacity(capacity: u64, needed: u64) -> u64 {
    let mut capacity = capacity.max(1);
    while capacity < needed {
        capacity *= 2;
    }
    capacity
}

/// The six planes of a view-proj frustum (DirectX depth, 0..1): left,
/// right, bottom, top, near, far. A point `p` is inside a plane where
/// `plane.xyz . p + plane.w >= 0` (normals point inward, unnormalised).
struct Frustum {
    planes: [glam::Vec4; 6],
}

impl Frustum {
    fn from_view_proj(m: glam::Mat4) -> Self {
        let (x, y, z, w) = (m.row(0), m.row(1), m.row(2), m.row(3));
        Self {
            planes: [w + x, w - x, w + y, w - y, z, w - z],
        }
    }

    /// Whether the world-space box `min..max` may touch the frustum. Never
    /// false for a visible box; a box just outside a corner can pass.
    fn intersects_aabb(&self, min: glam::Vec3, max: glam::Vec3) -> bool {
        let center = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        self.planes.iter().all(|p| {
            let n = p.truncate();
            n.dot(center) + p.w >= -n.abs().dot(half)
        })
    }
}

/// World-space bounds of the local box `(min, max)` under `m`.
fn world_aabb(m: glam::Mat4, (min, max): (glam::Vec3, glam::Vec3)) -> (glam::Vec3, glam::Vec3) {
    let center = m.transform_point3((min + max) * 0.5);
    let half = glam::Mat3::from_mat4(m).abs() * ((max - min) * 0.5);
    (center - half, center + half)
}

/// Groups `items` by key: each key's values land contiguously at the end of
/// `out` (keys in order of first appearance, values in input order). Returns
/// each key with its range in `out`.
fn batch<K: Copy + Eq + std::hash::Hash, T>(
    items: impl IntoIterator<Item = (K, T)>,
    out: &mut Vec<T>,
) -> Vec<(K, std::ops::Range<u32>)> {
    let mut groups: Vec<(K, Vec<T>)> = Vec::new();
    let mut index = std::collections::HashMap::new();
    for (key, value) in items {
        let i = *index.entry(key).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[i].1.push(value);
    }
    groups
        .into_iter()
        .map(|(key, values)| {
            let start = out.len() as u32;
            out.extend(values);
            (key, start..out.len() as u32)
        })
        .collect()
}

/// What `render_scene` drew. The SSAO prepass draws the camera's batches
/// again (same counts as the 3D pass).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Instanced draws in the 3D pass: one per (model, part) seen.
    pub draw_calls: u32,
    /// Instances drawn in the 3D pass (entities times model parts).
    pub instances_drawn: u32,
    /// Entities outside the camera frustum.
    pub culled: u32,
    /// Instanced draws into the shadow map.
    pub shadow_draw_calls: u32,
    /// Entities outside the shadow box.
    pub shadow_culled: u32,
    /// Point / spot lights shaded, and visible ones dropped for
    /// `RenderQuality::max_lights`.
    pub lights: u32,
    pub lights_dropped: u32,
    /// Lights with shadows this frame, and the shadow views (atlas tiles)
    /// they drew.
    pub shadowed_lights: u32,
    pub shadow_views: u32,
    /// Shadow-casting lights (beyond `RenderQuality::shadowed_lights`'s
    /// nearest) that did not fit the shadow atlas and drew unshadowed.
    pub shadowed_lights_dropped: u32,
    /// Light-in-cluster entries over the shared index list's capacity; the
    /// clusters at its end miss lights. 0 when everything fits.
    pub light_index_overflow: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightUniform {
    direction: [f32; 3],
    ambient: f32,
    color: [f32; 3],
    _pad: f32,
    sky_color: [f32; 3],
    _pad1: f32,
    ground_color: [f32; 3],
    _pad2: f32,
    light_view_proj: [[f32; 4]; 4],
    /// Every shadow cascade's matrix (`cascades::CascadeSet`).
    cascade_view_proj: [[[f32; 4]; 4]; cascades::MAX_CASCADES],
    cascade_far: [f32; 4],
    /// x cascade count, y shadow distance.
    cascade_info: [f32; 4],
    cascade_forward: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// Tangent along +U (xyz) and bitangent sign (w = +1 or -1):
    /// bitangent = cross(normal, tangent) * w.
    pub tangent: [f32; 4],
}

/// Per-vertex tangents from positions and UVs, written into `vertices`:
/// per-triangle U and V directions accumulated per vertex, made
/// perpendicular to the normal (Gram-Schmidt), handedness from the
/// accumulated V direction. glTF's V runs down the image while normal maps
/// store green = up, so the bitangent is -dP/dV. Vertices with no usable
/// UVs get any tangent perpendicular to the normal.
fn generate_tangents(vertices: &mut [Vertex], indices: &[u32]) {
    use glam::{Vec2, Vec3};
    let mut tan = vec![Vec3::ZERO; vertices.len()];
    let mut bitan = vec![Vec3::ZERO; vertices.len()];
    for tri in indices.chunks_exact(3) {
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| i as usize);
        if [a, b, c].iter().any(|&i| i >= vertices.len()) {
            continue;
        }
        let p = |i: usize| Vec3::from_array(vertices[i].position);
        let uv = |i: usize| Vec2::from_array(vertices[i].uv);
        let (e1, e2) = (p(b) - p(a), p(c) - p(a));
        let (d1, d2) = (uv(b) - uv(a), uv(c) - uv(a));
        let det = d1.x * d2.y - d2.x * d1.y;
        if det.abs() < 1e-12 {
            continue;
        }
        let t = (e1 * d2.y - e2 * d1.y) / det;
        let bt = (e2 * d1.x - e1 * d2.x) / det;
        for i in [a, b, c] {
            tan[i] += t;
            bitan[i] += bt;
        }
    }
    for (v, (t, bt)) in vertices.iter_mut().zip(tan.into_iter().zip(bitan)) {
        let n = Vec3::from_array(v.normal);
        let t = (t - n * n.dot(t))
            .try_normalize()
            .unwrap_or_else(|| n.any_orthonormal_vector());
        let w = if n.cross(t).dot(bt) > 0.0 { -1.0 } else { 1.0 };
        v.tangent = t.extend(w).to_array();
    }
}

/// glTF metallic-roughness factors of one material (group 2, binding 5).
/// Each factor multiplies its texture; missing textures bind 1x1 defaults
/// (white, flat normal, white emissive: the factors alone).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MaterialUniform {
    /// Linear RGBA.
    pub base_color: [f32; 4],
    /// Linear HDR radiance added on top of the lit colour.
    pub emissive: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    /// Scales the tangent-space normal map's X and Y.
    pub normal_scale: f32,
    /// Alpha-mask cutoff: fragments with alpha below it are discarded;
    /// 0 = no mask.
    pub alpha_cutoff: f32,
    pub _pad: f32,
}

impl MaterialUniform {
    /// The factors of a glTF material (glTF defaults where unset).
    pub fn from_gltf(material: &gltf::Material) -> Self {
        let pbr = material.pbr_metallic_roughness();
        Self {
            base_color: pbr.base_color_factor(),
            emissive: material
                .emissive_factor()
                .map(|c| c * material.emissive_strength().unwrap_or(1.0)),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            normal_scale: material.normal_texture().map_or(1.0, |n| n.scale()),
            alpha_cutoff: match material.alpha_mode() {
                gltf::material::AlphaMode::Mask => material.alpha_cutoff().unwrap_or(0.5),
                _ => 0.0,
            },
            _pad: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct LineVertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
}

/// How a material is drawn besides its factors and textures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Surface {
    /// Alpha blended: drawn sorted back to front after the opaque pass.
    pub blend: bool,
    /// Back faces drawn too.
    pub double_sided: bool,
    /// Alpha masked: the prepass and shadow pass discard below the cutoff.
    pub masked: bool,
}

impl Surface {
    pub fn from_gltf(material: &gltf::Material) -> Self {
        Self {
            blend: material.alpha_mode() == gltf::material::AlphaMode::Blend,
            double_sided: material.double_sided(),
            masked: material.alpha_mode() == gltf::material::AlphaMode::Mask,
        }
    }
}

/// 1x1 textures bound where a material has none.
struct DefaultTextures {
    /// Base colour, metallic-roughness and emissive: the factors alone.
    white: wgpu::TextureView,
    /// Normal map (0.5, 0.5, 1): the vertex normal unchanged.
    flat_normal: wgpu::TextureView,
    /// Bloom of a target without a bloom chain: no glow.
    black: wgpu::TextureView,
}

/// `builtin:cube`: rough white dielectric under the checkerboard.
const CHECKER_MATERIAL: MaterialUniform = MaterialUniform {
    base_color: [1.0; 4],
    emissive: [0.0; 3],
    metallic: 0.0,
    roughness: 0.8,
    normal_scale: 1.0,
    alpha_cutoff: 0.0,
    _pad: 0.0,
};

/// A group 2 bind group: `textures` are base colour, metallic-roughness,
/// normal and emissive; `None` binds the matching default.
fn material_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    defaults: &DefaultTextures,
    factors: &MaterialUniform,
    textures: [Option<&wgpu::TextureView>; 4],
) -> wgpu::BindGroup {
    let [base, metallic_roughness, normal, emissive] = textures;
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("material uniform buffer"),
        contents: bytemuck::bytes_of(factors),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    fn view(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
        wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        }
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material bind group"),
        layout,
        entries: &[
            view(0, base.unwrap_or(&defaults.white)),
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            view(2, metallic_roughness.unwrap_or(&defaults.white)),
            view(3, normal.unwrap_or(&defaults.flat_normal)),
            view(4, emissive.unwrap_or(&defaults.white)),
            wgpu::BindGroupEntry {
                binding: 5,
                resource: uniform.as_entire_binding(),
            },
        ],
    })
}

pub struct Mesh {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
    pub index_format: wgpu::IndexFormat,
}

/// A model loaded from a glTF file. Each primitive becomes one `Mesh` with
/// its material bind group (group 2). Draw by zipping `meshes` with
/// `material_bind_groups`.
pub struct LoadedModel {
    pub meshes: Vec<Mesh>,
    /// Per-primitive material (see [`SceneRenderer::create_material_bind_group`]).
    pub material_bind_groups: Vec<wgpu::BindGroup>,
    /// `Surface` of each material (same order).
    pub surfaces: Vec<Surface>,
    /// Local-space bounds of each part (same order).
    pub part_bounds: Vec<(glam::Vec3, glam::Vec3)>,
    /// Local-space bounds (min, max) of every part, node transforms included.
    pub bounds: (glam::Vec3, glam::Vec3),
}

/// Convert a decoded glTF image (`gltf::image::Data`) into an
/// `image::DynamicImage` the renderer can upload. glTF stores pixels as
/// 8-bit or 16-bit channels; we convert everything to RGBA8 for upload.
fn image_from_gltf_data(data: &gltf::image::Data) -> image::DynamicImage {
    use gltf::image::Format;
    use image::{ImageBuffer, Rgba};
    let (width, height) = (data.width, data.height);
    match data.format {
        Format::R8G8B8A8 => ImageBuffer::from_raw(width, height, data.pixels.clone())
            .map(image::DynamicImage::ImageRgba8)
            .expect("gltf R8G8B8A8 pixel buffer size mismatch"),
        Format::R8G8B8 => {
            let rgb: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
                ImageBuffer::from_raw(width, height, data.pixels.clone())
                    .expect("gltf R8G8B8 pixel buffer size mismatch");
            image::DynamicImage::ImageRgb8(rgb).to_rgba8().into()
        }
        Format::R8G8 => {
            let gray_alpha: ImageBuffer<image::LumaA<u8>, Vec<u8>> =
                ImageBuffer::from_raw(width, height, data.pixels.clone())
                    .expect("gltf R8G8 pixel buffer size mismatch");
            image::DynamicImage::ImageLumaA8(gray_alpha)
                .to_rgba8()
                .into()
        }
        Format::R8 => {
            let gray: ImageBuffer<image::Luma<u8>, Vec<u8>> =
                ImageBuffer::from_raw(width, height, data.pixels.clone())
                    .expect("gltf R8 pixel buffer size mismatch");
            image::DynamicImage::ImageLuma8(gray).to_rgba8().into()
        }
        // 16-bit and float formats: convert channel-by-channel to RGBA8.
        // Base color textures are rarely 16-bit; this is a safety net.
        _ => {
            let mut rgba = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(width, height);
            let channels = match data.format {
                Format::R16G16B16A16 => 4,
                Format::R16G16B16 => 3,
                Format::R16G16 => 2,
                Format::R16 => 1,
                Format::R32G32B32FLOAT => 3,
                _ => 1,
            };
            let stride = channels * 2; // 16-bit = 2 bytes per channel
            for (x, y, pixel) in rgba.enumerate_pixels_mut() {
                let offset = ((y * width + x) as usize) * stride;
                if offset + stride > data.pixels.len() {
                    break;
                }
                let read_ch = |c: usize| -> u8 {
                    let off = offset + c * 2;
                    if off + 1 < data.pixels.len() {
                        // Take the high byte of the 16-bit value.
                        data.pixels[off + 1]
                    } else {
                        0
                    }
                };
                match channels {
                    1 => *pixel = Rgba([read_ch(0), read_ch(0), read_ch(0), 255]),
                    2 => *pixel = Rgba([read_ch(0), read_ch(0), read_ch(0), read_ch(1)]),
                    3 => *pixel = Rgba([read_ch(0), read_ch(1), read_ch(2), 255]),
                    _ => *pixel = Rgba([read_ch(0), read_ch(1), read_ch(2), read_ch(3)]),
                }
            }
            image::DynamicImage::ImageRgba8(rgba)
        }
    }
}

/// mesh.wgsl with the GI helpers and every GI backend's shading prepended
/// (mesh and voxel pipelines).
const MESH_WGSL: &str = concat!(
    include_str!("../../../assets/shaders/gi_common.wgsl"),
    include_str!("../../../assets/shaders/ddgi_common.wgsl"),
    include_str!("../../../assets/shaders/gi_ddgi.wgsl"),
    include_str!("../../../assets/shaders/gi_ssgi.wgsl"),
    include_str!("../../../assets/shaders/mesh.wgsl")
);

/// Format of the scene colour target. The tonemap pass maps it to the display.
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Default MSAA sample count of the 3D scene pass (colour and depth);
/// `RenderQuality::msaa` sets it per frame.
pub const MSAA_SAMPLES: u32 = 4;

/// Default width and height of the sun's shadow map, in texels;
/// `RenderQuality::shadow_map_size` sets it per frame.
pub const SHADOW_MAP_SIZE: u32 = 2048;

/// Sample counts the pipeline cache has a slot for (1, 2, 4).
const MSAA_SLOTS: usize = 3;

/// Pipeline cache slot of a sample count from [`effective_msaa`].
fn msaa_slot(samples: u32) -> usize {
    samples.trailing_zeros() as usize
}

/// The sample count used for `requested`: the largest `supported` count not
/// above it (1 is always supported).
fn effective_msaa(requested: u32, supported: &[u32]) -> u32 {
    supported
        .iter()
        .copied()
        .filter(|&n| n <= requested)
        .max()
        .unwrap_or(1)
}

/// The pipelines of the 3D pass whose sample count follows
/// `RenderQuality::msaa`, built once per count on first use.
struct MsaaPipelines {
    /// The mesh pipeline per GI mode (`GI_MODE` override): off, SSGI, DDGI.
    mesh: [wgpu::RenderPipeline; 3],
    sky: wgpu::RenderPipeline,
    world_lines: wgpu::RenderPipeline,
}

/// What draws batch by: the model and the id of its material override set
/// (0 = none).
type DrawKey = (Handle<Model>, u32);

type MaterialHandle = melon_core::Handle<melon_core::Material>;

/// One view's mesh draws by kind. The prepass lists (`plain`, `masked`)
/// hold every non-blend draw once; `single` and `two_sided` split the same
/// draws by culling for the 3D pass (a masked draw is in both).
#[derive(Default)]
struct Calls<'a> {
    plain: Vec<(&'a Mesh, std::ops::Range<u32>)>,
    masked: Vec<DrawCall<'a>>,
    single: Vec<DrawCall<'a>>,
    two_sided: Vec<DrawCall<'a>>,
    /// Far to near, one instance each, with the instance's world position.
    blend: Vec<(glam::Vec3, DrawCall<'a>)>,
}

/// One shadow view's draws: plain casters and alpha-masked ones.
type ShadowDraws<'a> = (Vec<(&'a Mesh, std::ops::Range<u32>)>, Vec<DrawCall<'a>>);

type DrawCall<'a> = (&'a Mesh, &'a wgpu::BindGroup, std::ops::Range<u32>);

/// A mesh pipeline for surfaces the opaque, back-face-culled one does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Variant {
    /// Opaque, back faces drawn too.
    TwoSided,
    /// Alpha blended (back to front, no depth write, back faces drawn).
    Blend,
}

/// Most levels in a view's bloom mip chain.
pub const BLOOM_LEVELS: usize = 6;

/// Bloom mip chain sizes for a `width` x `height` view: half resolution
/// first, each next level half the last (rounded down), at most
/// `BLOOM_LEVELS`, stopping before a side would drop below 1 px.
pub fn bloom_mip_sizes(width: u32, height: u32) -> Vec<(u32, u32)> {
    std::iter::successors(Some((width / 2, height / 2)), |&(w, h)| {
        Some((w / 2, h / 2))
    })
    .take_while(|&(w, h)| w >= 1 && h >= 1)
    .take(BLOOM_LEVELS)
    .collect()
}

/// World -> shadow clip space for a directional light. An orthographic box of
/// half-size `radius` around `center`, seen from `to_light`.
pub fn directional_light_view_proj(
    to_light: glam::Vec3,
    center: glam::Vec3,
    radius: f32,
) -> glam::Mat4 {
    use glam::camera::rh::{proj::directx, view};
    // look_at degenerates when the view direction is parallel to `up`.
    let up = if to_light.y.abs() > 0.99 {
        glam::Vec3::Z
    } else {
        glam::Vec3::Y
    };
    let eye = center + to_light * (2.0 * radius);
    directx::orthographic(-radius, radius, -radius, radius, 0.0, 4.0 * radius)
        * view::look_at_mat4(eye, center, up)
}

/// What `Renderer::record_view` records: the scene view of the frame (world
/// lines, overlay, quality, GI, timings) or an auxiliary view such as a
/// thumbnail.
enum View<'a> {
    Main {
        world_lines: &'a [LineVertex],
        overlay: &'a [OverlayDraw],
    },
    Auxiliary,
}

/// Offscreen HDR colour target that the 3D pass renders into. With MSAA the
/// pass draws into the multisampled texture and resolves into the
/// single-sample one, which the tonemap pass reads; with 1 sample it draws
/// into the single-sample one directly.
pub struct HdrTarget {
    /// `None` at 1 sample.
    msaa: Option<(wgpu::Texture, wgpu::TextureView)>,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Binds this target as the tonemap pass input.
    bind_group: wgpu::BindGroup,
    size: winit::dpi::PhysicalSize<u32>,
}

impl HdrTarget {
    /// Resolve target: the single-sample image the tonemap pass reads.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Colour attachment of the 3D pass: the multisampled texture, or
    /// `view()` at 1 sample. Resolve into `resolve_target()`.
    pub fn msaa_view(&self) -> &wgpu::TextureView {
        self.msaa.as_ref().map_or(&self.view, |(_, view)| view)
    }

    /// `view()` when multisampled, else `None` (nothing to resolve).
    pub fn resolve_target(&self) -> Option<&wgpu::TextureView> {
        self.msaa.as_ref().map(|_| &self.view)
    }

    pub fn sample_count(&self) -> u32 {
        self.msaa.as_ref().map_or(1, |(t, _)| t.sample_count())
    }

    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }
}

/// Growable vertex buffer for line lists. Each draw site that can appear in
/// the same frame needs its own: `queue.write_buffer` lands before the GPU
/// runs, so a shared buffer would hold only the last upload.
#[derive(Default)]
struct LineBuffer {
    buffer: Option<wgpu::Buffer>,
    capacity: wgpu::BufferAddress,
}

impl LineBuffer {
    /// Uploads `vertices` and returns the buffer and the byte length used.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[LineVertex],
    ) -> (&wgpu::Buffer, wgpu::BufferAddress) {
        let needed = std::mem::size_of_val(vertices) as wgpu::BufferAddress;
        if needed > self.capacity {
            self.capacity = needed.max(self.capacity * 2);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("line vertex buffer"),
                size: self.capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buffer = self.buffer.as_ref().expect("allocated above");
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(vertices));
        (buffer, needed)
    }
}

pub struct SceneRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_format: wgpu::TextureFormat,
    /// Group 0 layout (also built into the MSAA pipelines later).
    frame_layout: wgpu::BindGroupLayout,
    /// Sample counts the adapter renders every 3D pass format at.
    msaa_counts: Vec<u32>,
    /// Sample count of the 3D pass now (`set_msaa`).
    msaa: u32,
    /// The 3D pass pipelines per sample count (`msaa_slot`), built on first use.
    msaa_pipelines: [Option<MsaaPipelines>; MSAA_SLOTS],
    /// Mesh pipelines for double-sided and alpha-blended materials, per
    /// (sample count slot, GI mode index), built on first use
    /// (`ensure_variant`).
    variants: std::collections::HashMap<(usize, usize, Variant), wgpu::RenderPipeline>,
    tonemap_pipeline: wgpu::RenderPipeline,
    tonemap_bind_group_layout: wgpu::BindGroupLayout,
    /// Bloom: one source texture + sampler per pass, the first (Karis)
    /// downsample, the other downsamples and the additive upsample.
    bloom_layout: wgpu::BindGroupLayout,
    bloom_sampler: wgpu::Sampler,
    bloom_first_pipeline: wgpu::RenderPipeline,
    bloom_down_pipeline: wgpu::RenderPipeline,
    bloom_up_pipeline: wgpu::RenderPipeline,
    /// Tonemap bloom uniform of zeros (intensity 0), for targets without bloom.
    bloom_off_buffer: wgpu::Buffer,
    /// Group 1 layout: the instance storage buffer (rebuilt on growth).
    model_bind_group_layout: wgpu::BindGroupLayout,
    material_bind_group_layout: wgpu::BindGroupLayout,
    /// The one sampler every material binds (repeat, linear).
    material_sampler: wgpu::Sampler,
    default_textures: DefaultTextures,
    camera_buffer: wgpu::Buffer,
    light_buffer: wgpu::Buffer,
    environment_buffer: wgpu::Buffer,
    /// Group 0: per-frame data shared by every pipeline (camera, light,
    /// shadow map, environment).
    frame_bind_group: wgpu::BindGroup,
    /// Image-based ambient (group 0, bindings 7 to 10).
    ibl: ibl::IblGpu,
    /// The sky colours and hemisphere ambient the IBL was last asked for.
    ibl_sky: ibl::SkyParams,
    ibl_hdr: Option<(std::sync::Arc<ibl::HdrImage>, f32)>,
    /// What the IBL data was built for (`refresh_ibl`).
    ibl_built: Option<(ibl::SkyParams, Option<(usize, u32)>)>,
    /// Auxiliary views (thumbnails) leave the IBL as the main view built it.
    ibl_frozen: bool,
    /// Times the IBL data was recomputed, and the shadow map / atlas
    /// reallocated (tests count them; a thumbnail must not add any).
    ibl_rebuilds: u32,
    shadow_rebuilds: u32,
    /// Last shadow-atlas / index-list overflow that was reported (log once
    /// per change).
    warned_shadow_cap: u32,
    warned_index_overflow: u32,
    shadow_pipeline: wgpu::RenderPipeline,
    /// Shadow pass for alpha-masked casters (reads the material, group 2).
    shadow_mask_pipeline: wgpu::RenderPipeline,
    /// Light view-proj per cascade for the shadow pass (group 0 of
    /// `shadow_pipeline`): separate buffers, since writes apply at submit.
    shadow_camera_buffers: Vec<wgpu::Buffer>,
    shadow_bind_groups: Vec<wgpu::BindGroup>,
    /// Point / spot shadows: the depth atlas (4 x 4 while no light casts
    /// shadows), the tile records the shaders read, and a camera uniform
    /// per drawn tile (`ensure_local_cameras`).
    local_atlas: wgpu::Texture,
    local_atlas_view: wgpu::TextureView,
    local_views_buffer: wgpu::Buffer,
    shadow_camera_layout: wgpu::BindGroupLayout,
    local_shadow_cameras: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    /// The sun's shadow map, one layer per cascade
    /// (`set_shadow_map_size` / `set_cascades` recreate it), its array view
    /// for sampling and a view per layer to render into.
    shadow_texture: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    shadow_layers: Vec<wgpu::TextureView>,
    shadow_sampler: wgpu::Sampler,
    /// `builtin:cube`'s material: the checkerboard as base colour.
    checker_material: wgpu::BindGroup,

    /// SSAO: depth/normal prepass, AO and blur pipelines, their group 1
    /// layouts and the shared kernel/settings uniform.
    prepass_pipeline: wgpu::RenderPipeline,
    /// Prepass for alpha-masked meshes (reads the material, group 2).
    prepass_mask_pipeline: wgpu::RenderPipeline,
    ssao_pipeline: wgpu::RenderPipeline,
    ssao_blur_pipeline: wgpu::RenderPipeline,
    ssao_layout: wgpu::BindGroupLayout,
    ssao_blur_layout: wgpu::BindGroupLayout,
    ssao_buffer: wgpu::Buffer,
    /// Group 3 of the mesh pipeline: per-view lighting inputs.
    lighting_layout: wgpu::BindGroupLayout,
    /// The main view's point / spot lights and cluster lists (group 3
    /// bindings 9 to 12), written each frame.
    frame_lights: lights::LightBuffers,
    /// Group 3 with a 1x1 white AO texture: no occlusion (AO off).
    unoccluded_lighting: wgpu::BindGroup,
    /// Group 3 of the GI voxel passes: `unoccluded_lighting` without the
    /// voxel volume they write.
    voxel_pass_lighting: wgpu::BindGroup,
    /// The GI backends (resources, passes) and the last frame's mode.
    gi: gi::Gi,
    /// Depth-tested lines inside the 3D pass (grid, axes, selection box).
    world_lines: LineBuffer,
    /// Group 1: per-instance records, `instance_capacity` of them.
    instance_buffer: wgpu::Buffer,
    instance_bind_group: wgpu::BindGroup,
    instance_capacity: u64,
    instance_staging: Vec<InstanceData>,
}

pub struct Renderer {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    size: winit::dpi::PhysicalSize<u32>,
    window: Arc<Window>,
    /// From `RenderQuality::present` (`set_present`, `choose_present_mode`).
    present_mode: wgpu::PresentMode,
    /// Present modes the surface offers.
    present_modes: Vec<wgpu::PresentMode>,
    /// `MELON_PRESENT`, read once at startup.
    present_env: Option<String>,
    /// The adapter's backend (`backend_order`).
    backend: wgpu::Backend,
    scene: SceneRenderer,
    /// GPU models per asset handle, used by `render_scene`.
    model_cache: ModelCache,
    /// GPU materials (`.mat` files) and their textures.
    materials: materials::MaterialCache,
    /// The decoded HDR sky image: (handle id, file version, image).
    sky_cache: Option<(u32, u32, Option<std::sync::Arc<ibl::HdrImage>>)>,
    /// Solid editor shapes drawn after tonemapping.
    overlay: overlay::OverlayRenderer,
    /// Copies a viewport image onto the swapchain (a game without the editor UI).
    present: present::Present,
    /// GPU pass timings; `None` without timestamp queries.
    timer: Option<timings::GpuTimer>,
}

/// Background of the scene and of the window behind the editor UI.
pub const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.06,
    g: 0.07,
    b: 0.09,
    a: 1.0,
};

// ponytail: fixed shadow box around the origin; fit it to the camera frustum
// (or use cascades) once scenes outgrow 40 m.
pub const SHADOW_CENTER: glam::Vec3 = glam::Vec3::ZERO;
pub const SHADOW_RADIUS: f32 = 20.0;

impl std::ops::Deref for Renderer {
    type Target = SceneRenderer;

    fn deref(&self) -> &Self::Target {
        &self.scene
    }
}

impl std::ops::DerefMut for Renderer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.scene
    }
}

/// `Vsync` is `Fifo`. `MailboxCapped` is `Mailbox` when the surface offers
/// it (no vsync wait in the acquire, no tearing; the host caps the frame
/// rate), else `Fifo`. `MELON_PRESENT=fifo` forces `Fifo` (debug A/B).
fn choose_present_mode(
    preference: melon_core::PresentPreference,
    available: &[wgpu::PresentMode],
    forced: Option<&str>,
) -> wgpu::PresentMode {
    if preference == melon_core::PresentPreference::MailboxCapped
        && forced != Some("fifo")
        && available.contains(&wgpu::PresentMode::Mailbox)
    {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    }
}

/// Backend sets to try in order. Windows tries DX12 first (AMD's Vulkan
/// driver there offers no `Mailbox`), then the default set.
/// `MELON_BACKEND=vulkan|dx12|auto` overrides it; unknown values mean auto.
fn backend_order(windows: bool, forced: Option<&str>) -> Vec<wgpu::Backends> {
    match forced {
        Some("vulkan") => return vec![wgpu::Backends::VULKAN],
        Some("dx12") => return vec![wgpu::Backends::DX12],
        None | Some("auto") => {}
        Some(other) => tracing::warn!("MELON_BACKEND={other:?} unknown, using auto"),
    }
    if windows {
        vec![wgpu::Backends::DX12, wgpu::Backends::default()]
    } else {
        vec![wgpu::Backends::default()]
    }
}

/// Sample counts (of 1, 2, 4) the 3D pass formats all support: HDR colour,
/// depth and the overlay. Without adapter format features, WebGPU's 1 and 4.
fn msaa_counts(adapter: &wgpu::Adapter, device: &wgpu::Device) -> Vec<u32> {
    let adapter_specific = device
        .features()
        .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
    [1, 2, 4]
        .into_iter()
        .filter(|&n| {
            if !adapter_specific {
                return n != 2;
            }
            [
                HDR_FORMAT,
                wgpu::TextureFormat::Depth32Float,
                overlay::OVERLAY_FORMAT,
            ]
            .iter()
            .all(|&f| {
                adapter
                    .get_texture_format_features(f)
                    .flags
                    .sample_count_supported(n)
            })
        })
        .collect()
}

/// Group 1 layout: one read-only storage buffer of `InstanceData`.
fn instance_buffer_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("instance bind group layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// An instance buffer of `capacity` records and its group 1 bind group.
fn create_instance_buffer(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    capacity: u64,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instance buffer"),
        size: capacity * std::mem::size_of::<InstanceData>() as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("instance bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    (buffer, bind_group)
}

impl Renderer {
    #[instrument(skip(window), level = "info")]
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        Self::new_with_dx12_compiler(window, wgpu::Dx12Compiler::default()).await
    }

    /// `new` with the DX12 shader compiler used unless `WGPU_DX12_COMPILER` is set.
    /// A packaged game passes `Fxc`, which ships with Windows; `Auto` looks for
    /// `dxcompiler.dll` (a Windows SDK file) first.
    #[instrument(skip(window), level = "info")]
    pub async fn new_with_dx12_compiler(
        window: Arc<Window>,
        dx12_compiler: wgpu::Dx12Compiler,
    ) -> Result<Self> {
        let forced = std::env::var("MELON_BACKEND").ok();
        let mut found = None;
        for backends in backend_order(cfg!(windows), forced.as_deref()) {
            let mut descriptor = wgpu::InstanceDescriptor {
                backends,
                ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
            };
            if std::env::var_os("WGPU_DX12_COMPILER").is_none() {
                descriptor.backend_options.dx12.shader_compiler = dx12_compiler.clone();
            }
            let instance = wgpu::Instance::new(descriptor);
            if let Ok(adapter) = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
            {
                found = Some((instance, adapter));
                break;
            }
        }
        let (instance, adapter) = found.context("no suitable wgpu adapter found")?;
        let backend = adapter.get_info().backend;
        // Timestamps for `RenderTimings`; adapter format features for MSAA
        // 2x (WebGPU guarantees only 1 and 4). Both only when offered.
        let wanted =
            timings::GpuTimer::FEATURES | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: adapter.features() & wanted,
                ..Default::default()
            })
            .await
            .context("failed to request wgpu device")?;
        let msaa_counts = msaa_counts(&adapter, &device);

        let size = window.inner_size();
        let surface = instance
            .create_surface(window.clone())
            .context("failed to create wgpu surface from window")?;
        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .first()
            .copied()
            .context("surface reported no usable formats")?;
        let present_env = std::env::var("MELON_PRESENT").ok();
        let present_mode = choose_present_mode(
            melon_core::RenderQuality::default().present,
            &caps.present_modes,
            present_env.as_deref(),
        );

        let timer = timings::GpuTimer::new(&device, &queue);
        let scene = SceneRenderer::new(device, queue, surface_format, msaa_counts);
        let model_cache = ModelCache::new(&scene);
        let overlay =
            overlay::OverlayRenderer::new(&scene.device, surface_format.add_srgb_suffix());
        let present = present::Present::new(&scene.device, surface_format.add_srgb_suffix());
        tracing::info!(
            msaa = ?scene.msaa_counts,
            timestamps = timer.is_some(),
            "renderer features"
        );

        let renderer = Self {
            instance,
            surface,
            size,
            window,
            present_mode,
            present_modes: caps.present_modes,
            present_env,
            backend,
            scene,
            model_cache,
            materials: Default::default(),
            sky_cache: None,
            overlay,
            present,
            timer,
        };
        renderer.configure_surface();
        Ok(renderer)
    }

    pub fn scene(&self) -> &SceneRenderer {
        &self.scene
    }

    /// `Mailbox` does not wait for vblank: the host caps the frame rate then.
    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.present_mode
    }

    /// The graphics API in use (DX12 first on Windows, `MELON_BACKEND`).
    pub fn backend(&self) -> wgpu::Backend {
        self.backend
    }

    /// Switches the present mode to `preference` (see
    /// `choose_present_mode`), reconfiguring the surface; true when it
    /// changed (the host then switches its frame cap). Call between frames.
    pub fn set_present(&mut self, preference: melon_core::PresentPreference) -> bool {
        let mode =
            choose_present_mode(preference, &self.present_modes, self.present_env.as_deref());
        if mode == self.present_mode {
            return false;
        }
        self.present_mode = mode;
        self.configure_surface();
        tracing::info!(?mode, "present mode");
        true
    }

    /// Per-pass GPU ms averaged over about a second, `None` where not
    /// measured (no timestamp support, pass off). `budget_ms` is left to
    /// the host (monitor refresh).
    pub fn timings(&self) -> melon_core::RenderTimings {
        melon_core::RenderTimings {
            msaa: Some(self.scene.msaa()),
            ..self
                .timer
                .as_ref()
                .map(timings::GpuTimer::timings)
                .unwrap_or_default()
        }
    }

    /// Load `handle` through the model cache (as `render_scene` does) and
    /// return its bounds; `None` if the file failed to load (the cache would
    /// draw the built-in cube in its place).
    pub fn model_bounds(
        &mut self,
        server: &mut melon_core::AssetServer,
        handle: melon_core::Handle<melon_core::Model>,
    ) -> Option<(glam::Vec3, glam::Vec3)> {
        self.model_cache.prepare(&self.scene, server, handle);
        server.bounds(handle)
    }

    /// `handle`'s model is loaded in the GPU cache.
    pub fn model_cached(&self, handle: melon_core::Handle<melon_core::Model>) -> bool {
        self.model_cache.contains(handle)
    }

    /// Render a thumbnail of one model and return a view egui can sample
    /// (non-sRGB, like `ViewportTarget::ui_view`). `None` if it fails to load.
    /// Records and submits its own command buffer: call it after the frame's
    /// `end_frame`, never between `render_scene` and that frame's submit
    /// (shared uniforms are written with `queue.write_buffer`).
    pub fn render_thumbnail(
        &mut self,
        server: &mut melon_core::AssetServer,
        handle: melon_core::Handle<melon_core::Model>,
        target: &mut ViewportTarget,
    ) -> Option<wgpu::TextureView> {
        let texture = self.render_thumbnail_texture(server, handle, target)?;
        Some(texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(texture.format().remove_srgb_suffix()),
            ..Default::default()
        }))
    }

    /// `render_thumbnail`, returning the texture itself.
    fn render_thumbnail_texture(
        &mut self,
        server: &mut melon_core::AssetServer,
        handle: melon_core::Handle<melon_core::Model>,
        target: &mut ViewportTarget,
    ) -> Option<wgpu::Texture> {
        debug_assert_eq!(
            (target.size().width, target.size().height),
            (THUMBNAIL_SIZE, THUMBNAIL_SIZE)
        );
        // A model only the thumbnail needed is freed after the render (a
        // folder of models would otherwise stay in VRAM); one the scene
        // already drew stays.
        let was_cached = self.model_cache.contains(handle);
        let Some((min, max)) = self.model_bounds(server, handle) else {
            if !was_cached {
                self.model_cache.evict(handle);
            }
            return None;
        };
        let (view_proj, eye) = thumbnail_camera(min, max);
        let frame = FrameData {
            view_proj,
            eye,
            models: vec![(glam::Mat4::IDENTITY, handle)],
            ..Default::default()
        };
        let texture = self.thumbnail_of(&frame, server, target);
        if !was_cached {
            self.model_cache.evict(handle);
        }
        Some(texture)
    }

    /// A material on a sphere (`builtin:sphere`), for the Content Browser:
    /// the same auxiliary view and own submission as a model thumbnail.
    pub fn render_material_thumbnail(
        &mut self,
        server: &mut melon_core::AssetServer,
        material: melon_core::Handle<melon_core::Material>,
        target: &mut ViewportTarget,
    ) -> Option<wgpu::TextureView> {
        let texture = self.render_material_thumbnail_texture(server, material, target)?;
        Some(texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(texture.format().remove_srgb_suffix()),
            ..Default::default()
        }))
    }

    fn render_material_thumbnail_texture(
        &mut self,
        server: &mut melon_core::AssetServer,
        material: melon_core::Handle<melon_core::Material>,
        target: &mut ViewportTarget,
    ) -> Option<wgpu::Texture> {
        let sphere: melon_core::Handle<melon_core::Model> = server.load("builtin:sphere");
        self.model_cache.prepare(&self.scene, server, sphere);
        let (min, max) = server.bounds(sphere)?;
        let (view_proj, eye) = thumbnail_camera(min, max);
        let mut frame = FrameData {
            view_proj,
            eye,
            models: vec![(glam::Mat4::IDENTITY, sphere)],
            ..Default::default()
        };
        frame.overrides.insert(0, vec![Some(material)]);
        Some(self.thumbnail_of(&frame, server, target))
    }

    /// Record `frame` as an auxiliary view into `target`, copy the result
    /// and submit it on its own.
    fn thumbnail_of(
        &mut self,
        frame: &FrameData,
        server: &mut melon_core::AssetServer,
        target: &mut ViewportTarget,
    ) -> wgpu::Texture {
        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("thumbnail"),
            });
        self.record_view(&mut encoder, frame, server, target, View::Auxiliary);
        let source = target.color_texture();
        let format = source.format();
        let texture = self.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("thumbnail"),
            size: source.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[format.remove_srgb_suffix()],
        });
        encoder.copy_texture_to_texture(
            source.as_image_copy(),
            texture.as_image_copy(),
            source.size(),
        );
        self.queue().submit([encoder.finish()]);
        texture
    }

    /// Record one view of `frame` into `target`: shadow pass, SSAO passes, 3D
    /// pass (HDR, MSAA) with the meshes, the sky and depth-tested
    /// `world_lines`, the bloom passes, the tonemap pass onto the target's
    /// display image, then the `overlay` shapes.
    /// `frame.quality` picks the passes; a new shadow map size or sample
    /// count recreates the shadow map, the target's MSAA textures and the
    /// pipelines first (the display image stays). Timestamps between the
    /// stages feed `timings` when supported.
    /// Models load (or hot-reload) here on first use. Entities are culled
    /// per view and drawn instanced, one call per (model, part).
    pub fn render_scene(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &FrameData,
        server: &mut melon_core::AssetServer,
        target: &mut ViewportTarget,
        world_lines: &[LineVertex],
        overlay: &[OverlayDraw],
    ) -> RenderStats {
        self.record_view(
            encoder,
            frame,
            server,
            target,
            View::Main {
                world_lines,
                overlay,
            },
        )
    }

    /// The decoded image of `Environment::sky_texture`, reloaded when its
    /// file changes; a broken file is reported once and leaves the
    /// procedural sky.
    fn sky_image(
        &mut self,
        server: &melon_core::AssetServer,
        texture: Option<Handle<melon_core::Texture>>,
    ) -> Option<std::sync::Arc<ibl::HdrImage>> {
        let handle = texture?;
        let version = server.version(handle);
        if let Some((id, v, image)) = &self.sky_cache
            && *id == handle.id()
            && *v == version
        {
            return image.clone();
        }
        let path = server.path(handle).to_string();
        let image = match server
            .read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| ibl::HdrImage::from_bytes(&bytes))
        {
            Ok(image) => Some(std::sync::Arc::new(image)),
            Err(error) => {
                tracing::warn!("could not load sky {path}: {error:#}");
                None
            }
        };
        self.sky_cache = Some((handle.id(), version, image.clone()));
        image
    }

    /// `render_scene` for a `View::Main`; a `View::Auxiliary` (thumbnail)
    /// leaves the world state alone: GI shaded as Off without updating it,
    /// the current shadow map size and sample count (its quality's are
    /// ignored), no GPU timestamps.
    fn record_view(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &FrameData,
        server: &mut melon_core::AssetServer,
        target: &mut ViewportTarget,
        view: View<'_>,
    ) -> RenderStats {
        let main = matches!(view, View::Main { .. });
        let (world_lines, overlay) = match view {
            View::Main {
                world_lines,
                overlay,
            } => (world_lines, overlay),
            View::Auxiliary => (&[][..], &[][..]),
        };
        let quality = &frame.quality;
        // An auxiliary view keeps the sky, IBL and atlas the last main view
        // built (a thumbnail must not reallocate shared textures).
        let sky = main
            .then(|| self.sky_image(server, frame.environment.sky_texture))
            .flatten();
        let scene = &mut self.scene;
        scene.ibl_frozen = !main;
        if main {
            scene.set_sky_texture(sky.map(|image| (image, frame.environment.sky_intensity)));
            scene.set_shadow_map_size(quality.shadow_map_size);
            scene.set_msaa(quality.msaa);
        }
        scene.match_msaa(target);
        let gi_mode = if main {
            gi::frame_mode(&frame.environment, quality.gi_mode)
        } else {
            melon_core::GiMode::Off
        };
        scene.match_gi_view(
            target,
            (gi_mode == melon_core::GiMode::Ssgi).then_some(quality.ssgi_resolution),
        );
        let target = &*target;
        scene.update_camera(frame.view_proj, frame.eye);
        scene.update_environment(&frame.environment);
        scene.update_ssao(&frame.environment);
        // The lights and their cluster lists of this view; other views draw
        // without local lights.
        // The shadow atlas exists at full size only while a light casts shadows.
        if main {
            let wants_atlas =
                quality.shadowed_lights > 0 && frame.lights.iter().any(|l| l.cast_shadows);
            scene.set_local_shadow_atlas(if wants_atlas {
                quality.local_shadow_size
            } else {
                4
            });
        }
        let (mut cluster, mut cluster_data) = if main {
            let size = target.size();
            let grid = lights::ClusterGrid::new(
                frame.view_proj,
                frame.eye,
                [size.width as f32, size.height as f32],
            );
            let data = lights::build(
                &grid,
                &frame.lights,
                quality.max_lights as usize,
                quality.shadowed_lights as usize,
                (gi_mode == melon_core::GiMode::Ddgi).then(gi::ddgi::volume_bounds),
            );
            (grid.uniform(), data)
        } else {
            (lights::ClusterUniform::empty(), Default::default())
        };
        // Point / spot shadows: a tile per spot light, six per point light.
        let mut local_views: Vec<glam::Mat4> = Vec::new();
        let mut local_layout = (4, 1);
        let mut shadowed_dropped = 0;
        if cluster_data.shadowed > 0 {
            let mut tans = Vec::new();
            // The atlas holds a limited number of tiles; the farthest
            // shadowed lights beyond that draw without shadows.
            let fits = local_shadows::lights_that_fit(
                scene.local_atlas.width(),
                (0..cluster_data.shadowed).map(|slot| {
                    local_shadows::view_count(&frame.lights[cluster_data.source[slot]])
                }),
            );
            shadowed_dropped = cluster_data.shadowed - fits;
            scene.warn_shadow_cap(shadowed_dropped);
            cluster_data.shadowed = fits;
            for slot in 0..cluster_data.shadowed {
                let light = &frame.lights[cluster_data.source[slot]];
                let (views, tan_half) = local_shadows::views_for(light);
                cluster_data.lights[slot].spot[3] = local_views.len() as f32;
                tans.extend(std::iter::repeat_n(tan_half, views.len()));
                local_views.extend(views);
            }
            let atlas = scene.local_atlas.width();
            local_layout = local_shadows::tile_layout(atlas, local_views.len());
            let (tile, grid) = local_layout;
            let records: Vec<local_shadows::ShadowViewGpu> = local_views
                .iter()
                .zip(&tans)
                .enumerate()
                .map(|(i, (m, tan_half))| {
                    let (x, y) = local_shadows::tile_origin(i, tile, grid);
                    let a = atlas as f32;
                    local_shadows::ShadowViewGpu {
                        view_proj: m.to_cols_array_2d(),
                        rect: [x as f32 / a, y as f32 / a, tile as f32 / a, tile as f32 / a],
                        params: [2.0 * tan_half / tile as f32, 0.0, 0.0, 0.0],
                    }
                })
                .collect();
            scene
                .queue
                .write_buffer(&scene.local_views_buffer, 0, bytemuck::cast_slice(&records));
        }
        cluster.dims[3] = cluster_data.shadowed as u32;
        scene
            .frame_lights
            .write(&scene.queue, &cluster, &cluster_data);
        scene.warn_index_overflow(cluster_data.overflow);
        let stats_lights = (
            cluster_data.lights.len() as u32,
            cluster_data.dropped as u32,
            cluster_data.shadowed as u32,
        );
        if main {
            scene.gi.update(
                &scene.queue,
                &frame.environment,
                gi_mode,
                &target.lighting.gi,
                frame.view_proj,
                frame.eye,
            );
        } else {
            // Draw with the GI-off pipeline; DDGI's probes and update count
            // and its uniform stay as the last main view left them.
            scene.gi.mode = melon_core::GiMode::Off;
        }
        // Auxiliary views (thumbnails) keep the shadow map as it is and use
        // one cascade.
        let cascade_count = if main {
            scene.set_cascades(quality.cascades);
            scene.cascade_count() as usize
        } else {
            1
        };
        let cascade_set = cascades::fit(
            frame.view_proj,
            frame.eye,
            frame.light.to_light(),
            cascade_count,
            quality.shadow_distance as f32,
            scene.shadow_map_size(),
        );
        scene.update_light_cascades(&frame.light, &cascade_set);
        for (_, handle) in &frame.models {
            self.model_cache.prepare(&self.scene, server, *handle);
        }
        for handle in frame.overrides.values().flatten().flatten() {
            self.materials.prepare(&self.scene, server, *handle);
        }

        // Cull per view (the shadow pass against the shadow box, so casters
        // off screen still shade the view), then batch by model: every part
        // of a model draws its instances in one call.
        let camera = Frustum::from_view_proj(frame.view_proj);
        let shadow_boxes: Vec<Frustum> = cascade_set
            .view_proj
            .iter()
            .map(|m| Frustum::from_view_proj(*m))
            .collect();
        let mut stats = RenderStats {
            lights: stats_lights.0,
            lights_dropped: stats_lights.1,
            shadowed_lights: stats_lights.2,
            shadow_views: local_views.len() as u32,
            shadowed_lights_dropped: shadowed_dropped as u32,
            light_index_overflow: cluster_data.overflow as u32,
            ..Default::default()
        };
        let (mut seen, mut voxelized) = (Vec::new(), Vec::new());
        let mut casters: Vec<Vec<_>> = vec![Vec::new(); shadow_boxes.len()];
        let local_boxes: Vec<Frustum> = local_views
            .iter()
            .map(|m| Frustum::from_view_proj(*m))
            .collect();
        let mut local_casters: Vec<Vec<_>> = vec![Vec::new(); local_boxes.len()];
        // Entities with the same material overrides share a set id (0 = none),
        // so they batch together.
        let mut set_ids: std::collections::HashMap<&[Option<MaterialHandle>], u32> =
            Default::default();
        let mut sets: Vec<&[Option<MaterialHandle>]> = Vec::new();
        for (i, (matrix, handle)) in frame.models.iter().enumerate() {
            let set = frame.overrides.get(&i).map_or(0, |parts| {
                *set_ids.entry(parts.as_slice()).or_insert_with(|| {
                    sets.push(parts);
                    sets.len() as u32
                })
            });
            let key = (*handle, set);
            let (min, max) = world_aabb(*matrix, self.model_cache.model(server, *handle).bounds);
            if gi_mode == melon_core::GiMode::Ddgi && gi::ddgi::in_volume(min, max) {
                voxelized.push((key, *matrix));
            }
            if camera.intersects_aabb(min, max) {
                seen.push((key, *matrix));
            } else {
                stats.culled += 1;
            }
            let mut cast = false;
            for (list, shadow_box) in casters.iter_mut().zip(&shadow_boxes) {
                if shadow_box.intersects_aabb(min, max) {
                    list.push((key, *matrix));
                    cast = true;
                }
            }
            if !cast {
                stats.shadow_culled += 1;
            }
            for (list, view_box) in local_casters.iter_mut().zip(&local_boxes) {
                if view_box.intersects_aabb(min, max) {
                    list.push((key, *matrix));
                }
            }
        }
        let mut matrices = Vec::with_capacity(seen.len() * 2);
        let seen = batch(seen, &mut matrices);
        let casters: Vec<_> = casters
            .into_iter()
            .map(|list| batch(list, &mut matrices))
            .collect();
        let local_casters: Vec<_> = local_casters
            .into_iter()
            .map(|list| batch(list, &mut matrices))
            .collect();
        let voxelized = batch(voxelized, &mut matrices);
        self.scene.update_instances(&matrices);

        let (cache, materials, assets) = (&self.model_cache, &self.materials, &*server);
        let part_material = |key: DrawKey, part: usize| {
            let model = cache.model(assets, key.0);
            let own = key.1.checked_sub(1).and_then(|set| {
                let handle = sets[set as usize].get(part).copied().flatten()?;
                materials.get(handle)
            });
            own.unwrap_or_else(|| {
                (
                    &model.material_bind_groups[part],
                    model.surfaces.get(part).copied().unwrap_or_default(),
                )
            })
        };
        let eye = frame.eye;
        let textured = |batches: &[(DrawKey, std::ops::Range<u32>)]| {
            let mut calls = Calls::default();
            for (key, instances) in batches {
                let model = cache.model(assets, key.0);
                for (part, mesh) in model.meshes.iter().enumerate() {
                    let (group, surface) = part_material(*key, part);
                    if surface.blend {
                        let centre = (model.bounds.0 + model.bounds.1) * 0.5;
                        for i in instances.clone() {
                            let at = matrices[i as usize].transform_point3(centre);
                            calls.blend.push((at, (mesh, group, i..i + 1)));
                        }
                        continue;
                    }
                    let call = (mesh, group, instances.clone());
                    if surface.masked {
                        calls.masked.push(call.clone());
                    } else {
                        calls.plain.push((mesh, instances.clone()));
                    }
                    if surface.double_sided {
                        calls.two_sided.push(call);
                    } else {
                        calls.single.push(call);
                    }
                }
            }
            sort_back_to_front(eye, &mut calls.blend);
            calls
        };
        let calls = textured(&seen);
        let voxel_calls = textured(&voxelized);
        let draw_calls = &calls.single;
        let voxel_draw_calls: Vec<_> = voxel_calls
            .single
            .iter()
            .chain(&voxel_calls.two_sided)
            .cloned()
            .collect();
        let blend_calls: Vec<_> = calls.blend.iter().map(|(_, call)| call.clone()).collect();
        // Per shadow view (cascade or point / spot tile): the plain casters
        // and the alpha-masked ones.
        let casters_of = |view: &Vec<(DrawKey, std::ops::Range<u32>)>| {
            let (mut plain, mut masked) = (Vec::new(), Vec::new());
            for (key, instances) in view {
                let model = self.model_cache.model(server, key.0);
                for (part, mesh) in model.meshes.iter().enumerate() {
                    let (group, surface) = part_material(*key, part);
                    if surface.blend {
                        continue;
                    }
                    if surface.masked {
                        masked.push((mesh, group, instances.clone()));
                    } else {
                        plain.push((mesh, instances.clone()));
                    }
                }
            }
            (plain, masked)
        };
        let shadow_calls: Vec<ShadowDraws> = casters.iter().map(casters_of).collect();
        let local_calls: Vec<ShadowDraws> = local_casters.iter().map(casters_of).collect();
        let all_draws = [draw_calls, &calls.two_sided, &blend_calls];
        stats.draw_calls = all_draws.iter().map(|c| c.len() as u32).sum();
        stats.instances_drawn = all_draws
            .iter()
            .flat_map(|c| c.iter())
            .map(|(_, _, r)| r.len() as u32)
            .sum();
        stats.shadow_draw_calls = shadow_calls
            .iter()
            .chain(&local_calls)
            .map(|(plain, masked)| (plain.len() + masked.len()) as u32)
            .sum();

        // The draw lists borrow the model cache; the scene is a separate field.
        let scene = &mut self.scene;
        let mut untimed = None;
        let timer = if main { &mut self.timer } else { &mut untimed };
        let mark = |encoder: &mut wgpu::CommandEncoder, timer: &Option<timings::GpuTimer>, m| {
            if let Some(timer) = timer {
                timer.mark(encoder, m);
            }
        };
        if let Some(timer) = timer.as_mut() {
            timer.begin(encoder);
        }
        if !calls.two_sided.is_empty() {
            scene.ensure_variant(Variant::TwoSided);
        }
        if !blend_calls.is_empty() {
            scene.ensure_variant(Variant::Blend);
        }
        for (layer, (plain, masked)) in shadow_calls.iter().enumerate() {
            scene.render_cascade(encoder, layer, plain, masked);
        }
        if !local_views.is_empty() {
            scene.render_local_shadows(encoder, &local_views, local_layout, &local_calls);
        }
        mark(encoder, timer, timings::Mark::Shadows);
        // The depth/normal prepass feeds SSAO and SSGI. AO off (quality or
        // strength 0): skip the SSAO passes and bind white, so the image
        // matches the renderer without SSAO.
        let ao_on = quality.ao && frame.environment.ao_strength > 0.0;
        let ssgi_on = gi_mode == melon_core::GiMode::Ssgi;
        if ao_on || ssgi_on {
            scene.render_prepass(encoder, &calls.plain, &calls.masked, &target.ssao);
        }
        if ao_on {
            scene.render_ssao(encoder, &target.ssao);
        }
        mark(encoder, timer, timings::Mark::Ao);
        // GI off: no GI passes; the mesh shader keeps the hemisphere
        // ambient, so the image matches the renderer without GI.
        scene.gi.render(
            encoder,
            &gi::GiPasses {
                frame: &scene.frame_bind_group,
                instances: &scene.instance_bind_group,
                voxel_lighting: &scene.voxel_pass_lighting,
                voxel_draws: &voxel_draw_calls,
                view: &target.lighting.gi,
            },
        );
        mark(encoder, timer, timings::Mark::Gi);
        let lighting = if ao_on {
            &target.lighting.with_ao
        } else {
            &target.lighting.without_ao
        };

        let hdr = target.hdr_target();
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("melon_render 3D pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: hdr.msaa_view(),
                    depth_slice: None,
                    resolve_target: hdr.resolve_target(),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: target.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            scene.draw_meshes_textured(&mut pass, draw_calls, lighting);
            if !calls.two_sided.is_empty() {
                scene.draw_meshes_variant(&mut pass, &calls.two_sided, lighting, Variant::TwoSided);
            }
            scene.draw_sky(&mut pass);
            if !blend_calls.is_empty() {
                scene.draw_meshes_variant(&mut pass, &blend_calls, lighting, Variant::Blend);
            }
            scene.draw_world_lines(&mut pass, world_lines, frame.view_proj);
        }
        mark(encoder, timer, timings::Mark::Scene);
        // Bloom off (quality, strength 0, or no level fits): skip the passes
        // and tonemap with the black bloom image, so the image matches the
        // renderer without bloom.
        let bloom_on = quality.bloom && frame.environment.bloom_intensity > 0.0;
        let tonemap_group = match &target.bloom {
            Some(bloom) if bloom_on => {
                scene.render_bloom(encoder, bloom, frame.environment.bloom_intensity);
                &bloom.tonemap_bind_group
            }
            _ => &hdr.bind_group,
        };
        mark(encoder, timer, timings::Mark::Bloom);
        let color_view = target.color_view();
        drop(scene.tonemap_with(encoder, tonemap_group, &color_view));
        self.overlay.render(
            &self.scene.device,
            &self.scene.queue,
            encoder,
            &target.overlay,
            target.depth_view(),
            &color_view,
            frame.view_proj,
            frame.eye,
            overlay,
        );
        if let Some(timer) = timer.as_mut() {
            timer.finish(
                encoder,
                timings::Ran {
                    ao: ao_on,
                    prepass_for_gi: ssgi_on && !ao_on,
                    gi: gi_mode != melon_core::GiMode::Off,
                    bloom: bloom_on && target.bloom.is_some(),
                },
            );
        }
        stats
    }

    /// The pass the editor UI draws into: the swapchain image cleared to
    /// `CLEAR_COLOR`, no depth.
    pub fn ui_pass(&self, frame: &mut Frame) -> wgpu::RenderPass<'static> {
        frame
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("melon_render egui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &frame.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime()
    }

    /// Copy `target`'s finished image onto the swapchain image of `frame`
    /// (a game without the editor UI). `target` must be window sized.
    pub fn present_target(&self, frame: &mut Frame, target: &ViewportTarget) {
        self.present.draw(
            &self.scene.device,
            &mut frame.encoder,
            &target.color_view,
            &frame.view,
        );
    }

    pub fn scene_mut(&mut self) -> &mut SceneRenderer {
        &mut self.scene
    }
}

impl SceneRenderer {
    /// `msaa_counts`: the sample counts the adapter supports for the 3D
    /// pass formats (1 always). Starts at the default quality (MSAA 4x,
    /// 2048 shadow map).
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        msaa_counts: Vec<u32>,
    ) -> Self {
        let uniform_entry = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        // Group 0 holds everything that changes once per frame; group 3
        // holds per-view lighting inputs (SSAO, later GI).
        let ibl_texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame bind group layout"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                uniform_entry(1, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                uniform_entry(4, wgpu::ShaderStages::FRAGMENT),
                // Image-based ambient: SH uniform, prefiltered environment,
                // its sampler, the BRDF lookup.
                uniform_entry(7, wgpu::ShaderStages::FRAGMENT),
                ibl_texture_entry(8),
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                ibl_texture_entry(10),
                // Point / spot shadows: the depth atlas and the tile records.
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 12,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let model_layout = instance_buffer_layout(&device);
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow camera bind group layout"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });
        let shadow_pipeline =
            Renderer::build_shadow_pipeline(&device, &shadow_layout, &model_layout, None);
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        // Group 2: one material. Every material binds all four textures
        // (1x1 defaults where the glTF has none), so the layout is fixed.
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material bind group layout"),
            entries: &[
                texture_entry(0), // base colour (sRGB)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(2), // metallic-roughness (linear: G roughness, B metallic)
                texture_entry(3), // tangent-space normal (linear)
                texture_entry(4), // emissive (sRGB)
                uniform_entry(5, wgpu::ShaderStages::FRAGMENT), // MaterialUniform
            ],
        });

        let shadow_mask_pipeline = Renderer::build_shadow_pipeline(
            &device,
            &shadow_layout,
            &model_layout,
            Some(&material_layout),
        );
        // Group 3: per-view lighting inputs: AO (R, 1 = unoccluded), then
        // every GI backend's bindings (gi/mod.rs).
        let mut lighting_entries = vec![texture_entry(0)];
        lighting_entries.extend(gi::Gi::layout_entries());
        lighting_entries.extend(lights::LightBuffers::layout_entries());
        let lighting_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lighting bind group layout"),
            entries: &lighting_entries,
        });

        let unfilterable_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let ssao_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ssao bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                unfilterable_entry(1),
                uniform_entry(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let ssao_blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ssao blur bind group layout"),
            entries: &[
                unfilterable_entry(1),
                uniform_entry(2, wgpu::ShaderStages::FRAGMENT),
                unfilterable_entry(3),
            ],
        });
        let prepass_pipeline =
            Renderer::build_prepass_pipeline(&device, &frame_layout, &model_layout, None);
        let prepass_mask_pipeline = Renderer::build_prepass_pipeline(
            &device,
            &frame_layout,
            &model_layout,
            Some(&material_layout),
        );
        let ssao_pipeline =
            Renderer::build_ssao_pipeline(&device, &frame_layout, &ssao_layout, "fs_ssao");
        let ssao_blur_pipeline =
            Renderer::build_ssao_pipeline(&device, &frame_layout, &ssao_blur_layout, "fs_blur");
        let ssao_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ssao uniform buffer"),
            size: std::mem::size_of::<SsaoUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        // Tonemap: scene HDR (textureLoad), bloom mip 0 + sampler, bloom uniform.
        let tonemap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tonemap bind group layout"),
            entries: &[
                unfilterable_entry(0),
                texture_entry(1),
                sampler_entry(2),
                uniform_entry(3, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let tonemap_pipeline =
            Renderer::build_tonemap_pipeline(&device, surface_format, &tonemap_layout);
        let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bloom bind group layout"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });
        let bloom_pipeline =
            |entry, blend| Renderer::build_bloom_pipeline(&device, &bloom_layout, entry, blend);
        let bloom_first_pipeline = bloom_pipeline("fs_downsample_first", None);
        let bloom_down_pipeline = bloom_pipeline("fs_downsample", None);
        // Upsample adds onto the level's own downsample.
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let bloom_up_pipeline = bloom_pipeline(
            "fs_upsample",
            Some(wgpu::BlendState {
                color: add,
                alpha: add,
            }),
        );
        let bloom_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bloom sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bloom_off_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bloom off uniform buffer"),
            contents: bytemuck::bytes_of(&[0.0f32; 4]),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera uniform buffer"),
            size: std::mem::size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light uniform buffer"),
            size: std::mem::size_of::<LightUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let environment_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("environment uniform buffer"),
            size: std::mem::size_of::<EnvironmentUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (shadow_texture, shadow_view, shadow_layers) =
            create_shadow_map(&device, SHADOW_MAP_SIZE, 1);
        // Linear filtering on a comparison sampler gives 2x2 hardware PCF per tap.
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow comparison sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let shadow_camera_buffers: Vec<wgpu::Buffer> = (0..cascades::MAX_CASCADES)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("shadow camera uniform buffer"),
                    size: std::mem::size_of::<CameraUniform>() as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let shadow_bind_groups: Vec<wgpu::BindGroup> = shadow_camera_buffers
            .iter()
            .map(|buffer| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shadow camera bind group"),
                    layout: &shadow_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                })
            })
            .collect();

        let (local_atlas, local_atlas_view) = create_local_atlas(&device, 4);
        let local_views_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("local shadow views"),
            size: (local_shadows::MAX_VIEWS * size_of::<local_shadows::ShadowViewGpu>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ibl_sky = ibl::SkyParams::default();
        let ibl = ibl::IblGpu::new(&device, &queue, &ibl::Source::Procedural(ibl_sky));
        let frame_bind_group = frame_bind_group(
            &device,
            &frame_layout,
            [&camera_buffer, &light_buffer, &environment_buffer],
            &shadow_view,
            &shadow_sampler,
            &ibl,
            (&local_atlas_view, &local_views_buffer),
        );

        let texture_1x1 = |label, rgba: [u8; 4]| {
            let texture = device.create_texture_with_data(
                &queue,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &rgba,
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let default_textures = DefaultTextures {
            white: texture_1x1("default white texture", [255; 4]),
            flat_normal: texture_1x1("default flat normal texture", [128, 128, 255, 255]),
            black: texture_1x1("default black texture", [0, 0, 0, 255]),
        };
        let material_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let checker = Renderer::create_checkerboard_texture(&device, &queue)
            .create_view(&wgpu::TextureViewDescriptor::default());
        let checker_material = material_bind_group(
            &device,
            &material_layout,
            &material_sampler,
            &default_textures,
            &CHECKER_MATERIAL,
            [Some(&checker), None, None, None],
        );

        let gi = gi::Gi::new(
            &device,
            &gi::GiInputs {
                frame_layout: &frame_layout,
                light: &light_buffer,
                shadow_view: &shadow_view,
                shadow_sampler: &shadow_sampler,
                local_atlas: &local_atlas_view,
                local_views: &local_views_buffer,
                model_layout: &model_layout,
                material_layout: &material_layout,
                lighting_layout: &lighting_layout,
            },
        );
        let white_ao = wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&default_textures.white),
        };
        let empty_lights = lights::LightBuffers::new(&device);
        let frame_lights = lights::LightBuffers::new(&device);
        let lighting = |label, voxels, lights: &lights::LightBuffers| {
            let mut entries = vec![white_ao.clone()];
            entries.extend(gi.entries(None, voxels));
            entries.extend(lights.entries());
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &lighting_layout,
                entries: &entries,
            })
        };
        let unoccluded_lighting = lighting("unoccluded lighting bind group", true, &empty_lights);
        // The voxel passes see the frame's lights (the shadowed ones inject).
        let voxel_pass_lighting =
            lighting("gi voxel pass lighting bind group", false, &frame_lights);

        let (instance_buffer, instance_bind_group) =
            create_instance_buffer(&device, &model_layout, INITIAL_INSTANCE_CAPACITY);

        let mut scene = Self {
            device,
            queue,
            surface_format,
            frame_layout,
            msaa_counts,
            msaa: 0,
            msaa_pipelines: Default::default(),
            variants: Default::default(),
            tonemap_pipeline,
            tonemap_bind_group_layout: tonemap_layout,
            bloom_layout,
            bloom_sampler,
            bloom_first_pipeline,
            bloom_down_pipeline,
            bloom_up_pipeline,
            bloom_off_buffer,
            model_bind_group_layout: model_layout,
            material_bind_group_layout: material_layout,
            material_sampler,
            default_textures,
            camera_buffer,
            light_buffer,
            environment_buffer,
            frame_bind_group,
            ibl,
            ibl_sky,
            ibl_hdr: None,
            ibl_built: Some((ibl_sky, None)),
            ibl_frozen: false,
            ibl_rebuilds: 0,
            shadow_rebuilds: 0,
            warned_shadow_cap: 0,
            warned_index_overflow: 0,
            shadow_pipeline,
            shadow_mask_pipeline,
            shadow_camera_buffers,
            shadow_bind_groups,
            local_atlas,
            local_atlas_view,
            local_views_buffer,
            shadow_camera_layout: shadow_layout,
            local_shadow_cameras: Vec::new(),
            shadow_texture,
            shadow_view,
            shadow_layers,
            shadow_sampler,
            checker_material,
            prepass_pipeline,
            prepass_mask_pipeline,
            ssao_pipeline,
            ssao_blur_pipeline,
            ssao_layout,
            ssao_blur_layout,
            ssao_buffer,
            lighting_layout,
            frame_lights,
            unoccluded_lighting,
            voxel_pass_lighting,
            gi,
            world_lines: LineBuffer::default(),
            instance_buffer,
            instance_bind_group,
            instance_capacity: INITIAL_INSTANCE_CAPACITY,
            instance_staging: Vec::new(),
        };
        scene.set_msaa(MSAA_SAMPLES);
        scene
    }

    /// Recreates the sun shadow map at `size` texels (clamped to 256 up to
    /// the device limit) and every bind group reading it, when the size
    /// changed. The shading adapts: mesh.wgsl reads the texel size from the
    /// texture.
    pub fn set_shadow_map_size(&mut self, size: u32) {
        let size = size.clamp(256, self.device.limits().max_texture_dimension_2d);
        self.rebuild_shadow_map(size, self.shadow_layers.len() as u32);
    }

    /// Sets the number of sun shadow cascades (1 to 4), recreating the
    /// shadow map when it changes.
    pub fn set_cascades(&mut self, count: u32) {
        let count = count.clamp(1, cascades::MAX_CASCADES as u32);
        self.rebuild_shadow_map(self.shadow_texture.width(), count);
    }

    /// Number of cascade layers the shadow map has.
    pub fn cascade_count(&self) -> u32 {
        self.shadow_layers.len() as u32
    }

    fn rebuild_shadow_map(&mut self, size: u32, layers: u32) {
        if self.shadow_texture.width() == size && self.shadow_layers.len() as u32 == layers {
            return;
        }
        (self.shadow_texture, self.shadow_view, self.shadow_layers) =
            create_shadow_map(&self.device, size, layers);
        self.shadow_bindings_changed();
        self.shadow_rebuilds += 1;
        tracing::debug!(size, layers, "shadow map recreated");
    }

    /// Edge of the point / spot shadow atlas, recreated when it changes
    /// (a 4 x 4 stand-in while nothing casts shadows).
    pub fn set_local_shadow_atlas(&mut self, size: u32) {
        let size = size.clamp(4, self.device.limits().max_texture_dimension_2d);
        if self.local_atlas.width() == size {
            return;
        }
        (self.local_atlas, self.local_atlas_view) = create_local_atlas(&self.device, size);
        self.shadow_bindings_changed();
        self.shadow_rebuilds += 1;
        tracing::debug!(size, "local shadow atlas recreated");
    }

    /// Warn when shadow-casting lights did not fit the atlas (once per
    /// change of the count).
    fn warn_shadow_cap(&mut self, dropped: usize) {
        if dropped as u32 != self.warned_shadow_cap && dropped > 0 {
            tracing::warn!(
                dropped,
                "{dropped} shadow-casting lights do not fit the local shadow atlas and draw without shadows; raise its size or lower the shadowed lights"
            );
        }
        self.warned_shadow_cap = dropped as u32;
    }

    /// Warn when the cluster light index list overflowed (once per change).
    fn warn_index_overflow(&mut self, overflow: usize) {
        if overflow as u32 != self.warned_index_overflow && overflow > 0 {
            tracing::warn!(
                overflow,
                "cluster light index list is full: {overflow} light entries dropped, some screen areas miss lights"
            );
        }
        self.warned_index_overflow = overflow as u32;
    }

    /// Rebind everything that reads the shadow textures (group 0, the GI
    /// voxel passes).
    fn shadow_bindings_changed(&mut self) {
        self.rebuild_frame_bind_group();
        self.gi.set_shadow_map(
            &self.device,
            &gi::GiInputs {
                frame_layout: &self.frame_layout,
                light: &self.light_buffer,
                shadow_view: &self.shadow_view,
                shadow_sampler: &self.shadow_sampler,
                local_atlas: &self.local_atlas_view,
                local_views: &self.local_views_buffer,
                model_layout: &self.model_bind_group_layout,
                material_layout: &self.material_bind_group_layout,
                lighting_layout: &self.lighting_layout,
            },
        );
    }

    /// Camera uniforms and bind groups for at least `count` local shadow tiles.
    fn ensure_local_cameras(&mut self, count: usize) {
        while self.local_shadow_cameras.len() < count {
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("local shadow camera uniform"),
                size: std::mem::size_of::<CameraUniform>() as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("local shadow camera bind group"),
                layout: &self.shadow_camera_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            self.local_shadow_cameras.push((buffer, group));
        }
    }

    /// Draw every point / spot shadow tile into the atlas in one pass: tile
    /// `i` renders `tiles[i]` with its view-projection into its square of
    /// the atlas (`tile` texels, `grid` tiles per row).
    fn render_local_shadows(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        views: &[glam::Mat4],
        layout: (u32, u32),
        draws: &[ShadowDraws],
    ) {
        self.ensure_local_cameras(views.len());
        for ((buffer, _), m) in self.local_shadow_cameras.iter().zip(views) {
            let camera = CameraUniform {
                view_proj: m.to_cols_array_2d(),
                ..Zeroable::zeroed()
            };
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(&[camera]));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("melon_render local shadow pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.local_atlas_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let (tile, grid) = layout;
        for (i, (plain, masked)) in draws.iter().enumerate() {
            let (x, y) = local_shadows::tile_origin(i, tile, grid);
            pass.set_viewport(x as f32, y as f32, tile as f32, tile as f32, 0.0, 1.0);
            pass.set_scissor_rect(x, y, tile, tile);
            pass.set_bind_group(0, &self.local_shadow_cameras[i].1, &[]);
            pass.set_bind_group(1, &self.instance_bind_group, &[]);
            pass.set_pipeline(&self.shadow_pipeline);
            for (mesh, instances) in plain {
                pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
            }
            if !masked.is_empty() {
                pass.set_pipeline(&self.shadow_mask_pipeline);
                for (mesh, material, instances) in masked {
                    pass.set_bind_group(2, *material, &[]);
                    pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                    pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                    pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
                }
            }
        }
    }

    /// Group 0 over the current buffers, shadow map and IBL textures.
    fn rebuild_frame_bind_group(&mut self) {
        self.frame_bind_group = frame_bind_group(
            &self.device,
            &self.frame_layout,
            [
                &self.camera_buffer,
                &self.light_buffer,
                &self.environment_buffer,
            ],
            &self.shadow_view,
            &self.shadow_sampler,
            &self.ibl,
            (&self.local_atlas_view, &self.local_views_buffer),
        );
    }

    /// Use `hdr` (an equirect image and its intensity) as the sky and the
    /// image-based ambient from the next `update_environment` /
    /// `update_light`; `None` = the procedural sky.
    pub fn set_sky_texture(&mut self, hdr: Option<(std::sync::Arc<ibl::HdrImage>, f32)>) {
        self.ibl_hdr = hdr;
    }

    /// Recompute the IBL data when its inputs (sky colours, hemisphere
    /// ambient, HDR image) changed since the last build.
    fn refresh_ibl(&mut self) {
        if self.ibl_frozen {
            return;
        }
        let key = (
            self.ibl_sky,
            self.ibl_hdr.as_ref().map(|(image, intensity)| {
                (std::sync::Arc::as_ptr(image) as usize, intensity.to_bits())
            }),
        );
        if self.ibl_built == Some(key) {
            return;
        }
        let source = match &self.ibl_hdr {
            Some((image, intensity)) => ibl::Source::Hdr(image, *intensity),
            None => ibl::Source::Procedural(self.ibl_sky),
        };
        self.ibl.rebuild(&self.queue, &source);
        self.ibl_rebuilds += 1;
        self.ibl_built = Some(key);
    }

    /// Edge of the sun shadow map in texels.
    pub fn shadow_map_size(&self) -> u32 {
        self.shadow_texture.width()
    }

    /// Sets the 3D pass sample count to `requested` (or the nearest lower
    /// count the adapter supports, [`effective_msaa`]), building its
    /// pipelines on first use. Targets follow in `render_scene`.
    pub fn set_msaa(&mut self, requested: u32) {
        let samples = effective_msaa(requested, &self.msaa_counts);
        self.msaa = samples;
        let slot = &mut self.msaa_pipelines[msaa_slot(samples)];
        if slot.is_none() {
            let layouts = [
                &self.frame_layout,
                &self.model_bind_group_layout,
                &self.material_bind_group_layout,
                &self.lighting_layout,
            ];
            let mesh =
                |mode| Renderer::build_mesh_pipeline(&self.device, &layouts, mode, samples, None);
            use melon_core::GiMode::{Ddgi, Off, Ssgi};
            *slot = Some(MsaaPipelines {
                mesh: [mesh(Off), mesh(Ssgi), mesh(Ddgi)],
                sky: Renderer::build_sky_pipeline(&self.device, &self.frame_layout, samples),
                world_lines: Renderer::build_world_line_pipeline(
                    &self.device,
                    &self.frame_layout,
                    samples,
                ),
            });
            tracing::debug!(samples, "msaa pipelines built");
        }
    }

    /// Sample count of the 3D pass now.
    pub fn msaa(&self) -> u32 {
        self.msaa
    }

    fn pipelines(&self) -> &MsaaPipelines {
        self.msaa_pipelines[msaa_slot(self.msaa)]
            .as_ref()
            .expect("set_msaa builds the pipelines")
    }

    /// Recreates `target`'s multisampled textures (HDR colour, depth,
    /// overlay) when they do not have the current sample count. The display
    /// image and everything single-sample stay.
    fn match_msaa(&self, target: &mut ViewportTarget) {
        if target.sample_count() == self.msaa {
            return;
        }
        let size = target.size;
        target.hdr_target.msaa = self.create_hdr_msaa(size);
        (target._depth_texture, target.depth_view) =
            Renderer::create_depth_target(&self.device, size, self.msaa);
        target.overlay = overlay::OverlayTargets::new(&self.device, size, self.msaa);
        tracing::debug!(samples = self.msaa, "viewport msaa targets recreated");
    }

    /// Creates `target`'s SSGI textures at `ssgi`'s resolution (recreated,
    /// history and all, when it changes) and drops them when it is `None`,
    /// rebuilding its group 3 to match.
    fn match_gi_view(&self, target: &mut ViewportTarget, ssgi: Option<melon_core::SsgiResolution>) {
        if target.lighting.gi.ssgi.as_ref().map(|v| v.resolution) == ssgi {
            return;
        }
        let gi = gi::GiView {
            ssgi: ssgi.map(|resolution| {
                self.gi.create_ssgi_view(
                    &self.device,
                    target.size,
                    resolution,
                    &gi::ssgi::SsgiViewInputs {
                        depth: &target.ssao.depth_view,
                        normal: &target.ssao.normal_view,
                        hdr: target.hdr_target.view(),
                    },
                )
            }),
        };
        target.lighting = self.view_lighting(&target.ssao, gi);
    }

    /// A view's group 3 with its AO and with white AO, over `gi`.
    fn view_lighting(&self, ssao: &SsaoTargets, gi: gi::GiView) -> ViewLighting {
        let lighting = |label, ao| {
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(ao),
            }];
            entries.extend(self.gi.entries(Some(&gi), true));
            entries.extend(self.frame_lights.entries());
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.lighting_layout,
                entries: &entries,
            })
        };
        ViewLighting {
            with_ao: lighting("lighting bind group", &ssao.ao_view),
            without_ao: lighting("open lighting bind group", &self.default_textures.white),
            gi,
        }
    }
}

/// A `Depth32Float` sun shadow map of `layers` layers of `size` x `size`
/// texels: the array view to sample and a view per layer to render into.
fn create_shadow_map(
    device: &wgpu::Device,
    size: u32,
    layers: u32,
) -> (wgpu::Texture, wgpu::TextureView, Vec<wgpu::TextureView>) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shadow map"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let layer_views = (0..layers)
        .map(|layer| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            })
        })
        .collect();
    (texture, view, layer_views)
}

/// Group 0: camera, light, the shadow map + comparison sampler, environment
/// (`buffers`: camera, light, environment).
fn frame_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    [camera, light, environment]: [&wgpu::Buffer; 3],
    shadow_view: &wgpu::TextureView,
    shadow_sampler: &wgpu::Sampler,
    ibl: &ibl::IblGpu,
    (local_atlas, local_views): (&wgpu::TextureView, &wgpu::Buffer),
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("frame bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: light.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(shadow_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: environment.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: ibl.uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: wgpu::BindingResource::TextureView(&ibl.specular),
            },
            wgpu::BindGroupEntry {
                binding: 9,
                resource: wgpu::BindingResource::Sampler(&ibl.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 10,
                resource: wgpu::BindingResource::TextureView(&ibl.brdf),
            },
            wgpu::BindGroupEntry {
                binding: 11,
                resource: wgpu::BindingResource::TextureView(local_atlas),
            },
            wgpu::BindGroupEntry {
                binding: 12,
                resource: local_views.as_entire_binding(),
            },
        ],
    })
}

/// A `Depth32Float` atlas of `size` x `size` texels for point / spot shadows.
fn create_local_atlas(device: &wgpu::Device, size: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("local shadow atlas"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

impl Renderer {
    /// `layouts`: frame, model, material, lighting (groups 0 to 3). `gi`:
    /// where the diffuse ambient comes from (`Off`: the hemisphere).
    fn build_mesh_pipeline(
        device: &wgpu::Device,
        layouts: &[&wgpu::BindGroupLayout; 4],
        gi: melon_core::GiMode,
        samples: u32,
        variant: Option<Variant>,
    ) -> wgpu::RenderPipeline {
        let blend = variant == Some(Variant::Blend);
        let constants = [
            ("GI_MODE", gi::shader_mode(gi)),
            ("ALPHA_BLEND", f64::from(blend)),
        ];
        let compilation_options = wgpu::PipelineCompilationOptions {
            constants: &constants,
            ..Default::default()
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh.wgsl"),
            source: wgpu::ShaderSource::Wgsl(MESH_WGSL.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh pipeline layout"),
            bind_group_layouts: &layouts.map(Some),
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render mesh pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: compilation_options.clone(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,  // position
                        1 => Float32x3,  // normal
                        2 => Float32x2,  // uv
                        3 => Float32x4,  // tangent + handedness
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: variant.is_none().then_some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(!blend),
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
                compilation_options: compilation_options.clone(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(if blend {
                        wgpu::BlendState::ALPHA_BLENDING
                    } else {
                        wgpu::BlendState::REPLACE
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// SSAO depth/normal prepass: single-sample, depth-writing, back faces
    /// culled like the mesh pipeline.
    /// With `masked` (the material layout, group 2) the alpha-masked
    /// variant: reads the base colour alpha and discards below the cutoff.
    fn build_prepass_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        model_layout: &wgpu::BindGroupLayout,
        masked: Option<&wgpu::BindGroupLayout>,
    ) -> wgpu::RenderPipeline {
        let shader = device
            .create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/prepass.wgsl"));
        let (vs, fs) = if masked.is_some() {
            ("vs_mask", "fs_mask")
        } else {
            ("vs_main", "fs_main")
        };
        let layouts = [Some(frame_layout), Some(model_layout), masked];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("prepass pipeline layout"),
            bind_group_layouts: &layouts[..if masked.is_some() { 3 } else { 2 }],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render prepass pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(vs),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
                }],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(PREPASS_NORMAL_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// A fullscreen SSAO pass (`entry` in ssao.wgsl) writing `AO_FORMAT`.
    fn build_ssao_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        layout: &wgpu::BindGroupLayout,
        entry: &str,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/ssao.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ssao pipeline layout"),
            bind_group_layouts: &[Some(frame_layout), Some(layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(AO_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn build_tonemap_pipeline(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        tonemap_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader = device
            .create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/tonemap.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tonemap pipeline layout"),
            bind_group_layouts: &[Some(tonemap_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render tonemap pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format.add_srgb_suffix(),
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// A fullscreen bloom pass (`entry` in bloom.wgsl) into one HDR mip.
    fn build_bloom_pipeline(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        entry: &str,
        blend: Option<wgpu::BlendState>,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/bloom.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bloom pipeline layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// With `masked` (the material layout, group 2) the alpha-masked
    /// variant: reads the base colour alpha and discards below the cutoff.
    fn build_shadow_pipeline(
        device: &wgpu::Device,
        shadow_layout: &wgpu::BindGroupLayout,
        model_layout: &wgpu::BindGroupLayout,
        masked: Option<&wgpu::BindGroupLayout>,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/shadow.wgsl"));
        let (vs, fs) = if masked.is_some() {
            ("vs_mask", Some("fs_mask"))
        } else {
            ("vs_main", None)
        };
        let layouts = [Some(shadow_layout), Some(model_layout), masked];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow pipeline layout"),
            bind_group_layouts: &layouts[..if masked.is_some() { 3 } else { 2 }],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render shadow pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(vs),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2],
                }],
            },
            primitive: wgpu::PrimitiveState {
                // No culling: the nearest surface (a closed mesh's front
                // faces) stores depth, and single-sided quads cast from both
                // sides. Culling front faces stores a resting object's
                // underside, level with the floor, and light leaks along its
                // base. Acne on lit faces is handled by the small bias plus
                // the receiver's normal offset in mesh.wgsl.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 1,
                    slope_scale: 1.5,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: fs.map(|entry_point| wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Line pipeline for the 3D pass: HDR target, multisampled, depth-tested
    /// but not depth-writing, so lines hide behind geometry.
    fn build_world_line_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        samples: u32,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/line.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world line pipeline layout"),
            bind_group_layouts: &[Some(frame_layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render world line pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
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
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Sky pipeline for the 3D pass: a fullscreen triangle at the far plane,
    /// depth-tested `LessEqual` without depth writes, so it shows only where
    /// no geometry was drawn.
    fn build_sky_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        samples: u32,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/sky.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky pipeline layout"),
            bind_group_layouts: &[Some(frame_layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render sky pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
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
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn create_depth_target(
        device: &wgpu::Device,
        size: winit::dpi::PhysicalSize<u32>,
        samples: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth texture"),
            size: wgpu::Extent3d {
                width: size.width.max(1),
                height: size.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn create_checkerboard_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
        const SIZE: u32 = 64;
        const TILE: u32 = 8;
        let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let checker = ((x / TILE) + (y / TILE)).is_multiple_of(2);
                if checker {
                    pixels.extend_from_slice(&[220, 60, 180, 255]); // melon-pink
                } else {
                    pixels.extend_from_slice(&[255, 255, 255, 255]); // white
                }
            }
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("albedo checkerboard"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 4),
                rows_per_image: Some(SIZE),
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );

        texture
    }
}

impl SceneRenderer {
    #[instrument(skip(self), level = "debug")]
    pub fn cube_mesh(&self) -> Mesh {
        // 24 vertices (6 faces x 4), 36 indices. Each face has a normal + UVs.
        // UVs map so the checkerboard repeats once per face.
        let mut vertices: [Vertex; 24] = [
            // +X (normal +X)
            Vertex {
                position: [0.5, -0.5, -0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [0.0, 0.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, 0.5, -0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [0.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, 0.5, 0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [1.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, -0.5, 0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [1.0, 0.0],
                tangent: [0.0; 4],
            },
            // -X (normal -X)
            Vertex {
                position: [-0.5, -0.5, 0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [0.0, 0.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, 0.5, 0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [0.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, 0.5, -0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [1.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, -0.5, -0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [1.0, 0.0],
                tangent: [0.0; 4],
            },
            // +Y (normal +Y)
            Vertex {
                position: [0.5, 0.5, -0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 0.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, 0.5, -0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, 0.5, 0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [1.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, 0.5, 0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [1.0, 0.0],
                tangent: [0.0; 4],
            },
            // -Y (normal -Y)
            Vertex {
                position: [0.5, -0.5, 0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [0.0, 0.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, -0.5, 0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [0.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, -0.5, -0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [1.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, -0.5, -0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [1.0, 0.0],
                tangent: [0.0; 4],
            },
            // +Z (normal +Z)
            Vertex {
                position: [0.5, -0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, 0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, 0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [1.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, -0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [1.0, 0.0],
                tangent: [0.0; 4],
            },
            // -Z (normal -Z)
            Vertex {
                position: [-0.5, -0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [0.0, 0.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [-0.5, 0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [0.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, 0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [1.0, 1.0],
                tangent: [0.0; 4],
            },
            Vertex {
                position: [0.5, -0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [1.0, 0.0],
                tangent: [0.0; 4],
            },
        ];
        let indices: [u16; 36] = [
            0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16,
            17, 18, 16, 18, 19, 20, 21, 22, 20, 22, 23,
        ];
        generate_tangents(&mut vertices, &indices.map(u32::from));

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("cube vertex buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("cube index buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        Mesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            index_format: wgpu::IndexFormat::Uint16,
        }
    }

    /// Write the instance records (group 1) for this frame: one per matrix,
    /// in order; draw instance `i` with instance range `i..i + 1`. The
    /// buffer doubles until it fits.
    pub fn update_instances(&mut self, matrices: &[glam::Mat4]) {
        self.instance_staging.clear();
        self.instance_staging
            .extend(matrices.iter().map(|m| InstanceData {
                model: m.to_cols_array_2d(),
                normal_matrix: m.inverse().transpose().to_cols_array_2d(),
            }));
        let needed = matrices.len() as u64;
        if needed > self.instance_capacity {
            self.instance_capacity = grown_capacity(self.instance_capacity, needed);
            (self.instance_buffer, self.instance_bind_group) = create_instance_buffer(
                &self.device,
                &self.model_bind_group_layout,
                self.instance_capacity,
            );
            tracing::debug!(capacity = self.instance_capacity, "instance buffer grown");
        }
        self.queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instance_staging),
        );
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    pub fn create_viewport_target(&self, size: winit::dpi::PhysicalSize<u32>) -> ViewportTarget {
        let hdr = self.create_hdr_target(size);
        let bloom = self.create_bloom_targets(hdr.view(), size);
        let ssao = self.create_ssao_targets(size);
        // No SSGI textures until the view runs SSGI (`match_gi_view`).
        let lighting = self.view_lighting(&ssao, gi::GiView { ssgi: None });
        ViewportTarget::new(
            &self.device,
            self.surface_format.add_srgb_suffix(),
            size,
            hdr,
            ssao,
            bloom,
            lighting,
        )
    }

    /// The prepass and AO textures of one view and their bind groups.
    fn create_ssao_targets(&self, size: winit::dpi::PhysicalSize<u32>) -> SsaoTargets {
        let texture = |label, format, extra_usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size.width.max(1),
                    height: size.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | extra_usage,
                view_formats: &[],
            })
        };
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let none = wgpu::TextureUsages::empty();
        let depth_view = view(&texture(
            "prepass depth",
            wgpu::TextureFormat::Depth32Float,
            none,
        ));
        let normal_view = view(&texture("prepass normal", PREPASS_NORMAL_FORMAT, none));
        let raw_view = view(&texture("raw ao", AO_FORMAT, none));
        // COPY_SRC: debug readback (snapshot example).
        let ao_texture = texture("ao", AO_FORMAT, wgpu::TextureUsages::COPY_SRC);
        let ao_view = view(&ao_texture);
        let entry = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let uniform = wgpu::BindGroupEntry {
            binding: 2,
            resource: self.ssao_buffer.as_entire_binding(),
        };
        let ssao_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ssao bind group"),
            layout: &self.ssao_layout,
            entries: &[
                entry(0, &depth_view),
                entry(1, &normal_view),
                uniform.clone(),
            ],
        });
        let blur_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ssao blur bind group"),
            layout: &self.ssao_blur_layout,
            entries: &[entry(1, &normal_view), uniform, entry(3, &raw_view)],
        });
        SsaoTargets {
            depth_view,
            normal_view,
            raw_view,
            ao_texture,
            ao_view,
            ssao_bind_group,
            blur_bind_group,
        }
    }

    /// The multisampled HDR colour texture of a `size` target at the
    /// current sample count; `None` at 1 sample.
    fn create_hdr_msaa(
        &self,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> Option<(wgpu::Texture, wgpu::TextureView)> {
        (self.msaa > 1).then(|| {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("hdr scene msaa texture"),
                size: wgpu::Extent3d {
                    width: size.width.max(1),
                    height: size.height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: self.msaa,
                dimension: wgpu::TextureDimension::D2,
                format: HDR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        })
    }

    /// Creates an HDR scene colour target at the current sample count. Zero
    /// sizes clamp to 1x1.
    pub fn create_hdr_target(&self, size: winit::dpi::PhysicalSize<u32>) -> HdrTarget {
        let size = winit::dpi::PhysicalSize::new(size.width.max(1), size.height.max(1));
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr scene color texture"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group =
            self.tonemap_bind_group(&view, &self.default_textures.black, &self.bloom_off_buffer);
        HdrTarget {
            msaa: self.create_hdr_msaa(size),
            _texture: texture,
            view,
            bind_group,
            size,
        }
    }

    /// Tonemap group 0: the scene image `hdr`, the `bloom` image and the
    /// bloom uniform `settings` (intensity, 1 / levels).
    fn tonemap_bind_group(
        &self,
        hdr: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
        settings: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tonemap bind group"),
            layout: &self.tonemap_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(hdr),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(bloom),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.bloom_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: settings.as_entire_binding(),
                },
            ],
        })
    }

    /// One view's bloom chain over its resolved HDR image `hdr`; `None` when
    /// the view is too small for one level.
    fn create_bloom_targets(
        &self,
        hdr: &wgpu::TextureView,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> Option<BloomTargets> {
        let sizes = bloom_mip_sizes(size.width.max(1), size.height.max(1));
        let &(width, height) = sizes.first()?;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bloom texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            // Mip k is (width >> k, height >> k): the sizes above.
            mip_level_count: sizes.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mip_views: Vec<wgpu::TextureView> = (0..sizes.len() as u32)
            .map(|mip| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: mip,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let source = |view: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bloom source bind group"),
                layout: &self.bloom_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.bloom_sampler),
                    },
                ],
            })
        };
        let settings = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bloom uniform buffer"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(BloomTargets {
            hdr_source: source(hdr),
            mip_sources: mip_views.iter().map(source).collect(),
            tonemap_bind_group: self.tonemap_bind_group(hdr, &mip_views[0], &settings),
            mip_views,
            settings,
        })
    }

    /// The bloom chain of one view: downsample the resolved HDR image level
    /// by level, then upsample back, adding each level, into mip 0. Writes
    /// the tonemap's bloom uniform (`intensity`, 1 / levels).
    fn render_bloom(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bloom: &BloomTargets,
        intensity: f32,
    ) {
        let levels = bloom.mip_views.len();
        self.queue.write_buffer(
            &bloom.settings,
            0,
            bytemuck::bytes_of(&[intensity, 1.0 / levels as f32, 0.0, 0.0]),
        );
        // (target mip, source, pipeline, clear first)
        let downs = (0..levels).map(|i| match i {
            0 => (0, &bloom.hdr_source, &self.bloom_first_pipeline, true),
            _ => (
                i,
                &bloom.mip_sources[i - 1],
                &self.bloom_down_pipeline,
                true,
            ),
        });
        let ups = (0..levels - 1)
            .rev()
            .map(|i| (i, &bloom.mip_sources[i + 1], &self.bloom_up_pipeline, false));
        for (mip, source, pipeline, clear) in downs.chain(ups) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("melon_render bloom pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &bloom.mip_views[mip],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Downsamples overwrite every pixel; upsamples add.
                        load: if clear {
                            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, source, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Begins a pass on `target` that tonemaps `hdr` over it (no bloom).
    /// The pass stays open so callers can draw overlays (gizmo lines) on the
    /// display image.
    pub fn tonemap_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        hdr: &HdrTarget,
        target: &wgpu::TextureView,
    ) -> wgpu::RenderPass<'e> {
        self.tonemap_with(encoder, &hdr.bind_group, target)
    }

    /// `tonemap_pass` with a tonemap group 0 (scene image, bloom, settings).
    fn tonemap_with<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        target: &wgpu::TextureView,
    ) -> wgpu::RenderPass<'e> {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("melon_render tonemap pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The fullscreen triangle overwrites every pixel.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.tonemap_pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
        pass
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
}

impl Renderer {
    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }

    #[instrument(skip(self), level = "debug")]
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.configure_surface();
    }

    fn configure_surface(&self) {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.surface_format,
            view_formats: vec![self.surface_format.add_srgb_suffix()],
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            width: self.size.width,
            height: self.size.height,
            desired_maximum_frame_latency: 2,
            present_mode: self.present_mode,
        };
        self.surface.configure(&self.device, &config);
    }
}

impl SceneRenderer {
    /// `view_proj` must be invertible (sky rays); `eye` is the camera position.
    pub fn update_camera(&self, view_proj: glam::Mat4, eye: glam::Vec3) {
        let uniform = CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            eye: eye.to_array(),
            _pad: 0.0,
        };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[uniform]));
    }

    /// Sky colours and fog density for the mesh and sky shaders.
    pub fn update_environment(&mut self, environment: &melon_core::Environment) {
        if !self.ibl_frozen {
            self.ibl_sky.zenith = environment.zenith_color;
            self.ibl_sky.horizon = environment.horizon_color;
        }
        self.refresh_ibl();
        let uniform = EnvironmentUniform {
            zenith_color: environment.zenith_color.to_array(),
            fog_density: environment.fog_density,
            horizon_color: environment.horizon_color.to_array(),
            _pad: 0.0,
        };
        self.queue.write_buffer(
            &self.environment_buffer,
            0,
            bytemuck::cast_slice(&[uniform]),
        );
    }

    /// `light_view_proj` is the shadow box, see [`directional_light_view_proj`].
    pub fn update_light(&mut self, light: &melon_core::Light, light_view_proj: glam::Mat4) {
        self.update_light_cascades(light, &cascades::CascadeSet::single(light_view_proj));
    }

    /// `update_light` with the sun's shadow cascades.
    fn update_light_cascades(&mut self, light: &melon_core::Light, set: &cascades::CascadeSet) {
        if !self.ibl_frozen {
            self.ibl_sky.ambient = light.ambient;
            self.ibl_sky.sky_color = light.sky_color;
            self.ibl_sky.ground_color = light.ground_color;
        }
        self.refresh_ibl();
        // With an HDR sky the hemisphere colours (read by GI misses and the
        // GI-off fallbacks) follow the mean radiance of the image above and
        // below the horizon.
        let (sky_color, ground_color) = if self.ibl_hdr.is_some() {
            let scale = 1.0 / light.ambient.max(1e-3);
            (self.ibl.mean_up * scale, self.ibl.mean_down * scale)
        } else {
            (light.sky_color, light.ground_color)
        };
        let uniform = LightUniform {
            direction: light.to_light().to_array(),
            ambient: light.ambient,
            color: light.color.to_array(),
            _pad: 0.0,
            sky_color: sky_color.to_array(),
            _pad1: 0.0,
            ground_color: ground_color.to_array(),
            _pad2: 0.0,
            light_view_proj: set.view_proj[0].to_cols_array_2d(),
            cascade_view_proj: std::array::from_fn(|i| {
                set.view_proj
                    .get(i)
                    .unwrap_or(&set.view_proj[0])
                    .to_cols_array_2d()
            }),
            cascade_far: set.far,
            cascade_info: [set.view_proj.len() as f32, set.distance, 0.0, 0.0],
            cascade_forward: set.forward.extend(0.0).to_array(),
        };
        self.queue
            .write_buffer(&self.light_buffer, 0, bytemuck::cast_slice(&[uniform]));
        // The shadow shader reads only `view_proj`.
        for (buffer, m) in self.shadow_camera_buffers.iter().zip(&set.view_proj) {
            let shadow_camera = CameraUniform {
                view_proj: m.to_cols_array_2d(),
                ..Zeroable::zeroed()
            };
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(&[shadow_camera]));
        }
    }

    /// SSAO kernel, radius and strength for the SSAO passes.
    pub fn update_ssao(&self, environment: &melon_core::Environment) {
        let uniform = SsaoUniform {
            kernel: ssao_kernel(),
            radius: environment.ao_radius,
            strength: environment.ao_strength,
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.ssao_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    /// Depth/normal prepass of every mesh into `ssao`'s prepass targets
    /// (read by SSAO and SSGI). Call after `update_camera` and
    /// `update_instances`, before the 3D pass.
    fn render_prepass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        draw_calls: &[(&Mesh, std::ops::Range<u32>)],
        masked: &[(&Mesh, &wgpu::BindGroup, std::ops::Range<u32>)],
        ssao: &SsaoTargets,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("melon_render ssao prepass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &ssao.normal_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &ssao.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.prepass_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_bind_group(1, &self.instance_bind_group, &[]);
        for (mesh, instances) in draw_calls {
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
        }
        if !masked.is_empty() {
            pass.set_pipeline(&self.prepass_mask_pipeline);
            for (mesh, material, instances) in masked {
                pass.set_bind_group(2, *material, &[]);
                pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
            }
        }
    }

    /// The SSAO and blur passes into `ssao`'s AO texture. Call after
    /// `update_ssao` and `render_prepass`, before the 3D pass.
    fn render_ssao(&self, encoder: &mut wgpu::CommandEncoder, ssao: &SsaoTargets) {
        for (label, view, pipeline, bind_group) in [
            (
                "melon_render ssao pass",
                &ssao.raw_view,
                &self.ssao_pipeline,
                &ssao.ssao_bind_group,
            ),
            (
                "melon_render ssao blur pass",
                &ssao.ao_view,
                &self.ssao_blur_pipeline,
                &ssao.blur_bind_group,
            ),
        ] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The fullscreen triangle overwrites every pixel.
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.frame_bind_group, &[]);
            pass.set_bind_group(1, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Renders each `(mesh, instance range)` into the sun's shadow map. Call
    /// before the 3D pass, after `update_light` and `update_instances`.
    pub fn render_shadow_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        draw_calls: &[(&Mesh, std::ops::Range<u32>)],
    ) {
        self.render_shadow_pass_masked(encoder, draw_calls, &[]);
    }

    /// `render_shadow_pass` plus alpha-masked casters (each with its
    /// material, which discards below its cutoff).
    pub fn render_shadow_pass_masked(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        draw_calls: &[(&Mesh, std::ops::Range<u32>)],
        masked: &[(&Mesh, &wgpu::BindGroup, std::ops::Range<u32>)],
    ) {
        self.render_cascade(encoder, 0, draw_calls, masked);
    }

    /// One shadow cascade: the casters into layer `layer` with the matrix
    /// of `update_light_cascades`.
    fn render_cascade(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        layer: usize,
        draw_calls: &[(&Mesh, std::ops::Range<u32>)],
        masked: &[(&Mesh, &wgpu::BindGroup, std::ops::Range<u32>)],
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("melon_render shadow pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.shadow_layers[layer],
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shadow_pipeline);
        pass.set_bind_group(0, &self.shadow_bind_groups[layer], &[]);
        pass.set_bind_group(1, &self.instance_bind_group, &[]);
        for (mesh, instances) in draw_calls {
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
        }
        if !masked.is_empty() {
            pass.set_pipeline(&self.shadow_mask_pipeline);
            for (mesh, material, instances) in masked {
                pass.set_bind_group(2, *material, &[]);
                pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
            }
        }
    }

    /// Upload an arbitrary image to a wgpu texture. The caller picks the
    /// format; the bytes must match (`width * height * bytes_per_pixel`).
    #[instrument(skip(self), level = "debug")]
    pub fn create_texture_from_bytes(
        &self,
        pixels: &[u8],
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("loaded texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * format.block_copy_size(None).unwrap()),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    /// `create_texture_from_image` as sRGB colour data or as linear data
    /// (metallic-roughness, normals).
    pub fn create_texture_from_image_as(
        &self,
        img: &image::DynamicImage,
        srgb: bool,
    ) -> wgpu::Texture {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        let format = if srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        self.create_texture_from_bytes(&rgba, width, height, format)
    }

    /// Convert an `image::DynamicImage` to RGBA8 and upload it as a texture.
    /// Returns the texture; pass it to `create_material_bind_group`.
    #[instrument(skip(self), level = "debug")]
    pub fn create_texture_from_image(&self, img: &image::DynamicImage) -> wgpu::Texture {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        self.create_texture_from_bytes(&rgba, width, height, wgpu::TextureFormat::Rgba8UnormSrgb)
    }

    /// A material bind group (group 2) from `factors` and optional
    /// textures: base colour (sRGB), metallic-roughness (linear, G =
    /// roughness, B = metallic), tangent-space normal (linear) and emissive
    /// (sRGB). Missing textures bind white (normal: flat), so the factors
    /// apply alone.
    pub fn create_material_bind_group(
        &self,
        factors: &MaterialUniform,
        textures: [Option<&wgpu::Texture>; 4],
    ) -> wgpu::BindGroup {
        let views = textures.map(|t| t.map(|t| t.create_view(&Default::default())));
        material_bind_group(
            &self.device,
            &self.material_bind_group_layout,
            &self.material_sampler,
            &self.default_textures,
            factors,
            [0, 1, 2, 3].map(|i| views[i].as_ref()),
        )
    }

    /// Load a glTF / GLB file. Every primitive reachable from the default
    /// scene's node tree becomes one `Mesh`, with its node's world transform
    /// baked into the vertices, plus its base colour texture (or `None`).
    #[instrument(skip(self), level = "info")]
    pub fn load_gltf(&self, path: &str) -> Result<LoadedModel> {
        let (document, buffers, images) =
            gltf::import(path).with_context(|| format!("failed to import glTF file: {path}"))?;
        let model = self.build_model(&document, &buffers, &images)?;
        tracing::info!("loaded glTF {}: {} part(s)", path, model.meshes.len());
        Ok(model)
    }

    /// Load every glTF mesh of a file from one import: entry `i` is mesh `i`
    /// (`<file>#mesh<i>`), its primitives in mesh-local space, no node
    /// transform baked in. A mesh that fails gets its own error.
    #[instrument(skip(self), level = "info")]
    pub fn load_gltf_meshes(&self, path: &str) -> Result<Vec<Result<LoadedModel>>> {
        let imported =
            gltf::import(path).with_context(|| format!("failed to import glTF file: {path}"))?;
        Ok(self.meshes_of(path, imported))
    }

    /// `load_gltf_meshes` from memory (a packed game; external files in `files`, see `packed_gltf`).
    pub fn load_gltf_meshes_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        files: &std::collections::HashMap<String, Vec<u8>>,
    ) -> Result<Vec<Result<LoadedModel>>> {
        let imported = packed_gltf::import_with(name, bytes, files)?;
        Ok(self.meshes_of(name, imported))
    }

    fn meshes_of(
        &self,
        path: &str,
        (document, buffers, images): (
            gltf::Document,
            Vec<gltf::buffer::Data>,
            Vec<gltf::image::Data>,
        ),
    ) -> Vec<Result<LoadedModel>> {
        document
            .meshes()
            .map(|gltf_mesh| {
                let mut model = LoadedModel {
                    meshes: Vec::new(),
                    material_bind_groups: Vec::new(),
                    surfaces: Vec::new(),
                    part_bounds: Vec::new(),
                    bounds: (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN)),
                };
                for primitive in gltf_mesh.primitives() {
                    self.add_primitive(
                        &primitive,
                        glam::Mat4::IDENTITY,
                        &buffers,
                        &images,
                        &mut model,
                    )?;
                }
                if model.meshes.is_empty() {
                    anyhow::bail!("{path} mesh {} has no primitives", gltf_mesh.index());
                }
                Ok(model)
            })
            .collect()
    }

    /// Load one glTF mesh (`<file>#mesh<N>`); see `load_gltf_meshes`.
    pub fn load_gltf_mesh(&self, path: &str, mesh: usize) -> Result<LoadedModel> {
        self.load_gltf_meshes(path)?
            .into_iter()
            .nth(mesh)
            .with_context(|| format!("{path} has no mesh {mesh}"))?
    }

    /// Load a glTF / GLB from memory (embedded or data-URI buffers only).
    pub fn load_gltf_bytes(&self, bytes: &[u8]) -> Result<LoadedModel> {
        let (document, buffers, images) =
            gltf::import_slice(bytes).context("failed to import glTF data")?;
        self.build_model(&document, &buffers, &images)
    }

    /// A UV sphere of radius 0.5 (`builtin:sphere`, material thumbnails),
    /// in the checkerboard material.
    pub fn builtin_sphere_model(&self) -> LoadedModel {
        const SEGMENTS: u32 = 32;
        const RINGS: u32 = 16;
        let mut vertices = Vec::new();
        for ring in 0..=RINGS {
            let v = ring as f32 / RINGS as f32;
            let theta = v * std::f32::consts::PI;
            for seg in 0..=SEGMENTS {
                let u = seg as f32 / SEGMENTS as f32;
                let phi = u * std::f32::consts::TAU;
                let normal = glam::Vec3::new(
                    theta.sin() * phi.cos(),
                    theta.cos(),
                    theta.sin() * phi.sin(),
                );
                let tangent = glam::Vec3::new(-phi.sin(), 0.0, phi.cos());
                vertices.push(Vertex {
                    position: (normal * 0.5).to_array(),
                    normal: normal.to_array(),
                    uv: [u, v],
                    tangent: tangent.extend(1.0).to_array(),
                });
            }
        }
        let mut indices: Vec<u32> = Vec::new();
        for ring in 0..RINGS {
            for seg in 0..SEGMENTS {
                let a = ring * (SEGMENTS + 1) + seg;
                let b = a + SEGMENTS + 1;
                indices.extend([a, a + 1, b, a + 1, b + 1, b]);
            }
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sphere vertex buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sphere index buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        LoadedModel {
            meshes: vec![Mesh {
                vertex_buffer,
                index_buffer,
                index_count: indices.len() as u32,
                index_format: wgpu::IndexFormat::Uint32,
            }],
            material_bind_groups: vec![self.checker_material.clone()],
            surfaces: vec![Surface::default()],
            part_bounds: vec![(glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5))],
            bounds: (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5)),
        }
    }

    /// Load a glTF from the pack: `name` is its pack path, `files` its external
    /// buffers and images (`packed_gltf::external_files`).
    pub fn load_gltf_packed(
        &self,
        name: &str,
        bytes: &[u8],
        files: &std::collections::HashMap<String, Vec<u8>>,
    ) -> Result<LoadedModel> {
        let (document, buffers, images) = packed_gltf::import_with(name, bytes, files)?;
        self.build_model(&document, &buffers, &images)
    }

    /// The procedural unit cube with the checkerboard (`builtin:cube`).
    pub fn builtin_cube_model(&self) -> LoadedModel {
        LoadedModel {
            meshes: vec![self.cube_mesh()],
            material_bind_groups: vec![self.checker_material.clone()],
            surfaces: vec![Surface::default()],
            part_bounds: vec![(glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5))],
            bounds: (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5)),
        }
    }

    fn build_model(
        &self,
        document: &gltf::Document,
        buffers: &[gltf::buffer::Data],
        images: &[gltf::image::Data],
    ) -> Result<LoadedModel> {
        let mut model = LoadedModel {
            meshes: Vec::new(),
            material_bind_groups: Vec::new(),
            surfaces: Vec::new(),
            part_bounds: Vec::new(),
            bounds: (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN)),
        };
        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next())
            .context("glTF file has no scene")?;
        for node in scene.nodes() {
            self.add_node(&node, glam::Mat4::IDENTITY, buffers, images, &mut model)?;
        }
        if model.meshes.is_empty() {
            anyhow::bail!("glTF scene contains no meshes");
        }
        Ok(model)
    }

    fn add_node(
        &self,
        node: &gltf::Node,
        parent: glam::Mat4,
        buffers: &[gltf::buffer::Data],
        images: &[gltf::image::Data],
        model: &mut LoadedModel,
    ) -> Result<()> {
        let world = parent * glam::Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                self.add_primitive(&primitive, world, buffers, images, model)?;
            }
        }
        for child in node.children() {
            self.add_node(&child, world, buffers, images, model)?;
        }
        Ok(())
    }

    fn add_primitive(
        &self,
        primitive: &gltf::Primitive,
        world: glam::Mat4,
        buffers: &[gltf::buffer::Data],
        images: &[gltf::image::Data],
        model: &mut LoadedModel,
    ) -> Result<()> {
        let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|d| d.as_ref()));

        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .map(|it| it.collect())
            .unwrap_or_default();
        if positions.is_empty() {
            anyhow::bail!("primitive {} has no positions", primitive.index());
        }
        let normals: Vec<[f32; 3]> = reader
            .read_normals()
            .map(|it| it.collect())
            .unwrap_or_else(|| vec![[0.0, 0.0, 1.0]; positions.len()]);
        let uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .map(|it| it.into_f32().collect())
            .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);
        if positions.len() != normals.len() || positions.len() != uvs.len() {
            anyhow::bail!(
                "primitive {} attribute length mismatch: {} positions, {} normals, {} uvs",
                primitive.index(),
                positions.len(),
                normals.len(),
                uvs.len()
            );
        }

        let tangents: Option<Vec<[f32; 4]>> = reader
            .read_tangents()
            .map(|it| it.collect())
            .filter(|t: &Vec<[f32; 4]>| t.len() == positions.len());

        // Bake the node transform; normals use the inverse transpose,
        // tangents the transform itself. A mirroring transform flips the
        // bitangent, so the handedness flips with it.
        let normal_matrix = world.inverse().transpose();
        let mirrored = world.determinant() < 0.0;
        let mut vertices: Vec<Vertex> = positions
            .iter()
            .zip(&normals)
            .zip(&uvs)
            .enumerate()
            .map(|(i, ((&pos, &norm), &uv))| {
                let p = world.transform_point3(glam::Vec3::from_array(pos));
                model.bounds.0 = model.bounds.0.min(p);
                model.bounds.1 = model.bounds.1.max(p);
                let n = normal_matrix
                    .transform_vector3(glam::Vec3::from_array(norm))
                    .normalize_or_zero();
                let tangent = tangents.as_ref().map_or([0.0; 4], |t| {
                    let [x, y, z, w] = t[i];
                    let t = world
                        .transform_vector3(glam::Vec3::new(x, y, z))
                        .normalize_or_zero();
                    t.extend(if mirrored { -w } else { w }).to_array()
                });
                Vertex {
                    position: p.to_array(),
                    normal: n.to_array(),
                    uv,
                    tangent,
                }
            })
            .collect();

        let mut indices: Vec<u32> = match reader.read_indices() {
            Some(indices) => indices.into_u32().collect(),
            None => (0..positions.len() as u32).collect(),
        };
        // Generated from the baked vertices, before any winding flip: the
        // handedness comes from the UV layout, not the winding.
        if tangents.is_none() {
            generate_tangents(&mut vertices, &indices);
        }
        // A mirroring transform flips triangle winding; flip it back so back-face
        // culling keeps the outside.
        if mirrored {
            for tri in indices.chunks_exact_mut(3) {
                tri.swap(1, 2);
            }
        }

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gltf vertex buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gltf index buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
        for v in &vertices {
            lo = lo.min(glam::Vec3::from_array(v.position));
            hi = hi.max(glam::Vec3::from_array(v.position));
        }
        model.part_bounds.push((lo, hi));
        model.meshes.push(Mesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            index_format: wgpu::IndexFormat::Uint32,
        });

        // ponytail: every texture reference uploads its image again, no mips;
        // cache per image and generate mips when models share textures.
        let material = primitive.material();
        let pbr = material.pbr_metallic_roughness();
        let upload = |texture: Option<gltf::Texture>, srgb: bool| {
            let data = images.get(texture?.source()?.index())?;
            let rgba = image_from_gltf_data(data).to_rgba8();
            let format = if srgb {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            };
            Some(self.create_texture_from_bytes(&rgba, rgba.width(), rgba.height(), format))
        };
        let textures = [
            upload(pbr.base_color_texture().map(|t| t.texture()), true),
            upload(pbr.metallic_roughness_texture().map(|t| t.texture()), false),
            upload(material.normal_texture().map(|t| t.texture()), false),
            upload(material.emissive_texture().map(|t| t.texture()), true),
        ];
        model
            .material_bind_groups
            .push(self.create_material_bind_group(
                &MaterialUniform::from_gltf(&material),
                [0, 1, 2, 3].map(|i| textures[i].as_ref()),
            ));
        model.surfaces.push(Surface::from_gltf(&material));
        Ok(())
    }
}

impl Renderer {
    /// Acquire the surface texture and begin a new frame.
    /// Returns `None` if the surface is lost/outdated/occluded.
    pub fn begin_frame(&mut self) -> Option<Frame> {
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                drop(t);
                self.configure_surface();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.configure_surface();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .expect("failed to recreate lost surface");
                self.configure_surface();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return None;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                tracing::error!("wgpu surface validation error during get_current_texture");
                return None;
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.surface_format.add_srgb_suffix()),
                ..Default::default()
            });

        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        Some(Frame {
            surface_texture,
            view,
            encoder,
        })
    }
}

impl SceneRenderer {
    /// Draw 3D meshes into an existing render pass.
    /// Each draw call is `(mesh, instance range)`, the range indexing the
    /// records of `update_instances`. Every mesh gets the checkerboard
    /// material, without ambient occlusion.
    pub fn draw_meshes(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        draw_calls: &[(&Mesh, std::ops::Range<u32>)],
    ) {
        pass.set_pipeline(&self.pipelines().mesh[0]);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_bind_group(1, &self.instance_bind_group, &[]);
        pass.set_bind_group(2, &self.checker_material, &[]);
        pass.set_bind_group(3, &self.unoccluded_lighting, &[]);
        for (mesh, instances) in draw_calls {
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
        }
    }

    /// Draw 3D meshes with per-draw-call materials.
    /// Each draw call is `(mesh, material_bind_group, instance range)`;
    /// `lighting` is the view's group 3 (per-view lighting inputs). Diffuse
    /// ambient comes from the GI mode of the last `render_scene`.
    pub fn draw_meshes_textured(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        draw_calls: &[(&Mesh, &wgpu::BindGroup, std::ops::Range<u32>)],
        lighting: &wgpu::BindGroup,
    ) {
        let mesh = &self.pipelines().mesh;
        pass.set_pipeline(match self.gi.mode {
            melon_core::GiMode::Off => &mesh[0],
            melon_core::GiMode::Ssgi => &mesh[1],
            melon_core::GiMode::Ddgi => &mesh[2],
        });
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_bind_group(1, &self.instance_bind_group, &[]);
        pass.set_bind_group(3, lighting, &[]);
        for (mesh, material, instances) in draw_calls {
            pass.set_bind_group(2, *material, &[]);
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
        }
    }

    /// Builds the double-sided / blend mesh pipeline for the current sample
    /// count and GI mode if it does not exist yet.
    fn ensure_variant(&mut self, variant: Variant) {
        let key = (msaa_slot(self.msaa), self.gi.mode as usize, variant);
        if self.variants.contains_key(&key) {
            return;
        }
        let layouts = [
            &self.frame_layout,
            &self.model_bind_group_layout,
            &self.material_bind_group_layout,
            &self.lighting_layout,
        ];
        let pipeline = Renderer::build_mesh_pipeline(
            &self.device,
            &layouts,
            self.gi.mode,
            self.msaa,
            Some(variant),
        );
        self.variants.insert(key, pipeline);
    }

    /// `draw_meshes_textured` with a variant pipeline
    /// (`ensure_variant` first).
    fn draw_meshes_variant(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        draw_calls: &[(&Mesh, &wgpu::BindGroup, std::ops::Range<u32>)],
        lighting: &wgpu::BindGroup,
        variant: Variant,
    ) {
        let key = (msaa_slot(self.msaa), self.gi.mode as usize, variant);
        let pipeline = self.variants.get(&key).expect("ensure_variant built it");
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_bind_group(1, &self.instance_bind_group, &[]);
        pass.set_bind_group(3, lighting, &[]);
        for (mesh, material, instances) in draw_calls {
            pass.set_bind_group(2, *material, &[]);
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, instances.clone());
        }
    }

    /// Sky into the 3D pass, after the meshes and before the world lines
    /// (lines write no depth, so the sky would cover them).
    pub fn draw_sky(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipelines().sky);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Depth-tested lines in the 3D (HDR, multisampled) pass: grid, axes,
    /// selection box. Hidden behind geometry.
    pub fn draw_world_lines(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        vertices: &[LineVertex],
        view_proj: glam::Mat4,
    ) {
        if vertices.is_empty() {
            return;
        }
        self.write_line_camera(view_proj);
        let (buffer, len) = self.world_lines.upload(&self.device, &self.queue, vertices);
        let pipeline = &self.msaa_pipelines[msaa_slot(self.msaa)]
            .as_ref()
            .expect("set_msaa builds the pipelines")
            .world_lines;
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..len));
        pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Writes the shared frame camera buffer. Every write lands before the
    /// frame is submitted, so all passes see the last value: callers must pass
    /// this frame's camera `view_proj` (they all do), never a special one.
    /// Only `view_proj` is written, so the eye and inverse from
    /// `update_camera` stay.
    fn write_line_camera(&self, view_proj: glam::Mat4) {
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&view_proj.to_cols_array()),
        );
    }
}

impl Renderer {
    /// Submit the frame's command encoder and present the surface texture.
    /// Calls `device.poll` after submit to force wgpu to GC completed buffers
    /// (workaround for wgpu #5397 — short-lived buffers leak without poll).
    pub fn end_frame(&self, frame: Frame) {
        self.queue.submit([frame.encoder.finish()]);
        self.device.poll(wgpu::PollType::Poll).ok();
        self.window.pre_present_notify();
        frame.surface_texture.present();
    }
}

pub struct ViewportTarget {
    color_texture: Arc<wgpu::Texture>,
    color_view: Arc<wgpu::TextureView>,
    ui_view: wgpu::TextureView,
    ui_view_format: wgpu::TextureFormat,
    _depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    hdr_target: HdrTarget,
    ssao: SsaoTargets,
    /// `None` when the view is too small for one bloom level.
    bloom: Option<BloomTargets>,
    lighting: ViewLighting,
    overlay: overlay::OverlayTargets,
    size: winit::dpi::PhysicalSize<u32>,
}

/// One view's bloom chain: an `HDR_FORMAT` texture at half resolution with
/// one mip per level (`bloom_mip_sizes`), and the bind groups reading it.
struct BloomTargets {
    /// Render target view of each mip.
    mip_views: Vec<wgpu::TextureView>,
    /// Bloom source: the view's resolved HDR image (first downsample).
    hdr_source: wgpu::BindGroup,
    /// Bloom source: mip i.
    mip_sources: Vec<wgpu::BindGroup>,
    /// Tonemap group 0: the HDR image, mip 0 and `settings`.
    tonemap_bind_group: wgpu::BindGroup,
    /// Tonemap bloom uniform (intensity, 1 / levels), written per frame.
    settings: wgpu::Buffer,
}

/// One view's lighting inputs: its GI resources (SSGI textures and
/// history) and group 3 of the mesh pipeline with them.
struct ViewLighting {
    gi: gi::GiView,
    /// Group 3 with the view's AO texture.
    with_ao: wgpu::BindGroup,
    /// Group 3 with a white AO texture (AO off).
    without_ao: wgpu::BindGroup,
}

/// One view's SSAO targets: the single-sample depth/normal prepass, the raw
/// and blurred AO (full resolution), and the bind groups reading them.
struct SsaoTargets {
    depth_view: wgpu::TextureView,
    normal_view: wgpu::TextureView,
    raw_view: wgpu::TextureView,
    ao_texture: wgpu::Texture,
    ao_view: wgpu::TextureView,
    ssao_bind_group: wgpu::BindGroup,
    blur_bind_group: wgpu::BindGroup,
}

impl ViewportTarget {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: winit::dpi::PhysicalSize<u32>,
        hdr_target: HdrTarget,
        ssao: SsaoTargets,
        bloom: Option<BloomTargets>,
        lighting: ViewLighting,
    ) -> Self {
        let samples = hdr_target.sample_count();
        let size = winit::dpi::PhysicalSize::new(size.width.max(1), size.height.max(1));
        let color_texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("editor viewport color texture"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[format.remove_srgb_suffix()],
        }));
        let color_view =
            Arc::new(color_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let ui_view_format = format.remove_srgb_suffix();
        let ui_view = color_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("editor viewport ui view"),
            format: Some(ui_view_format),
            ..Default::default()
        });
        let (depth_texture, depth_view) = Renderer::create_depth_target(device, size, samples);
        let overlay = overlay::OverlayTargets::new(device, size, samples);

        Self {
            color_texture,
            color_view,
            ui_view,
            ui_view_format,
            _depth_texture: depth_texture,
            depth_view,
            hdr_target,
            ssao,
            bloom,
            lighting,
            overlay,
            size,
        }
    }

    /// The blurred SSAO term (`R8Unorm`, 1 = unoccluded) of the last
    /// `render_scene`; not written while `Environment::ao_strength` is 0.
    pub fn ao_texture(&self) -> &wgpu::Texture {
        &self.ssao.ao_texture
    }

    /// Scene colour target the 3D pass renders into before tonemapping.
    pub fn hdr_target(&self) -> &HdrTarget {
        &self.hdr_target
    }

    /// MSAA sample count of the 3D pass targets (colour and depth).
    pub fn sample_count(&self) -> u32 {
        self.hdr_target.sample_count()
    }

    /// The resolution of the view's SSGI textures; `None` = it holds none
    /// (only while it runs SSGI).
    pub fn ssgi_resolution(&self) -> Option<melon_core::SsgiResolution> {
        self.lighting.gi.ssgi.as_ref().map(|v| v.resolution)
    }

    pub fn color_texture(&self) -> Arc<wgpu::Texture> {
        self.color_texture.clone()
    }

    pub fn color_texture_handle(&self) -> wgpu::Texture {
        self.color_texture.as_ref().clone()
    }

    pub fn color_view(&self) -> Arc<wgpu::TextureView> {
        self.color_view.clone()
    }

    /// Non-sRGB view of the colour texture for UI libraries that treat texture
    /// samples as gamma-encoded (egui). Sampling the sRGB view would decode
    /// the image twice and darken it.
    pub fn ui_view(&self) -> &wgpu::TextureView {
        &self.ui_view
    }

    pub fn ui_view_format(&self) -> wgpu::TextureFormat {
        self.ui_view_format
    }

    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }

    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }
}

/// A frame in progress: holds the surface texture, its view, and a command encoder.
/// The host can insert additional render passes (e.g. for egui) between
/// `begin_frame` and `end_frame`.
pub struct Frame {
    surface_texture: wgpu::SurfaceTexture,
    pub view: wgpu::TextureView,
    pub encoder: wgpu::CommandEncoder,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId};

    #[test]
    fn vertex_size_is_48_bytes() {
        assert_eq!(std::mem::size_of::<Vertex>(), 48);
    }

    #[test]
    fn present_mode_prefers_mailbox_unless_fifo_is_forced() {
        use melon_core::PresentPreference::{MailboxCapped, Vsync};
        use wgpu::PresentMode::{Fifo, Immediate, Mailbox};
        let capped =
            |available: &[_], forced| choose_present_mode(MailboxCapped, available, forced);
        assert_eq!(capped(&[Fifo, Mailbox], None), Mailbox);
        assert_eq!(capped(&[Fifo, Immediate], None), Fifo);
        assert_eq!(capped(&[Fifo, Mailbox], Some("fifo")), Fifo);
        assert_eq!(capped(&[Fifo, Mailbox], Some("other")), Mailbox);
        assert_eq!(choose_present_mode(Vsync, &[Fifo, Mailbox], None), Fifo);
    }

    #[test]
    fn msaa_falls_back_to_the_nearest_lower_supported_count() {
        assert_eq!(effective_msaa(4, &[1, 2, 4]), 4);
        assert_eq!(effective_msaa(2, &[1, 4]), 1);
        assert_eq!(effective_msaa(8, &[1, 2, 4]), 4);
        assert_eq!(effective_msaa(3, &[1, 2, 4]), 2);
        assert_eq!(effective_msaa(0, &[1, 4]), 1);
    }

    #[test]
    fn msaa_pipeline_cache_has_one_slot_per_sample_count() {
        let slots = [1, 2, 4].map(msaa_slot);
        assert_eq!(slots, [0, 1, 2]);
        assert!(slots.iter().all(|&s| s < MSAA_SLOTS));
    }

    #[test]
    fn backend_order_prefers_dx12_on_windows_unless_forced() {
        use wgpu::Backends;
        let all = Backends::default();
        assert_eq!(backend_order(true, None), [Backends::DX12, all]);
        assert_eq!(backend_order(false, None), [all]);
        assert_eq!(backend_order(true, Some("auto")), [Backends::DX12, all]);
        assert_eq!(backend_order(true, Some("vulkan")), [Backends::VULKAN]);
        assert_eq!(backend_order(false, Some("dx12")), [Backends::DX12]);
        assert_eq!(backend_order(true, Some("bogus")), [Backends::DX12, all]);
        assert_eq!(backend_order(false, Some("bogus")), [all]);
    }

    /// Unit quad in the XY plane facing +Z, UVs given per corner
    /// (corners: (0,0), (1,0), (1,1), (0,1) in XY).
    fn quad(uvs: [[f32; 2]; 4]) -> Vec<Vertex> {
        [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
            .iter()
            .zip(uvs)
            .map(|(&[x, y], uv)| Vertex {
                position: [x, y, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv,
                tangent: [0.0; 4],
            })
            .collect()
    }

    #[test]
    fn tangents_follow_u_with_positive_handedness() {
        let mut v = quad([[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
        generate_tangents(&mut v, &[0, 1, 2, 0, 2, 3]);
        for t in v.iter().map(|v| v.tangent) {
            assert!(
                (glam::Vec4::from_array(t) - glam::Vec4::new(1.0, 0.0, 0.0, 1.0)).length() < 1e-5,
                "{t:?}"
            );
        }
    }

    #[test]
    fn tangents_of_mirrored_uvs_have_negative_handedness() {
        // U still runs along +X, V is flipped: the bitangent points the other way.
        let mut v = quad([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        generate_tangents(&mut v, &[0, 1, 2, 0, 2, 3]);
        for t in v.iter().map(|v| v.tangent) {
            assert!(
                (glam::Vec4::from_array(t) - glam::Vec4::new(1.0, 0.0, 0.0, -1.0)).length() < 1e-5,
                "{t:?}"
            );
        }
    }

    #[test]
    fn material_uniform_is_48_bytes_16_aligned() {
        assert_eq!(std::mem::size_of::<MaterialUniform>(), 48);
        assert_eq!(std::mem::size_of::<MaterialUniform>() % 16, 0);
    }

    #[test]
    fn gltf_default_material_values() {
        // TWO_NODE_GLTF has no material: glTF's default material applies.
        let gltf = gltf::Gltf::from_slice(TWO_NODE_GLTF.as_bytes()).unwrap();
        let primitive = gltf
            .document
            .meshes()
            .next()
            .unwrap()
            .primitives()
            .next()
            .unwrap();
        let m = MaterialUniform::from_gltf(&primitive.material());
        assert_eq!(m.base_color, [1.0; 4]);
        assert_eq!((m.metallic, m.roughness, m.normal_scale), (1.0, 1.0, 1.0));
        assert_eq!(m.emissive, [0.0; 3]);
    }

    #[test]
    fn gltf_emissive_strength_scales_emissive() {
        let json = r#"{"asset":{"version":"2.0"},"extensionsUsed":["KHR_materials_emissive_strength"],
            "materials":[{"emissiveFactor":[1.0,0.5,0.125],
            "extensions":{"KHR_materials_emissive_strength":{"emissiveStrength":4.0}}}]}"#;
        let gltf = gltf::Gltf::from_slice(json.as_bytes()).unwrap();
        let m = MaterialUniform::from_gltf(&gltf.document.materials().next().unwrap());
        assert_eq!(m.emissive, [4.0, 2.0, 0.5]);
    }

    #[test]
    fn bloom_mip_sizes_halve_and_stop_above_one_pixel() {
        assert_eq!(
            bloom_mip_sizes(1279, 719),
            [
                (639, 359),
                (319, 179),
                (159, 89),
                (79, 44),
                (39, 22),
                (19, 11)
            ]
        );
        assert_eq!(bloom_mip_sizes(64, 64).len(), BLOOM_LEVELS);
        assert_eq!(bloom_mip_sizes(5, 3), [(2, 1)]);
        assert!(bloom_mip_sizes(1, 1).is_empty());
        assert!(bloom_mip_sizes(1, 1000).is_empty());
    }

    #[test]
    fn line_vertex_size_is_24_bytes() {
        assert_eq!(std::mem::size_of::<LineVertex>(), 24);
    }

    #[test]
    fn line_vertex_field_offsets() {
        let v = LineVertex {
            position: [1.0, 2.0, 3.0],
            color: [4.0, 5.0, 6.0],
        };
        let base = &v as *const _ as usize;
        let pos = &v.position as *const _ as usize - base;
        let col = &v.color as *const _ as usize - base;
        assert_eq!(pos, 0, "position should be at offset 0");
        assert_eq!(col, 12, "color should be at offset 12");
    }

    #[test]
    fn vertex_field_offsets() {
        let v = Vertex {
            position: [1.0, 2.0, 3.0],
            normal: [4.0, 5.0, 6.0],
            uv: [7.0, 8.0],
            tangent: [9.0, 10.0, 11.0, 12.0],
        };
        let base = &v as *const _ as usize;
        let pos = &v.position as *const _ as usize - base;
        let norm = &v.normal as *const _ as usize - base;
        let uv = &v.uv as *const _ as usize - base;
        let tangent = &v.tangent as *const _ as usize - base;
        assert_eq!(pos, 0, "position should be at offset 0");
        assert_eq!(norm, 12, "normal should be at offset 12");
        assert_eq!(uv, 24, "uv should be at offset 24");
        assert_eq!(tangent, 32, "tangent should be at offset 32");
    }

    #[test]
    fn camera_uniform_size_is_144() {
        assert_eq!(std::mem::size_of::<CameraUniform>(), 144);
    }

    #[test]
    fn environment_uniform_size_is_32() {
        assert_eq!(std::mem::size_of::<EnvironmentUniform>(), 32);
    }

    #[test]
    fn default_camera_view_proj_is_invertible() {
        // The sky rebuilds view rays from the inverse. Checked for the core
        // default and the editor's default pose (eye (0, 4, 8) on the origin).
        let editor = melon_core::Camera {
            eye: glam::Vec3::new(0.0, 4.0, 8.0),
            center: glam::Vec3::ZERO,
            ..Default::default()
        };
        for vp in [
            melon_core::Camera::default().view_proj(),
            editor.view_proj(),
        ] {
            assert!(vp.determinant().abs() > 1e-6, "{vp}");
            assert!((vp * vp.inverse()).abs_diff_eq(glam::Mat4::IDENTITY, 1e-4));
        }
    }

    #[test]
    fn instance_data_size_is_128() {
        assert_eq!(std::mem::size_of::<InstanceData>(), 128);
    }

    /// Camera at (0, 0, 5) looking down -Z, near 0.1, far 100.
    fn test_frustum() -> Frustum {
        use glam::camera::rh::{proj::directx, view};
        let view_proj = directx::perspective(60f32.to_radians(), 1.0, 0.1, 100.0)
            * view::look_at_mat4(
                glam::Vec3::new(0.0, 0.0, 5.0),
                glam::Vec3::ZERO,
                glam::Vec3::Y,
            );
        Frustum::from_view_proj(view_proj)
    }

    #[test]
    fn frustum_planes_keep_points_in_front_only() {
        let f = test_frustum();
        let inside = |p: glam::Vec3| f.intersects_aabb(p, p);
        assert!(inside(glam::Vec3::ZERO), "in front");
        assert!(inside(glam::Vec3::new(0.0, 0.0, -90.0)), "before far");
        assert!(!inside(glam::Vec3::new(0.0, 0.0, 6.0)), "behind the eye");
        assert!(!inside(glam::Vec3::new(0.0, 0.0, -96.0)), "beyond far");
        assert!(!inside(glam::Vec3::new(50.0, 0.0, 0.0)), "off to the right");
    }

    #[test]
    fn frustum_aabb_inside_outside_straddling() {
        let f = test_frustum();
        let v = glam::Vec3::splat;
        assert!(f.intersects_aabb(v(-0.5), v(0.5)), "inside");
        assert!(
            !f.intersects_aabb(
                glam::Vec3::new(40.0, -1.0, -1.0),
                glam::Vec3::new(42.0, 1.0, 1.0)
            ),
            "outside"
        );
        // Mostly right of the view, one end inside.
        assert!(
            f.intersects_aabb(
                glam::Vec3::new(1.0, -1.0, -1.0),
                glam::Vec3::new(40.0, 1.0, 1.0)
            ),
            "straddling the right plane"
        );
        // Through the eye: half behind it.
        assert!(
            f.intersects_aabb(
                glam::Vec3::new(-1.0, -1.0, 3.0),
                glam::Vec3::new(1.0, 1.0, 8.0)
            ),
            "straddling the near plane"
        );
    }

    #[test]
    fn world_aabb_follows_rotation_and_scale() {
        let m = glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::new(2.0, 1.0, 1.0),
            glam::Quat::from_rotation_y(90f32.to_radians()),
            glam::Vec3::new(10.0, 0.0, 0.0),
        );
        let (min, max) = world_aabb(m, (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5)));
        assert!(
            min.abs_diff_eq(glam::Vec3::new(9.5, -0.5, -1.0), 1e-5),
            "{min}"
        );
        assert!(
            max.abs_diff_eq(glam::Vec3::new(10.5, 0.5, 1.0), 1e-5),
            "{max}"
        );
    }

    #[test]
    fn batch_groups_keys_and_keeps_instance_order() {
        let mut out = vec![100];
        let batches = batch([(7, 1), (3, 2), (7, 3), (7, 4), (3, 5)], &mut out);
        assert_eq!(batches, vec![(7, 1..4), (3, 4..6)]);
        assert_eq!(out, vec![100, 1, 3, 4, 2, 5]);
    }

    #[test]
    fn instance_capacity_doubles_until_it_fits() {
        assert_eq!(grown_capacity(256, 256), 256);
        assert_eq!(grown_capacity(256, 257), 512);
        assert_eq!(grown_capacity(256, 3000), 4096);
        assert_eq!(grown_capacity(0, 3), 4);
    }

    #[test]
    fn light_uniform_size_is_432() {
        assert_eq!(std::mem::size_of::<LightUniform>(), 432);
    }

    #[test]
    fn ssao_uniform_size_is_272() {
        assert_eq!(std::mem::size_of::<SsaoUniform>(), 272);
    }

    #[test]
    fn ssao_kernel_lies_in_unit_hemisphere_and_is_deterministic() {
        let kernel = ssao_kernel();
        for k in kernel {
            let v = glam::Vec3::new(k[0], k[1], k[2]);
            assert!(v.z > 0.0 && v.length() < 1.0 && v.length() > 0.05, "{v}");
        }
        assert_eq!(kernel, ssao_kernel());
    }

    fn clip(m: glam::Mat4, p: glam::Vec3) -> glam::Vec3 {
        let c = m * p.extend(1.0);
        c.truncate() / c.w
    }

    #[test]
    fn light_view_proj_center_maps_to_middle() {
        let to_light = glam::Vec3::new(1.0, 1.0, 0.0).normalize();
        let c = clip(
            directional_light_view_proj(to_light, glam::Vec3::new(3.0, 0.0, -2.0), 20.0),
            glam::Vec3::new(3.0, 0.0, -2.0),
        );
        assert!(c.x.abs() < 1e-4 && c.y.abs() < 1e-4, "xy = {c:?}");
        assert!(c.z > 0.0 && c.z < 1.0, "z = {}", c.z);
    }

    #[test]
    fn light_view_proj_nearer_point_has_smaller_depth() {
        let to_light = glam::Vec3::new(1.0, 1.0, 0.0).normalize();
        let m = directional_light_view_proj(to_light, glam::Vec3::ZERO, 20.0);
        assert!(clip(m, to_light).z < clip(m, -to_light).z);
    }

    #[test]
    fn light_view_proj_vertical_light_is_finite() {
        let m = directional_light_view_proj(glam::Vec3::Y, glam::Vec3::ZERO, 20.0);
        assert!(m.to_cols_array().iter().all(|v| v.is_finite()), "{m:?}");
    }

    // --- Shared GPU test scene ---
    // Floor + small occluder seen from straight above, sun at 45 degrees from +X.
    // White light on white albedo, so every colour channel holds the same value.

    const TEST_SIZE: u32 = 64;
    /// Share of a shadow edge's contrast (percent) that default bloom must keep.
    const EDGE_KEEP_PERCENT: i32 = 85;
    /// World (-2, 0, 0): on the floor, inside the occluder's shadow.
    const SHADOW_PX: (u32, u32) = (16, 32);
    /// World (2, 0, 0): on the floor, in the open.
    const LIT_PX: (u32, u32) = (48, 32);
    /// World (-3.44, 0, 0): on the floor, in the open, left of the shadow.
    const FAR_LEFT_PX: (u32, u32) = (4, 32);

    /// The test scene's sun: 45 degrees from +X. White ambient from every
    /// side, so all colour channels match (`value_at` reads one).
    fn test_light() -> melon_core::Light {
        melon_core::Light {
            direction: -glam::Vec3::new(1.0, 1.0, 0.0).normalize(),
            sky_color: glam::Vec3::ONE,
            ground_color: glam::Vec3::ONE,
            ..Default::default()
        }
    }

    /// Renders the test scene and returns TEST_SIZE x TEST_SIZE RGBA8 sRGB pixels.
    /// The shadow box has half-size `shadow_radius` around `shadow_center`.
    /// `from_below` puts the camera under the floor, looking up at its underside.
    fn render_test_scene(
        r: &mut Renderer,
        light: &melon_core::Light,
        from_below: bool,
        shadow_center: glam::Vec3,
        shadow_radius: f32,
    ) -> Vec<u8> {
        render_test_scene_with(r, light, from_below, shadow_center, shadow_radius, None)
    }

    /// `render_test_scene` with the floor drawn in `floor` (default: white).
    fn render_test_scene_with(
        r: &mut Renderer,
        light: &melon_core::Light,
        from_below: bool,
        shadow_center: glam::Vec3,
        shadow_radius: f32,
        floor: Option<&wgpu::BindGroup>,
    ) -> Vec<u8> {
        use glam::camera::rh::{proj::directx, view};
        use glam::{Mat4, Quat, Vec3};

        let device = r.device().clone();
        let size = winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE);
        let color = test_color_texture(&device, r.surface_format().add_srgb_suffix());
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let (_depth, depth_view) = Renderer::create_depth_target(&device, size, r.msaa());

        let eye = Vec3::new(0.0, if from_below { -10.0 } else { 10.0 }, 0.0);
        r.update_camera(
            directx::orthographic(-4.0, 4.0, -4.0, 4.0, 0.1, 20.0)
                * view::look_at_mat4(eye, Vec3::ZERO, Vec3::NEG_Z),
            eye,
        );
        // No fog, so the lit/shadow thresholds keep their meaning.
        r.update_environment(&melon_core::Environment {
            fog_density: 0.0,
            gi_intensity: 0.0,
            ..Default::default()
        });
        r.update_light(
            light,
            directional_light_view_proj(light.to_light(), shadow_center, shadow_radius),
        );
        r.update_instances(&[
            Mat4::from_scale_rotation_translation(
                Vec3::new(8.0, 0.2, 8.0),
                Quat::IDENTITY,
                Vec3::new(0.0, -0.1, 0.0),
            ),
            Mat4::from_scale_rotation_translation(
                Vec3::splat(0.5),
                Quat::IDENTITY,
                Vec3::new(0.0, 2.0, 0.0),
            ),
        ]);
        // White rough dielectric: the checker factors on the default textures.
        let white_bg = r.create_material_bind_group(&CHECKER_MATERIAL, [None; 4]);
        let cube = r.cube_mesh();

        let hdr = r.create_hdr_target(size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        r.render_shadow_pass(&mut encoder, &[(&cube, 0..2)]);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: hdr.msaa_view(),
                    depth_slice: None,
                    resolve_target: Some(hdr.view()),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            r.draw_meshes_textured(
                &mut pass,
                &[
                    (&cube, floor.unwrap_or(&white_bg), 0..1),
                    (&cube, &white_bg, 1..2),
                ],
                &r.unoccluded_lighting,
            );
        }
        r.tonemap_pass(&mut encoder, &hdr, &color_view);
        read_back(&device, r.queue(), encoder, &color)
    }

    /// The test floor (seen from above) in a material with these factors and
    /// an optional normal map; returns the pixels.
    fn render_floor(
        r: &mut Renderer,
        light: &melon_core::Light,
        factors: MaterialUniform,
        normal_map: Option<&wgpu::Texture>,
    ) -> Vec<u8> {
        let material = r.create_material_bind_group(&factors, [None, None, normal_map, None]);
        render_test_scene_with(r, light, false, glam::Vec3::ZERO, 20.0, Some(&material))
    }

    /// The test light with the sun placed so the floor at LIT_PX mirrors it
    /// into the test camera (eye (0, 10, 0)): the half-vector there is the
    /// floor normal, the peak of every highlight.
    fn mirror_sun() -> melon_core::Light {
        melon_core::Light {
            direction: -glam::Vec3::new(2.0, 10.0, 0.0).normalize(),
            ..test_light()
        }
    }

    /// The brightest pixel (RGBA) around LIT_PX: with `mirror_sun`, the
    /// highlight's peak.
    fn highlight_peak(renderer: &Renderer, pixels: &[u8]) -> [u8; 4] {
        (LIT_PX.0 - 3..=LIT_PX.0 + 3)
            .flat_map(|x| (LIT_PX.1 - 3..=LIT_PX.1 + 3).map(move |y| (x, y)))
            .map(|px| pixel_at(renderer, pixels, px))
            .max_by_key(|p| p[..3].iter().map(|&c| u32::from(c)).sum::<u32>())
            .unwrap_or_default()
    }

    /// Renders `models` through `render_scene` from `camera` and returns the
    /// TEST_SIZE x TEST_SIZE display pixels.
    fn render_frame(
        r: &mut Renderer,
        camera: melon_core::Camera,
        light: melon_core::Light,
        environment: melon_core::Environment,
        models: Vec<(glam::Mat4, melon_core::Handle<melon_core::Model>)>,
        server: &mut melon_core::AssetServer,
    ) -> Vec<u8> {
        render_frame_stats(r, camera, light, environment, models, server).0
    }

    /// `render_frame`, plus what `render_scene` drew.
    fn render_frame_stats(
        r: &mut Renderer,
        camera: melon_core::Camera,
        light: melon_core::Light,
        environment: melon_core::Environment,
        models: Vec<(glam::Mat4, melon_core::Handle<melon_core::Model>)>,
        server: &mut melon_core::AssetServer,
    ) -> (Vec<u8>, RenderStats) {
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let frame = FrameData {
            view_proj: camera.view_proj(),
            eye: camera.eye,
            light,
            environment,
            quality: melon_core::RenderQuality::default(),
            models,
            overrides: Default::default(),
            lights: Default::default(),
        };
        let mut encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let stats = r.render_scene(&mut encoder, &frame, server, &mut target, &[], &[]);
        (
            read_back(r.device(), r.queue(), encoder, &target.color_texture),
            stats,
        )
    }

    /// Looking straight down at the origin from 10 m (sees about +-4 m).
    fn top_down_camera() -> melon_core::Camera {
        melon_core::Camera {
            eye: glam::Vec3::new(0.0, 10.0, 0.0),
            center: glam::Vec3::ZERO,
            up: glam::Vec3::NEG_Z,
            aspect: 1.0,
            ..Default::default()
        }
    }

    /// No fog, AO, bloom or GI: thresholds keep their meaning.
    fn plain_environment() -> melon_core::Environment {
        melon_core::Environment {
            fog_density: 0.0,
            ao_strength: 0.0,
            bloom_intensity: 0.0,
            gi_intensity: 0.0,
            ..Default::default()
        }
    }

    /// Frames rendered before reading a GI result, so the probes converge
    /// (a running mean, then hysteresis 0.97).
    const GI_FRAMES: u32 = 60;

    /// `render_frame` into one target, `GI_FRAMES` times when GI is on (once
    /// when off), in the default GI mode (DDGI); returns the last frame's
    /// pixels.
    fn render_converged(
        r: &mut Renderer,
        camera: melon_core::Camera,
        light: melon_core::Light,
        environment: melon_core::Environment,
        models: &[(glam::Mat4, melon_core::Handle<melon_core::Model>)],
        server: &mut melon_core::AssetServer,
    ) -> Vec<u8> {
        let quality = melon_core::RenderQuality::default();
        render_converged_in(r, camera, light, (environment, quality), models, server)
    }

    /// `render_converged` with these quality settings.
    fn render_converged_in(
        r: &mut Renderer,
        camera: melon_core::Camera,
        light: melon_core::Light,
        settings: (melon_core::Environment, melon_core::RenderQuality),
        models: &[(glam::Mat4, melon_core::Handle<melon_core::Model>)],
        server: &mut melon_core::AssetServer,
    ) -> Vec<u8> {
        let frames = if settings.0.gi_intensity > 0.0 {
            GI_FRAMES
        } else {
            1
        };
        let sequence = vec![settings; frames as usize];
        render_sequence(r, camera, light, &sequence, models, server)
    }

    /// The default quality in GI mode `mode`.
    fn quality_in(mode: melon_core::GiMode) -> melon_core::RenderQuality {
        melon_core::RenderQuality {
            gi_mode: mode,
            ..Default::default()
        }
    }

    /// One `render_scene` per (environment, quality), consecutive frames
    /// into one target; returns the last frame's pixels.
    fn render_sequence(
        r: &mut Renderer,
        camera: melon_core::Camera,
        light: melon_core::Light,
        frames: &[(melon_core::Environment, melon_core::RenderQuality)],
        models: &[(glam::Mat4, melon_core::Handle<melon_core::Model>)],
        server: &mut melon_core::AssetServer,
    ) -> Vec<u8> {
        // Handle ids restart with every AssetServer: drop models cached for
        // another test's server.
        r.model_cache = ModelCache::new(&r.scene);
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        for (i, &(environment, quality)) in frames.iter().enumerate() {
            let frame = FrameData {
                view_proj: camera.view_proj(),
                eye: camera.eye,
                light,
                environment,
                quality,
                models: models.to_vec(),
                overrides: Default::default(),
                lights: Default::default(),
            };
            let mut encoder = r
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            r.render_scene(&mut encoder, &frame, server, &mut target, &[], &[]);
            if i + 1 == frames.len() {
                return read_back(r.device(), r.queue(), encoder, &target.color_texture);
            }
            r.queue().submit([encoder.finish()]);
        }
        panic!("render_sequence needs at least one environment")
    }

    /// What one frame of `render_qualities` left: its pixels, the target's
    /// MSAA sample count and the resolution of its SSGI textures (`None`:
    /// none).
    struct QualityFrame {
        pixels: Vec<u8>,
        samples: u32,
        ssgi: Option<melon_core::SsgiResolution>,
    }

    /// One `render_scene` per quality into one target, each read back.
    /// Fails on any wgpu validation error.
    fn render_qualities(
        r: &mut Renderer,
        camera: melon_core::Camera,
        light: melon_core::Light,
        environment: melon_core::Environment,
        qualities: &[melon_core::RenderQuality],
        models: &[(glam::Mat4, melon_core::Handle<melon_core::Model>)],
        server: &mut melon_core::AssetServer,
    ) -> Result<Vec<QualityFrame>, String> {
        r.model_cache = ModelCache::new(&r.scene);
        let scope = r.device().push_error_scope(wgpu::ErrorFilter::Validation);
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let mut frames = Vec::new();
        for &quality in qualities {
            let frame = FrameData {
                view_proj: camera.view_proj(),
                eye: camera.eye,
                light,
                environment,
                quality,
                models: models.to_vec(),
                overrides: Default::default(),
                lights: Default::default(),
            };
            let mut encoder = r
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            r.render_scene(&mut encoder, &frame, server, &mut target, &[], &[]);
            frames.push(QualityFrame {
                pixels: read_back(r.device(), r.queue(), encoder, &target.color_texture),
                samples: target.sample_count(),
                ssgi: target.ssgi_resolution(),
            });
        }
        match pollster::block_on(scope.pop()) {
            Some(e) => Err(format!("wgpu validation error: {e}")),
            None => Ok(frames),
        }
    }

    /// Pixels that differ between two readbacks.
    fn differing(a: &[u8], b: &[u8]) -> usize {
        a.chunks(4).zip(b.chunks(4)).filter(|(a, b)| a != b).count()
    }

    /// `plain_environment` with GI at full strength.
    fn gi_environment() -> melon_core::Environment {
        melon_core::Environment {
            gi_intensity: 1.0,
            ..plain_environment()
        }
    }

    /// A box of `size` centred at `center` in model `model`.
    fn slab(
        model: melon_core::Handle<melon_core::Model>,
        size: glam::Vec3,
        center: glam::Vec3,
    ) -> (glam::Mat4, melon_core::Handle<melon_core::Model>) {
        (
            glam::Mat4::from_scale_rotation_translation(size, glam::Quat::IDENTITY, center),
            model,
        )
    }

    /// The GI test room: interior 5 x 4 x 5 m around the origin (x and z in
    /// -2.5..2.5), 0.4 m walls, red at +x, green at -x, grey back wall
    /// (+z), ceiling and floor, open toward -z where the default sun comes in.
    fn test_room(
        server: &mut melon_core::AssetServer,
    ) -> Vec<(glam::Mat4, melon_core::Handle<melon_core::Model>)> {
        use glam::Vec3;
        let grey = server.load("assets/models/review/grey_cube.glb");
        let red = server.load("assets/models/review/red_panel.glb");
        let green = server.load("assets/models/review/green_panel.glb");
        let o = TEST_ROOM_OFFSET;
        vec![
            slab(
                grey,
                Vec3::new(5.8, 0.1, 7.4),
                Vec3::new(0.0, -0.05, -0.8) + o,
            ),
            // Walls end under the ceiling and at the back wall: overlapping
            // slabs share face planes, which z-fight (striped edges).
            slab(red, Vec3::new(0.4, 4.0, 5.0), Vec3::new(2.7, 2.0, 0.0) + o),
            slab(
                green,
                Vec3::new(0.4, 4.0, 5.0),
                Vec3::new(-2.7, 2.0, 0.0) + o,
            ),
            slab(grey, Vec3::new(5.8, 4.0, 0.4), Vec3::new(0.0, 2.0, 2.7) + o),
            slab(grey, Vec3::new(5.8, 0.4, 5.4), Vec3::new(0.0, 4.2, 0.2) + o),
        ]
    }

    /// Shift of the test room: it then sits on the probe and voxel grids
    /// exactly like the snapshot `cornell` room (inner faces 0.4 voxel into
    /// a voxel, probes 0.35 m outside the side walls), where bright corner
    /// rims showed. On voxel boundaries the faces would hide them.
    const TEST_ROOM_OFFSET: glam::Vec3 = glam::Vec3::new(-0.5, 0.0, -0.5);

    /// The test room's light: the default sun with a dim hemisphere, so
    /// bounce dominates the shade.
    fn test_room_light() -> melon_core::Light {
        melon_core::Light {
            sky_color: glam::Vec3::splat(0.1),
            ground_color: glam::Vec3::splat(0.1),
            ..Default::default()
        }
    }

    /// Asset server at the repository root (review models).
    fn repo_server() -> melon_core::AssetServer {
        melon_core::AssetServer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..").into())
    }

    /// Sum of the RGB channels of a pixel.
    fn rgb_sum(p: [u8; 4]) -> u32 {
        p[..3].iter().map(|&c| u32::from(c)).sum()
    }

    /// Pixel of a TEST_SIZE target that world point `p` projects to.
    fn screen_px(view_proj: glam::Mat4, p: glam::Vec3) -> (u32, u32) {
        let ndc = view_proj.project_point3(p);
        let size = TEST_SIZE as f32;
        (
            ((ndc.x * 0.5 + 0.5) * size) as u32,
            ((0.5 - ndc.y * 0.5) * size) as u32,
        )
    }

    /// A 1 m `builtin:cube` on a floor, seen from (3, 2.5, 3), lit by the
    /// white hemisphere only (no sun), so the image is all ambient. Returns
    /// the RGB sums at the floor next to the cube's +X face and at an open
    /// floor point behind the cube's silhouette, more than 2 * `ao_radius`
    /// from it (sample rays there cross the cube).
    fn ao_scene_pixels(r: &mut Renderer, ao_strength: f32) -> (u32, u32) {
        use glam::{Mat4, Quat, Vec3};
        use melon_core::{AssetServer, Handle, Model};
        let mut server = AssetServer::new(".".into());
        let cube: Handle<Model> = server.load("builtin:cube");
        let floor = Mat4::from_scale_rotation_translation(
            Vec3::new(8.0, 0.2, 8.0),
            Quat::IDENTITY,
            Vec3::new(0.0, -0.1, 0.0),
        );
        let camera = melon_core::Camera {
            eye: Vec3::new(3.0, 2.5, 3.0),
            center: Vec3::ZERO,
            aspect: 1.0,
            ..Default::default()
        };
        // Grey sky and ground keep the floor in the tonemap's steep midtones.
        let light = melon_core::Light {
            ambient: 1.0,
            sky_color: glam::Vec3::splat(0.35),
            ground_color: glam::Vec3::splat(0.35),
            ..test_light()
        };
        let env = melon_core::Environment {
            fog_density: 0.0,
            ao_strength,
            bloom_intensity: 0.0,
            gi_intensity: 0.0,
            ..Default::default()
        };
        let models = vec![(floor, cube), (Mat4::from_translation(Vec3::Y * 0.5), cube)];
        let px = render_frame(r, camera, light, env, models, &mut server);
        let rgb = |p: Vec3| {
            pixel_at(r, &px, screen_px(camera.view_proj(), p))[..3]
                .iter()
                .map(|&c| u32::from(c))
                .sum()
        };
        (
            rgb(Vec3::new(0.55, 0.0, 0.0)),
            rgb(Vec3::new(-1.8, 0.0, -1.8)),
        )
    }

    /// Clears an HDR target to `value` (all channels), tonemaps it to sRGB8
    /// and returns the centre pixel.
    fn tonemap_value(r: &Renderer, value: f64) -> u8 {
        let device = r.device();
        let hdr = r.create_hdr_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let color = test_color_texture(device, r.surface_format().add_srgb_suffix());
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test hdr clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: hdr.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: value,
                        g: value,
                        b: value,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        r.tonemap_pass(&mut encoder, &hdr, &color_view);
        let pixels = read_back(device, r.queue(), encoder, &color);
        value_at(&pixels, (TEST_SIZE / 2, TEST_SIZE / 2))
    }

    fn test_color_texture(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test color texture"),
            size: wgpu::Extent3d {
                width: TEST_SIZE,
                height: TEST_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Submits `encoder` with a copy of `texture` appended and returns its
    /// tightly packed RGBA8 rows.
    fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mut encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) -> Vec<u8> {
        let row = texture.width() * 4;
        let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test readback buffer"),
            size: (padded * texture.height()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |res| {
            res.expect("readback map failed")
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("readback poll failed");
        let data = buffer.get_mapped_range(..);
        data.chunks(padded as usize)
            .flat_map(|r| &r[..row as usize])
            .copied()
            .collect()
    }

    /// First channel at pixel `(x, y)`; all channels match in the test scene.
    /// Draws one white world line along X through a row centre, with the
    /// depth buffer cleared to `depth_clear`, and returns the pixels.
    fn render_world_line(r: &mut Renderer, depth_clear: f32) -> Vec<u8> {
        use glam::Vec3;
        use glam::camera::rh::{proj::directx, view};

        let device = r.device().clone();
        let size = winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE);
        let color = test_color_texture(&device, r.surface_format().add_srgb_suffix());
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let (_depth, depth_view) = Renderer::create_depth_target(&device, size, r.msaa());
        let view_proj = directx::orthographic(-4.0, 4.0, -4.0, 4.0, 0.1, 20.0)
            * view::look_at_mat4(Vec3::new(0.0, 10.0, 0.0), Vec3::ZERO, Vec3::NEG_Z);
        let line = [
            LineVertex {
                position: [-4.0, 0.0, 0.0625],
                color: [1.0, 1.0, 1.0],
            },
            LineVertex {
                position: [4.0, 0.0, 0.0625],
                color: [1.0, 1.0, 1.0],
            },
        ];
        let hdr = r.create_hdr_target(size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world line test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: hdr.msaa_view(),
                    depth_slice: None,
                    resolve_target: Some(hdr.view()),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(depth_clear),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            r.draw_world_lines(&mut pass, &line, view_proj);
        }
        r.tonemap_pass(&mut encoder, &hdr, &color_view);
        read_back(&device, r.queue(), encoder, &color)
    }

    /// One triangle (0,0,0)-(1,0,0)-(0,1,0) used by two nodes at x = -2 and x = +3.
    const TWO_NODE_GLTF: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1]}],
        "nodes": [
            {"mesh": 0, "translation": [-2, 0, 0]},
            {"mesh": 0, "translation": [3, 0, 0]}
        ],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "buffers": [{"byteLength": 36,
            "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA"}],
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3,
            "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]}]
    }"#;

    #[test]
    fn gltf_nodes_reads_two_node_tree() {
        let nodes = gltf_nodes_from_slice(TWO_NODE_GLTF.as_bytes()).unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(
            nodes
                .iter()
                .all(|n| n.parent.is_none() && n.mesh == Some(0))
        );
        assert_eq!(nodes[0].translation, glam::Vec3::new(-2.0, 0.0, 0.0));
        assert_eq!(nodes[1].translation, glam::Vec3::new(3.0, 0.0, 0.0));
    }

    /// A quad facing the default camera (normal +Z), 2x2 m, centred at `z`.
    fn facing_quad(z: f32, color: [f32; 4], depth: OverlayDepth) -> OverlayDraw {
        OverlayDraw {
            shape: OverlayShape::Quad,
            transform: glam::Mat4::from_translation(glam::Vec3::new(-1.0, 1.0, z))
                * glam::Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2)
                * glam::Mat4::from_scale(glam::Vec3::splat(2.0)),
            color,
            depth,
        }
    }

    /// Render a `size` target with optional cube at the origin and the given
    /// overlay; returns the RGBA of the centre pixel.
    fn overlay_centre(
        renderer: &mut Renderer,
        size: (u32, u32),
        cube: bool,
        overlay: &[OverlayDraw],
    ) -> [u8; 4] {
        use melon_core::{AssetServer, Handle, Model};
        let mut server = AssetServer::new(".".into());
        let h: Handle<Model> = server.load("builtin:cube");
        let camera = melon_core::Camera::default();
        let frame = FrameData {
            view_proj: camera.view_proj(),
            eye: camera.eye,
            light: melon_core::Light::default(),
            environment: melon_core::Environment {
                ao_strength: 0.0,
                bloom_intensity: 0.0,
                gi_intensity: 0.0,
                ..Default::default()
            },
            quality: melon_core::RenderQuality::default(),
            models: if cube {
                vec![(glam::Mat4::IDENTITY, h)]
            } else {
                vec![]
            },
            overrides: Default::default(),
            lights: Default::default(),
        };
        let mut target =
            renderer.create_viewport_target(winit::dpi::PhysicalSize::new(size.0, size.1));
        let mut encoder = renderer
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        renderer.render_scene(&mut encoder, &frame, &mut server, &mut target, &[], overlay);
        if size != (TEST_SIZE, TEST_SIZE) {
            renderer.queue().submit([encoder.finish()]);
            return [0; 4];
        }
        let pixels = read_back(
            renderer.device(),
            renderer.queue(),
            encoder,
            &target.color_texture,
        );
        pixel_at(renderer, &pixels, (TEST_SIZE / 2, TEST_SIZE / 2))
    }

    /// RGBA at pixel `(x, y)` of a TEST_SIZE readback.
    fn pixel_at(renderer: &Renderer, pixels: &[u8], (x, y): (u32, u32)) -> [u8; 4] {
        let i = ((y * TEST_SIZE + x) * 4) as usize;
        let px = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
        // The display texture may be BGRA: report as RGBA.
        if renderer.surface_format().remove_srgb_suffix() == wgpu::TextureFormat::Bgra8Unorm {
            [px[2], px[1], px[0], px[3]]
        } else {
            px
        }
    }

    /// An `AssetServer` whose next handle ids are not used by the other GPU
    /// assertions: they share one renderer, and its `ModelCache` is keyed
    /// by handle id (one server per app, so ids are unique there).
    fn fresh_ids_server(root: std::path::PathBuf, offset: u32) -> melon_core::AssetServer {
        let mut server = melon_core::AssetServer::new(root);
        for i in 0..offset {
            let _: melon_core::Handle<melon_core::Model> = server.load(&format!("unused/{i}.glb"));
        }
        server
    }

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_models_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets/models")).unwrap();
        dir
    }

    fn value_at(pixels: &[u8], (x, y): (u32, u32)) -> u8 {
        pixels[((y * TEST_SIZE + x) * 4) as usize]
    }

    // --- GPU tests via headless event loop harness ---
    // winit allows only ONE EventLoop per process, so all GPU assertions run
    // inside a single test that creates one loop + one renderer.

    type GpuAssertion = Box<dyn FnOnce(&mut Renderer) -> Result<(), String> + Send>;

    struct GpuTestRunner {
        assertions: Vec<GpuAssertion>,
        results: Vec<Result<(), String>>,
        window: Option<Arc<Window>>,
        renderer: Option<Renderer>,
        done: bool,
    }

    impl GpuTestRunner {
        fn new(assertions: Vec<GpuAssertion>) -> Self {
            Self {
                assertions,
                results: Vec::new(),
                window: None,
                renderer: None,
                done: false,
            }
        }
    }

    impl ApplicationHandler for GpuTestRunner {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("melon_render test")
                            .with_visible(false),
                    )
                    .expect("failed to create test window"),
            );
            let renderer = match pollster::block_on(Renderer::new(window.clone())) {
                Ok(r) => r,
                Err(e) => {
                    self.results
                        .push(Err(format!("renderer init failed: {e:?}")));
                    self.done = true;
                    event_loop.exit();
                    return;
                }
            };
            self.window = Some(window);
            self.renderer = Some(renderer);
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            if let WindowEvent::CloseRequested = event {
                event_loop.exit();
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.done {
                event_loop.exit();
                return;
            }
            if let Some(mut renderer) = self.renderer.take() {
                while let Some(assertion) = self.assertions.pop() {
                    self.results.push(assertion(&mut renderer));
                }
                self.done = true;
            }
            event_loop.exit();
        }
    }

    #[test]
    fn thumbnail_camera_centres_and_contains_the_box() {
        let (min, max) = (
            glam::Vec3::new(-1.0, 0.0, -2.0),
            glam::Vec3::new(3.0, 2.0, 2.0),
        );
        let (view_proj, eye) = thumbnail_camera(min, max);
        let centre = view_proj.project_point3((min + max) / 2.0);
        assert!(centre.x.abs() < 1e-4 && centre.y.abs() < 1e-4, "{centre}");
        for i in 0..8 {
            let corner = glam::Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            );
            let p = view_proj.project_point3(corner);
            assert!(
                p.x.abs() <= 1.0 && p.y.abs() <= 1.0 && (0.0..=1.0).contains(&p.z),
                "{p}"
            );
        }
        assert!(
            eye.x > max.x && eye.y > max.y && eye.z > max.z,
            "three-quarter view from +x+y+z"
        );
    }

    #[test]
    fn thumbnail_camera_handles_flat_and_far_bounds() {
        let p = glam::Vec3::new(1000.0, -50.0, 3.0);
        let (view_proj, eye) = thumbnail_camera(p, p);
        assert!(view_proj.is_finite() && eye.is_finite());
        let centre = view_proj.project_point3(p);
        assert!(centre.x.abs() < 1e-3 && centre.y.abs() < 1e-3, "{centre}");
    }

    /// Renders a GI reset frame, then two main frames of the GI room at
    /// `quality`, with and without two thumbnails between them: the second
    /// frame must be byte-identical, and the thumbnails must keep the shadow
    /// map, the sample count and the scene target's MSAA textures.
    fn thumbnails_between_frames(
        renderer: &mut Renderer,
        quality: melon_core::RenderQuality,
        name: &str,
    ) -> Result<(), String> {
        use melon_core::{GiMode, Handle, Model};
        // The GI test room: its colours bleed only through DDGI. Handle ids
        // past the other checks' (the model cache is shared, keyed by id).
        let mut server = fresh_ids_server(repo_server().root().to_path_buf(), 5000);
        let models = test_room(&mut server);
        let camera = melon_core::Camera {
            eye: glam::Vec3::new(0.0, 2.0, -8.5),
            center: glam::Vec3::new(0.0, 1.9, 1.0),
            aspect: 1.0,
            ..Default::default()
        };
        let mut scene_target =
            renderer.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let frame = |quality| FrameData {
            view_proj: camera.view_proj(),
            eye: camera.eye,
            light: test_room_light(),
            environment: melon_core::Environment::default(),
            quality,
            models: models.clone(),
            overrides: Default::default(),
            lights: Default::default(),
        };
        let reset = frame(melon_core::RenderQuality {
            gi_mode: GiMode::Off,
            ..quality
        });
        let main = frame(quality);
        let root = temp_project(name);
        std::fs::write(root.join("assets/models/m.gltf"), TWO_NODE_GLTF).unwrap();
        let mut thumb_server = fresh_ids_server(root.clone(), 6000);
        let h: Handle<Model> = thumb_server.load("assets/models/m.gltf");
        let mut thumb_target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(
            THUMBNAIL_SIZE,
            THUMBNAIL_SIZE,
        ));
        let identity = |renderer: &Renderer, target: &ViewportTarget| {
            (
                renderer.scene.shadow_texture.clone(),
                renderer.scene.msaa(),
                target.hdr_target.msaa.as_ref().map(|(t, _)| t.clone()),
                target._depth_texture.clone(),
            )
        };
        let mut run = |renderer: &mut Renderer, thumbnails: bool| -> Result<Vec<u8>, String> {
            let mut counts = None;
            for (i, frame) in [&reset, &main, &main].into_iter().enumerate() {
                if i == 2 && thumbnails {
                    counts = Some((renderer.scene.ibl_rebuilds, renderer.scene.shadow_rebuilds));
                    let before = identity(renderer, &scene_target);
                    for _ in 0..2 {
                        renderer
                            .render_thumbnail(&mut thumb_server, h, &mut thumb_target)
                            .ok_or(format!("{name}: no thumbnail"))?;
                    }
                    if identity(renderer, &scene_target) != before {
                        return Err(format!("{name}: shadow map or MSAA targets rebuilt"));
                    }
                }
                let mut encoder = renderer
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                renderer.render_scene(
                    &mut encoder,
                    frame,
                    &mut server,
                    &mut scene_target,
                    &[],
                    &[],
                );
                // The thumbnails and the main frame after them rebuild
                // neither the IBL nor the shadow textures.
                if let Some(counts) = counts
                    && counts != (renderer.scene.ibl_rebuilds, renderer.scene.shadow_rebuilds)
                {
                    return Err(format!("{name}: a thumbnail rebuilt the IBL or atlas"));
                }
                if i < 2 {
                    renderer.queue().submit([encoder.finish()]);
                } else {
                    return Ok(read_back(
                        renderer.device(),
                        renderer.queue(),
                        encoder,
                        &scene_target.color_texture,
                    ));
                }
            }
            unreachable!()
        };
        // The first run settles the targets (MSAA, quality switch).
        run(renderer, false)?;
        let plain = run(renderer, false);
        let with_thumbnails = run(renderer, true);
        // End on a GI-off frame, like the other GI checks.
        let mut encoder = renderer
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        renderer.render_scene(
            &mut encoder,
            &reset,
            &mut server,
            &mut scene_target,
            &[],
            &[],
        );
        renderer.queue().submit([encoder.finish()]);
        let _ = std::fs::remove_dir_all(&root);
        if plain? == with_thumbnails? {
            Ok(())
        } else {
            Err(format!("{name}: the next frame's pixels changed"))
        }
    }

    /// A project folder with `assets/materials` and `assets/textures`.
    fn material_project(name: &str) -> std::path::PathBuf {
        let dir = temp_project(name);
        std::fs::create_dir_all(dir.join("assets/materials")).unwrap();
        std::fs::create_dir_all(dir.join("assets/textures")).unwrap();
        dir
    }

    fn write_mat(root: &std::path::Path, name: &str, ron: &str) {
        std::fs::write(root.join(format!("assets/materials/{name}.mat")), ron).unwrap();
    }

    /// Slabs of `builtin:cube` seen from above, each with its material (or
    /// the checkerboard); returns the display pixels. Caches are reset: the
    /// GPU checks share one renderer and handle ids repeat across servers.
    fn render_slabs(
        r: &mut Renderer,
        server: &mut melon_core::AssetServer,
        slabs: &[(glam::Vec3, glam::Vec3, Option<&str>)],
    ) -> Vec<u8> {
        render_slabs_in(r, server, slabs, plain_environment())
    }

    fn render_slabs_in(
        r: &mut Renderer,
        server: &mut melon_core::AssetServer,
        slabs: &[(glam::Vec3, glam::Vec3, Option<&str>)],
        environment: melon_core::Environment,
    ) -> Vec<u8> {
        r.materials = Default::default();
        r.model_cache = ModelCache::new(&r.scene);
        let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
        let mut frame = FrameData {
            view_proj: top_down_camera().view_proj(),
            eye: top_down_camera().eye,
            light: test_light(),
            environment,
            ..Default::default()
        };
        for (i, (size, at, material)) in slabs.iter().enumerate() {
            frame.models.push((
                glam::Mat4::from_scale_rotation_translation(*size, glam::Quat::IDENTITY, *at),
                cube,
            ));
            if let Some(path) = material {
                frame.overrides.insert(
                    i,
                    vec![Some(server.load(&format!("assets/materials/{path}.mat")))],
                );
            }
        }
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let mut encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        r.render_scene(&mut encoder, &frame, server, &mut target, &[], &[]);
        read_back(r.device(), r.queue(), encoder, &target.color_texture)
    }

    const CENTRE_PX: (u32, u32) = (TEST_SIZE / 2, TEST_SIZE / 2);

    fn floor_slab(material: &'static str) -> [(glam::Vec3, glam::Vec3, Option<&'static str>); 1] {
        [(
            glam::Vec3::new(8.0, 0.2, 8.0),
            glam::Vec3::new(0.0, -0.1, 0.0),
            Some(material),
        )]
    }

    fn check_material_missing_texture(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_missing_texture");
        let plain = "(base_color: (0.2, 0.6, 0.2, 1.0), metallic: 0.0, roughness: 0.8";
        write_mat(&root, "plain", &format!("{plain})"));
        write_mat(
            &root,
            "missing",
            &format!(
                "{plain}, base_color_texture: Some((guid: None, path: \"assets/textures/none.png\")))"
            ),
        );
        let mut server = fresh_ids_server(root.clone(), 7000);
        let a = render_slabs(r, &mut server, &floor_slab("plain"));
        let b = render_slabs(r, &mut server, &floor_slab("missing"));
        let built = r.materials.builds;
        if built != 1 {
            return Err(format!("the .mat should have loaded (builds {built})"));
        }
        let _ = std::fs::remove_dir_all(&root);
        let (pa, pb) = (pixel_at(r, &a, CENTRE_PX), pixel_at(r, &b, CENTRE_PX));
        if pa != pb {
            return Err(format!(
                "missing texture should bind the 1x1 fallback: {pa:?} vs {pb:?}"
            ));
        }
        if pa[1] <= pa[0].saturating_add(20) {
            return Err(format!("the material colour should show (green): {pa:?}"));
        }
        Ok(())
    }

    fn check_texture_hot_reload(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_hot_reload");
        write_mat(
            &root,
            "tex",
            "(metallic: 0.0, roughness: 0.8, base_color_texture: Some((guid: None, path: \"assets/textures/t.png\")))",
        );
        let png = root.join("assets/textures/t.png");
        let write = |rgb: [u8; 3], later: u64| {
            image::RgbaImage::from_pixel(2, 2, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
                .save(&png)
                .unwrap();
            let file = std::fs::OpenOptions::new().write(true).open(&png).unwrap();
            file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(later))
                .unwrap();
        };
        let mut server = fresh_ids_server(root.clone(), 7100);
        write([255, 0, 0], 2);
        let before = render_slabs(r, &mut server, &floor_slab("tex"));
        let before = pixel_at(r, &before, CENTRE_PX);
        write([0, 0, 255], 5);
        server.poll_changes();
        // Same renderer caches: only the texture version changed.
        let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
        let material: melon_core::Handle<melon_core::Material> =
            server.load("assets/materials/tex.mat");
        let builds = r.materials.builds;
        let mut frame = FrameData {
            view_proj: top_down_camera().view_proj(),
            eye: top_down_camera().eye,
            light: test_light(),
            environment: plain_environment(),
            ..Default::default()
        };
        frame.models.push((
            glam::Mat4::from_scale_rotation_translation(
                glam::Vec3::new(8.0, 0.2, 8.0),
                glam::Quat::IDENTITY,
                glam::Vec3::new(0.0, -0.1, 0.0),
            ),
            cube,
        ));
        frame.overrides.insert(0, vec![Some(material)]);
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let mut encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        r.render_scene(&mut encoder, &frame, &mut server, &mut target, &[], &[]);
        let after = read_back(r.device(), r.queue(), encoder, &target.color_texture);
        let after = pixel_at(r, &after, CENTRE_PX);
        let rebuilt = r.materials.builds - builds;
        let _ = std::fs::remove_dir_all(&root);
        if before[0] <= before[2].saturating_add(40) {
            return Err(format!("first texture is red: {before:?}"));
        }
        if rebuilt != 1 || after[2] <= after[0].saturating_add(40) {
            return Err(format!(
                "texture hot reload: {rebuilt} rebuilds, now {after:?} (want blue)"
            ));
        }
        Ok(())
    }

    fn check_alpha_mask_and_blend(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_alpha");
        let colour = |rgb: &str, alpha: f32, mode: &str| {
            format!(
                "(base_color: ({rgb}, {alpha:?}), metallic: 0.0, roughness: 0.8, alpha_mode: {mode})"
            )
        };
        write_mat(&root, "green", &colour("0.0, 0.8, 0.0", 1.0, "Opaque"));
        write_mat(&root, "red", &colour("0.8, 0.0, 0.0", 1.0, "Opaque"));
        write_mat(
            &root,
            "cut_low",
            &colour("0.8, 0.0, 0.0", 0.3, "Mask(cutoff: 0.5)"),
        );
        write_mat(
            &root,
            "cut_high",
            &colour("0.8, 0.0, 0.0", 0.8, "Mask(cutoff: 0.5)"),
        );
        write_mat(&root, "glass", &colour("0.8, 0.0, 0.0", 0.5, "Blend"));
        let mut server = fresh_ids_server(root.clone(), 7200);
        let floor = (
            glam::Vec3::new(8.0, 0.2, 8.0),
            glam::Vec3::new(0.0, -0.1, 0.0),
            Some("green"),
        );
        let slab = |material| {
            (
                glam::Vec3::new(3.0, 0.1, 3.0),
                glam::Vec3::new(0.0, 0.5, 0.0),
                Some(material),
            )
        };
        let mut px = |slabs: &[_]| {
            let pixels = render_slabs(r, &mut server, slabs);
            pixel_at(r, &pixels, CENTRE_PX)
        };
        let ground = px(&[floor]);
        let opaque = px(&[floor, slab("red")]);
        let masked_out = px(&[floor, slab("cut_low")]);
        let masked_in = px(&[floor, slab("cut_high")]);
        let blended = px(&[floor, slab("glass")]);
        let built = r.materials.builds;
        if built != 2 {
            return Err(format!(
                "the .mat files should have loaded (builds {built})"
            ));
        }
        let _ = std::fs::remove_dir_all(&root);
        if masked_out != ground {
            return Err(format!(
                "Mask below the cutoff should vanish: {masked_out:?} vs {ground:?}"
            ));
        }
        if masked_in != opaque {
            return Err(format!(
                "Mask above the cutoff should draw: {masked_in:?} vs {opaque:?}"
            ));
        }
        let between = |v: u8, a: u8, b: u8| v > a.min(b) && v < a.max(b);
        if !between(blended[0], ground[0], opaque[0]) || !between(blended[1], ground[1], opaque[1])
        {
            return Err(format!(
                "Blend should mix the slab with what is behind: {blended:?} between {ground:?} and {opaque:?}"
            ));
        }
        Ok(())
    }

    /// A white floor lit only by `lights` (no sun, no ambient), seen from
    /// above; returns the display pixels and the frame's stats.
    fn render_lit(
        r: &mut Renderer,
        lights: Vec<lights::LocalLight>,
        quality: melon_core::RenderQuality,
    ) -> (Vec<u8>, RenderStats) {
        let sun = melon_core::Light {
            color: glam::Vec3::ZERO,
            ambient: 0.0,
            ..test_light()
        };
        render_floor_under(r, lights, quality, sun, plain_environment(), None)
    }

    /// `render_lit` under the given sun / ambient and sky.
    fn render_floor_under(
        r: &mut Renderer,
        lights: Vec<lights::LocalLight>,
        quality: melon_core::RenderQuality,
        light: melon_core::Light,
        environment: melon_core::Environment,
        sky_png: Option<[u8; 3]>,
    ) -> (Vec<u8>, RenderStats) {
        let root = material_project("lit_floor");
        write_mat(
            &root,
            "white",
            "(base_color: (0.8, 0.8, 0.8, 1.0), metallic: 0.0, roughness: 0.8)",
        );
        let mut server = fresh_ids_server(root.clone(), 7300);
        r.materials = Default::default();
        r.model_cache = ModelCache::new(&r.scene);
        let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
        let material = server.load("assets/materials/white.mat");
        let mut environment = environment;
        if let Some(rgb) = sky_png {
            let file = root.join("assets/textures/sky.png");
            image::RgbaImage::from_pixel(8, 4, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
                .save(&file)
                .unwrap();
            environment.sky_texture = Some(server.load("assets/textures/sky.png"));
        }
        let mut frame = FrameData {
            view_proj: top_down_camera().view_proj(),
            eye: top_down_camera().eye,
            light,
            environment,
            quality,
            lights,
            ..Default::default()
        };
        frame.models.push((
            glam::Mat4::from_scale_rotation_translation(
                glam::Vec3::new(12.0, 0.2, 12.0),
                glam::Quat::IDENTITY,
                glam::Vec3::new(0.0, -0.1, 0.0),
            ),
            cube,
        ));
        frame.overrides.insert(0, vec![Some(material)]);
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let mut encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let stats = r.render_scene(&mut encoder, &frame, &mut server, &mut target, &[], &[]);
        let pixels = read_back(r.device(), r.queue(), encoder, &target.color_texture);
        let _ = std::fs::remove_dir_all(&root);
        (pixels, stats)
    }

    fn check_ibl_uniform_sky(r: &mut Renderer) -> Result<(), String> {
        // A white rough dielectric under a uniform sky of radiance L (no
        // sun): it shows about L times its albedo (diffuse irradiance = L,
        // plus a little reflection).
        let l = 0.5f32;
        let light = melon_core::Light {
            color: glam::Vec3::ZERO,
            ambient: 1.0,
            sky_color: glam::Vec3::splat(l),
            ground_color: glam::Vec3::splat(l),
            ..test_light()
        };
        let environment = melon_core::Environment {
            zenith_color: glam::Vec3::splat(l),
            horizon_color: glam::Vec3::splat(l),
            ..plain_environment()
        };
        let quality = melon_core::RenderQuality::default();
        let (pixels, _) = render_floor_under(r, vec![], quality, light, environment, None);
        let got = i32::from(value_at(&pixels, CENTRE_PX));
        let want = i32::from(tonemap_value(r, f64::from(l) * 0.8));
        if (got - want).abs() > 12 {
            return Err(format!(
                "uniform sky: floor shows {got}, expected about {want}"
            ));
        }
        // Doubling the sky doubles the light: brighter, never darker.
        let bright = melon_core::Light {
            sky_color: glam::Vec3::splat(2.0 * l),
            ground_color: glam::Vec3::splat(2.0 * l),
            ..light
        };
        let (pixels, _) = render_floor_under(r, vec![], quality, bright, environment, None);
        if i32::from(value_at(&pixels, CENTRE_PX)) <= got {
            return Err("a brighter sky should brighten the floor".into());
        }
        Ok(())
    }

    fn point_light(at: glam::Vec3, intensity: f32, range: f32) -> lights::LocalLight {
        lights::LocalLight {
            position: at,
            direction: glam::Vec3::NEG_Y,
            color: glam::Vec3::splat(intensity),
            range,
            spot: None,
            cast_shadows: false,
        }
    }

    fn check_local_lights(r: &mut Renderer) -> Result<(), String> {
        let quality = melon_core::RenderQuality::default();
        let corner = (4, 4);
        let (dark, _) = render_lit(r, vec![], quality);
        let dark_centre = value_at(&dark, CENTRE_PX);
        // Only the sky reflection (IBL specular) is left without lights.
        let dark_far = value_at(&dark, corner);
        if dark_centre > 90 {
            return Err(format!("no light, no sun: nearly black ({dark_centre})"));
        }
        // Above the floor, in range: the floor under it lights up, the far
        // corner (about 4 m away, past the 3 m range) does not.
        let lit_at = glam::Vec3::new(0.0, 2.0, 0.0);
        let (lit, stats) = render_lit(r, vec![point_light(lit_at, 20.0, 3.0)], quality);
        let (centre, far) = (value_at(&lit, CENTRE_PX), value_at(&lit, corner));
        if stats.lights != 1 || centre < dark_centre + 50 || far > dark_far + 2 {
            return Err(format!(
                "point light: {} lights, floor under it {centre} (want > 60), past its range {far} (want black)",
                stats.lights
            ));
        }
        // Out of range (2 m up, range 1.5): adds nothing at all.
        let (out, _) = render_lit(r, vec![point_light(lit_at, 20.0, 1.5)], quality);
        if differing(&out, &dark) > 0 {
            return Err("a light out of range changed the image".into());
        }
        // A spot pointing down lights its cone only.
        let spot = lights::LocalLight {
            spot: Some((0.2, 0.5)),
            ..point_light(glam::Vec3::new(0.0, 3.0, 0.0), 40.0, 8.0)
        };
        let (cone, _) = render_lit(r, vec![spot], quality);
        let (inside, outside) = (value_at(&cone, CENTRE_PX), value_at(&cone, corner));
        if inside < dark_centre + 40 || outside > dark_far + 2 {
            return Err(format!("spot cone: centre {inside}, outside {outside}"));
        }
        // 300 lights on a grid: the cap keeps `max_lights`, the rest drop.
        let many: Vec<_> = (0..300)
            .map(|i| {
                let (x, z) = ((i % 20) as f32 - 9.5, (i / 20) as f32 - 7.0);
                point_light(glam::Vec3::new(x * 0.4, 1.0, z * 0.4), 2.0, 2.0)
            })
            .collect();
        let (_, stats) = render_lit(r, many, quality);
        if stats.lights != quality.max_lights || stats.lights_dropped != 300 - quality.max_lights {
            return Err(format!(
                "light cap: {} shaded, {} dropped (max_lights {})",
                stats.lights, stats.lights_dropped, quality.max_lights
            ));
        }
        Ok(())
    }

    fn check_far_cascade_shadow(r: &mut Renderer) -> Result<(), String> {
        use glam::Vec3;
        // A 4 m cube 30 m from the camera casts onto the floor: that depth
        // is in the third cascade (splits about 10, 27 and 100 m).
        let mut server = repo_server();
        let grey = server.load("assets/models/review/grey_cube.glb");
        let models = vec![
            slab(
                grey,
                Vec3::new(120.0, 0.2, 120.0),
                Vec3::new(0.0, -0.1, -30.0),
            ),
            slab(grey, Vec3::splat(4.0), Vec3::new(0.0, 2.0, -30.0)),
        ];
        let camera = melon_core::Camera {
            eye: Vec3::new(0.0, 6.0, 0.0),
            center: Vec3::new(0.0, 0.0, -30.0),
            aspect: 1.0,
            ..Default::default()
        };
        let light = melon_core::Light {
            direction: Vec3::new(-1.0, -1.0, 0.0).normalize(),
            ambient: 0.1,
            sky_color: Vec3::ONE,
            ground_color: Vec3::ONE,
            ..Default::default()
        };
        let pixels = render_frame(r, camera, light, plain_environment(), models, &mut server);
        let view_proj = camera.view_proj();
        let shadowed = value_at(&pixels, screen_px(view_proj, Vec3::new(-4.0, 0.0, -30.0)));
        let lit = value_at(&pixels, screen_px(view_proj, Vec3::new(4.0, 0.0, -30.0)));
        if shadowed + 30 > lit {
            return Err(format!(
                "far cascade: floor in the cube's shadow {shadowed}, lit floor {lit} (want a gap over 30)"
            ));
        }
        Ok(())
    }

    /// `render_lit`'s white floor plus, when `occluder`, a 2 m slab one metre
    /// up; returns the pixels and stats. Light `lights`, no sun.
    fn render_with_occluder(
        r: &mut Renderer,
        lights: Vec<lights::LocalLight>,
        quality: melon_core::RenderQuality,
        occluder: bool,
        gi_intensity: f32,
    ) -> (Vec<u8>, RenderStats) {
        let root = material_project("lit_occluder");
        write_mat(
            &root,
            "white",
            "(base_color: (0.8, 0.8, 0.8, 1.0), metallic: 0.0, roughness: 0.8)",
        );
        let mut server = fresh_ids_server(root.clone(), 7400);
        r.materials = Default::default();
        r.model_cache = ModelCache::new(&r.scene);
        let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
        let material = server.load("assets/materials/white.mat");
        let mut frame = FrameData {
            view_proj: top_down_camera().view_proj(),
            eye: top_down_camera().eye,
            light: melon_core::Light {
                color: glam::Vec3::ZERO,
                ambient: 0.0,
                ..test_light()
            },
            environment: melon_core::Environment {
                gi_intensity,
                ..plain_environment()
            },
            quality,
            lights,
            ..Default::default()
        };
        let slab = |size: glam::Vec3, at: glam::Vec3| {
            glam::Mat4::from_scale_rotation_translation(size, glam::Quat::IDENTITY, at)
        };
        frame.models.push((
            slab(
                glam::Vec3::new(12.0, 0.2, 12.0),
                glam::Vec3::new(0.0, -0.1, 0.0),
            ),
            cube,
        ));
        frame.overrides.insert(0, vec![Some(material)]);
        if occluder {
            frame.models.push((
                slab(
                    glam::Vec3::new(2.0, 0.2, 2.0),
                    glam::Vec3::new(0.0, 1.0, 0.0),
                ),
                cube,
            ));
            frame.overrides.insert(1, vec![Some(material)]);
        }
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let mut encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let stats = r.render_scene(&mut encoder, &frame, &mut server, &mut target, &[], &[]);
        let pixels = read_back(r.device(), r.queue(), encoder, &target.color_texture);
        let _ = std::fs::remove_dir_all(&root);
        (pixels, stats)
    }

    fn check_local_light_shadows(r: &mut Renderer) -> Result<(), String> {
        let quality = melon_core::RenderQuality::default();
        let at = glam::Vec3::new(0.0, 3.0, 0.0);
        // On the floor, 1.25 m from the centre: in the slab's shadow (its
        // edge casts out to 1.5 m) yet visible from above (the slab hides
        // 1 m).
        let probe = screen_px(
            top_down_camera().view_proj(),
            glam::Vec3::new(1.25, 0.0, 0.0),
        );
        let point = lights::LocalLight {
            cast_shadows: true,
            ..point_light(at, 30.0, 8.0)
        };
        let spot = lights::LocalLight {
            spot: Some((0.5, 0.9)),
            ..point
        };
        for (name, light) in [("point", point), ("spot", spot)] {
            let (open, stats) = render_with_occluder(r, vec![light], quality, false, 0.0);
            let (shaded, _) = render_with_occluder(r, vec![light], quality, true, 0.0);
            let (lit, dark) = (value_at(&open, probe), value_at(&shaded, probe));
            let expected_views = if name == "point" { 6 } else { 1 };
            if stats.shadowed_lights != 1 || stats.shadow_views != expected_views {
                return Err(format!(
                    "{name}: {} shadowed lights, {} views (want 1 and {expected_views})",
                    stats.shadowed_lights, stats.shadow_views
                ));
            }
            if dark + 25 > lit {
                return Err(format!(
                    "{name} light shadow: floor in the shadow {dark}, unshadowed {lit} (want a gap over 25)"
                ));
            }
            // Without the flag the same slab casts nothing.
            let plain = lights::LocalLight {
                cast_shadows: false,
                ..light
            };
            let (unshadowed, _) = render_with_occluder(r, vec![plain], quality, true, 0.0);
            if value_at(&unshadowed, probe) + 5 < lit {
                return Err(format!("{name}: an unshadowed light cast a shadow"));
            }
        }
        // The cap: five shadow-casting lights, two allowed.
        let capped = melon_core::RenderQuality {
            shadowed_lights: 2,
            ..quality
        };
        let many: Vec<_> = (0..5)
            .map(|i| lights::LocalLight {
                position: glam::Vec3::new(i as f32 - 2.0, 3.0, 2.0),
                ..point
            })
            .collect();
        let (_, stats) = render_with_occluder(r, many.clone(), capped, true, 0.0);
        if stats.shadowed_lights != 2 || stats.shadow_views != 12 {
            return Err(format!(
                "cap: {} shadowed lights, {} views (want 2 and 12)",
                stats.shadowed_lights, stats.shadow_views
            ));
        }
        // With DDGI on, a shadowed light injects into the voxels: the frame renders.
        let (_, stats) = render_with_occluder(r, many, quality, true, 1.0);
        if stats.shadowed_lights != 5 {
            return Err(format!(
                "ddgi frame: {} shadowed lights",
                stats.shadowed_lights
            ));
        }
        Ok(())
    }

    fn check_hdr_sky_is_drawn(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("hdr_sky");
        image::RgbaImage::from_pixel(8, 4, image::Rgba([255, 0, 0, 255]))
            .save(root.join("assets/textures/sky.png"))
            .unwrap();
        let mut server = fresh_ids_server(root.clone(), 7500);
        let camera = melon_core::Camera {
            eye: glam::Vec3::ZERO,
            center: glam::Vec3::new(0.0, 0.0, -10.0),
            aspect: 1.0,
            ..Default::default()
        };
        let frame = FrameData {
            view_proj: camera.view_proj(),
            eye: camera.eye,
            light: test_light(),
            environment: melon_core::Environment {
                sky_texture: true.then(|| server.load("assets/textures/sky.png")),
                ..plain_environment()
            },
            ..Default::default()
        };
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let mut encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        r.render_scene(&mut encoder, &frame, &mut server, &mut target, &[], &[]);
        let pixels = read_back(r.device(), r.queue(), encoder, &target.color_texture);
        let _ = std::fs::remove_dir_all(&root);
        let sky = pixel_at(r, &pixels, CENTRE_PX);
        if sky[0] < sky[2].saturating_add(100) {
            return Err(format!("the image sky should be drawn red: {sky:?}"));
        }
        Ok(())
    }

    fn check_material_thumbnail(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_thumb");
        write_mat(
            &root,
            "red",
            "(base_color: (0.9, 0.05, 0.05, 1.0), metallic: 0.0, roughness: 0.5)",
        );
        let mut server = fresh_ids_server(root.clone(), 7600);
        r.materials = Default::default();
        r.model_cache = ModelCache::new(&r.scene);
        let handle: melon_core::Handle<melon_core::Material> =
            server.load("assets/materials/red.mat");
        let mut target = r.create_viewport_target(winit::dpi::PhysicalSize::new(
            THUMBNAIL_SIZE,
            THUMBNAIL_SIZE,
        ));
        let texture = r
            .render_material_thumbnail_texture(&mut server, handle, &mut target)
            .ok_or("no material thumbnail")?;
        let encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let pixels = read_back(r.device(), r.queue(), encoder, &texture);
        let _ = std::fs::remove_dir_all(&root);
        let at = |x: u32, y: u32| {
            let i = ((y * THUMBNAIL_SIZE + x) * 4) as usize;
            if r.surface_format().remove_srgb_suffix() == wgpu::TextureFormat::Bgra8Unorm {
                [pixels[i + 2], pixels[i + 1], pixels[i]]
            } else {
                [pixels[i], pixels[i + 1], pixels[i + 2]]
            }
        };
        let centre = at(THUMBNAIL_SIZE / 2, THUMBNAIL_SIZE / 2);
        let corner = at(2, 2);
        let sphere: melon_core::Handle<melon_core::Model> = server.load("builtin:sphere");
        if server.part_bounds(sphere).len() != 1 {
            return Err("the sphere should publish its one part's bounds".into());
        }
        if centre[0] < centre[1].saturating_add(60) {
            return Err(format!("the sphere should be red: {centre:?}"));
        }
        if corner[0] > corner[1].saturating_add(30) {
            return Err(format!("the corner should be background: {corner:?}"));
        }
        Ok(())
    }

    fn check_packed_materials_textures_and_sky(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("packed_mat");
        write_mat(
            &root,
            "m",
            "(metallic: 0.0, base_color_texture: Some((guid: None, path: \"assets/textures/t.png\")))",
        );
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .save(root.join("assets/textures/t.png"))
            .unwrap();
        let pak = root.join("game.pak");
        melon_core::pack::build_pack(&root, &[], &pak).unwrap();
        // Only the pack may serve them now.
        std::fs::remove_file(root.join("assets/materials/m.mat")).unwrap();
        std::fs::remove_file(root.join("assets/textures/t.png")).unwrap();
        let mut server = fresh_ids_server(root.clone(), 7700);
        server.set_pack(Arc::new(melon_core::pack::Pack::open(&pak).unwrap()));
        r.materials = Default::default();
        let material: melon_core::Handle<melon_core::Material> =
            server.load("assets/materials/m.mat");
        let texture: melon_core::Handle<melon_core::Texture> = server.load("assets/textures/t.png");
        r.materials.prepare(&r.scene, &mut server, material);
        let built = r.materials.get(material).is_some();
        let loaded = r.materials.texture(texture, true).is_some();
        let sky = r.sky_image(&server, Some(texture)).is_some();
        let _ = std::fs::remove_dir_all(&root);
        if !built || !loaded || !sky {
            return Err(format!(
                "packed assets: material {built}, texture {loaded}, sky {sky} (want all true)"
            ));
        }
        Ok(())
    }

    /// A masked-out slab must also vanish from the prepass: with AO on it
    /// would otherwise darken the floor beside it.
    fn check_masked_out_leaves_no_ao(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_mask_ao");
        write_mat(
            &root,
            "green",
            "(base_color: (0.0, 0.8, 0.0, 1.0), metallic: 0.0, roughness: 0.8)",
        );
        write_mat(
            &root,
            "cut",
            "(base_color: (0.8, 0.0, 0.0, 0.3), metallic: 0.0, roughness: 0.8, alpha_mode: Mask(cutoff: 0.5))",
        );
        let mut server = fresh_ids_server(root.clone(), 7800);
        let floor = (
            glam::Vec3::new(8.0, 0.2, 8.0),
            glam::Vec3::new(0.0, -0.1, 0.0),
            Some("green"),
        );
        let slab = (
            glam::Vec3::new(3.0, 0.1, 3.0),
            glam::Vec3::new(0.0, 0.3, 0.0),
            Some("cut"),
        );
        let environment = melon_core::Environment {
            ao_strength: 1.0,
            ..plain_environment()
        };
        let probe = screen_px(
            top_down_camera().view_proj(),
            glam::Vec3::new(1.7, 0.0, 0.0),
        );
        let mut px = |slabs: &[_]| {
            let pixels = render_slabs_in(r, &mut server, slabs, environment);
            pixel_at(r, &pixels, probe)
        };
        let (ground, masked) = (px(&[floor]), px(&[floor, slab]));
        let _ = std::fs::remove_dir_all(&root);
        if ground != masked {
            return Err(format!(
                "a masked-out slab darkened the floor beside it: {ground:?} vs {masked:?}"
            ));
        }
        Ok(())
    }

    /// Two blend panels submitted near first: the sort draws the far one
    /// first, so the near (blue) one dominates the mix.
    fn check_blend_sorted_far_to_near(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_blend_sort");
        let glass = |rgb: &str| {
            format!("(base_color: ({rgb}, 0.5), metallic: 0.0, roughness: 0.8, alpha_mode: Blend)")
        };
        write_mat(&root, "blue", &glass("0.0, 0.0, 0.8"));
        write_mat(&root, "red", &glass("0.8, 0.0, 0.0"));
        let mut server = fresh_ids_server(root.clone(), 7900);
        let panel = |height: f32, material| {
            (
                glam::Vec3::new(3.0, 0.1, 3.0),
                glam::Vec3::new(0.0, height, 0.0),
                Some(material),
            )
        };
        let pixels = render_slabs(r, &mut server, &[panel(2.0, "blue"), panel(1.0, "red")]);
        let [red, _, blue, _] = pixel_at(r, &pixels, CENTRE_PX);
        let _ = std::fs::remove_dir_all(&root);
        if blue <= red {
            return Err(format!(
                "the near blue panel should dominate: red {red}, blue {blue}"
            ));
        }
        Ok(())
    }

    /// A light off the view centre lights the floor under it, not its
    /// mirror image (cluster tile rows and columns match the shader's).
    fn check_off_centre_light_lights_its_own_floor(r: &mut Renderer) -> Result<(), String> {
        let quality = melon_core::RenderQuality::default();
        let view_proj = top_down_camera().view_proj();
        let under = glam::Vec3::new(3.0, 0.0, -3.0);
        let light = point_light(glam::Vec3::new(3.0, 1.5, -3.0), 20.0, 3.0);
        let (pixels, _) = render_lit(r, vec![light], quality);
        let lit = value_at(&pixels, screen_px(view_proj, under));
        for (name, mirror) in [
            ("x", glam::Vec3::new(-3.0, 0.0, -3.0)),
            ("z", glam::Vec3::new(3.0, 0.0, 3.0)),
        ] {
            let other = value_at(&pixels, screen_px(view_proj, mirror));
            if lit < other.saturating_add(50) {
                return Err(format!(
                    "light at (3, -3): floor under it {lit}, mirrored in {name} {other}"
                ));
            }
        }
        Ok(())
    }

    /// Edge frames in one target: cascade count, shadow distance, light caps
    /// switched live, a shadow-casting light inside a mesh, a NaN light and
    /// a zero-range one. No validation error, and the scene still draws.
    fn check_edge_quality_and_lights(r: &mut Renderer) -> Result<(), String> {
        let mut server = repo_server();
        let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
        r.model_cache = ModelCache::new(&r.scene);
        let slab = |size: glam::Vec3, at: glam::Vec3| {
            glam::Mat4::from_scale_rotation_translation(size, glam::Quat::IDENTITY, at)
        };
        let models = vec![
            (
                slab(
                    glam::Vec3::new(12.0, 0.2, 12.0),
                    glam::Vec3::new(0.0, -0.1, 0.0),
                ),
                cube,
            ),
            (
                slab(glam::Vec3::splat(2.0), glam::Vec3::new(0.0, 1.0, 0.0)),
                cube,
            ),
        ];
        let mut nan = point_light(glam::Vec3::new(2.0, 1.0, 2.0), 5.0, 4.0);
        nan.color = glam::Vec3::splat(f32::NAN);
        let lights = vec![
            // Inside the 2 m cube, shadowed.
            lights::LocalLight {
                cast_shadows: true,
                ..point_light(glam::Vec3::new(0.0, 1.0, 0.0), 20.0, 6.0)
            },
            nan,
            point_light(glam::Vec3::new(-2.0, 1.0, 2.0), 5.0, 0.0),
        ];
        let base = melon_core::RenderQuality::default();
        let scope = r.device().push_error_scope(wgpu::ErrorFilter::Validation);
        let mut target =
            r.create_viewport_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let mut centres = Vec::new();
        for (cascades, distance, shadowed, max) in [
            (1, 1, 1, 256),
            (4, 200, 4, 256),
            (2, 1, 0, 0),
            (3, 50, 8, 1),
            (4, 200, 4, 256),
        ] {
            let frame = FrameData {
                view_proj: top_down_camera().view_proj(),
                eye: top_down_camera().eye,
                light: test_light(),
                environment: plain_environment(),
                quality: melon_core::RenderQuality {
                    cascades,
                    shadow_distance: distance,
                    shadowed_lights: shadowed,
                    max_lights: max,
                    ..base
                },
                models: models.clone(),
                overrides: Default::default(),
                lights: lights.clone(),
            };
            let mut encoder = r
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            r.render_scene(&mut encoder, &frame, &mut server, &mut target, &[], &[]);
            let pixels = read_back(r.device(), r.queue(), encoder, &target.color_texture);
            centres.push(value_at(&pixels, CENTRE_PX));
        }
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(format!("wgpu validation error: {e}"));
        }
        if centres.contains(&0) {
            return Err(format!("the cube top went black: {centres:?}"));
        }
        Ok(())
    }

    /// A Blend material still gives a thumbnail.
    fn check_blend_material_thumbnail(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_thumb_blend");
        write_mat(
            &root,
            "glass",
            "(base_color: (0.9, 0.05, 0.05, 0.5), metallic: 0.0, roughness: 0.5, alpha_mode: Blend)",
        );
        let mut server = fresh_ids_server(root.clone(), 8000);
        r.materials = Default::default();
        r.model_cache = ModelCache::new(&r.scene);
        let handle: melon_core::Handle<melon_core::Material> =
            server.load("assets/materials/glass.mat");
        let mut target = r.create_viewport_target(winit::dpi::PhysicalSize::new(
            THUMBNAIL_SIZE,
            THUMBNAIL_SIZE,
        ));
        let texture = r
            .render_material_thumbnail_texture(&mut server, handle, &mut target)
            .ok_or("no thumbnail for a Blend material")?;
        let encoder = r
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let pixels = read_back(r.device(), r.queue(), encoder, &texture);
        let _ = std::fs::remove_dir_all(&root);
        let i = (((THUMBNAIL_SIZE / 2) * THUMBNAIL_SIZE + THUMBNAIL_SIZE / 2) * 4) as usize;
        let bgra = r.surface_format().remove_srgb_suffix() == wgpu::TextureFormat::Bgra8Unorm;
        let (red, green) = (pixels[i + if bgra { 2 } else { 0 }], pixels[i + 1]);
        if red < green.saturating_add(12) {
            return Err(format!(
                "the Blend sphere should show its red: {:?}",
                &pixels[i..i + 4]
            ));
        }
        Ok(())
    }

    /// A texture slot naming an unknown GUID with no path, or a file that is
    /// not an image, behaves like a missing texture: the factors alone.
    fn check_material_bad_texture_refs(r: &mut Renderer) -> Result<(), String> {
        let root = material_project("mat_bad_refs");
        std::fs::write(root.join("assets/textures/notimage.png"), b"not a png").unwrap();
        let plain = "(base_color: (0.2, 0.6, 0.2, 1.0), metallic: 0.0, roughness: 0.8";
        write_mat(&root, "plain", &format!("{plain})"));
        let refs = [
            (
                "guid_only",
                "(guid: Some(\"00000000-0000-0000-0000-000000000000\"), path: \"\")",
            ),
            (
                "garbage",
                "(guid: None, path: \"assets/textures/notimage.png\")",
            ),
            (
                "normal_slot",
                "(guid: None, path: \"assets/materials/plain.mat\")",
            ),
        ];
        for (name, tex) in refs {
            write_mat(
                &root,
                name,
                &format!("{plain}, base_color_texture: Some({tex}), normal_texture: Some({tex}))"),
            );
        }
        let mut server = fresh_ids_server(root.clone(), 8100);
        let base = render_slabs(r, &mut server, &floor_slab("plain"));
        let base = pixel_at(r, &base, CENTRE_PX);
        for (name, _) in refs {
            let pixels = render_slabs(r, &mut server, &floor_slab(name));
            let got = pixel_at(r, &pixels, CENTRE_PX);
            if got != base {
                let _ = std::fs::remove_dir_all(&root);
                return Err(format!(
                    "{name}: {got:?} differs from the factors alone {base:?}"
                ));
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// Dragging a sky slider changes the IBL every frame: the prefiltered
    /// texture is rewritten in place, never reallocated.
    fn check_ibl_updates_in_place(r: &mut Renderer) -> Result<(), String> {
        let before = r.scene.ibl.specular_texture().clone();
        let rebuilds = r.scene.ibl_rebuilds;
        for i in 1..6 {
            r.update_environment(&melon_core::Environment {
                zenith_color: glam::Vec3::splat(0.1 * i as f32),
                ..Default::default()
            });
        }
        if r.scene.ibl_rebuilds < rebuilds + 5 {
            return Err("the sky change did not rebuild the IBL".into());
        }
        if r.scene.ibl.specular_texture() != &before {
            return Err("the IBL texture was reallocated".into());
        }
        Ok(())
    }

    #[test]
    fn gpu_tests() {
        let assertions: Vec<GpuAssertion> = vec![
            Box::new(|renderer| {
                // Live quality switches gate like a strength of 0: AO off,
                // bloom off and GI mode Off each give exactly the image of
                // the old switch (ao_strength, bloom_intensity, gi_intensity
                // 0). SSGI textures exist only on SSGI frames.
                use melon_core::{GiMode, RenderQuality};
                let mut server = repo_server();
                let models = test_room(&mut server);
                let camera = melon_core::Camera {
                    eye: glam::Vec3::new(0.0, 2.0, -8.5),
                    center: glam::Vec3::new(0.0, 1.9, 1.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                let light = test_room_light();
                let lit = melon_core::Environment {
                    fog_density: 0.0,
                    ..Default::default()
                };
                let q = RenderQuality::default();
                let mut one = |env, quality| {
                    render_qualities(
                        renderer,
                        camera,
                        light,
                        env,
                        &[quality],
                        &models,
                        &mut server,
                    )
                    .map(|mut f| f.remove(0).pixels)
                };
                let no_gi = melon_core::Environment {
                    gi_intensity: 0.0,
                    ..lit
                };
                let pairs = [
                    (
                        "AO",
                        one(
                            melon_core::Environment {
                                ao_strength: 0.0,
                                ..no_gi
                            },
                            q,
                        )?,
                        one(no_gi, RenderQuality { ao: false, ..q })?,
                    ),
                    (
                        "bloom",
                        one(
                            melon_core::Environment {
                                bloom_intensity: 0.0,
                                ..no_gi
                            },
                            q,
                        )?,
                        one(no_gi, RenderQuality { bloom: false, ..q })?,
                    ),
                    ("GI", one(no_gi, q)?, one(lit, quality_in(GiMode::Off))?),
                ];
                for (name, by_strength, by_quality) in pairs {
                    let n = differing(&by_strength, &by_quality);
                    if n > 0 {
                        return Err(format!(
                            "{name} off by quality differs from strength 0 in {n} pixels"
                        ));
                    }
                }
                // A live resolution change recreates them (fresh history).
                use melon_core::SsgiResolution::{Half, Quarter};
                let ssgi = quality_in(GiMode::Ssgi);
                let quarter = RenderQuality {
                    ssgi_resolution: Quarter,
                    ..ssgi
                };
                let frames = render_qualities(
                    renderer,
                    camera,
                    light,
                    lit,
                    &[q, ssgi, ssgi, quarter, ssgi, quality_in(GiMode::Off), q],
                    &models,
                    &mut server,
                )?;
                let held: Vec<_> = frames.iter().map(|f| f.ssgi).collect();
                let expected = [
                    None,
                    Some(Half),
                    Some(Half),
                    Some(Quarter),
                    Some(Half),
                    None,
                    None,
                ];
                if held != expected {
                    return Err(format!(
                        "SSGI textures should exist only on SSGI frames, at their resolution (DDGI, SSGI half x2, quarter, half, Off, DDGI): {held:?}"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // Shadow map size rebuild: 2048, 1024, 4096, then 2048 again
                // in one target. The map is recreated each time, 1024 moves
                // the shadow edge, and back at 2048 the image is exactly the
                // first one.
                use glam::Vec3;
                use melon_core::RenderQuality;
                let mut server = repo_server();
                let grey = server.load("assets/models/review/grey_cube.glb");
                let models = [
                    slab(grey, Vec3::new(8.0, 0.2, 8.0), Vec3::new(0.0, -0.1, 0.0)),
                    slab(grey, Vec3::splat(1.0), Vec3::new(0.0, 2.0, 0.0)),
                ];
                let sized = |shadow_map_size| RenderQuality {
                    shadow_map_size,
                    ..Default::default()
                };
                let mut sizes = Vec::new();
                let mut frames = Vec::new();
                for size in [2048, 1024, 4096, 2048] {
                    let frame = render_qualities(
                        renderer,
                        top_down_camera(),
                        test_light(),
                        plain_environment(),
                        &[sized(size)],
                        &models,
                        &mut server,
                    )?;
                    sizes.push(renderer.shadow_map_size());
                    frames.extend(frame);
                }
                if sizes != [2048, 1024, 4096, 2048] {
                    return Err(format!("shadow map sizes {sizes:?}"));
                }
                let moved = differing(&frames[0].pixels, &frames[1].pixels);
                let back = differing(&frames[0].pixels, &frames[3].pixels);
                if moved == 0 || back > 0 {
                    return Err(format!(
                        "shadow map rebuild: 1024 changes {moved} pixels (want > 0), back at 2048 {back} (want 0)"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // MSAA rebuild: 4, 1, 2, 4 in one target. The target follows
                // each count (2 may fall back to 1 without adapter support),
                // the red cube's rotated silhouette over the grey floor has
                // blended edge pixels at 4 and none at 1, and back at 4 the
                // image is exactly the first one.
                use glam::{Mat4, Quat, Vec3};
                use melon_core::RenderQuality;
                let mut server = repo_server();
                let grey = server.load("assets/models/review/grey_cube.glb");
                let red = server.load("assets/models/review/red_panel.glb");
                let models = [
                    slab(grey, Vec3::new(8.0, 0.2, 8.0), Vec3::new(0.0, -0.1, 0.0)),
                    (
                        Mat4::from_scale_rotation_translation(
                            Vec3::new(3.0, 0.5, 3.0),
                            Quat::from_rotation_y(0.5),
                            Vec3::new(0.0, 0.25, 0.0),
                        ),
                        red,
                    ),
                ];
                let light = melon_core::Light {
                    direction: Vec3::NEG_Y,
                    ..test_light()
                };
                let msaa = |msaa| RenderQuality {
                    msaa,
                    ..Default::default()
                };
                let frames = render_qualities(
                    renderer,
                    top_down_camera(),
                    light,
                    plain_environment(),
                    &[msaa(4), msaa(1), msaa(2), msaa(4)],
                    &models,
                    &mut server,
                )?;
                let samples: Vec<u32> = frames.iter().map(|f| f.samples).collect();
                let two = effective_msaa(2, &renderer.scene.msaa_counts);
                if samples != [4, 1, two, 4] {
                    return Err(format!("sample counts {samples:?} (want [4, 1, {two}, 4])"));
                }
                // Pixels between the floor's and the cube's red/green ratio.
                let blended = |pixels: &[u8]| {
                    let ratio = |p: [u8; 4]| f32::from(p[0]) / f32::from(p[1]).max(1.0);
                    let (floor, cube) = (
                        ratio(pixel_at(renderer, pixels, (2, 2))),
                        ratio(pixel_at(renderer, pixels, (TEST_SIZE / 2, TEST_SIZE / 2))),
                    );
                    (0..TEST_SIZE * TEST_SIZE)
                        .map(|i| ratio(pixel_at(renderer, pixels, (i % TEST_SIZE, i / TEST_SIZE))))
                        .filter(|&q| {
                            q > floor + 0.15 * (cube - floor) && q < cube - 0.15 * (cube - floor)
                        })
                        .count()
                };
                let (soft, hard) = (blended(&frames[0].pixels), blended(&frames[1].pixels));
                let back = differing(&frames[0].pixels, &frames[3].pixels);
                if soft < hard + 10 || back > 0 {
                    return Err(format!(
                        "MSAA: {soft} blended edge pixels at 4x, {hard} at 1x (want 10 more); back at 4x {back} pixels differ (want 0)"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // GPU timings: with timestamp queries the renderer reports
                // per-pass ms within a few frames (readbacks map after their
                // submit); without them everything stays `None`.
                let mut server = repo_server();
                let models = test_room(&mut server);
                let camera = melon_core::Camera {
                    eye: glam::Vec3::new(0.0, 2.0, -8.5),
                    center: glam::Vec3::new(0.0, 1.9, 1.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                // A fresh timer: earlier checks' frames (bloom off) would
                // share its averaging window.
                renderer.timer = timings::GpuTimer::new(renderer.device(), renderer.queue());
                let supported = renderer.timer.is_some();
                let mut timings = renderer.timings();
                for _ in 0..5 {
                    render_qualities(
                        renderer,
                        camera,
                        test_room_light(),
                        melon_core::Environment::default(),
                        &[melon_core::RenderQuality::default()],
                        &models,
                        &mut server,
                    )?;
                    timings = renderer.timings();
                    if timings.total.is_some() {
                        break;
                    }
                }
                let measured = [
                    timings.gi,
                    timings.ao,
                    timings.shadows,
                    timings.aa,
                    timings.bloom,
                    timings.total,
                ];
                let ok = if supported {
                    measured
                        .iter()
                        .all(|t| t.is_some_and(|ms| (0.0..1000.0).contains(&ms)))
                } else {
                    measured.iter().all(Option::is_none)
                };
                if !ok {
                    return Err(format!(
                        "GPU timings (timestamps supported: {supported}): {timings:?}"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // GI sky calibration: an open floor in a pillar's shadow, far
                // from walls, sees only the sky. With DDGI and with SSGI (at
                // half and quarter resolution) it must match the hemisphere
                // ambient they replace (within 10%), not the brighter
                // background sky.
                use glam::Vec3;
                use melon_core::{GiMode, SsgiResolution};
                let mut server = repo_server();
                let grey = server.load("assets/models/review/grey_cube.glb");
                let models = [
                    slab(grey, Vec3::new(8.0, 0.2, 8.0), Vec3::new(0.0, -0.1, 0.0)),
                    slab(grey, Vec3::new(0.4, 4.0, 0.4), Vec3::new(2.0, 2.0, 0.0)),
                ];
                let light = melon_core::Light {
                    direction: test_light().direction,
                    ..Default::default()
                };
                let camera = top_down_camera();
                let px = screen_px(camera.view_proj(), Vec3::new(-1.0, 0.0, 0.0));
                let mut shade = |mode, ssgi_resolution| {
                    let env = melon_core::Environment {
                        gi_intensity: if mode == GiMode::Off { 0.0 } else { 1.0 },
                        ..gi_environment()
                    };
                    let quality = melon_core::RenderQuality {
                        ssgi_resolution,
                        ..quality_in(mode)
                    };
                    let settings = (env, quality);
                    let pixels = render_converged_in(
                        renderer,
                        camera,
                        light,
                        settings,
                        &models,
                        &mut server,
                    );
                    pixel_at(renderer, &pixels, px)
                };
                let half = SsgiResolution::Half;
                let off = shade(GiMode::Off, half);
                for (name, on) in [
                    ("DDGI", shade(GiMode::Ddgi, half)),
                    ("SSGI half", shade(GiMode::Ssgi, half)),
                    ("SSGI quarter", shade(GiMode::Ssgi, SsgiResolution::Quarter)),
                ] {
                    let ratio = rgb_sum(on) as f32 / rgb_sum(off).max(1) as f32;
                    if (ratio - 1.0).abs() > 0.1 {
                        return Err(format!(
                            "{name} should keep a shaded open floor at the hemisphere level: {on:?} with it, {off:?} without (ratio {ratio:.3})"
                        ));
                    }
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // SSGI colour bleeding: the red-wall scene of the DDGI bleed
                // check, seen from the wall's sunlit side so the wall is on
                // screen (SSGI only bounces what it sees). The floor in
                // front of the lit face turns red with SSGI at half and at
                // quarter resolution, stays grey without.
                use glam::Vec3;
                use melon_core::{GiMode, SsgiResolution};
                let mut server = repo_server();
                let grey = server.load("assets/models/review/grey_cube.glb");
                let red = server.load("assets/models/review/red_panel.glb");
                let models = [
                    slab(grey, Vec3::new(8.0, 0.2, 8.0), Vec3::new(0.0, -0.1, 0.0)),
                    slab(red, Vec3::new(0.4, 2.0, 4.0), Vec3::new(-0.2, 1.0, 0.0)),
                ];
                let camera = melon_core::Camera {
                    eye: Vec3::new(3.5, 2.5, 0.0),
                    center: Vec3::new(0.0, 0.6, 0.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                let px = screen_px(camera.view_proj(), Vec3::new(0.3, 0.0, 0.0));
                let light = melon_core::Light {
                    direction: -Vec3::new(1.0, 0.25, 0.0).normalize(),
                    sky_color: Vec3::ZERO,
                    ground_color: Vec3::ZERO,
                    ..test_light()
                };
                let mut ratio = |mode, ssgi_resolution| {
                    let env = melon_core::Environment {
                        zenith_color: Vec3::ZERO,
                        horizon_color: Vec3::ZERO,
                        gi_intensity: if mode == GiMode::Off { 0.0 } else { 1.0 },
                        ..gi_environment()
                    };
                    let quality = melon_core::RenderQuality {
                        ssgi_resolution,
                        ..quality_in(mode)
                    };
                    let settings = (env, quality);
                    let pixels = render_converged_in(
                        renderer,
                        camera,
                        light,
                        settings,
                        &models,
                        &mut server,
                    );
                    let p = pixel_at(renderer, &pixels, px);
                    f32::from(p[0]) / f32::from(p[1]).max(1.0)
                };
                let off = ratio(GiMode::Off, SsgiResolution::Half);
                for resolution in [SsgiResolution::Half, SsgiResolution::Quarter] {
                    let on = ratio(GiMode::Ssgi, resolution);
                    if on < off + 0.05 {
                        return Err(format!(
                            "SSGI at {resolution:?} should bleed red onto the floor: R/G {on:.3} with SSGI, {off:.3} without"
                        ));
                    }
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // Mode switching frame to frame: Off, SSGI, DDGI, then Off
                // again in one target gives exactly the Off image of a view
                // that never switched (AO on, so the shared prepass runs).
                use melon_core::GiMode;
                let mut server = repo_server();
                let models = test_room(&mut server);
                let camera = melon_core::Camera {
                    eye: glam::Vec3::new(0.0, 2.0, -8.5),
                    center: glam::Vec3::new(0.0, 1.9, 1.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                let off = (
                    melon_core::Environment {
                        ao_strength: 1.0,
                        ..plain_environment()
                    },
                    melon_core::RenderQuality::default(),
                );
                let on = |mode| {
                    let env = melon_core::Environment {
                        ao_strength: 1.0,
                        ..gi_environment()
                    };
                    (env, quality_in(mode))
                };
                let light = test_room_light();
                let switched = render_sequence(
                    renderer,
                    camera,
                    light,
                    &[
                        off,
                        on(GiMode::Ssgi),
                        on(GiMode::Ssgi),
                        on(GiMode::Ddgi),
                        on(GiMode::Ddgi),
                        on(GiMode::Ssgi),
                        off,
                    ],
                    &models,
                    &mut server,
                );
                let never = render_sequence(renderer, camera, light, &[off], &models, &mut server);
                let differing = switched
                    .chunks(4)
                    .zip(never.chunks(4))
                    .filter(|(a, b)| a != b)
                    .count();
                if differing > 0 {
                    return Err(format!(
                        "switching GI modes changed the final Off frame: {differing} pixels differ"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // No bright rims at inside corners: the shaded back wall in its
                // top and bottom corners by the green wall (8 cm from each
                // face) and the floor by the back wall are not brighter than
                // the same surface 0.5 m away (0.7 m diagonally in a corner). Seen from the
                // room's front like the snapshot (the view bias points out
                // of the room, not away from the side wall) through an 8
                // degree lens (about 2.5 cm per pixel).
                use glam::Vec3;
                let mut server = repo_server();
                let models = test_room(&mut server);
                let env = melon_core::Environment {
                    gi_intensity: 1.0,
                    ..plain_environment()
                };
                let o = TEST_ROOM_OFFSET;
                let checks = [
                    (
                        "top corner (back wall, green wall, ceiling)",
                        Vec3::new(-2.5, 4.0, 2.5),
                        Vec3::new(-2.42, 3.9, 2.5),
                        Vec3::new(-1.92, 3.4, 2.5),
                    ),
                    (
                        "bottom corner (back wall, green wall, floor)",
                        Vec3::new(-2.5, 0.0, 2.5),
                        Vec3::new(-2.42, 0.1, 2.5),
                        Vec3::new(-1.92, 0.6, 2.5),
                    ),
                    (
                        "floor by the back wall",
                        Vec3::new(-1.0, 0.0, 2.5),
                        Vec3::new(-1.0, 0.0, 2.42),
                        Vec3::new(-1.0, 0.0, 1.92),
                    ),
                ];
                let mut sums = Vec::new();
                for (name, center, corner, away) in checks {
                    let camera = melon_core::Camera {
                        eye: Vec3::new(0.0, 2.0, -8.5) + o,
                        center: center + o,
                        aspect: 1.0,
                        fov_y_radians: 8f32.to_radians(),
                        ..Default::default()
                    };
                    let pixels = render_converged(
                        renderer,
                        camera,
                        test_room_light(),
                        env,
                        &models,
                        &mut server,
                    );
                    let at = |p: Vec3| {
                        rgb_sum(pixel_at(
                            renderer,
                            &pixels,
                            screen_px(camera.view_proj(), p + o),
                        ))
                    };
                    sums.push((name, at(corner), at(away)));
                }
                // End on a GI-off frame (the mesh pipeline follows it).
                let camera = melon_core::Camera::default();
                render_converged(
                    renderer,
                    camera,
                    test_room_light(),
                    plain_environment(),
                    &models,
                    &mut server,
                );
                for (name, c, a) in sums {
                    // Tolerance: 2 sRGB steps per channel.
                    if c > a + 6 {
                        return Err(format!(
                            "bright rim at the {name}: RGB sum {c} at the corner, {a} 0.5 m away"
                        ));
                    }
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // Guard: the front faces of the test room's red wall and
                // ceiling (sunlit, coplanar at z = -2.5) show no stripes
                // (z-fighting of overlapping slabs, or shadow acne). Seen
                // obliquely from the room camera, 7 m away (the snapshot's
                // angle and depth precision), through a 6 degree lens (about
                // 1 cm per pixel): neighbouring pixels of each face differ by
                // at most 2 sRGB steps per channel.
                use glam::Vec3;
                let mut server = repo_server();
                let models = test_room(&mut server);
                let camera = melon_core::Camera {
                    eye: Vec3::new(0.0, 2.0, -8.5) + TEST_ROOM_OFFSET,
                    center: Vec3::new(2.7, 4.0, -2.5) + TEST_ROOM_OFFSET,
                    aspect: 1.0,
                    fov_y_radians: 6f32.to_radians(),
                    ..Default::default()
                };
                let pixels = render_converged(
                    renderer,
                    camera,
                    melon_core::Light::default(),
                    plain_environment(),
                    &models,
                    &mut server,
                );
                let face = |y: f32| Vec3::new(2.7, y, -2.5) + TEST_ROOM_OFFSET;
                let x = screen_px(camera.view_proj(), face(4.0)).0;
                let row = |y: f32| screen_px(camera.view_proj(), face(y)).1;
                let steepest = |y0: f32, y1: f32| {
                    (row(y1)..row(y0))
                        .map(|y| {
                            let a = rgb_sum(pixel_at(renderer, &pixels, (x, y)));
                            let b = rgb_sum(pixel_at(renderer, &pixels, (x, y + 1)));
                            a.abs_diff(b)
                        })
                        .max()
                        .unwrap_or(0)
                };
                for (name, step) in [
                    ("red wall", steepest(3.72, 3.95)),
                    ("ceiling", steepest(4.05, 4.28)),
                ] {
                    if step > 6 {
                        return Err(format!(
                            "{name} front face is striped: neighbouring pixels differ by {step}"
                        ));
                    }
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // GI colour bleeding: a sunlit red wall (0.4 m, more than a
                // voxel) on a grey floor, black sky and hemisphere (so the
                // bounce is not diluted), low sun (the wall gets far more of it than the
                // floor). With GI the floor beside the wall's lit face turns
                // red; without it, it stays grey.
                use glam::Vec3;
                let mut server = repo_server();
                let grey = server.load("assets/models/review/grey_cube.glb");
                let red = server.load("assets/models/review/red_panel.glb");
                let models = [
                    slab(grey, Vec3::new(8.0, 0.2, 8.0), Vec3::new(0.0, -0.1, 0.0)),
                    slab(red, Vec3::new(0.4, 2.0, 4.0), Vec3::new(-0.2, 1.0, 0.0)),
                ];
                let camera = top_down_camera();
                let px = screen_px(camera.view_proj(), Vec3::new(0.3, 0.0, 0.0));
                let mut ratio = |gi_intensity| {
                    let env = melon_core::Environment {
                        zenith_color: Vec3::ZERO,
                        horizon_color: Vec3::ZERO,
                        gi_intensity,
                        ..plain_environment()
                    };
                    let light = melon_core::Light {
                        direction: -Vec3::new(1.0, 0.25, 0.0).normalize(),
                        sky_color: Vec3::ZERO,
                        ground_color: Vec3::ZERO,
                        ..test_light()
                    };
                    let pixels =
                        render_converged(renderer, camera, light, env, &models, &mut server);
                    let p = pixel_at(renderer, &pixels, px);
                    f32::from(p[0]) / f32::from(p[1]).max(1.0)
                };
                let (on, off) = (ratio(1.0), ratio(0.0));
                if on < off + 0.05 {
                    return Err(format!(
                        "GI should bleed red onto the floor: R/G {on:.3} with GI, {off:.3} without"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // GI bounce into shade: the back wall of an open-front room
                // (red and green sides, sun in through the opening, not
                // reaching the back) with a dim hemisphere. GI brings the
                // sunlit floor's bounce and the sky in.
                use glam::Vec3;
                let mut server = repo_server();
                let models = test_room(&mut server);
                let camera = melon_core::Camera {
                    eye: Vec3::new(0.0, 2.0, -8.5),
                    center: Vec3::new(0.0, 1.9, 1.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                let light = test_room_light();
                let px = screen_px(
                    camera.view_proj(),
                    Vec3::new(0.0, 2.0, 2.5) + TEST_ROOM_OFFSET,
                );
                let mut back_wall = |gi_intensity| {
                    let env = melon_core::Environment {
                        gi_intensity,
                        ..plain_environment()
                    };
                    let pixels =
                        render_converged(renderer, camera, light, env, &models, &mut server);
                    rgb_sum(pixel_at(renderer, &pixels, px))
                };
                let (on, off) = (back_wall(1.0), back_wall(0.0));
                if on < off + 60 {
                    return Err(format!(
                        "GI should light the shaded back wall: RGB sum {on} with GI, {off} without"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // No leak: a closed box (0.4 m walls, above the 0.35 m
                // minimum, see AGENTS.md) on a sunlit floor, black sky, no hemisphere: inside,
                // only light leaking through the walls could show. Control:
                // the box's outer -z wall, which the sun misses, does get
                // bounce from the sunlit floor in front of it.
                use glam::Vec3;
                let mut server = repo_server();
                let grey = server.load("assets/models/review/grey_cube.glb");
                let (wall, side, cap) = (
                    Vec3::new(0.4, 2.2, 3.3),
                    Vec3::new(2.5, 2.2, 0.4),
                    Vec3::new(3.3, 0.4, 3.3),
                );
                // Interior [0, 2.5] x [0.4, 2.6] x [0, 2.5]; a probe sits at
                // (1.25, 0.75, 1.25).
                let models = [
                    slab(grey, Vec3::new(8.0, 0.2, 8.0), Vec3::new(0.0, -0.1, 0.0)),
                    slab(grey, cap, Vec3::new(1.25, 0.2, 1.25)),
                    slab(grey, cap, Vec3::new(1.25, 2.8, 1.25)),
                    slab(grey, wall, Vec3::new(-0.2, 1.5, 1.25)),
                    slab(grey, wall, Vec3::new(2.7, 1.5, 1.25)),
                    slab(grey, side, Vec3::new(1.25, 1.5, -0.2)),
                    slab(grey, side, Vec3::new(1.25, 1.5, 2.7)),
                ];
                let light = melon_core::Light {
                    ambient: 0.0,
                    ..test_light()
                };
                let env = |gi_intensity| melon_core::Environment {
                    zenith_color: Vec3::ZERO,
                    horizon_color: Vec3::ZERO,
                    gi_intensity,
                    ..plain_environment()
                };
                let inside = melon_core::Camera {
                    eye: Vec3::new(1.25, 1.5, 0.3),
                    center: Vec3::new(1.25, 1.0, 2.5),
                    aspect: 1.0,
                    ..Default::default()
                };
                // RGB sum of the brightest pixel (channel order does not matter).
                let brightest = |pixels: &[u8]| {
                    pixels
                        .chunks(4)
                        .map(|p| rgb_sum([p[0], p[1], p[2], 0]))
                        .max()
                        .unwrap_or(0)
                };
                let leak_on =
                    render_converged(renderer, inside, light, env(1.0), &models, &mut server);
                let leak_on = brightest(&leak_on);
                let leak_off =
                    render_converged(renderer, inside, light, env(0.0), &models, &mut server);
                let leak_off = brightest(&leak_off);
                let camera = melon_core::Camera {
                    eye: Vec3::new(1.25, 1.5, -4.0),
                    center: Vec3::new(1.25, 1.5, 0.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                let shade = screen_px(camera.view_proj(), Vec3::new(1.25, 1.5, -0.4));
                let control = |r: &mut Renderer, server: &mut melon_core::AssetServer, gi| {
                    let pixels = render_converged(r, camera, light, env(gi), &models, server);
                    rgb_sum(pixel_at(r, &pixels, shade))
                };
                let control_on = control(renderer, &mut server, 1.0);
                let control_off = control(renderer, &mut server, 0.0);
                // Tolerance: 3 sRGB steps per channel (0.4 m walls measure 3
                // over all three channels; walls of one voxel leak hundreds).
                if leak_on > leak_off + 9 {
                    return Err(format!(
                        "light leaks into the closed box: brightest RGB sum {leak_on} with GI, {leak_off} without"
                    ));
                }
                if control_on < control_off + 30 {
                    return Err(format!(
                        "control: the shaded outer wall should get bounce: {control_on} with GI, {control_off} without"
                    ));
                }
                Ok(())
            }),
            Box::new(|renderer| {
                // The review emissive cube 8 m away on black: about 8 pixels
                // wide and HDR bright (12, 1.2, 0.14). Bloom lights up the black
                // pixels beside it.
                use glam::{Mat4, Vec3};
                let mut server = melon_core::AssetServer::new(
                    concat!(env!("CARGO_MANIFEST_DIR"), "/../..").into(),
                );
                let cube: melon_core::Handle<melon_core::Model> =
                    server.load("assets/models/review/emissive_cube.glb");
                let camera = melon_core::Camera {
                    aspect: 1.0,
                    ..Default::default()
                };
                let light = melon_core::Light {
                    color: Vec3::ZERO,
                    sky_color: Vec3::ZERO,
                    ground_color: Vec3::ZERO,
                    ..Default::default()
                };
                let at = camera.eye + Vec3::NEG_Z * 8.0;
                let edge = screen_px(camera.view_proj(), at + Vec3::X * 0.5);
                let beside = (edge.0 + 4, edge.1);
                let mut px = |bloom_intensity| {
                    let env = melon_core::Environment {
                        zenith_color: Vec3::ZERO,
                        horizon_color: Vec3::ZERO,
                        bloom_intensity,
                        ..plain_environment()
                    };
                    let models = vec![(Mat4::from_translation(at), cube)];
                    let px = render_frame(renderer, camera, light, env, models, &mut server);
                    pixel_at(renderer, &px, beside)
                };
                let (off, on) = (
                    px(0.0),
                    px(melon_core::Environment::default().bloom_intensity),
                );
                if off[0] <= 2 && on[0] >= off[0] + 8 {
                    Ok(())
                } else {
                    Err(format!(
                        "bloom_spreads_bright_spot: {beside:?} off {off:?}, on {on:?}"
                    ))
                }
            }),
            Box::new(|renderer| {
                // Sunlit floor with the cube's shadow, at the default bloom.
                // No bright source near, so bloom adds no haze far from the
                // cube, and the shadow edge (lit floor 0.4 m outside, shadow
                // 0.4 m inside) keeps nearly all its contrast.
                use glam::{Mat4, Quat, Vec3};
                let mut server = melon_core::AssetServer::new(".".into());
                let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
                let floor = Mat4::from_scale_rotation_translation(
                    Vec3::new(8.0, 0.2, 8.0),
                    Quat::IDENTITY,
                    Vec3::new(0.0, -0.1, 0.0),
                );
                let models = vec![(floor, cube), (Mat4::from_translation(Vec3::Y * 0.5), cube)];
                let camera = top_down_camera();
                let px_of = |p: Vec3| screen_px(camera.view_proj(), p);
                let (open, lit, shadow) = (
                    px_of(Vec3::new(2.5, 0.0, 2.5)),
                    // The shadow runs from x = -0.5 to -1.5 (sun at 45 degrees).
                    px_of(Vec3::new(-1.9, 0.0, 0.0)),
                    px_of(Vec3::new(-1.1, 0.0, 0.0)),
                );
                let mut render = |bloom_intensity| {
                    let env = melon_core::Environment {
                        bloom_intensity,
                        ..plain_environment()
                    };
                    let px = render_frame(
                        renderer,
                        camera,
                        test_light(),
                        env,
                        models.clone(),
                        &mut server,
                    );
                    let at = |p| pixel_at(renderer, &px, p);
                    (at(open), i32::from(at(lit)[0]) - i32::from(at(shadow)[0]))
                };
                let (off, edge_off) = render(0.0);
                let (on, edge_on) = render(melon_core::Environment::default().bloom_intensity);
                let no_haze = off.iter().zip(on).all(|(&a, b)| a.abs_diff(b) <= 2);
                if no_haze && edge_on * 100 >= edge_off * EDGE_KEEP_PERCENT {
                    Ok(())
                } else {
                    Err(format!(
                        "bloom_adds_no_haze_keeps_edges: open {open:?} off {off:?}, on {on:?}; \
                         edge contrast off {edge_off}, on {edge_on}"
                    ))
                }
            }),
            Box::new(|renderer| {
                // 2999 tiny cubes in a grid plus a lone 1 m cube, instance
                // 3000 of the batch: past the old 1024 cap and the initial
                // instance buffer, so the buffer grows.
                use glam::{Mat4, Vec3};
                let mut server = melon_core::AssetServer::new(".".into());
                let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
                let mut models: Vec<(Mat4, _)> = (0..2999)
                    .map(|i| {
                        let at = Vec3::new(
                            -3.0 + 0.1 * (i % 60) as f32,
                            0.0,
                            0.5 + 0.06 * (i / 60) as f32,
                        );
                        (
                            Mat4::from_scale_rotation_translation(
                                Vec3::splat(0.04),
                                glam::Quat::IDENTITY,
                                at,
                            ),
                            cube,
                        )
                    })
                    .collect();
                let lone = Vec3::new(-2.0, 0.5, -2.0);
                models.push((Mat4::from_translation(lone), cube));
                let camera = top_down_camera();
                let px = screen_px(camera.view_proj(), lone);
                let (sky, _) = render_frame_stats(
                    renderer,
                    camera,
                    test_light(),
                    plain_environment(),
                    vec![],
                    &mut server,
                );
                let (crowd, stats) = render_frame_stats(
                    renderer,
                    camera,
                    test_light(),
                    plain_environment(),
                    models,
                    &mut server,
                );
                let (sky, crowd) = (pixel_at(renderer, &sky, px), pixel_at(renderer, &crowd, px));
                if sky != crowd
                    && stats.instances_drawn == 3000
                    && stats.culled == 0
                    && stats.draw_calls == 1
                {
                    Ok(())
                } else {
                    Err(format!(
                        "instances_past_old_cap: pixel {crowd:?} vs sky {sky:?} at the 3000th instance, stats {stats:?} (want 3000 drawn, 0 culled, 1 draw)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // A wall right of the view (x = 8, 10 m tall) inside the
                // shadow box: culled for the camera, its shadow (sun from +X,
                // 45 degrees) still falls on the floor in view.
                use glam::{Mat4, Quat, Vec3};
                let mut server = melon_core::AssetServer::new(".".into());
                let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
                let floor = (
                    Mat4::from_scale_rotation_translation(
                        Vec3::new(40.0, 0.2, 40.0),
                        Quat::IDENTITY,
                        Vec3::new(0.0, -0.1, 0.0),
                    ),
                    cube,
                );
                let wall = (
                    Mat4::from_scale_rotation_translation(
                        Vec3::new(0.6, 10.0, 2.0),
                        Quat::IDENTITY,
                        Vec3::new(8.0, 5.0, 0.0),
                    ),
                    cube,
                );
                let camera = top_down_camera();
                let px = screen_px(camera.view_proj(), Vec3::new(2.0, 0.0, 0.0));
                let (open, _) = render_frame_stats(
                    renderer,
                    camera,
                    test_light(),
                    plain_environment(),
                    vec![floor],
                    &mut server,
                );
                let (shaded, stats) = render_frame_stats(
                    renderer,
                    camera,
                    test_light(),
                    plain_environment(),
                    vec![floor, wall],
                    &mut server,
                );
                let (open, shaded) = (value_at(&open, px), value_at(&shaded, px));
                if shaded + 30 < open && stats.culled == 1 && stats.shadow_culled == 0 {
                    Ok(())
                } else {
                    Err(format!(
                        "offscreen_caster_still_casts: floor {shaded} with the wall vs {open} without (want 30+ darker), stats {stats:?} (want the wall culled for the camera only)"
                    ))
                }
            }),
            Box::new(|renderer| {
                let (contact_on, open_on) = ao_scene_pixels(renderer, 1.0);
                let (contact_off, open_off) = ao_scene_pixels(renderer, 0.0);
                if contact_on + 30 < contact_off && open_on.abs_diff(open_off) <= 3 {
                    Ok(())
                } else {
                    Err(format!(
                        "ssao_darkens_contact_only: contact {contact_on} (AO) vs {contact_off} (off), want 30+ darker (RGB sum); open floor {open_on} vs {open_off}, want within 3"
                    ))
                }
            }),
            Box::new(|renderer| {
                // Grey dielectric floor mirroring the sun into the camera.
                let floor = |roughness| MaterialUniform {
                    base_color: [0.5, 0.5, 0.5, 1.0],
                    roughness,
                    ..CHECKER_MATERIAL
                };
                let smooth = render_floor(renderer, &mirror_sun(), floor(0.05), None);
                let rough = render_floor(renderer, &mirror_sun(), floor(1.0), None);
                let smooth = highlight_peak(renderer, &smooth)[0];
                let rough = highlight_peak(renderer, &rough)[0];
                if smooth > rough.saturating_add(40) {
                    Ok(())
                } else {
                    Err(format!(
                        "smooth_highlight_brighter_than_rough: smooth {smooth}, rough {rough} (want smooth > rough + 40)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // Red base under the overhead highlight: a metal reflects red,
                // a dielectric's highlight stays white over its red diffuse.
                let red = |metallic| MaterialUniform {
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    metallic,
                    roughness: 0.2,
                    ..CHECKER_MATERIAL
                };
                let metal = render_floor(renderer, &mirror_sun(), red(1.0), None);
                let metal = highlight_peak(renderer, &metal);
                let plastic = render_floor(renderer, &mirror_sun(), red(0.0), None);
                let plastic = highlight_peak(renderer, &plastic);
                if metal[0] > 200
                    && metal[1] < 40
                    && metal[2] < 40
                    && plastic[1] > 200
                    && plastic[2] > 200
                {
                    Ok(())
                } else {
                    Err(format!(
                        "metal_specular_is_tinted: metal {metal:?} (want red), dielectric {plastic:?} (want near-white)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // A 4x4 grid of normals tilted 35 degrees, alternating
                // between +-tangent and +-bitangent from texel to texel, so
                // the floor catches more or less sun everywhere.
                let texels: Vec<u8> = (0..16)
                    .flat_map(|i| {
                        let tilt = |odd| if odd { 200 } else { 56 };
                        [tilt(i % 2 == 1), tilt((i / 4) % 2 == 1), 220, 255]
                    })
                    .collect();
                let bumps = renderer.create_texture_from_bytes(
                    &texels,
                    4,
                    4,
                    wgpu::TextureFormat::Rgba8Unorm,
                );
                let flat = render_floor(renderer, &test_light(), CHECKER_MATERIAL, None);
                let bumpy = render_floor(renderer, &test_light(), CHECKER_MATERIAL, Some(&bumps));
                // The open floor right of the occluder's shadow.
                let delta: u32 = (40..60)
                    .flat_map(|x| (8..56).map(move |y| (x, y)))
                    .map(|px| u32::from(value_at(&flat, px).abs_diff(value_at(&bumpy, px))))
                    .sum();
                let mean = delta as f32 / (20.0 * 48.0);
                if mean > 8.0 {
                    Ok(())
                } else {
                    Err(format!(
                        "normal_map_changes_shading: mean delta {mean:.2} (want > 8)"
                    ))
                }
            }),
            Box::new(|renderer| {
                let root = temp_project("thumb_evict");
                std::fs::write(root.join("assets/models/m.gltf"), TWO_NODE_GLTF).unwrap();
                std::fs::write(root.join("assets/models/n.gltf"), TWO_NODE_GLTF).unwrap();
                let mut server = fresh_ids_server(root.clone(), 4000);
                let only_thumb: melon_core::Handle<melon_core::Model> =
                    server.load("assets/models/m.gltf");
                let in_scene: melon_core::Handle<melon_core::Model> =
                    server.load("assets/models/n.gltf");
                let mut target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(
                    THUMBNAIL_SIZE,
                    THUMBNAIL_SIZE,
                ));
                // The scene already draws `in_scene`: its model is cached.
                renderer.model_bounds(&mut server, in_scene);
                let a = renderer.render_thumbnail(&mut server, only_thumb, &mut target);
                let b = renderer.render_thumbnail(&mut server, in_scene, &mut target);
                let _ = std::fs::remove_dir_all(&root);
                if a.is_none() || b.is_none() {
                    return Err("thumbnail_frees_models_only_it_loaded: no thumbnail".into());
                }
                match (
                    renderer.model_cached(only_thumb),
                    renderer.model_cached(in_scene),
                ) {
                    (false, true) => Ok(()),
                    other => Err(format!(
                        "thumbnail_frees_models_only_it_loaded: cached (thumb-only, in-scene) = {other:?}"
                    )),
                }
            }),
            Box::new(|renderer| {
                let root = temp_project("thumb");
                std::fs::write(root.join("assets/models/m.gltf"), TWO_NODE_GLTF).unwrap();
                let mut server = fresh_ids_server(root.clone(), 1000);
                let h: melon_core::Handle<melon_core::Model> = server.load("assets/models/m.gltf");
                let mut target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(
                    THUMBNAIL_SIZE,
                    THUMBNAIL_SIZE,
                ));
                let texture = renderer.render_thumbnail_texture(&mut server, h, &mut target);
                let _ = std::fs::remove_dir_all(&root);
                let texture = texture.ok_or("thumbnail_renders_the_model: no thumbnail")?;
                let encoder = renderer
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                let pixels = read_back(renderer.device(), renderer.queue(), encoder, &texture);
                // The test model is two small meshes either side of the
                // centre: any pixel other than the background shows it.
                let background = &pixels[0..3];
                let drawn = pixels.chunks(4).filter(|p| &p[0..3] != background).count();
                if drawn > 0 {
                    Ok(())
                } else {
                    Err("thumbnail_renders_the_model: only background pixels".into())
                }
            }),
            Box::new(|renderer| {
                let root = temp_project("thumb_bad");
                std::fs::write(root.join("assets/models/bad.glb"), b"not a glb").unwrap();
                let mut server = fresh_ids_server(root.clone(), 2000);
                let h: melon_core::Handle<melon_core::Model> = server.load("assets/models/bad.glb");
                let mut target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(
                    THUMBNAIL_SIZE,
                    THUMBNAIL_SIZE,
                ));
                let view = renderer.render_thumbnail(&mut server, h, &mut target);
                let _ = std::fs::remove_dir_all(&root);
                if view.is_none() {
                    Ok(())
                } else {
                    Err("thumbnail_of_a_broken_file_is_none: got a thumbnail".into())
                }
            }),
            Box::new(|renderer| {
                // DDGI on (default quality): thumbnails leave the world GI
                // state alone.
                thumbnails_between_frames(
                    renderer,
                    melon_core::RenderQuality::default(),
                    "thumbnail_does_not_disturb_the_next_scene_frame",
                )
            }),
            Box::new(|renderer| {
                // Shadow map and MSAA not at the thumbnail's (default)
                // quality: the thumbnail must not rebuild them.
                let quality = melon_core::RenderQuality {
                    shadow_map_size: 1024,
                    msaa: 1,
                    ..Default::default()
                };
                let result = thumbnails_between_frames(
                    renderer,
                    quality,
                    "thumbnail_keeps_shadow_map_and_msaa",
                );
                let default = melon_core::RenderQuality::default();
                let scene = renderer.scene_mut();
                scene.set_shadow_map_size(default.shadow_map_size);
                scene.set_msaa(default.msaa);
                result
            }),
            Box::new(|renderer| {
                let model = renderer
                    .load_gltf_bytes(TWO_NODE_GLTF.as_bytes())
                    .map_err(|e| format!("gltf_two_nodes_load_whole: {e:?}"))?;
                let (min, max) = model.bounds;
                if model.meshes.len() == 2 && min.x <= -2.0 && max.x >= 3.0 {
                    Ok(())
                } else {
                    Err(format!(
                        "gltf_two_nodes_load_whole: {} meshes, bounds {min} .. {max}",
                        model.meshes.len()
                    ))
                }
            }),
            Box::new(|renderer| {
                let root = temp_project("mesh");
                let file = root.join("assets/models/m.gltf");
                std::fs::write(&file, TWO_NODE_GLTF).unwrap();
                let result = renderer.scene().load_gltf_mesh(&file.to_string_lossy(), 0);
                let _ = std::fs::remove_dir_all(&root);
                let model = result.map_err(|e| format!("load_gltf_mesh_unbaked: {e:?}"))?;
                let (min, max) = model.bounds;
                if model.meshes.len() == 1
                    && min == glam::Vec3::ZERO
                    && max == glam::Vec3::new(1.0, 1.0, 0.0)
                {
                    Ok(())
                } else {
                    Err(format!(
                        "load_gltf_mesh_unbaked: {} meshes, bounds {min} .. {max}",
                        model.meshes.len()
                    ))
                }
            }),
            Box::new(|renderer| {
                use melon_core::{AssetServer, Handle, Model};
                let root = temp_project("siblings");
                // Two meshes (same triangle), one node each.
                let two_meshes = TWO_NODE_GLTF
                    .replace(r#"{"mesh": 0, "translation": [3, 0, 0]}"#, r#"{"mesh": 1, "translation": [3, 0, 0]}"#)
                    .replace(
                        r#""meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}]"#,
                        r#""meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}, {"primitives": [{"attributes": {"POSITION": 0}}]}]"#,
                    );
                std::fs::write(root.join("assets/models/m.gltf"), two_meshes).unwrap();
                let mut server = AssetServer::new(root.clone());
                let m0: Handle<Model> = server.load("assets/models/m.gltf#mesh0");
                let m1: Handle<Model> = server.load("assets/models/m.gltf#mesh1");
                let mut cache = ModelCache::new(renderer.scene());
                cache.prepare(renderer.scene(), &mut server, m0);
                // mesh1 was never prepared: it must come from the same import.
                let bounds = cache.model(&server, m1).bounds;
                let _ = std::fs::remove_dir_all(&root);
                if bounds == (glam::Vec3::ZERO, glam::Vec3::new(1.0, 1.0, 0.0)) {
                    Ok(())
                } else {
                    Err(format!(
                        "fragments_share_one_import: mesh1 bounds {bounds:?}"
                    ))
                }
            }),
            Box::new(|renderer| {
                use melon_core::{AssetServer, Handle, Model};
                let root = temp_project("fragment");
                std::fs::write(root.join("assets/models/m.gltf"), TWO_NODE_GLTF).unwrap();
                let mut server = AssetServer::new(root.clone());
                let h: Handle<Model> = server.load("assets/models/m.gltf#mesh5");
                let mut cache = ModelCache::new(renderer.scene());
                let model = cache.get(renderer.scene(), &mut server, h);
                let ok = model.meshes.len() == 1
                    && model.bounds == (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5));
                let _ = std::fs::remove_dir_all(&root);
                if ok {
                    Ok(())
                } else {
                    Err("cache_missing_mesh_fragment_gives_placeholder".into())
                }
            }),
            Box::new(|renderer| {
                use melon_core::{AssetServer, Handle, Model};
                let root = temp_project("missing");
                let mut server = AssetServer::new(root.clone());
                let h: Handle<Model> = server.load("assets/models/nope.glb");
                let mut cache = ModelCache::new(renderer.scene());
                let model = cache.get(renderer.scene(), &mut server, h);
                let ok = model.meshes.len() == 1
                    && model.bounds == (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5));
                let _ = std::fs::remove_dir_all(&root);
                if ok {
                    Ok(())
                } else {
                    Err("cache_missing_file_gives_placeholder".into())
                }
            }),
            Box::new(|renderer| {
                use melon_core::{AssetServer, Handle, Model};
                let root = temp_project("reload");
                let file = root.join("assets/models/m.gltf");
                std::fs::write(&file, TWO_NODE_GLTF).unwrap();
                let mut server = AssetServer::new(root.clone());
                let h: Handle<Model> = server.load("assets/models/m.gltf");
                server.poll_changes();
                let mut cache = ModelCache::new(renderer.scene());
                let first = cache.get(renderer.scene(), &mut server, h).meshes.len();
                std::fs::write(&file, "garbage").unwrap();
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&file)
                    .unwrap()
                    .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
                    .unwrap();
                server.poll_changes();
                let version = server.version(h);
                let after = cache.get(renderer.scene(), &mut server, h).meshes.len();
                let _ = std::fs::remove_dir_all(&root);
                if first == 2 && version == 1 && after == 2 {
                    Ok(())
                } else {
                    Err(format!(
                        "cache_keeps_previous_on_broken_reload: first {first}, version {version}, after {after}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let px = overlay_centre(
                    renderer,
                    (TEST_SIZE, TEST_SIZE),
                    false,
                    &[facing_quad(0.0, [1.0, 0.0, 0.0, 1.0], OverlayDepth::OnTop)],
                );
                if px[0] >= 250 && px[1] <= 5 && px[2] <= 5 {
                    Ok(())
                } else {
                    Err(format!("overlay_on_top_exact_colour: centre {px:?}"))
                }
            }),
            Box::new(|renderer| {
                let plain = overlay_centre(renderer, (TEST_SIZE, TEST_SIZE), true, &[]);
                let red = [1.0, 0.0, 0.0, 1.0];
                let tested = overlay_centre(
                    renderer,
                    (TEST_SIZE, TEST_SIZE),
                    true,
                    &[facing_quad(-2.0, red, OverlayDepth::SceneTested)],
                );
                let on_top = overlay_centre(
                    renderer,
                    (TEST_SIZE, TEST_SIZE),
                    true,
                    &[facing_quad(-2.0, red, OverlayDepth::OnTop)],
                );
                if tested == plain && on_top != plain {
                    Ok(())
                } else {
                    Err(format!(
                        "overlay_scene_tested_hidden_behind_cube: plain {plain:?} tested {tested:?} on_top {on_top:?}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let px = overlay_centre(
                    renderer,
                    (TEST_SIZE, TEST_SIZE),
                    false,
                    &[
                        facing_quad(0.0, [1.0, 0.0, 0.0, 0.5], OverlayDepth::OnTop),
                        facing_quad(-1.0, [0.0, 0.0, 1.0, 1.0], OverlayDepth::OnTop),
                    ],
                );
                if px[2] > 60 {
                    Ok(())
                } else {
                    Err(format!(
                        "overlay_transparent_does_not_hide_opaque_behind: centre {px:?}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let quad = [facing_quad(0.0, [1.0, 0.0, 0.0, 1.0], OverlayDepth::OnTop)];
                overlay_centre(renderer, (TEST_SIZE, TEST_SIZE), false, &quad);
                overlay_centre(renderer, (32, 16), false, &quad);
                Ok(())
            }),
            Box::new(|renderer| {
                use melon_core::{AssetServer, Handle, Model};
                let mut server = AssetServer::new(".".into());
                let cube: Handle<Model> = server.load("builtin:cube");
                let mut centre = |models: Vec<(glam::Mat4, Handle<Model>)>| {
                    let mut target = renderer.create_viewport_target(
                        winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE),
                    );
                    let frame = FrameData {
                        view_proj: melon_core::Camera::default().view_proj(),
                        eye: melon_core::Camera::default().eye,
                        light: melon_core::Light::default(),
                        environment: melon_core::Environment {
                            ao_strength: 0.0,
                            bloom_intensity: 0.0,
                            gi_intensity: 0.0,
                            ..Default::default()
                        },
                        quality: melon_core::RenderQuality::default(),
                        models,
                        overrides: Default::default(),
                        lights: Default::default(),
                    };
                    let mut encoder = renderer
                        .device()
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    renderer.render_scene(&mut encoder, &frame, &mut server, &mut target, &[], &[]);

                    let pixels = read_back(
                        renderer.device(),
                        renderer.queue(),
                        encoder,
                        &target.color_texture,
                    );
                    value_at(&pixels, (TEST_SIZE / 2, TEST_SIZE / 2))
                };
                let empty = centre(vec![]);
                let with_cube = centre(vec![(glam::Mat4::IDENTITY, cube)]);
                if empty != with_cube {
                    Ok(())
                } else {
                    Err(format!(
                        "render_scene_draws_a_cube: centre {empty} with and without the cube"
                    ))
                }
            }),
            Box::new(|renderer| {
                // Red horizon, blue zenith, sun behind both views.
                let env = melon_core::Environment {
                    zenith_color: glam::Vec3::Z,
                    horizon_color: glam::Vec3::X,
                    fog_density: 0.0,
                    ao_strength: 0.0,
                    bloom_intensity: 0.0,
                    gi_intensity: 0.0,
                    ..Default::default()
                };
                let light = melon_core::Light {
                    direction: glam::Vec3::new(0.0, -1.0, -1.0).normalize(),
                    ..Default::default()
                };
                let mut server = melon_core::AssetServer::new(".".into());
                let up_camera = melon_core::Camera {
                    eye: glam::Vec3::ZERO,
                    center: glam::Vec3::new(0.0, 10.0, -1.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                let level_camera = melon_core::Camera {
                    aspect: 1.0,
                    ..Default::default()
                };
                let centre = (TEST_SIZE / 2, TEST_SIZE / 2);
                let up = render_frame(renderer, up_camera, light, env, vec![], &mut server);
                let up = pixel_at(renderer, &up, centre);
                let level = render_frame(renderer, level_camera, light, env, vec![], &mut server);
                let level = pixel_at(renderer, &level, centre);
                if up[2] > up[0] && level[0] > level[2] {
                    Ok(())
                } else {
                    Err(format!(
                        "sky_zenith_vs_horizon: up {up:?} (want blue > red), level {level:?} (want red > blue)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // A ray ~3 degrees below the horizon is still the fog colour
                // (grey 0.5 here), so the fogged floor edge meets it smoothly.
                let env = melon_core::Environment {
                    zenith_color: glam::Vec3::Z,
                    horizon_color: glam::Vec3::splat(0.5),
                    fog_density: 0.0,
                    ao_strength: 0.0,
                    bloom_intensity: 0.0,
                    gi_intensity: 0.0,
                    ..Default::default()
                };
                let light = melon_core::Light {
                    direction: glam::Vec3::new(0.0, -1.0, -1.0).normalize(),
                    ..Default::default()
                };
                let camera = melon_core::Camera {
                    aspect: 1.0,
                    ..Default::default()
                };
                let mut server = melon_core::AssetServer::new(".".into());
                let px = render_frame(renderer, camera, light, env, vec![], &mut server);
                let below = pixel_at(renderer, &px, (TEST_SIZE / 2, TEST_SIZE / 2 + 4));
                let want = tonemap_value(renderer, 0.5);
                if below[..3].iter().all(|&c| c.abs_diff(want) <= 4) {
                    Ok(())
                } else {
                    Err(format!(
                        "sky_just_below_horizon_is_fog_colour: {below:?}, want ~{want}"
                    ))
                }
            }),
            Box::new(|renderer| {
                // Dark sky; looking straight at the sun gives a white HDR disc.
                let env = melon_core::Environment {
                    zenith_color: glam::Vec3::splat(0.02),
                    horizon_color: glam::Vec3::splat(0.02),
                    fog_density: 0.0,
                    ao_strength: 0.0,
                    bloom_intensity: 0.0,
                    gi_intensity: 0.0,
                    ..Default::default()
                };
                let light = melon_core::Light::default();
                let camera = melon_core::Camera {
                    eye: glam::Vec3::ZERO,
                    center: light.to_light(),
                    aspect: 1.0,
                    ..Default::default()
                };
                let mut server = melon_core::AssetServer::new(".".into());
                let px = render_frame(renderer, camera, light, env, vec![], &mut server);
                let sun = pixel_at(renderer, &px, (TEST_SIZE / 2, TEST_SIZE / 2));
                let corner = pixel_at(renderer, &px, (0, 0));
                if sun[..3].iter().all(|&c| c > 250) && corner[0] < 60 {
                    Ok(())
                } else {
                    Err(format!("sky_sun_disc: centre {sun:?}, corner {corner:?}"))
                }
            }),
            Box::new(|renderer| {
                // A floor pixel ~10 m away, blue fog: fog pulls it toward blue.
                use melon_core::{AssetServer, Handle, Model};
                let mut server = AssetServer::new(".".into());
                let cube: Handle<Model> = server.load("builtin:cube");
                let floor = glam::Mat4::from_scale_rotation_translation(
                    glam::Vec3::new(100.0, 0.1, 100.0),
                    glam::Quat::IDENTITY,
                    glam::Vec3::new(0.0, -0.05, 0.0),
                );
                let camera = melon_core::Camera {
                    eye: glam::Vec3::Y,
                    center: glam::Vec3::new(0.0, 0.0, -10.0),
                    aspect: 1.0,
                    ..Default::default()
                };
                let mut floor_px = |fog_density| {
                    let env = melon_core::Environment {
                        horizon_color: glam::Vec3::Z,
                        fog_density,
                        ao_strength: 0.0,
                        bloom_intensity: 0.0,
                        gi_intensity: 0.0,
                        ..Default::default()
                    };
                    let light = melon_core::Light::default();
                    let px = render_frame(
                        renderer,
                        camera,
                        light,
                        env,
                        vec![(floor, cube)],
                        &mut server,
                    );
                    pixel_at(renderer, &px, (TEST_SIZE / 2, TEST_SIZE / 2))
                };
                let clear = floor_px(0.0);
                let foggy = floor_px(0.2);
                let blueness = |p: [u8; 4]| i32::from(p[2]) - i32::from(p[0]);
                if blueness(foggy) > blueness(clear) + 40 {
                    Ok(())
                } else {
                    Err(format!(
                        "fog_pulls_far_floor_to_horizon: clear {clear:?}, foggy {foggy:?}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(8, 8));
                if target.ui_view_format().is_srgb() {
                    Err("viewport_ui_view_is_not_srgb: egui would decode it twice".into())
                } else {
                    Ok(())
                }
            }),
            Box::new(|renderer| {
                // Line at depth ~0.5: visible over a far-cleared depth buffer,
                // hidden behind a near-cleared one (depth test on).
                let brightest = |px: &[u8]| (31..=33).map(|y| value_at(px, (48, y))).max();
                let shown = brightest(&render_world_line(renderer, 1.0)).unwrap_or(0);
                let hidden = brightest(&render_world_line(renderer, 0.0)).unwrap_or(0);
                if shown > 150 && hidden < 30 {
                    Ok(())
                } else {
                    Err(format!(
                        "world_lines_depth_tested: shown={shown} (want > 150), hidden={hidden} (want < 30)"
                    ))
                }
            }),
            Box::new(|renderer| {
                let hdr = renderer.create_hdr_target(winit::dpi::PhysicalSize::new(8, 8));
                if hdr.sample_count() == MSAA_SAMPLES {
                    Ok(())
                } else {
                    Err(format!(
                        "hdr_target_is_multisampled: {} samples (want {MSAA_SAMPLES})",
                        hdr.sample_count()
                    ))
                }
            }),
            Box::new(|renderer| {
                let pixels =
                    render_test_scene(renderer, &test_light(), false, glam::Vec3::ZERO, 20.0);
                let (shadow, lit) = (value_at(&pixels, SHADOW_PX), value_at(&pixels, LIT_PX));
                if shadow < 170 && lit > 200 {
                    Ok(())
                } else {
                    Err(format!(
                        "occluder_casts_shadow: SHADOW_PX={shadow} (want < 170), LIT_PX={lit} (want > 200)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // A 1 m cube resting on the floor, sun from +X: its shadow
                // covers the floor from x = -0.5 to -1.5. Seen from above
                // with a narrow lens (about 8 mm per pixel) so the floor
                // right at the base on the shadow side resolves.
                use glam::{Mat4, Quat, Vec3};
                let mut server = melon_core::AssetServer::new(".".into());
                let cube: melon_core::Handle<melon_core::Model> = server.load("builtin:cube");
                let floor = Mat4::from_scale_rotation_translation(
                    Vec3::new(8.0, 0.2, 8.0),
                    Quat::IDENTITY,
                    Vec3::new(0.0, -0.1, 0.0),
                );
                let camera = melon_core::Camera {
                    eye: Vec3::new(-0.6, 6.0, 0.0),
                    center: Vec3::new(-0.6, 0.0, 0.0),
                    up: Vec3::NEG_Z,
                    fov_y_radians: 5f32.to_radians(),
                    aspect: 1.0,
                    ..Default::default()
                };
                let pixels = render_frame(
                    renderer,
                    camera,
                    test_light(),
                    plain_environment(),
                    vec![(floor, cube), (Mat4::from_translation(Vec3::Y * 0.5), cube)],
                    &mut server,
                );
                let at = |x: f32| {
                    let px = screen_px(camera.view_proj(), Vec3::new(x, 0.0, 0.0));
                    (px, value_at(&pixels, px))
                };
                // About 2 px from the base, and 30 px into the shadow.
                let ((edge_px, edge), (inside_px, inside)) = (at(-0.515), at(-0.75));
                if edge.abs_diff(inside) <= 4 && inside < 170 {
                    Ok(())
                } else {
                    Err(format!(
                        "no_light_leak_at_contact: floor at the base {edge_px:?}={edge}, inside the shadow {inside_px:?}={inside} (want within 4, inside < 170)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // The open floor around LIT_PX, far from the occluder's
                // shadow, must match the same floor with no shadow map at all
                // (shadow box moved away): no acne, at 45 and 15 degrees.
                let mut worst = Vec::new();
                for elevation in [45f32, 15.0] {
                    let e = elevation.to_radians();
                    let light = melon_core::Light {
                        direction: -glam::Vec3::new(e.cos(), e.sin(), 0.0),
                        ..test_light()
                    };
                    let shadowed =
                        render_test_scene(renderer, &light, false, glam::Vec3::ZERO, 20.0);
                    let unshadowed = render_test_scene(
                        renderer,
                        &light,
                        false,
                        glam::Vec3::new(100.0, 0.0, 0.0),
                        1.0,
                    );
                    let region = (LIT_PX.0 - 4..=LIT_PX.0 + 4)
                        .flat_map(|x| (LIT_PX.1 - 4..=LIT_PX.1 + 4).map(move |y| (x, y)));
                    let diff = region
                        .clone()
                        .map(|px| value_at(&shadowed, px).abs_diff(value_at(&unshadowed, px)))
                        .max()
                        .unwrap_or(0);
                    let values: Vec<u8> = region.map(|px| value_at(&shadowed, px)).collect();
                    let (min, max) = (values.iter().min(), values.iter().max());
                    worst.push((elevation, diff, min.copied(), max.copied()));
                }
                let lit_45 = worst[0].2.unwrap_or(0);
                if worst.iter().all(|&(_, diff, min, max)| {
                    diff <= 1 && max.unwrap_or(0) - min.unwrap_or(0) <= 2
                }) && lit_45 > 200
                {
                    Ok(())
                } else {
                    Err(format!(
                        "lit_floor_has_no_acne: (elevation, max diff vs no shadow map, min, max) {worst:?} (want diff <= 1, spread <= 2, min at 45 deg > 200)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // Shadowed floor faces up: only the sky colour reaches it.
                let light = melon_core::Light {
                    sky_color: glam::Vec3::X,
                    ground_color: glam::Vec3::Z,
                    ..test_light()
                };
                let up = render_test_scene(renderer, &light, false, glam::Vec3::ZERO, 20.0);
                let px = pixel_at(renderer, &up, SHADOW_PX);
                // The floor's underside faces down: only the ground colour.
                let down = render_test_scene(renderer, &light, true, glam::Vec3::ZERO, 20.0);
                let below = pixel_at(renderer, &down, (TEST_SIZE / 2, TEST_SIZE / 2));
                if px[0] > px[2] && below[2] > below[0] {
                    Ok(())
                } else {
                    Err(format!(
                        "hemisphere_ambient: up-facing SHADOW_PX {px:?} (want red > blue), down-facing centre {below:?} (want blue > red)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // The box edge nearest FAR_LEFT_PX runs through the occluder,
                // and FAR_LEFT_PX sits within the box's depth range but past
                // that edge. Without the uv-range check, clamp-to-edge would
                // read the occluder's depth there and shadow it.
                let pixels = render_test_scene(
                    renderer,
                    &test_light(),
                    false,
                    glam::Vec3::new(0.12, 0.0, 0.0),
                    1.5,
                );
                let v = value_at(&pixels, FAR_LEFT_PX);
                if v > 200 {
                    Ok(())
                } else {
                    Err(format!(
                        "outside_shadow_box_is_lit: FAR_LEFT_PX={v}, want > 200"
                    ))
                }
            }),
            Box::new(|renderer| {
                let size = renderer
                    .create_hdr_target(winit::dpi::PhysicalSize::new(0, 0))
                    .size();
                if size == winit::dpi::PhysicalSize::new(1, 1) {
                    Ok(())
                } else {
                    Err(format!("hdr_target_clamps_zero_size: got {size:?}"))
                }
            }),
            Box::new(|renderer| {
                // Untonemapped, HDR 1.0 would read 255; a curve compresses it
                // and still keeps brighter values brighter.
                let one = tonemap_value(renderer, 1.0);
                let eight = tonemap_value(renderer, 8.0);
                if one < 250 && eight > one {
                    Ok(())
                } else {
                    Err(format!(
                        "tonemap_compresses_highlights: px(1.0)={one}, px(8.0)={eight}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let pixels =
                    render_test_scene(renderer, &test_light(), false, glam::Vec3::ZERO, 20.0);
                let v = value_at(&pixels, LIT_PX);
                if v > 200 {
                    Ok(())
                } else {
                    Err(format!(
                        "lit_floor_is_bright: expected > 200 at LIT_PX, got {v}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let mesh = renderer.cube_mesh();
                if mesh.index_count == 36 {
                    Ok(())
                } else {
                    Err(format!("expected 36 indices, got {}", mesh.index_count))
                }
            }),
            Box::new(|renderer| {
                let _ = renderer.surface_format();
                Ok(())
            }),
            Box::new(|renderer| {
                let _ = renderer.size();
                Ok(())
            }),
            Box::new(|renderer| {
                // Create a 2x2 RGBA8 texture from raw bytes and verify it can
                // be used to build a material bind group (proves the texture + layout
                // path works end-to-end without panicking).
                let pixels: [u8; 16] = [255; 16];
                let texture = renderer.create_texture_from_bytes(
                    &pixels,
                    2,
                    2,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                );
                let _bg = renderer.create_material_bind_group(
                    &CHECKER_MATERIAL,
                    [Some(&texture), None, None, None],
                );
                Ok(())
            }),
            Box::new(|renderer| {
                // Load the sample GLB and verify it has 1 mesh with 36 indices
                // and a material.
                let glb_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/models/cube.glb");
                let model = renderer
                    .load_gltf(glb_path)
                    .map_err(|e| format!("load_gltf failed: {e:?}"))?;
                if model.meshes.len() != 1 {
                    return Err(format!("expected 1 mesh, got {}", model.meshes.len()));
                }
                if model.meshes[0].index_count != 36 {
                    return Err(format!(
                        "expected 36 indices, got {}",
                        model.meshes[0].index_count
                    ));
                }
                if model.material_bind_groups.len() != 1 {
                    return Err(format!(
                        "expected 1 material bind group, got {}",
                        model.material_bind_groups.len()
                    ));
                }
                Ok(())
            }),
            Box::new(check_material_missing_texture),
            Box::new(check_texture_hot_reload),
            Box::new(check_alpha_mask_and_blend),
            Box::new(check_local_lights),
            Box::new(check_ibl_uniform_sky),
            Box::new(check_far_cascade_shadow),
            Box::new(check_local_light_shadows),
            Box::new(check_hdr_sky_is_drawn),
            Box::new(check_material_thumbnail),
            Box::new(|renderer| {
                // A packed `.gltf` whose buffer is a separate `.bin` loads
                // through the AssetServer (`packed_gltf`), not a placeholder cube.
                use melon_core::{AssetServer, Handle, Model};
                let root = temp_project("packed_external");
                let gltf = TWO_NODE_GLTF.replace(
                    r#""uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA""#,
                    r#""uri": "m.bin""#,
                );
                assert!(gltf.contains("m.bin"));
                std::fs::write(root.join("assets/models/m.gltf"), gltf).unwrap();
                let mut bin = vec![0u8; 36];
                bin[12..16].copy_from_slice(&1.0f32.to_le_bytes()); // (1, 0, 0)
                bin[28..32].copy_from_slice(&1.0f32.to_le_bytes()); // (0, 1, 0)
                std::fs::write(root.join("assets/models/m.bin"), bin).unwrap();
                let pak = root.join("game.pak");
                melon_core::pack::build_pack(&root, &[], &pak).unwrap();
                let mut server = AssetServer::new(root.clone());
                server.set_pack(Arc::new(melon_core::pack::Pack::open(&pak).unwrap()));
                let h: Handle<Model> = server.load("assets/models/m.gltf");
                let mut cache = ModelCache::new(renderer.scene());
                let bounds = cache.get(renderer.scene(), &mut server, h).bounds;
                let _ = std::fs::remove_dir_all(&root);
                if bounds.1.x >= 3.0 {
                    Ok(())
                } else {
                    Err(format!("placeholder cube instead of the model: {bounds:?}"))
                }
            }),
            Box::new(check_packed_materials_textures_and_sky),
            Box::new(check_masked_out_leaves_no_ao),
            Box::new(check_blend_sorted_far_to_near),
            Box::new(check_off_centre_light_lights_its_own_floor),
            Box::new(check_edge_quality_and_lights),
            Box::new(check_blend_material_thumbnail),
            Box::new(check_material_bad_texture_refs),
            Box::new(check_ibl_updates_in_place),
        ];

        #[cfg(target_os = "windows")]
        let event_loop = {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            let mut builder = EventLoop::builder();
            builder.with_any_thread(true);
            builder.build().expect("failed to create event loop")
        };
        #[cfg(not(target_os = "windows"))]
        let event_loop = EventLoop::new().expect("failed to create event loop");

        event_loop.set_control_flow(ControlFlow::Poll);
        let mut runner = GpuTestRunner::new(assertions);
        let _ = event_loop.run_app(&mut runner);

        for result in &runner.results {
            match result {
                Ok(()) => {}
                Err(msg) => panic!("{msg}"),
            }
        }
        assert!(
            !runner.results.is_empty(),
            "GPU tests did not run (renderer init may have failed)"
        );
    }

    #[test]
    fn gltf_bounds_follow_node_transforms() {
        let json = r#"{
            "asset": {"version": "2.0"},
            "scenes": [{"nodes": [0, 1]}],
            "nodes": [{"mesh": 0, "translation": [10, 0, 0]}, {}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "buffers": [{"byteLength": 36,
                         "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}],
            "bufferViews": [{"buffer": 0, "byteLength": 36}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                           "min": [-1, -1, -1], "max": [1, 1, 1]}]
        }"#;
        let (min, max) = gltf_bounds_from_slice(json.as_bytes()).unwrap().unwrap();
        assert!(
            (min - glam::Vec3::new(9.0, -1.0, -1.0)).length() < 1e-5,
            "{min}"
        );
        assert!(
            (max - glam::Vec3::new(11.0, 1.0, 1.0)).length() < 1e-5,
            "{max}"
        );
    }

    #[test]
    fn gltf_bounds_none_without_meshes() {
        let json = r#"{"asset": {"version": "2.0"}, "scenes": [{"nodes": [0]}], "nodes": [{}]}"#;
        assert!(gltf_bounds_from_slice(json.as_bytes()).unwrap().is_none());
    }

    #[test]
    fn gltf_bounds_of_cube_glb() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/models/cube.glb");
        let (min, max) = gltf_bounds(std::path::Path::new(path)).unwrap().unwrap();
        assert!(min.is_finite() && max.is_finite());
        assert!((max - min).min_element() > 0.0);
    }
}
