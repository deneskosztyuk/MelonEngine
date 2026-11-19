use melon_core::Transform;
use melon_core::glam::{Mat4, Quat, Vec2, Vec3};
use melon_render::{OverlayDepth, OverlayDraw, OverlayShape};

use crate::GizmoMode;

pub const SNAP_MOVE: f32 = 0.25;
pub const SNAP_SCALE: f32 = 0.25;
pub const SNAP_ROTATE_DEG: f32 = 15.0;
pub const SNAP_ROTATE_RAD: f32 = SNAP_ROTATE_DEG * std::f32::consts::TAU / 360.0;

pub const GIZMO_SIZE_PX: f32 = 80.0;
/// Extra grab margin around every handle, in pixels.
pub const PICK_TOLERANCE_PX: f32 = 4.0;
pub const SCALE_MIN: f32 = 0.01;
pub const SCALE_MAX: f32 = 100.0;

pub const AXIS_X: Vec3 = Vec3::X;
pub const AXIS_Y: Vec3 = Vec3::Y;
pub const AXIS_Z: Vec3 = Vec3::Z;

pub fn ray_plane_intersect(
    origin: Vec3,
    dir: Vec3,
    plane_point: Vec3,
    plane_normal: Vec3,
) -> Option<Vec3> {
    let denom = dir.dot(plane_normal);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (plane_point - origin).dot(plane_normal) / denom;
    if t < 0.0 {
        return None;
    }
    Some(origin + dir * t)
}

pub fn project_onto_axis(vec: Vec3, axis: Vec3) -> f32 {
    vec.dot(axis)
}

pub fn compute_drag_plane(entity_pos: Vec3, axis: Vec3, camera_forward: Vec3) -> (Vec3, Vec3) {
    let cross = axis.cross(camera_forward);
    if cross.length() < 1e-4 {
        return (entity_pos, camera_forward);
    }
    let plane_normal = cross.cross(axis).normalize();
    (entity_pos, plane_normal)
}

pub fn wrap_angle_delta(delta: f32) -> f32 {
    let wrapped = (delta + std::f32::consts::PI) % std::f32::consts::TAU;
    let adjusted = if wrapped < 0.0 {
        wrapped + std::f32::consts::TAU
    } else {
        wrapped
    };
    adjusted - std::f32::consts::PI
}

pub fn snap_value(value: f32, snap: f32) -> f32 {
    if snap <= 0.0 {
        value
    } else {
        (value / snap).round() * snap
    }
}

pub fn snap_angle(radians: f32, snap_radians: f32) -> f32 {
    if snap_radians <= 0.0 {
        radians
    } else {
        (radians / snap_radians).round() * snap_radians
    }
}

// --- Unreal-style gizmo: handles as overlay shapes ---------------------------

/// A grabbable part of the gizmo. `Plane(i)` is the plane perpendicular to
/// axis i (it moves / scales along the other two).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoHandle {
    Axis(usize),
    Plane(usize),
    Center,
}

/// Where the gizmo is: its centre and axes in world space, and `size`, the
/// world length of `GIZMO_SIZE_PX` at its distance (constant screen size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GizmoPose {
    pub center: Vec3,
    pub rotation: Quat,
    pub size: f32,
}

impl GizmoPose {
    pub fn axis(&self, i: usize) -> Vec3 {
        self.rotation * [AXIS_X, AXIS_Y, AXIS_Z][i]
    }

    /// The two axes spanning `Plane(i)`.
    pub fn plane_axes(&self, i: usize) -> (Vec3, Vec3) {
        (self.axis((i + 1) % 3), self.axis((i + 2) % 3))
    }
}

// Proportions as fractions of `size` (pixels at the 80 px default).
pub const SHAFT_RADIUS: f32 = 2.5 / 80.0;
pub const CONE_RADIUS: f32 = 4.5 / 80.0;
pub const CONE_LENGTH: f32 = 18.0 / 80.0;
pub const TIP_CUBE: f32 = 10.0 / 80.0;
/// Rotate rings: tube radius = ring radius × `melon_render`'s `TORUS_TUBE`.
pub const PLANE_OFFSET: f32 = 0.3;
pub const PLANE_SIZE: f32 = 0.2;
pub const CENTER_SIZE: f32 = 10.0 / 80.0;

