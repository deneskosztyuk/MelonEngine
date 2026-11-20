//! Point and spot lights, clustered forward: the view frustum is cut into a
//! 16 x 9 x 24 grid of froxels (exponential depth slices), every light is
//! listed in the froxels its bounding sphere touches, and the mesh shader
//! loops over the light list of its fragment's froxel only, so cost scales
//! with lights per froxel, not total lights. The culling runs on the CPU
//! once per main view; the lists upload to storage buffers in group 3.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4Swizzles};
use wgpu::util::DeviceExt;

pub const CLUSTER_X: u32 = 16;
pub const CLUSTER_Y: u32 = 9;
pub const CLUSTER_Z: u32 = 24;
pub const CLUSTER_COUNT: usize = (CLUSTER_X * CLUSTER_Y * CLUSTER_Z) as usize;
/// Lights the GPU buffer holds (`RenderQuality::max_lights` caps it lower).
pub const LIGHT_CAPACITY: usize = 1024;
/// Entries of the shared light index list; overflow drops the rest.
pub const INDEX_CAPACITY: usize = 262_144;
/// Deepest slice boundary in metres, so a huge far plane does not stretch
/// the slices.
const MAX_CLUSTER_DEPTH: f32 = 500.0;

/// A point or spot light in world space, as `extract` hands it over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalLight {
    pub position: Vec3,
    /// Unit axis of a spot light (the way it shines).
    pub direction: Vec3,
    /// Linear colour times intensity.
    pub color: Vec3,
    pub range: f32,
    /// Spot cone (inner, outer) in radians; `None` = point light.
    pub spot: Option<(f32, f32)>,
    pub cast_shadows: bool,
}

/// One light as the shader reads it (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct GpuLight {
    /// xyz position, w range.
    pub position_range: [f32; 4],
    /// Linear colour times intensity.
    pub color: [f32; 4],
    /// xyz spot axis, w unused.
    pub direction: [f32; 4],
    /// x spot scale, y spot offset (`saturate(cos * x + y)`), z 1 for a
    /// spot, w shadow slot (-1 = none).
    pub spot: [f32; 4],
}

/// The cluster grid of a view (group 3, binding 12).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ClusterUniform {
    /// Target size in pixels.
    pub screen: [f32; 2],
    pub near: f32,
    /// Slices per natural-log unit of depth.
    pub depth_scale: f32,
    pub dims: [u32; 4],
    /// Camera forward axis (xyz).
    pub forward: [f32; 4],
}

impl ClusterUniform {
    /// One cluster, no lights: what views without lights bind.
    pub fn empty() -> Self {
        Self {
            screen: [1.0; 2],
            near: 1.0,
            depth_scale: 1.0,
            dims: [1; 4],
            forward: [0.0, 0.0, -1.0, 0.0],
        }
    }
}

/// The froxel grid of one camera.
pub struct ClusterGrid {
    view_proj: Mat4,
    eye: Vec3,
    forward: Vec3,
    near: f32,
    depth_scale: f32,
    screen: [f32; 2],
}

impl ClusterGrid {
    /// `view_proj` of a perspective or orthographic camera at `eye`;
    /// `screen` is the target size in pixels.
    pub fn new(view_proj: Mat4, eye: Vec3, screen: [f32; 2]) -> Self {
        let inverse = view_proj.inverse();
        let near_point = inverse.project_point3(Vec3::new(0.0, 0.0, 0.0));
        let far_point = inverse.project_point3(Vec3::new(0.0, 0.0, 1.0));
        let forward = (far_point - near_point).normalize_or(Vec3::NEG_Z);
        let near = (near_point - eye).dot(forward).max(1e-3);
        let far = (far_point - eye)
            .dot(forward)
            .min(MAX_CLUSTER_DEPTH)
            .max(near * 1.01);
        Self {
            view_proj,
            eye,
            forward,
            near,
            depth_scale: CLUSTER_Z as f32 / (far / near).ln(),
            screen,
        }
    }

    pub fn uniform(&self) -> ClusterUniform {
        ClusterUniform {
            screen: self.screen,
            near: self.near,
            depth_scale: self.depth_scale,
            dims: [CLUSTER_X, CLUSTER_Y, CLUSTER_Z, 0],
            forward: self.forward.extend(0.0).to_array(),
        }
    }

    /// Depth slice of a view depth (metres along the camera axis); the
    /// shader computes the same thing.
    pub fn slice(&self, depth: f32) -> u32 {
        (((depth.max(self.near) / self.near).ln() * self.depth_scale) as u32).min(CLUSTER_Z - 1)
    }

