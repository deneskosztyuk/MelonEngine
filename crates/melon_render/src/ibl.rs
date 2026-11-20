//! Image-based ambient light. Diffuse: the irradiance of the environment as
//! 9 spherical-harmonic coefficients; specular: the environment prefiltered
//! per roughness into the mips of one equirect texture, plus a BRDF lookup
//! (the split-sum approximation). The source is the procedural sky, or an
//! HDR equirect image on `Environment`. Everything is computed on the CPU
//! when the sky changes (a few tens of thousands of samples) and uploaded.
//!
//! The procedural source keeps the look the renderer had before IBL: the
//! diffuse environment is the sky / ground hemisphere ambient (whose
//! irradiance is exactly the old `mix(ground, sky, n.y * 0.5 + 0.5)`
//! blend), the reflected environment is the drawn sky.

use std::f32::consts::{PI, TAU};

use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};
use half::f16;

/// Mips of the prefiltered texture; mip `i` has roughness `i / (LEVELS - 1)`.
pub const LEVELS: u32 = 6;
/// Size of mip 0 (equirect, 2:1).
pub const BASE_SIZE: (u32, u32) = (128, 64);
/// Edge of the BRDF lookup (x = n.v, y = roughness).
pub const BRDF_SIZE: u32 = 32;

const SH_GRID: (u32, u32) = (64, 32);
const FILTER_SAMPLES: u32 = 48;
const BRDF_SAMPLES: u32 = 64;

/// A decoded HDR equirect image (linear radiance, row 0 = straight up).
pub struct HdrImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Vec3>,
}

impl HdrImage {
    /// Decode any image file the `image` crate reads (`.hdr`, `.png`, ...).
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        let image = image::load_from_memory(bytes)?.to_rgb32f();
        Ok(Self {
            width: image.width(),
            height: image.height(),
            pixels: image
                .pixels()
                .map(|p| Vec3::new(p[0], p[1], p[2]))
                .collect(),
        })
    }

    /// Bilinear radiance along `dir`.
    fn sample(&self, dir: Vec3) -> Vec3 {
        let uv = equirect_uv(dir);
        let x = uv.x * self.width as f32 - 0.5;
        let y = (uv.y * self.height as f32 - 0.5).clamp(0.0, self.height as f32 - 1.0);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |x: f32, y: f32| {
            let xi = (x as i64).rem_euclid(self.width as i64) as usize;
            let yi = (y as usize).min(self.height as usize - 1);
            self.pixels[yi * self.width as usize + xi]
        };
        let top = at(x0, y0).lerp(at(x0 + 1.0, y0), fx);
        let bottom = at(x0, y0 + 1.0).lerp(at(x0 + 1.0, y0 + 1.0), fx);
        top.lerp(bottom, fy)
    }
}

/// Equirect coordinates of a unit direction: u around +Y (0.5 at +X), v
/// from 0 (up) to 1 (down). mesh.wgsl and sky.wgsl compute the same.
pub fn equirect_uv(dir: Vec3) -> Vec2 {
    Vec2::new(
        dir.z.atan2(dir.x) / TAU + 0.5,
        dir.y.clamp(-1.0, 1.0).acos() / PI,
    )
}

/// The unit direction of equirect coordinates (inverse of `equirect_uv`).
pub fn equirect_dir(u: f32, v: f32) -> Vec3 {
    let (phi, theta) = ((u - 0.5) * TAU, v * PI);
    Vec3::new(
        theta.sin() * phi.cos(),
        theta.cos(),
        theta.sin() * phi.sin(),
    )
}

/// What the environment is made of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyParams {
    pub zenith: Vec3,
    pub horizon: Vec3,
    /// `Light::ambient`, `sky_color` and `ground_color`.
    pub ambient: f32,
    pub sky_color: Vec3,
    pub ground_color: Vec3,
}

impl Default for SkyParams {
    fn default() -> Self {
        Self::new(
            &melon_core::Environment::default(),
            &melon_core::Light::default(),
        )
    }
}