pub const HIGHLIGHT: [f32; 4] = [1.0, 0.85, 0.1, 1.0];
pub const CENTER_COLOR: [f32; 4] = [0.85, 0.85, 0.85, 1.0];
pub const PLANE_ALPHA: f32 = 0.5;

/// Display colour of axis `i` (theme X / Y / Z).
pub fn handle_color(i: usize) -> [f32; 4] {
    use crate::theme::colors::{AXIS_X, AXIS_Y, AXIS_Z};
    [AXIS_X, AXIS_Y, AXIS_Z][i].to_normalized_gamma_f32()
}

/// Unit shape placed from `origin` along `dir`: unit y → `dir * length`,
/// unit x / z → `radius`.
fn along(origin: Vec3, dir: Vec3, length: f32, radius: f32) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::new(radius, length, radius),
        Quat::from_rotation_arc(Vec3::Y, dir),
        origin,
    )
}

/// Unit quad / triangle (XZ, 0..1) placed with its corner at `corner`, x
/// along `a`, z along `b` (both already scaled).
fn flat(corner: Vec3, a: Vec3, b: Vec3) -> Mat4 {
    Mat4::from_cols(
        a.extend(0.0),
        a.cross(b).normalize_or_zero().extend(0.0),
        b.extend(0.0),
        corner.extend(1.0),
    )
}

/// Every shape of the gizmo with the handle it belongs to.
pub fn gizmo_parts(mode: GizmoMode, pose: &GizmoPose) -> Vec<(GizmoHandle, OverlayShape, Mat4)> {
    let s = pose.size;
    let c = pose.center;
    let mut parts = Vec::new();
    match mode {
        GizmoMode::Move | GizmoMode::Scale => {
            let tip = if mode == GizmoMode::Move {
                CONE_LENGTH
            } else {
                TIP_CUBE * 0.5
            };
            for i in 0..3 {
                let axis = pose.axis(i);
                let h = GizmoHandle::Axis(i);
                parts.push((
                    h,
                    OverlayShape::Cylinder,
                    along(c, axis, s * (1.0 - tip), s * SHAFT_RADIUS),
                ));
                if mode == GizmoMode::Move {
                    let base = c + axis * s * (1.0 - CONE_LENGTH);
                    parts.push((
                        h,
                        OverlayShape::Cone,
                        along(base, axis, s * CONE_LENGTH, s * CONE_RADIUS),
                    ));
                } else {
                    let cube = Mat4::from_scale_rotation_translation(
                        Vec3::splat(s * TIP_CUBE),
                        pose.rotation,
                        c + axis * s,
                    );
                    parts.push((h, OverlayShape::Cube, cube));
                }
                let (a, b) = pose.plane_axes(i);
                let corner = c + (a + b) * s * PLANE_OFFSET;
                let shape = if mode == GizmoMode::Move {
                    OverlayShape::Quad
                } else {
                    OverlayShape::Triangle
                };
                parts.push((
                    GizmoHandle::Plane(i),
                    shape,
                    flat(corner, a * s * PLANE_SIZE, b * s * PLANE_SIZE),
                ));
            }
            let centre = if mode == GizmoMode::Move {
                (
                    OverlayShape::Sphere,
                    Mat4::from_scale_rotation_translation(
                        Vec3::splat(s * CENTER_SIZE * 0.5),
                        pose.rotation,
                        c,
                    ),
                )
            } else {
                (
                    OverlayShape::Cube,
                    Mat4::from_scale_rotation_translation(
                        Vec3::splat(s * CENTER_SIZE),
                        pose.rotation,
                        c,
                    ),
                )
            };
            parts.push((GizmoHandle::Center, centre.0, centre.1));
        }
        GizmoMode::Rotate => {
            for i in 0..3 {
                let ring = Mat4::from_scale_rotation_translation(
                    Vec3::splat(s),
                    Quat::from_rotation_arc(Vec3::Y, pose.axis(i)),
                    c,
                );
                parts.push((GizmoHandle::Axis(i), OverlayShape::Torus, ring));
            }
        }
    }
    parts
}