    /// Cluster index of x, y, z.
    pub fn index(x: u32, y: u32, z: u32) -> usize {
        (x + CLUSTER_X * (y + CLUSTER_Y * z)) as usize
    }

    /// Inclusive cluster ranges `[x0, x1, y0, y1, z0, z1]` a bounding sphere
    /// touches (conservative: its bounding box), `None` if it is off screen
    /// or in front of the near plane.
    pub fn range_of(&self, centre: Vec3, radius: f32) -> Option<[u32; 6]> {
        let depth = (centre - self.eye).dot(self.forward);
        if depth + radius < self.near {
            return None;
        }
        let (z0, z1) = (self.slice(depth - radius), self.slice(depth + radius));
        let (mut lo, mut hi) = (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN));
        let mut crosses = false;
        for corner in 0..8 {
            let offset = Vec3::new(
                if corner & 1 == 0 { -radius } else { radius },
                if corner & 2 == 0 { -radius } else { radius },
                if corner & 4 == 0 { -radius } else { radius },
            );
            let clip = self.view_proj * (centre + offset).extend(1.0);
            if clip.w <= 1e-4 {
                crosses = true;
                break;
            }
            let ndc = clip.xy() / clip.w;
            lo = lo.min(ndc);
            hi = hi.max(ndc);
        }
        let tiles = |lo: f32, hi: f32, count: u32| {
            let at = |v: f32| ((v.clamp(0.0, 1.0) * count as f32) as u32).min(count - 1);
            (at(lo), at(hi))
        };
        let (x0, x1, y0, y1);
        if crosses {
            (x0, x1, y0, y1) = (0, CLUSTER_X - 1, 0, CLUSTER_Y - 1);
        } else {
            if hi.x < -1.0 || lo.x > 1.0 || hi.y < -1.0 || lo.y > 1.0 {
                return None;
            }
            // NDC y points up, tile rows count down from the top.
            (x0, x1) = tiles(lo.x * 0.5 + 0.5, hi.x * 0.5 + 0.5, CLUSTER_X);
            (y0, y1) = tiles(0.5 - hi.y * 0.5, 0.5 - lo.y * 0.5, CLUSTER_Y);
        }
        Some([x0, x1, y0, y1, z0, z1])
    }
}

/// What one view's frame uploads.
#[derive(Debug, Default)]
pub struct ClusterData {
    pub lights: Vec<GpuLight>,
    /// (offset, count) into `indices`, per cluster.
    pub ranges: Vec<[u32; 2]>,
    pub indices: Vec<u32>,
    /// Lights dropped for `max_lights`.
    pub dropped: usize,
    /// Shaded lights, in `lights` order, as given to `build` (callers match
    /// shadow slots to them).
    pub source: Vec<usize>,
    /// The first `shadowed` of `lights` cast shadows (the nearest
    /// shadow-casting ones, up to the cap).
    pub shadowed: usize,
}