impl SkyParams {
    pub fn new(environment: &melon_core::Environment, light: &melon_core::Light) -> Self {
        Self {
            zenith: environment.zenith_color,
            horizon: environment.horizon_color,
            ambient: light.ambient,
            sky_color: light.sky_color,
            ground_color: light.ground_color,
        }
    }
}

/// Environment source: the procedural sky or an HDR image times `intensity`.
#[derive(Clone, Copy)]
pub enum Source<'a> {
    Procedural(SkyParams),
    Hdr(&'a HdrImage, f32),
}

impl Source<'_> {
    /// Radiance reflected by smooth surfaces: the drawn sky (as mesh.wgsl
    /// `sky_radiance`, without the sun disc) or the image.
    pub fn specular(&self, dir: Vec3) -> Vec3 {
        match self {
            Source::Procedural(p) => {
                let above = p.horizon.lerp(p.zenith, dir.y.max(0.0).sqrt());
                let t = ((-dir.y) / 0.3).clamp(0.0, 1.0);
                let below = p
                    .horizon
                    .lerp(p.ambient * p.ground_color, t * t * (3.0 - 2.0 * t));
                if dir.y >= 0.0 { above } else { below }
            }
            Source::Hdr(image, intensity) => image.sample(dir) * *intensity,
        }
    }

    /// Radiance the diffuse ambient integrates: the hemisphere ambient or
    /// the image.
    pub fn diffuse(&self, dir: Vec3) -> Vec3 {
        match self {
            Source::Procedural(p) => {
                p.ambient
                    * if dir.y >= 0.0 {
                        p.sky_color
                    } else {
                        p.ground_color
                    }
            }
            Source::Hdr(image, intensity) => image.sample(dir) * *intensity,
        }
    }
}

/// The uniform the shaders read (group 0, binding 7).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct IblUniform {
    /// Irradiance / pi as SH coefficients (rgb, w unused): E(n) / pi =
    /// sum of `sh[i] * Y_i(n)`.
    pub sh: [[f32; 4]; 9],
    /// x highest mip, y 1 when the sky is drawn from the image, z image
    /// intensity.
    pub params: [f32; 4],
}

/// Everything computed for one environment.
pub struct IblData {
    pub uniform: IblUniform,
    /// RGBA16F texels (as bits) per mip, top to bottom, mip 0 first.
    pub levels: Vec<(u32, u32, Vec<[u16; 4]>)>,
    /// Mean diffuse radiance of the upper and lower hemisphere.
    pub mean_up: Vec3,
    pub mean_down: Vec3,
}

fn sh_basis(n: Vec3) -> [f32; 9] {
    [
        0.282_095,
        0.488_603 * n.y,
        0.488_603 * n.z,
        0.488_603 * n.x,
        1.092_548 * n.x * n.y,
        1.092_548 * n.y * n.z,
        0.315_392 * (3.0 * n.z * n.z - 1.0),
        1.092_548 * n.x * n.z,
        0.546_274 * (n.x * n.x - n.y * n.y),
    ]
}

/// Irradiance / pi of `source` as 9 SH coefficients (radiance projected,
/// then convolved with the clamped cosine: bands scaled by 1, 2/3, 1/4),
/// plus the mean radiance above and below the horizon.
fn project_diffuse(source: &Source) -> ([Vec3; 9], Vec3, Vec3) {
    let (w, h) = SH_GRID;
    let mut sh = [Vec3::ZERO; 9];
    let (mut up, mut down, mut up_weight, mut down_weight) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
    for j in 0..h {
        for i in 0..w {
            let (u, v) = ((i as f32 + 0.5) / w as f32, (j as f32 + 0.5) / h as f32);
            let dir = equirect_dir(u, v);
            let weight = (v * PI).sin() * (TAU / w as f32) * (PI / h as f32);
            let radiance = source.diffuse(dir);
            for (c, y) in sh.iter_mut().zip(sh_basis(dir)) {
                *c += radiance * y * weight;
            }
            if dir.y >= 0.0 {
                up += radiance * weight;
                up_weight += weight;
            } else {
                down += radiance * weight;
                down_weight += weight;
            }
        }
    }
    const BAND: [f32; 9] = [
        1.0,
        2.0 / 3.0,
        2.0 / 3.0,
        2.0 / 3.0,
        0.25,
        0.25,
        0.25,
        0.25,
        0.25,
    ];
    for (c, s) in sh.iter_mut().zip(BAND) {
        *c *= s;
    }
    (sh, up / up_weight, down / down_weight)
}