/// The gizmo as overlay draws (all on top). `highlight` (hovered or dragged)
/// is drawn yellow.
pub fn gizmo_draws(
    mode: GizmoMode,
    pose: &GizmoPose,
    highlight: Option<GizmoHandle>,
) -> Vec<OverlayDraw> {
    gizmo_parts(mode, pose)
        .into_iter()
        .map(|(handle, shape, transform)| {
            let [r, g, b, a] = match handle {
                GizmoHandle::Axis(i) => handle_color(i),
                GizmoHandle::Plane(i) => {
                    let [r, g, b, _] = handle_color(i);
                    [r, g, b, PLANE_ALPHA]
                }
                GizmoHandle::Center => CENTER_COLOR,
            };
            // Highlighted handles turn yellow; plane handles stay see-through.
            let color = if highlight == Some(handle) {
                [HIGHLIGHT[0], HIGHLIGHT[1], HIGHLIGHT[2], a]
            } else {
                [r, g, b, a]
            };
            OverlayDraw {
                shape,
                transform,
                color,
                depth: OverlayDepth::OnTop,
            }
        })
        .collect()
}

/// Distance along the ray (`dir` unit) to its closest approach to point `p`,
/// and that closest distance. `None` when `p` is behind the origin.
fn ray_point(origin: Vec3, dir: Vec3, p: Vec3) -> Option<(f32, f32)> {
    let t = (p - origin).dot(dir);
    (t > 0.0).then(|| (t, (origin + dir * t - p).length()))
}

/// Closest approach between the ray and segment `a..b`: (ray distance,
/// fraction along the segment, gap).
fn ray_segment(origin: Vec3, dir: Vec3, a: Vec3, b: Vec3) -> Option<(f32, f32, f32)> {
    let seg = b - a;
    let len2 = seg.length_squared();
    if len2 < 1e-12 {
        return ray_point(origin, dir, a).map(|(t, d)| (t, 0.0, d));
    }
    let w = origin - a;
    let (b_, c, d, e) = (dir.dot(seg), len2, dir.dot(w), seg.dot(w));
    let denom = c - b_ * b_; // dir is unit, so a = 1
    let u = if denom.abs() < 1e-9 {
        0.0
    } else {
        ((e - b_ * d) / denom).clamp(0.0, 1.0)
    };
    let p = a + seg * u;
    ray_point(origin, dir, p).map(|(t, gap)| (t, u, gap))
}