/// Pick the `max_lights` most important visible lights (brightest per
/// squared distance), put the nearest `shadow_cap` of the shadow-casting
/// ones first, and list them per cluster.
pub fn build(
    grid: &ClusterGrid,
    lights: &[LocalLight],
    max_lights: usize,
    shadow_cap: usize,
) -> ClusterData {
    let max_lights = max_lights.min(LIGHT_CAPACITY);
    let mut visible: Vec<(usize, [u32; 6], f32)> = lights
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            l.range > 0.0
                && l.range.is_finite()
                && l.position.is_finite()
                && l.color.is_finite()
                && l.direction.is_finite()
        })
        .filter_map(|(i, l)| {
            let range = grid.range_of(l.position, l.range)?;
            let importance = l.color.max_element() / (l.position.distance_squared(grid.eye) + 1.0);
            Some((i, range, importance))
        })
        .collect();
    let dropped = visible.len().saturating_sub(max_lights);
    visible.sort_by(|a, b| b.2.total_cmp(&a.2));
    visible.truncate(max_lights);
    let distance = |i: usize| lights[i].position.distance_squared(grid.eye);
    let mut casters: Vec<usize> = visible
        .iter()
        .map(|v| v.0)
        .filter(|&i| lights[i].cast_shadows)
        .collect();
    casters.sort_by(|&a, &b| distance(a).total_cmp(&distance(b)));
    casters.truncate(shadow_cap);
    // Shadowed lights first (nearest first), the rest in importance order.
    visible.sort_by_key(|v| casters.iter().position(|&c| c == v.0).unwrap_or(usize::MAX));
    let shadowed = casters.len();

    let mut counts = vec![0u32; CLUSTER_COUNT];
    let each_cluster = |range: &[u32; 6], mut f: Box<dyn FnMut(usize) + '_>| {
        for z in range[4]..=range[5] {
            for y in range[2]..=range[3] {
                for x in range[0]..=range[1] {
                    f(ClusterGrid::index(x, y, z));
                }
            }
        }
    };
    for (_, range, _) in &visible {
        each_cluster(range, Box::new(|c| counts[c] += 1));
    }
    let mut ranges = vec![[0u32; 2]; CLUSTER_COUNT];
    let mut total = 0u32;
    for (range, count) in ranges.iter_mut().zip(&counts) {
        let count = (*count).min((INDEX_CAPACITY as u32).saturating_sub(total));
        *range = [total, count];
        total += count;
    }
    let mut indices = vec![0u32; total as usize];
    let mut filled = vec![0u32; CLUSTER_COUNT];
    for (slot, (_, range, _)) in visible.iter().enumerate() {
        each_cluster(
            range,
            Box::new(|c| {
                if filled[c] < ranges[c][1] {
                    indices[(ranges[c][0] + filled[c]) as usize] = slot as u32;
                    filled[c] += 1;
                }
            }),
        );
    }
    let gpu = visible
        .iter()
        .map(|(i, _, _)| {
            let l = &lights[*i];
            let (scale, offset, kind) = match l.spot {
                Some((inner, outer)) => {
                    let (ci, co) = (inner.cos(), outer.cos());
                    let scale = 1.0 / (ci - co).max(1e-4);
                    (scale, -co * scale, 1.0)
                }
                None => (0.0, 0.0, 0.0),
            };
            GpuLight {
                position_range: l.position.extend(l.range).to_array(),
                color: l.color.extend(0.0).to_array(),
                direction: l.direction.extend(0.0).to_array(),
                spot: [scale, offset, kind, -1.0],
            }
        })
        .collect();
    ClusterData {
        lights: gpu,
        ranges,
        indices,
        dropped,
        source: visible.iter().map(|(i, _, _)| *i).collect(),
        shadowed,
    }
}

/// The storage buffers and uniform of one set of light data (group 3,
/// bindings 9 to 12).
pub struct LightBuffers {
    lights: wgpu::Buffer,
    ranges: wgpu::Buffer,
    indices: wgpu::Buffer,
    uniform: wgpu::Buffer,
}