fn hammersley(i: u32, count: u32) -> Vec2 {
    Vec2::new(
        (i as f32 + 0.5) / count as f32,
        i.reverse_bits() as f32 * 2.328_306_4e-10,
    )
}

/// A half vector around +Z distributed by GGX with `alpha` = roughness^2.
fn importance_ggx(xi: Vec2, alpha: f32) -> Vec3 {
    let cos = ((1.0 - xi.x) / (1.0 + (alpha * alpha - 1.0) * xi.x)).sqrt();
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let phi = TAU * xi.y;
    Vec3::new(sin * phi.cos(), sin * phi.sin(), cos)
}

fn to_half(c: Vec3) -> [u16; 4] {
    // Clamp to the half range: a huge sun in an HDR image must not become inf.
    let h = |v: f32| f16::from_f32(v.clamp(0.0, 60000.0)).to_bits();
    [h(c.x), h(c.y), h(c.z), f16::ONE.to_bits()]
}

/// The environment prefiltered per roughness (GGX, with n = v = r).
fn prefilter(source: &Source) -> Vec<(u32, u32, Vec<[u16; 4]>)> {
    (0..LEVELS)
        .map(|level| {
            let (w, h) = (BASE_SIZE.0 >> level, BASE_SIZE.1 >> level);
            let roughness = level as f32 / (LEVELS - 1) as f32;
            let alpha = roughness * roughness;
            let mut texels = Vec::with_capacity((w * h) as usize);
            for j in 0..h {
                for i in 0..w {
                    let n = equirect_dir((i as f32 + 0.5) / w as f32, (j as f32 + 0.5) / h as f32);
                    if level == 0 {
                        texels.push(to_half(source.specular(n)));
                        continue;
                    }
                    let up = if n.y.abs() < 0.999 { Vec3::Y } else { Vec3::X };
                    let tangent = up.cross(n).normalize();
                    let bitangent = n.cross(tangent);
                    let (mut sum, mut weight) = (Vec3::ZERO, 0.0);
                    for s in 0..FILTER_SAMPLES {
                        let hv = importance_ggx(hammersley(s, FILTER_SAMPLES), alpha);
                        let half = tangent * hv.x + bitangent * hv.y + n * hv.z;
                        let l = 2.0 * n.dot(half) * half - n;
                        let nl = n.dot(l);
                        if nl > 0.0 {
                            sum += source.specular(l) * nl;
                            weight += nl;
                        }
                    }
                    texels.push(to_half(sum / weight.max(1e-4)));
                }
            }
            (w, h, texels)
        })
        .collect()
}

/// Everything the shaders need for `source`.
pub fn build(source: &Source) -> IblData {
    let (sh, mean_up, mean_down) = project_diffuse(source);
    let mut uniform = IblUniform {
        params: [(LEVELS - 1) as f32, 0.0, 1.0, 0.0],
        ..Default::default()
    };
    for (slot, c) in uniform.sh.iter_mut().zip(sh) {
        *slot = c.extend(0.0).to_array();
    }
    if let Source::Hdr(_, intensity) = source {
        uniform.params[1] = 1.0;
        uniform.params[2] = *intensity;
    }
    IblData {
        uniform,
        levels: prefilter(source),
        mean_up,
        mean_down,
    }
}