/// The handle under the ray `origin + t * dir`, picked against the same
/// shapes the gizmo draws (widened by `tolerance` world units). The centre
/// handle wins wherever it is hit; otherwise the nearest hit wins.
pub fn pick_handle(
    mode: GizmoMode,
    pose: &GizmoPose,
    origin: Vec3,
    dir: Vec3,
    tolerance: f32,
) -> Option<GizmoHandle> {
    let dir = dir.normalize_or_zero();
    if dir == Vec3::ZERO || !pose.size.is_finite() || pose.size <= 0.0 {
        return None;
    }
    let (s, c) = (pose.size, pose.center);
    let mut best: Option<(f32, GizmoHandle)> = None;
    let mut consider = |t: f32, handle: GizmoHandle| {
        if best.is_none_or(|(bt, _)| t < bt) {
            best = Some((t, handle));
        }
    };
    match mode {
        GizmoMode::Move | GizmoMode::Scale => {
            if ray_point(origin, dir, c)
                .is_some_and(|(_, gap)| gap < s * CENTER_SIZE * 0.5 + tolerance)
            {
                return Some(GizmoHandle::Center);
            }
            let (tip_from, tip_radius) = match mode {
                GizmoMode::Move => (1.0 - CONE_LENGTH, CONE_RADIUS),
                _ => (1.0 - TIP_CUBE, TIP_CUBE * 0.5),
            };
            for i in 0..3 {
                let axis = pose.axis(i);
                let end = c + axis
                    * s
                    * if mode == GizmoMode::Scale {
                        1.0 + TIP_CUBE * 0.5
                    } else {
                        1.0
                    };
                if let Some((t, u, gap)) = ray_segment(origin, dir, c, end) {
                    let radius = if u >= tip_from {
                        tip_radius
                    } else {
                        SHAFT_RADIUS
                    };
                    if gap < s * radius + tolerance {
                        consider(t, GizmoHandle::Axis(i));
                    }
                }
                // Plane handle: hit the plane, then test the quad / triangle.
                let normal = axis;
                let denom = dir.dot(normal);
                if denom.abs() > 1e-6 {
                    let t = (c - origin).dot(normal) / denom;
                    if t > 0.0 {
                        let local = origin + dir * t - c;
                        let (a, b) = pose.plane_axes(i);
                        let tol = tolerance / s;
                        let (u, v) = (
                            local.dot(a) / s - PLANE_OFFSET,
                            local.dot(b) / s - PLANE_OFFSET,
                        );
                        let inside = match mode {
                            GizmoMode::Move => {
                                (-tol..=PLANE_SIZE + tol).contains(&u)
                                    && (-tol..=PLANE_SIZE + tol).contains(&v)
                            }
                            _ => u >= -tol && v >= -tol && u + v <= PLANE_SIZE + tol,
                        };
                        if inside {
                            consider(t, GizmoHandle::Plane(i));
                        }
                    }
                }
            }
        }
        GizmoMode::Rotate => {
            // Sample each ring: works at every viewing angle, edge-on included.
            const SAMPLES: usize = 96;
            let band = s * melon_render::TORUS_TUBE + tolerance;
            for i in 0..3 {
                let (a, b) = pose.plane_axes(i);
                for k in 0..SAMPLES {
                    let angle = std::f32::consts::TAU * k as f32 / SAMPLES as f32;
                    let p = c + (a * angle.cos() + b * angle.sin()) * s;
                    if let Some((t, gap)) = ray_point(origin, dir, p)
                        && gap < band
                    {
                        consider(t, GizmoHandle::Axis(i));
                    }
                }
            }
        }
    }
    best.map(|(_, handle)| handle)
}

// --- Drag math (world space; `start` is the world pose at press) ------------

/// Move along `axis` by how far the pointer's plane hit moved along it.
pub fn drag_move_axis(
    start: Transform,
    axis: Vec3,
    press_hit: Vec3,
    hit: Vec3,
    snap: bool,
) -> Transform {
    let mut delta = project_onto_axis(hit - press_hit, axis);
    if snap {
        delta = snap_value(delta, SNAP_MOVE);
    }
    Transform {
        translation: start.translation + axis * delta,
        ..start
    }
}

/// Move within the plane spanned by `axes`; each axis snaps on its own.
pub fn drag_move_plane(
    start: Transform,
    press_hit: Vec3,
    hit: Vec3,
    axes: [Vec3; 2],
    snap: bool,
) -> Transform {
    let delta = hit - press_hit;
    let offset = axes
        .iter()
        .map(|&axis| {
            let d = project_onto_axis(delta, axis);
            axis * if snap { snap_value(d, SNAP_MOVE) } else { d }
        })
        .sum::<Vec3>();
    Transform {
        translation: start.translation + offset,
        ..start
    }
}

/// Free move: hits are on the camera-facing plane through the gizmo centre,
/// so the distance from the camera stays the same. Snaps per world axis.
pub fn drag_move_center(start: Transform, press_hit: Vec3, hit: Vec3, snap: bool) -> Transform {
    let mut delta = hit - press_hit;
    if snap {
        delta = Vec3::new(
            snap_value(delta.x, SNAP_MOVE),
            snap_value(delta.y, SNAP_MOVE),
            snap_value(delta.z, SNAP_MOVE),
        );
    }
    Transform {
        translation: start.translation + delta,
        ..start
    }
}

/// Angle (radians, right-handed around `axis`) of where the ray hits the
/// plane through `center` perpendicular to `axis`. `None` when the plane is
/// seen nearly edge-on (use `screen_drag_angle` then) or is behind the ray.
pub fn rotate_angle(center: Vec3, axis: Vec3, origin: Vec3, dir: Vec3) -> Option<f32> {
    let dir = dir.normalize_or_zero();
    let denom = dir.dot(axis);
    if denom.abs() < 0.1 {
        return None;
    }
    let t = (center - origin).dot(axis) / denom;
    if t <= 0.0 {
        return None;
    }
    let p = origin + dir * t - center;
    let u = axis.any_orthonormal_vector();
    let w = axis.cross(u);
    Some(p.dot(w).atan2(p.dot(u)))
}

