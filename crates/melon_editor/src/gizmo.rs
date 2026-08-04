use melon_core::glam::Vec3;

pub const SNAP_MOVE: f32 = 0.25;
pub const SNAP_SCALE: f32 = 0.25;
pub const SNAP_ROTATE_DEG: f32 = 15.0;
pub const SNAP_ROTATE_RAD: f32 = SNAP_ROTATE_DEG * std::f32::consts::TAU / 360.0;

pub const PICK_THRESHOLD: f32 = 10.0;
pub const GIZMO_SIZE_PX: f32 = 80.0;
pub const DEGENERATE_CIRCLE_PX: f32 = 5.0;
pub const CIRCLE_SEGMENTS: usize = 32;
pub const SCALE_MIN: f32 = 0.01;
pub const SCALE_MAX: f32 = 100.0;

pub const AXIS_X: Vec3 = Vec3::X;
pub const AXIS_Y: Vec3 = Vec3::Y;
pub const AXIS_Z: Vec3 = Vec3::Z;

pub fn axis_color(index: usize) -> Vec3 {
    match index {
        0 => Vec3::new(1.0, 0.2, 0.2),
        1 => Vec3::new(0.2, 1.0, 0.2),
        2 => Vec3::new(0.2, 0.4, 1.0),
        _ => Vec3::ONE,
    }
}

pub fn hovered_color(index: usize) -> Vec3 {
    axis_color(index) * 0.5 + Vec3::new(0.8, 0.8, 0.0) * 0.5
}

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

pub fn plane_basis(axis: Vec3, plane_normal: Vec3) -> (Vec3, Vec3) {
    let tangent = plane_normal.cross(axis).normalize_or_zero();
    let bitangent = axis.cross(tangent).normalize_or_zero();
    (tangent, bitangent)
}

pub fn rotate_angle_in_plane(hit_point: Vec3, center: Vec3, tangent: Vec3, bitangent: Vec3) -> f32 {
    let v = (hit_point - center).normalize_or_zero();
    v.dot(tangent).atan2(v.dot(bitangent))
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

pub fn point_to_segment_dist_2d(
    point: melon_core::glam::Vec2,
    seg_start: melon_core::glam::Vec2,
    seg_end: melon_core::glam::Vec2,
) -> f32 {
    let seg_vec = seg_end - seg_start;
    let seg_len_sq = seg_vec.length_squared();
    if seg_len_sq < 1e-10 {
        return (point - seg_start).length();
    }
    let t = ((point - seg_start).dot(seg_vec) / seg_len_sq).clamp(0.0, 1.0);
    let closest = seg_start + seg_vec * t;
    (point - closest).length()
}

pub fn cube_handle_lines(center: Vec3, size: f32, _color: Vec3) -> Vec<[Vec3; 2]> {
    let s = size * 0.5;
    let corners = [
        center + Vec3::new(-s, -s, -s),
        center + Vec3::new(s, -s, -s),
        center + Vec3::new(s, s, -s),
        center + Vec3::new(-s, s, -s),
        center + Vec3::new(-s, -s, s),
        center + Vec3::new(s, -s, s),
        center + Vec3::new(s, s, s),
        center + Vec3::new(-s, s, s),
    ];
    let edges = [
        [0, 1],
        [1, 2],
        [2, 3],
        [3, 0],
        [4, 5],
        [5, 6],
        [6, 7],
        [7, 4],
        [0, 4],
        [1, 5],
        [2, 6],
        [3, 7],
    ];
    edges
        .iter()
        .map(|[a, b]| [corners[*a], corners[*b]])
        .collect()
}

pub fn circle_lines(
    center: Vec3,
    radius: f32,
    axis: Vec3,
    _color: Vec3,
    segments: usize,
) -> Vec<[Vec3; 2]> {
    let up = if axis.dot(Vec3::Y).abs() < 0.99 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let tangent = axis.cross(up).normalize();
    let bitangent = axis.cross(tangent).normalize();

    let points: Vec<Vec3> = (0..segments)
        .map(|i| {
            let angle = (i as f32 / segments as f32) * std::f32::consts::TAU;
            center + tangent * (radius * angle.cos()) + bitangent * (radius * angle.sin())
        })
        .collect();

    (0..segments)
        .map(|i| {
            let next = (i + 1) % segments;
            [points[i], points[next]]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn point_to_segment_on_segment() {
        let p = melon_core::glam::Vec2::new(5.0, 0.0);
        let s = melon_core::glam::Vec2::new(0.0, 0.0);
        let e = melon_core::glam::Vec2::new(10.0, 0.0);
        assert!((point_to_segment_dist_2d(p, s, e) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn point_to_segment_off_segment() {
        let p = melon_core::glam::Vec2::new(5.0, 3.0);
        let s = melon_core::glam::Vec2::new(0.0, 0.0);
        let e = melon_core::glam::Vec2::new(10.0, 0.0);
        assert!((point_to_segment_dist_2d(p, s, e) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn point_to_segment_past_end() {
        let p = melon_core::glam::Vec2::new(15.0, 0.0);
        let s = melon_core::glam::Vec2::new(0.0, 0.0);
        let e = melon_core::glam::Vec2::new(10.0, 0.0);
        assert!((point_to_segment_dist_2d(p, s, e) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn circle_lines_count() {
        let lines = circle_lines(Vec3::ZERO, 1.0, Vec3::Y, Vec3::ONE, 32);
        assert_eq!(lines.len(), 32);
    }

    #[test]
    fn cube_handle_lines_count() {
        let lines = cube_handle_lines(Vec3::ZERO, 0.1, Vec3::ONE);
        assert_eq!(lines.len(), 12);
    }
}