/// The split-sum scale and bias of F0 as `size` x `size` RG16F texels: x =
/// n.v, y = roughness.
pub fn brdf_lut(size: u32) -> Vec<[u16; 2]> {
    let mut texels = Vec::with_capacity((size * size) as usize);
    for j in 0..size {
        let roughness = ((j as f32 + 0.5) / size as f32).max(0.045);
        let alpha = roughness * roughness;
        for i in 0..size {
            let ndv = ((i as f32 + 0.5) / size as f32).max(1e-3);
            let v = Vec3::new((1.0 - ndv * ndv).sqrt(), 0.0, ndv);
            let (mut a, mut b) = (0.0, 0.0);
            for s in 0..BRDF_SAMPLES {
                let h = importance_ggx(hammersley(s, BRDF_SAMPLES), alpha);
                let l = 2.0 * v.dot(h) * h - v;
                let (ndl, ndh, vdh) = (l.z.max(0.0), h.z.max(0.0), v.dot(h).max(0.0));
                if ndl > 0.0 {
                    // Smith-GGX visibility times n.l, over the pdf.
                    let k = alpha / 2.0;
                    let g = |x: f32| x / (x * (1.0 - k) + k);
                    let g_vis = g(ndl) * g(ndv) * vdh / (ndh * ndv).max(1e-4);
                    let fc = (1.0 - vdh).powi(5);
                    a += (1.0 - fc) * g_vis;
                    b += fc * g_vis;
                }
            }
            let n = BRDF_SAMPLES as f32;
            texels.push([
                f16::from_f32(a / n).to_bits(),
                f16::from_f32(b / n).to_bits(),
            ]);
        }
    }
    texels
}

/// The GPU side: the SH uniform, the prefiltered texture and the BRDF
/// lookup (group 0, bindings 7 to 10 with the sampler).
pub struct IblGpu {
    pub uniform: wgpu::Buffer,
    pub specular: wgpu::TextureView,
    /// The prefiltered texture behind `specular`: rewritten in place by
    /// `rebuild`, never reallocated.
    specular_texture: wgpu::Texture,
    pub brdf: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    /// Mean diffuse radiance above and below the horizon of the last build.
    pub mean_up: Vec3,
    pub mean_down: Vec3,
}

fn upload_specular(device: &wgpu::Device, queue: &wgpu::Queue, data: &IblData) -> wgpu::Texture {
    use wgpu::util::DeviceExt;
    let bytes: Vec<u8> = data
        .levels
        .iter()
        .flat_map(|(_, _, texels)| bytemuck::cast_slice::<[u16; 4], u8>(texels).iter().copied())
        .collect();
    device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("ibl specular"),
            size: wgpu::Extent3d {
                width: BASE_SIZE.0,
                height: BASE_SIZE.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: LEVELS,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &bytes,
    )
}

/// Overwrite every mip of `texture` with `data`.
fn write_specular(queue: &wgpu::Queue, texture: &wgpu::Texture, data: &IblData) {
    for (level, (width, height, texels)) in data.levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 8),
                rows_per_image: Some(*height),
            },
            wgpu::Extent3d {
                width: *width,
                height: *height,
                depth_or_array_layers: 1,
            },
        );
    }
}