/// Edge-on ring fallback (Unreal style): the pointer's movement along the
/// ring's screen tangent, 0.01 rad per pixel.
pub fn screen_drag_angle(pointer_delta_px: Vec2, ring_tangent_px: Vec2) -> f32 {
    0.01 * pointer_delta_px.dot(ring_tangent_px.normalize_or_zero())
}

/// Rotate `start` by `delta_angle` around the world `axis`.
pub fn drag_rotate(start: Transform, axis: Vec3, delta_angle: f32, snap: bool) -> Transform {
    let angle = if snap {
        snap_angle(delta_angle, SNAP_ROTATE_RAD)
    } else {
        delta_angle
    };
    Transform {
        rotation: (Quat::from_axis_angle(axis, angle) * start.rotation).normalize(),
        ..start
    }
}

/// Scale the listed local axes (one for an axis handle, two for a plane
/// handle, all three for the centre) by `factor`, clamped to
/// `SCALE_MIN..SCALE_MAX`; snapping rounds the factor to `SNAP_SCALE`.
pub fn drag_scale(start: Transform, axes: &[usize], factor: f32, snap: bool) -> Transform {
    let mut factor = factor.clamp(SCALE_MIN, SCALE_MAX);
    if snap {
        factor = snap_value(factor, SNAP_SCALE).max(SNAP_SCALE);
    }
    let mut scale = start.scale;
    for &i in axes {
        scale[i] = (start.scale[i] * factor).clamp(SCALE_MIN, SCALE_MAX);
    }
    Transform { scale, ..start }
}

