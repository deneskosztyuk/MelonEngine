//! Cascaded sun shadows: the camera frustum up to the shadow distance is cut
//! into 1 to 4 depth slices (a practical split: a mix of logarithmic and
//! uniform), each slice gets an orthographic shadow box fitted to its
//! bounding sphere, snapped to the shadow map's texel grid so shadows do
//! not shimmer when the camera moves or turns. Pure CPU math; the renderer
//! draws one shadow pass per cascade into a layer of the shadow map array.

use glam::{Mat4, Vec3};

/// Most cascades (layers of the shadow map).
pub const MAX_CASCADES: usize = 4;

/// Weight of the logarithmic split (the rest is uniform).
const SPLIT_LAMBDA: f32 = 0.75;
/// Each cascade starts this share of the previous one's far depth, so the
/// two overlap where the shader blends them.
const OVERLAP: f32 = 0.8;
/// Casters this far (metres) toward the sun from a cascade's centre still
/// cast into it.
const CASTER_REACH: f32 = 200.0;
/// Box radii are rounded up to a multiple of this, so they do not wobble
/// with float noise.
const RADIUS_STEP: f32 = 1.0 / 16.0;

/// The cascades of one view.
#[derive(Clone, Debug)]
pub struct CascadeSet {
    /// World -> shadow clip space, nearest cascade first.
    pub view_proj: Vec<Mat4>,
    /// Far view depth of each cascade (slots past `view_proj.len()` are 0).
    pub far: [f32; MAX_CASCADES],
    /// Camera axis the depths are measured along.
    pub forward: Vec3,
    /// Depth at which the last cascade fades out.
    pub distance: f32,
}

impl CascadeSet {
    /// One cascade given as a matrix, covering every depth (direct drawing
    /// with a hand-made shadow box).
    pub fn single(view_proj: Mat4) -> Self {
        Self {
            view_proj: vec![view_proj],
            far: [1.0e9, 0.0, 0.0, 0.0],
            forward: Vec3::NEG_Z,
            distance: 1.0e9,
        }
    }
}

/// Far depth of each of `count` cascades between `near` and `distance`;
/// the last is `distance`.
pub fn splits(near: f32, distance: f32, count: usize) -> Vec<f32> {
    let near = near.max(0.3).min(distance * 0.5);
    (1..=count)
        .map(|i| {
            let t = i as f32 / count as f32;
            let log = near * (distance / near).powf(t);
            let uniform = near + (distance - near) * t;
            SPLIT_LAMBDA * log + (1.0 - SPLIT_LAMBDA) * uniform
        })
        .collect()
}

/// A camera's forward axis and its near and far depths along it.
pub fn camera_axis(view_proj: Mat4, eye: Vec3) -> (Vec3, f32, f32) {
    let inverse = view_proj.inverse();
    let near_point = inverse.project_point3(Vec3::new(0.0, 0.0, 0.0));
    let far_point = inverse.project_point3(Vec3::new(0.0, 0.0, 1.0));
    let forward = (far_point - near_point).normalize_or(Vec3::NEG_Z);
    (
        forward,
        (near_point - eye).dot(forward),
        (far_point - eye).dot(forward),
    )
}

/// The 8 corners of the camera frustum between view depths `a` and `b`.
fn slice_corners(view_proj: Mat4, eye: Vec3, forward: Vec3, a: f32, b: f32) -> [Vec3; 8] {
    let inverse = view_proj.inverse();
    let mut corners = [Vec3::ZERO; 8];
    for (i, corner) in corners.iter_mut().enumerate() {
        let (x, y) = (
            if i & 1 == 0 { -1.0 } else { 1.0 },
            if i & 2 == 0 { -1.0 } else { 1.0 },
        );
        let depth = if i & 4 == 0 { a } else { b };
        let near_point = inverse.project_point3(Vec3::new(x, y, 0.0));
        let far_point = inverse.project_point3(Vec3::new(x, y, 1.0));
        let (dn, df) = (
            (near_point - eye).dot(forward),
            (far_point - eye).dot(forward),
        );
        let t = (depth - dn) / (df - dn).max(1e-6);
        *corner = near_point + (far_point - near_point) * t;
    }
    corners
}