impl IblGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, source: &Source) -> Self {
        use wgpu::util::DeviceExt;
        let data = build(source);
        let brdf = device
            .create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("ibl brdf lookup"),
                    size: wgpu::Extent3d {
                        width: BRDF_SIZE,
                        height: BRDF_SIZE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rg16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                bytemuck::cast_slice(&brdf_lut(BRDF_SIZE)),
            )
            .create_view(&wgpu::TextureViewDescriptor::default());
        let specular_texture = upload_specular(device, queue, &data);
        Self {
            uniform: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ibl uniform"),
                contents: bytemuck::bytes_of(&data.uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            }),
            specular: specular_texture.create_view(&wgpu::TextureViewDescriptor::default()),
            specular_texture,
            brdf,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("ibl sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
            mean_up: data.mean_up,
            mean_down: data.mean_down,
        }
    }

    #[cfg(test)]
    pub fn specular_texture(&self) -> &wgpu::Texture {
        &self.specular_texture
    }

    /// Recompute for `source`: the uniform and the specular texture are
    /// rewritten in place, so no bind group changes.
    pub fn rebuild(&mut self, queue: &wgpu::Queue, source: &Source) {
        let data = build(source);
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&data.uniform));
        write_specular(queue, &self.specular_texture, &data);
        self.mean_up = data.mean_up;
        self.mean_down = data.mean_down;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_sky(radiance: f32) -> SkyParams {
        // Uniform: the same radiance in every direction, both ways.
        SkyParams {
            zenith: Vec3::splat(radiance),
            horizon: Vec3::splat(radiance),
            ambient: 1.0,
            sky_color: Vec3::splat(radiance),
            ground_color: Vec3::splat(radiance),
        }
    }

    /// Irradiance / pi in direction `n` from a built uniform.
    fn irradiance(data: &IblData, n: Vec3) -> Vec3 {
        let mut sum = Vec3::ZERO;
        for (c, y) in data.uniform.sh.iter().zip(sh_basis(n)) {
            sum += Vec3::new(c[0], c[1], c[2]) * y;
        }
        sum
    }

    fn texel(data: &IblData, level: usize, x: u32, y: u32) -> Vec3 {
        let (w, _, texels) = &data.levels[level];
        let t = texels[(y * w + x) as usize];
        Vec3::new(
            f16::from_bits(t[0]).to_f32(),
            f16::from_bits(t[1]).to_f32(),
            f16::from_bits(t[2]).to_f32(),
        )
    }

    #[test]
    fn a_uniform_sky_has_irradiance_equal_to_its_radiance() {
        let data = build(&Source::Procedural(white_sky(2.0)));
        for n in [
            Vec3::Y,
            Vec3::X,
            Vec3::NEG_Z,
            Vec3::new(1.0, 1.0, 1.0).normalize(),
        ] {
            let e = irradiance(&data, n);
            assert!(
                (e - Vec3::splat(2.0)).abs().max_element() < 0.02,
                "{n}: {e}"
            );
        }
    }

    #[test]
    fn the_hemisphere_sky_matches_the_old_ambient_blend() {
        let p = SkyParams::default();
        let data = build(&Source::Procedural(p));
        for n in [Vec3::Y, Vec3::NEG_Y, Vec3::X, Vec3::new(0.0, 0.6, 0.8)] {
            let old = p.ambient * p.ground_color.lerp(p.sky_color, n.y * 0.5 + 0.5);
            let new = irradiance(&data, n);
            // SH9 cannot hold the step exactly: within 6% of the old value.
            assert!(
                (new - old).abs().max_element() < 0.06 * old.max_element(),
                "{n}: old {old} new {new}"
            );
        }
    }

    #[test]
    fn prefiltering_a_uniform_sky_stays_uniform_and_rough_blurs_a_gradient() {
        let flat = build(&Source::Procedural(white_sky(3.0)));
        for level in 0..LEVELS as usize {
            let c = texel(&flat, level, 1, 1);
            assert!(
                (c - Vec3::splat(3.0)).abs().max_element() < 0.02,
                "level {level}: {c}"
            );
        }
        // The default sky: the sharp level has a horizon edge, the roughest
        // does not (up and down are closer together).
        let sky = build(&Source::Procedural(SkyParams::default()));
        let spread = |level: usize| {
            let h = sky.levels[level].1;
            (texel(&sky, level, 3, 1) - texel(&sky, level, 3, h - 2)).length()
        };
        assert!(
            spread(5) < spread(0) * 0.85,
            "{} vs {}",
            spread(5),
            spread(0)
        );
    }

    #[test]
    fn equirect_round_trips() {
        for d in [Vec3::X, Vec3::NEG_Z, Vec3::new(0.3, 0.5, -0.8).normalize()] {
            let uv = equirect_uv(d);
            assert!((equirect_dir(uv.x, uv.y) - d).length() < 1e-4, "{d}");
        }
    }

    #[test]
    fn brdf_lut_is_bounded_and_fresnel_bias_grows_at_grazing_angles() {
        let lut = brdf_lut(BRDF_SIZE);
        let at = |x: u32, y: u32| {
            let t = lut[(y * BRDF_SIZE + x) as usize];
            (f16::from_bits(t[0]).to_f32(), f16::from_bits(t[1]).to_f32())
        };
        let (scale, bias) = at(BRDF_SIZE - 2, 4);
        assert!(scale > 0.8 && scale <= 1.01 && bias < 0.1, "{scale} {bias}");
        let (_, grazing_bias) = at(1, 4);
        assert!(grazing_bias > bias);
        assert!(lut.iter().all(|t| f16::from_bits(t[0]).to_f32() <= 1.05));
    }
}