/// Uniform scale from screen drag since the press (Unreal style): right or
/// up grows, 100 px doubles.
pub fn uniform_scale_factor(pointer_delta_px: Vec2) -> f32 {
    1.0 + 0.01 * (pointer_delta_px.x - pointer_delta_px.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GizmoMode;
    use melon_render::{OverlayDepth, OverlayShape};

    fn pose() -> GizmoPose {
        GizmoPose {
            center: Vec3::ZERO,
            rotation: melon_core::glam::Quat::IDENTITY,
            size: 1.0,
        }
    }

    fn count(draws: &[melon_render::OverlayDraw], shape: OverlayShape) -> usize {
        draws.iter().filter(|d| d.shape == shape).count()
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn rotate_sweep_of_90_degrees_turns_90() {
        let a0 = rotate_angle(Vec3::ZERO, Vec3::Y, Vec3::new(1.0, 5.0, 0.0), Vec3::NEG_Y).unwrap();
        let a1 = rotate_angle(Vec3::ZERO, Vec3::Y, Vec3::new(0.0, 5.0, -1.0), Vec3::NEG_Y).unwrap();
        let delta = wrap_angle_delta(a1 - a0);
        assert!(
            (delta - std::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "{delta}"
        );
        let t = drag_rotate(Transform::default(), Vec3::Y, delta, false);
        assert!(
            t.rotation
                .angle_between(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))
                < 1e-4
        );
    }

    #[test]
    fn rotate_edge_on_returns_none() {
        assert_eq!(
            rotate_angle(Vec3::ZERO, Vec3::Y, Vec3::new(-5.0, 0.2, 0.0), Vec3::X),
            None
        );
    }

    #[test]
    fn edge_on_screen_drag_follows_ring_tangent() {
        let angle = screen_drag_angle(Vec2::new(50.0, 0.0), Vec2::new(2.0, 0.0));
        assert!((angle - 0.5).abs() < 1e-6);
    }

    #[test]
    fn plane_move_changes_only_plane_axes() {
        let t = drag_move_plane(
            Transform::default(),
            Vec3::ZERO,
            Vec3::new(0.4, 3.0, 0.7),
            [Vec3::Z, Vec3::X],
            false,
        );
        assert!(
            close(t.translation, Vec3::new(0.4, 0.0, 0.7)),
            "{}",
            t.translation
        );
    }

    #[test]
    fn center_move_keeps_depth_from_camera() {
        let start = Transform::default();
        let t = drag_move_center(start, Vec3::ZERO, Vec3::new(1.0, 1.0, 0.0), false);
        assert!(close(t.translation, Vec3::new(1.0, 1.0, 0.0)));
    }

    #[test]
    fn plane_scale_leaves_third_axis() {
        let start = Transform {
            scale: Vec3::new(1.0, 2.0, 3.0),
            ..Default::default()
        };
        let t = drag_scale(start, &[0, 2], 1.5, false);
        assert!(close(t.scale, Vec3::new(1.5, 2.0, 4.5)));
    }

    #[test]
    fn uniform_scale_factor_100px_doubles() {
        assert!((uniform_scale_factor(Vec2::new(100.0, 0.0)) - 2.0).abs() < 1e-6);
        assert!((uniform_scale_factor(Vec2::new(-50.0, 0.0)) - 0.5).abs() < 1e-6);
        assert!((uniform_scale_factor(Vec2::new(0.0, -100.0)) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn snapping_applies_to_each_drag() {
        let start = Transform::default();
        let moved = drag_move_axis(start, Vec3::X, Vec3::ZERO, Vec3::new(0.3, 0.0, 0.0), true);
        assert!(close(moved.translation, Vec3::new(0.25, 0.0, 0.0)));
        let turned = drag_rotate(start, Vec3::Y, 20f32.to_radians(), true);
        assert!(
            turned
                .rotation
                .angle_between(Quat::from_rotation_y(15f32.to_radians()))
                < 1e-4
        );
        let scaled = drag_scale(start, &[0], 1.3, true);
        assert!((scaled.scale.x - 1.25).abs() < 1e-5);
    }

    #[test]
    fn drags_under_rotated_scaled_parent_write_world_pose() {
        use melon_core::bevy_ecs::world::World;
        use melon_core::hierarchy::{set_world_transform, world_matrix};
        let mut world = World::new();
        let parent = world
            .spawn(Transform {
                translation: Vec3::new(5.0, 0.0, 0.0),
                rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                scale: Vec3::splat(2.0),
            })
            .id();
        let child = world
            .spawn((
                Transform::from_translation(Vec3::X),
                melon_core::ChildOf(parent),
            ))
            .id();
        let start = Transform::from_mat4(world_matrix(&world, child));
        let moved = drag_move_axis(start, Vec3::X, Vec3::ZERO, Vec3::X, false);
        set_world_transform(&mut world, child, moved);
        let now = world_matrix(&world, child).transform_point3(Vec3::ZERO);
        assert!(
            close(now, moved.translation),
            "{now} vs {}",
            moved.translation
        );
    }

    const EYE: Vec3 = Vec3::new(4.0, 3.0, 6.0);

    fn pick_at(mode: GizmoMode, target: Vec3) -> Option<GizmoHandle> {
        pick_handle(mode, &pose(), EYE, (target - EYE).normalize(), 0.02)
    }

    /// A point on each handle (where a user would aim).
    fn handle_points(mode: GizmoMode) -> Vec<(GizmoHandle, Vec3)> {
        let p = pose();
        let mut points = vec![];
        for i in 0..3 {
            let axis = p.axis(i);
            let (a, b) = p.plane_axes(i);
            match mode {
                GizmoMode::Rotate => {
                    let angle = 0.6f32;
                    points.push((
                        GizmoHandle::Axis(i),
                        (a * angle.cos() + b * angle.sin()) * p.size,
                    ));
                }
                _ => {
                    points.push((GizmoHandle::Axis(i), axis * 0.6 * p.size));
                    let inside = if mode == GizmoMode::Move {
                        PLANE_OFFSET + PLANE_SIZE * 0.5
                    } else {
                        PLANE_OFFSET + PLANE_SIZE * 0.3
                    };
                    points.push((GizmoHandle::Plane(i), (a + b) * inside * p.size));
                }
            }
        }
        if mode != GizmoMode::Rotate {
            points.push((GizmoHandle::Center, Vec3::ZERO));
        }
        points
    }

    #[test]
    fn ray_through_each_handle_picks_it() {
        for mode in [GizmoMode::Move, GizmoMode::Rotate, GizmoMode::Scale] {
            for (handle, point) in handle_points(mode) {
                assert_eq!(pick_at(mode, point), Some(handle), "{mode:?} {handle:?}");
            }
        }
    }

    #[test]
    fn ray_between_handles_picks_nothing() {
        let p = pose();
        let between = (p.axis(0) + p.axis(1)) * 0.9 * p.size;
        assert_eq!(pick_at(GizmoMode::Move, between), None);
        assert_eq!(pick_at(GizmoMode::Scale, between), None);
    }

    #[test]
    fn center_wins_over_planes() {
        assert_eq!(
            pick_at(GizmoMode::Move, Vec3::ZERO),
            Some(GizmoHandle::Center)
        );
        assert_eq!(
            pick_at(GizmoMode::Scale, Vec3::ZERO),
            Some(GizmoHandle::Center)
        );
    }

    #[test]
    fn rotate_ring_picked_on_band_only() {
        let diagonal = Vec3::new(1.0, 0.0, 1.0).normalize();
        let down = Vec3::NEG_Y;
        let on_ring = pick_handle(
            GizmoMode::Rotate,
            &pose(),
            diagonal + Vec3::Y * 5.0,
            down,
            0.02,
        );
        assert_eq!(on_ring, Some(GizmoHandle::Axis(1)));
        let inside = pick_handle(
            GizmoMode::Rotate,
            &pose(),
            diagonal * 0.5 + Vec3::Y * 5.0,
            down,
            0.02,
        );
        assert_eq!(inside, None);
    }

    #[test]
    fn axis_pointing_at_camera_is_finite() {
        let hit = pick_handle(GizmoMode::Move, &pose(), Vec3::Z * 5.0, Vec3::NEG_Z, 0.02);
        assert!(
            matches!(hit, Some(GizmoHandle::Center) | Some(GizmoHandle::Axis(2))),
            "{hit:?}"
        );
        for mode in [GizmoMode::Move, GizmoMode::Rotate, GizmoMode::Scale] {
            assert!(
                gizmo_draws(mode, &pose(), None)
                    .iter()
                    .all(|d| d.transform.is_finite())
            );
        }
    }

    #[test]
    fn move_builds_shafts_cones_planes_center() {
        let d = gizmo_draws(GizmoMode::Move, &pose(), None);
        assert_eq!(count(&d, OverlayShape::Cylinder), 3);
        assert_eq!(count(&d, OverlayShape::Cone), 3);
        assert_eq!(count(&d, OverlayShape::Quad), 3);
        assert_eq!(count(&d, OverlayShape::Sphere), 1);
        assert!(d.iter().all(|d| d.depth == OverlayDepth::OnTop));
    }

    #[test]
    fn rotate_builds_three_rings() {
        let d = gizmo_draws(GizmoMode::Rotate, &pose(), None);
        assert_eq!(count(&d, OverlayShape::Torus), 3);
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn scale_builds_shafts_cubes_triangles_center() {
        let d = gizmo_draws(GizmoMode::Scale, &pose(), None);
        assert_eq!(count(&d, OverlayShape::Cylinder), 3);
        assert_eq!(count(&d, OverlayShape::Cube), 4);
        assert_eq!(count(&d, OverlayShape::Triangle), 3);
    }

    #[test]
    fn highlighted_handle_is_yellow_others_keep_axis_colours() {
        let d = gizmo_draws(GizmoMode::Move, &pose(), Some(GizmoHandle::Axis(1)));
        let yellow: Vec<_> = d.iter().filter(|d| d.color == HIGHLIGHT).collect();
        assert_eq!(yellow.len(), 2, "shaft and cone of Y");
        let x = handle_color(0);
        assert!(
            d.iter()
                .any(|d| d.shape == OverlayShape::Cone && d.color == x)
        );
    }

    #[test]
    fn plane_handles_are_translucent() {
        let d = gizmo_draws(GizmoMode::Move, &pose(), None);
        assert!(
            d.iter()
                .filter(|d| d.shape == OverlayShape::Quad)
                .all(|d| d.color[3] == PLANE_ALPHA)
        );
    }

    #[test]
    fn ray_plane_hit() {
        let origin = Vec3::new(0.0, 5.0, 0.0);
        let dir = Vec3::new(0.0, -1.0, 0.0);
        let plane_point = Vec3::ZERO;
        let plane_normal = Vec3::Y;
        let hit = ray_plane_intersect(origin, dir, plane_point, plane_normal);
        assert!(hit.is_some());
        let hit = hit.unwrap();
        assert!((hit - Vec3::ZERO).length() < 1e-5, "should hit at origin");
    }

    #[test]
    fn ray_plane_miss_parallel() {
        let origin = Vec3::new(0.0, 5.0, 0.0);
        let dir = Vec3::new(1.0, 0.0, 0.0);
        let plane_normal = Vec3::Y;
        assert!(ray_plane_intersect(origin, dir, Vec3::ZERO, plane_normal).is_none());
    }

    #[test]
    fn ray_plane_behind_ray() {
        let origin = Vec3::new(0.0, 5.0, 0.0);
        let dir = Vec3::new(0.0, 1.0, 0.0);
        let plane_normal = Vec3::Y;
        assert!(ray_plane_intersect(origin, dir, Vec3::ZERO, plane_normal).is_none());
    }

    #[test]
    fn project_onto_axis_positive() {
        let v = Vec3::new(3.0, 1.0, 0.0);
        assert!((project_onto_axis(v, Vec3::X) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn project_onto_axis_negative() {
        let v = Vec3::new(-2.0, 0.0, 0.0);
        assert!((project_onto_axis(v, Vec3::X) - (-2.0)).abs() < 1e-6);
    }

    #[test]
    fn project_onto_axis_perpendicular() {
        let v = Vec3::new(0.0, 5.0, 0.0);
        assert!(project_onto_axis(v, Vec3::X).abs() < 1e-6);
    }

    #[test]
    fn compute_drag_plane_normal_case() {
        let (point, normal) = compute_drag_plane(Vec3::ZERO, Vec3::X, Vec3::new(0.0, 0.0, -1.0));
        assert!((point - Vec3::ZERO).length() < 1e-6);
        assert!(
            normal.dot(Vec3::X).abs() < 1e-5,
            "plane normal should be perpendicular to axis"
        );
    }

    #[test]
    fn compute_drag_plane_degenerate_fallback() {
        let (_point, normal) = compute_drag_plane(Vec3::ZERO, Vec3::X, Vec3::X);
        assert!(
            (normal - Vec3::X).length() < 1e-6,
            "should fall back to camera forward"
        );
    }

    #[test]
    fn wrap_angle_no_wrap() {
        assert!((wrap_angle_delta(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn wrap_angle_past_pi() {
        let result = wrap_angle_delta(std::f32::consts::PI + 0.5);
        assert!(
            (result - (-std::f32::consts::PI + 0.5)).abs() < 1e-5,
            "PI+0.5 should wrap to -PI+0.5, got {result}"
        );
    }

    #[test]
    fn wrap_angle_past_neg_pi() {
        let result = wrap_angle_delta(-std::f32::consts::PI - 0.5);
        assert!(
            (result - (std::f32::consts::PI - 0.5)).abs() < 1e-5,
            "-PI-0.5 should wrap to PI-0.5, got {result}"
        );
    }

    #[test]
    fn snap_value_exact_multiple() {
        assert!((snap_value(1.0, 0.25) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn snap_value_non_multiple() {
        assert!(
            (snap_value(1.1, 0.25) - 1.0).abs() < 1e-6
                || (snap_value(1.1, 0.25) - 1.25).abs() < 1e-6
        );
    }

    #[test]
    fn snap_value_zero_snap_no_snapping() {
        assert!((snap_value(1.37, 0.0) - 1.37).abs() < 1e-6);
    }

    #[test]
    fn snap_angle_exact() {
        let snap = 15.0_f32.to_radians();
        assert!((snap_angle(30.0_f32.to_radians(), snap) - 30.0_f32.to_radians()).abs() < 1e-5);
    }
}