/// Fit `count` cascades to the camera of `view_proj` at `eye`, for a sun
/// shining from `to_light` (unit, toward the sun), out to `distance`
/// metres, snapped to the texels of `map_size`-sized shadow maps.
pub fn fit(
    view_proj: Mat4,
    eye: Vec3,
    to_light: Vec3,
    count: usize,
    distance: f32,
    map_size: u32,
) -> CascadeSet {
    let count = count.clamp(1, MAX_CASCADES);
    let (forward, near, far) = camera_axis(view_proj, eye);
    let near = near.max(1e-3);
    let distance = distance.min(far).max(near * 1.01);
    let ends = splits(near, distance, count);
    let up = if to_light.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    // Rotation to light space (the eye position does not matter for it).
    let rotation = glam::camera::rh::view::look_at_mat4(to_light, Vec3::ZERO, up);
    let mut set = CascadeSet {
        view_proj: Vec::new(),
        far: [0.0; MAX_CASCADES],
        forward,
        distance,
    };
    let mut start = near;
    for (i, &end) in ends.iter().enumerate() {
        set.far[i] = end;
        let corners = slice_corners(view_proj, eye, forward, start, end);
        let centre = corners.iter().sum::<Vec3>() / 8.0;
        let radius = corners
            .iter()
            .map(|c| c.distance(centre))
            .fold(0.0, f32::max);
        let radius = (radius / RADIUS_STEP).ceil() * RADIUS_STEP;
        // Snap the centre to the texel grid in light space (x, y only).
        let texel = 2.0 * radius / map_size as f32;
        let in_light = rotation.transform_point3(centre);
        let snapped = Vec3::new(
            (in_light.x / texel).round() * texel,
            (in_light.y / texel).round() * texel,
            in_light.z,
        );
        let centre = centre + rotation.inverse().transform_vector3(snapped - in_light);
        let eye_at = centre + to_light * (CASTER_REACH + radius);
        let view = glam::camera::rh::view::look_at_mat4(eye_at, centre, up);
        let projection = glam::camera::rh::proj::directx::orthographic(
            -radius,
            radius,
            -radius,
            radius,
            0.0,
            CASTER_REACH + 2.0 * radius,
        );
        set.view_proj.push(projection * view);
        start = end * OVERLAP;
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::Camera;

    fn camera(eye: Vec3, center: Vec3) -> (Mat4, Vec3) {
        let camera = Camera {
            eye,
            center,
            aspect: 16.0 / 9.0,
            ..Default::default()
        };
        (camera.view_proj(), camera.eye)
    }

    fn sun() -> Vec3 {
        Vec3::new(0.4, 1.0, 0.3).normalize()
    }

    #[test]
    fn split_distances_grow_and_end_at_the_shadow_distance() {
        for count in 1..=4 {
            let s = splits(0.1, 100.0, count);
            assert_eq!(s.len(), count);
            assert!((s[count - 1] - 100.0).abs() < 1e-3, "{s:?}");
            assert!(s.windows(2).all(|w| w[0] < w[1]), "{s:?}");
        }
        // A practical split: nearer than a uniform one, farther than a
        // purely logarithmic one.
        let s = splits(0.3, 100.0, 3);
        let (uniform, log) = (100.0 / 3.0, 0.3 * (100.0f32 / 0.3).powf(1.0 / 3.0));
        assert!(s[0] < uniform && s[0] > log, "{s:?}");
    }

    #[test]
    fn every_cascade_box_contains_its_slice() {
        let (view_proj, eye) = camera(Vec3::new(0.0, 2.0, 0.0), Vec3::new(1.0, 1.0, -10.0));
        let set = fit(view_proj, eye, sun(), 3, 90.0, 2048);
        assert_eq!(set.view_proj.len(), 3);
        let (forward, near, _) = camera_axis(view_proj, eye);
        let mut start = near;
        for (i, m) in set.view_proj.iter().enumerate() {
            let end = set.far[i];
            for c in slice_corners(view_proj, eye, forward, start, end) {
                let p = m.project_point3(c);
                assert!(
                    p.x.abs() <= 1.001 && p.y.abs() <= 1.001 && (0.0..=1.0).contains(&p.z),
                    "cascade {i} misses a corner: {p}"
                );
            }
            start = end * OVERLAP;
        }
        assert!((set.far[2] - 90.0).abs() < 1e-2);
    }

    #[test]
    fn small_camera_moves_and_turns_do_not_move_shadow_texels() {
        let size = 2048u32;
        let probe = Vec3::new(3.0, 0.0, -12.0);
        let texel_of = |eye: Vec3, center: Vec3| {
            let (view_proj, eye) = camera(eye, center);
            let set = fit(view_proj, eye, sun(), 3, 90.0, size);
            set.view_proj
                .iter()
                .map(|m| {
                    let p = m.project_point3(probe);
                    // Shadow-map texel coordinates of the probe.
                    (
                        (p.x * 0.5 + 0.5) * size as f32,
                        (p.y * 0.5 + 0.5) * size as f32,
                    )
                })
                .collect::<Vec<_>>()
        };
        let base = texel_of(Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.0, 1.0, -10.0));
        let moved = [
            texel_of(Vec3::new(0.013, 2.0, 0.021), Vec3::new(0.013, 1.0, -9.979)),
            texel_of(Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.05, 1.0, -10.0)),
        ];
        for other in moved {
            for (i, (a, b)) in base.iter().zip(&other).enumerate() {
                // The probe stays on the same sub-texel position.
                let (dx, dy) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
                let frac = |d: f32| (d - d.round()).abs();
                assert!(
                    frac(dx) < 0.02 && frac(dy) < 0.02,
                    "cascade {i} shimmers: {a:?} -> {b:?}"
                );
            }
        }
    }

    #[test]
    fn one_cascade_covers_the_whole_distance() {
        let (view_proj, eye) = camera(Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0));
        let set = fit(view_proj, eye, sun(), 1, 50.0, 1024);
        assert_eq!(set.view_proj.len(), 1);
        assert!((set.far[0] - 50.0).abs() < 1e-2);
    }
}