impl LightBuffers {
    /// Empty: no lights, one cluster.
    pub fn new(device: &wgpu::Device) -> Self {
        let storage = |label, size: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            lights: storage("local lights", LIGHT_CAPACITY * size_of::<GpuLight>()),
            ranges: storage("cluster ranges", CLUSTER_COUNT * 8),
            indices: storage("cluster light indices", INDEX_CAPACITY * 4),
            uniform: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("cluster uniform"),
                contents: bytemuck::bytes_of(&ClusterUniform::empty()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            }),
        }
    }

    /// Bind group entries at bindings 9 to 12.
    pub fn entries(&self) -> [wgpu::BindGroupEntry<'_>; 4] {
        [
            (9, &self.lights),
            (10, &self.ranges),
            (11, &self.indices),
            (12, &self.uniform),
        ]
        .map(|(binding, buffer)| wgpu::BindGroupEntry {
            binding,
            resource: buffer.as_entire_binding(),
        })
    }

    /// Uploads a frame's data (applies at the next submit).
    pub fn write(&self, queue: &wgpu::Queue, uniform: &ClusterUniform, data: &ClusterData) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(uniform));
        if data.ranges.is_empty() {
            queue.write_buffer(&self.ranges, 0, &[0u8; CLUSTER_COUNT * 8]);
        } else {
            queue.write_buffer(&self.ranges, 0, bytemuck::cast_slice(&data.ranges));
        }
        if !data.lights.is_empty() {
            queue.write_buffer(&self.lights, 0, bytemuck::cast_slice(&data.lights));
        }
        if !data.indices.is_empty() {
            queue.write_buffer(&self.indices, 0, bytemuck::cast_slice(&data.indices));
        }
    }

    /// Layout entries of bindings 9 to 12 for the lighting group.
    pub fn layout_entries() -> [wgpu::BindGroupLayoutEntry; 4] {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let storage = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        [
            entry(9, storage),
            entry(10, storage),
            entry(11, storage),
            entry(
                12,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::Camera;

    fn camera() -> (Mat4, Vec3) {
        let camera = Camera {
            eye: Vec3::ZERO,
            center: Vec3::new(0.0, 0.0, -10.0),
            aspect: 16.0 / 9.0,
            ..Default::default()
        };
        (camera.view_proj(), camera.eye)
    }

    fn grid() -> ClusterGrid {
        let (view_proj, eye) = camera();
        ClusterGrid::new(view_proj, eye, [1600.0, 900.0])
    }

    fn point(position: Vec3, range: f32) -> LocalLight {
        LocalLight {
            position,
            direction: Vec3::NEG_Y,
            color: Vec3::splat(10.0),
            range,
            spot: None,
            cast_shadows: false,
        }
    }

    #[test]
    fn slices_grow_with_depth_and_cover_near_to_far() {
        let g = grid();
        assert_eq!(g.slice(0.0), 0);
        assert_eq!(g.slice(g.near), 0);
        assert_eq!(g.slice(1.0e6), CLUSTER_Z - 1);
        let slices: Vec<u32> = [0.2, 1.0, 5.0, 20.0, 80.0]
            .into_iter()
            .map(|d| g.slice(d))
            .collect();
        assert!(slices.windows(2).all(|w| w[0] < w[1]), "{slices:?}");
    }

    #[test]
    fn a_centred_light_covers_the_middle_tiles_at_its_depth_only() {
        let g = grid();
        let [x0, x1, y0, y1, z0, z1] = g.range_of(Vec3::new(0.0, 0.0, -10.0), 1.0).unwrap();
        assert!(x0 <= CLUSTER_X / 2 && CLUSTER_X / 2 <= x1);
        assert!(y0 <= CLUSTER_Y / 2 && CLUSTER_Y / 2 <= y1);
        assert!(
            x0 > 0 && x1 < CLUSTER_X - 1,
            "not the whole width: {x0}..{x1}"
        );
        assert!(y0 > 0 && y1 < CLUSTER_Y - 1);
        assert!(z0 <= g.slice(10.0) && g.slice(10.0) <= z1);
        assert!(
            z1 - z0 <= 2,
            "a 2 m ball at 10 m spans few slices: {z0}..{z1}"
        );
    }

    #[test]
    fn lights_behind_the_camera_or_off_to_the_side_are_in_no_cluster() {
        let g = grid();
        assert!(g.range_of(Vec3::new(0.0, 0.0, 10.0), 1.0).is_none());
        assert!(g.range_of(Vec3::new(100.0, 0.0, -10.0), 1.0).is_none());
        let data = build(&g, &[point(Vec3::new(0.0, 0.0, 10.0), 1.0)], 16, 0);
        assert!(data.lights.is_empty() && data.indices.is_empty());
    }

    #[test]
    fn each_light_is_listed_in_exactly_its_clusters() {
        let g = grid();
        let lights = [
            point(Vec3::new(0.0, 0.0, -10.0), 1.0),
            point(Vec3::new(6.0, 2.0, -20.0), 3.0),
        ];
        let data = build(&g, &lights, 16, 0);
        assert_eq!(data.lights.len(), 2);
        for (slot, &source) in data.source.iter().enumerate() {
            let l = lights[source];
            let [x0, x1, y0, y1, z0, z1] = g.range_of(l.position, l.range).unwrap();
            for z in 0..CLUSTER_Z {
                for y in 0..CLUSTER_Y {
                    for x in 0..CLUSTER_X {
                        let [offset, count] = data.ranges[ClusterGrid::index(x, y, z)];
                        let listed = data.indices[offset as usize..(offset + count) as usize]
                            .contains(&(slot as u32));
                        let inside = (x0..=x1).contains(&x)
                            && (y0..=y1).contains(&y)
                            && (z0..=z1).contains(&z);
                        assert_eq!(listed, inside, "light {slot} cluster ({x},{y},{z})");
                    }
                }
            }
        }
    }

    #[test]
    fn the_cap_keeps_the_nearest_brightest_lights() {
        let g = grid();
        let lights = [
            point(Vec3::new(0.0, 0.0, -40.0), 2.0),
            point(Vec3::new(0.0, 0.0, -5.0), 2.0),
            point(Vec3::new(0.0, 0.0, -12.0), 2.0),
        ];
        let data = build(&g, &lights, 2, 0);
        assert_eq!(data.dropped, 1);
        let mut kept = data.source.clone();
        kept.sort();
        assert_eq!(kept, [1, 2]);
    }

    #[test]
    fn the_shadow_cap_keeps_the_nearest_casters_first() {
        let g = grid();
        let caster = |z: f32| LocalLight {
            cast_shadows: true,
            ..point(Vec3::new(0.0, 0.0, z), 2.0)
        };
        let lights = [
            caster(-30.0),
            point(Vec3::new(0.0, 0.0, -3.0), 2.0),
            caster(-8.0),
            caster(-15.0),
            caster(-4.0),
        ];
        let data = build(&g, &lights, 16, 2);
        assert_eq!(data.shadowed, 2);
        // The two nearest casters, nearest first, ahead of everything else.
        assert_eq!(&data.source[..2], [4, 2]);
        assert_eq!(build(&g, &lights, 16, 0).shadowed, 0);
        assert_eq!(build(&g, &lights, 16, 9).shadowed, 4);
    }

    #[test]
    fn spot_cone_becomes_a_scale_and_offset() {
        let g = grid();
        let spot = LocalLight {
            spot: Some((0.3, 0.6)),
            ..point(Vec3::new(0.0, 0.0, -10.0), 5.0)
        };
        let l = build(&g, &[spot], 4, 0).lights[0];
        let at = |angle: f32| (angle.cos() * l.spot[0] + l.spot[1]).clamp(0.0, 1.0);
        assert!((at(0.3) - 1.0).abs() < 1e-4 && at(0.6).abs() < 1e-4 && at(0.45) > 0.3);
        assert_eq!(l.spot[2], 1.0);
    }


    #[test]
    fn tiles_follow_the_screen_right_is_high_x_up_is_low_y() {
        let g = grid();
        let at = |p: Vec3| g.range_of(p, 0.5).unwrap();
        let (right, left) = (at(Vec3::new(5.0, 0.0, -10.0)), at(Vec3::new(-5.0, 0.0, -10.0)));
        assert!(left[1] < right[0], "left {left:?} right {right:?}");
        let (up, down) = (at(Vec3::new(0.0, 3.0, -10.0)), at(Vec3::new(0.0, -3.0, -10.0)));
        assert!(up[3] < down[2], "up {up:?} down {down:?}");
    }

    #[test]
    fn a_light_straddling_a_slice_boundary_is_in_both_slices() {
        let g = grid();
        // Depth where slice 3 starts: near * exp(3 / depth_scale).
        let boundary = g.near * (3.0 / g.depth_scale).exp();
        let [.., z0, z1] = g.range_of(Vec3::new(0.0, 0.0, -boundary), 0.2).unwrap();
        assert!(z0 <= 2 && z1 >= 3, "{z0}..{z1} at boundary depth {boundary}");
    }

    #[test]
    fn broken_lights_are_dropped_not_shaded() {
        let g = grid();
        let good = point(Vec3::new(0.0, 0.0, -10.0), 3.0);
        let bad = [
            LocalLight { color: Vec3::splat(f32::NAN), ..good },
            LocalLight { color: Vec3::splat(f32::INFINITY), ..good },
            LocalLight { position: Vec3::new(f32::NAN, 0.0, -10.0), ..good },
            LocalLight { range: f32::NAN, ..good },
            LocalLight { range: f32::INFINITY, ..good },
            LocalLight { range: 0.0, ..good },
        ];
        for l in bad {
            let data = build(&g, &[l, good], 16, 4);
            assert_eq!(data.source, [1], "{l:?}");
            assert!(data.lights.iter().all(|l| l.color.iter().all(|c| c.is_finite())));
        }
    }

    #[test]
    fn a_spot_with_inner_at_or_past_outer_stays_finite() {
        let g = grid();
        for (inner, outer) in [(0.5, 0.5), (0.8, 0.4), (0.0, 0.0)] {
            let spot = LocalLight {
                spot: Some((inner, outer)),
                ..point(Vec3::new(0.0, 0.0, -10.0), 5.0)
            };
            let l = build(&g, &[spot], 4, 0).lights[0];
            assert!(l.spot.iter().all(|v| v.is_finite()), "{inner} {outer}: {:?}", l.spot);
        }
    }

    #[test]
    fn no_lights_and_one_over_the_cap() {
        let g = grid();
        let none = build(&g, &[], 16, 4);
        assert!(none.lights.is_empty() && none.indices.is_empty() && none.dropped == 0);
        assert_eq!(none.ranges.len(), CLUSTER_COUNT);
        let many: Vec<_> = (0..=LIGHT_CAPACITY)
            .map(|i| point(Vec3::new(i as f32 * 0.001, 0.0, -10.0), 1.0))
            .collect();
        let data = build(&g, &many, usize::MAX, 4);
        assert_eq!(data.lights.len(), LIGHT_CAPACITY);
        assert_eq!(data.dropped, 1);
        assert!(data.indices.len() <= INDEX_CAPACITY);
        assert!(data.indices.iter().all(|&i| (i as usize) < LIGHT_CAPACITY));
    }

    #[test]
    fn gpu_structs_are_16_byte_multiples() {
        assert_eq!(size_of::<GpuLight>(), 64);
        assert_eq!(size_of::<ClusterUniform>(), 48);
    }
}
