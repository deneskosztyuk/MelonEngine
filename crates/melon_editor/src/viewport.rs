//! Scene viewport logic that does not depend on the UI library: camera
//! navigation (Unity controls), click picking and the grid / selection lines.
//! The UI glue only fills a `ViewportInput` each frame.

use bevy_ecs::entity::Entity;
use bevy_ecs::query::With;
use bevy_ecs::world::World;
use melon_core::glam::{Vec2, Vec3};
use melon_core::{Camera, MeshRef, Transform};
use melon_render::LineVertex;

/// Radians of look/orbit per point of pointer movement.
const LOOK_SENSITIVITY: f32 = 0.0025;
/// Pan distance per point, as a fraction of the distance to the pivot.
const PAN_SENSITIVITY: f32 = 0.0015;
const PITCH_LIMIT: f32 = 89.0 * std::f32::consts::PI / 180.0;
const MIN_DISTANCE: f32 = 0.2;
const MIN_FLY_SPEED: f32 = 0.5;
const MAX_FLY_SPEED: f32 = 100.0;
const SHIFT_MULTIPLIER: f32 = 3.0;
/// Half size of every mesh today: the unit cube (`MeshRef::Cube` and `cube.glb`).
const MESH_HALF_EXTENT: f32 = 0.5;

const GRID_HALF_SIZE: i32 = 50;
/// The grid sits a hair above y = 0 so it does not z-fight a floor whose top is y = 0.
const GRID_Y: f32 = 0.005;
const GRID_MINOR_COLOR: [f32; 3] = [0.25, 0.25, 0.25];
const GRID_MAJOR_COLOR: [f32; 3] = [0.45, 0.45, 0.45];
pub const AXIS_X_COLOR: [f32; 3] = [0.9, 0.15, 0.15];
pub const AXIS_Z_COLOR: [f32; 3] = [0.15, 0.35, 0.9];
const SELECTION_COLOR: [f32; 3] = [1.0, 0.55, 0.0];

/// Pressed fly-mode keys (only used while the right button is held).
#[derive(Default, Clone, Copy, Debug)]
pub struct FlyKeys {
    pub w: bool,
    pub a: bool,
    pub s: bool,
    pub d: bool,
    pub q: bool,
    pub e: bool,
}

/// One frame of viewport input, in viewport-local points (origin top-left).
/// `*_down` means "a drag that started on the viewport is held", so a drag
/// begun over another panel never moves the camera.
#[derive(Default, Clone, Copy, Debug)]
pub struct ViewportInput {
    pub size: Vec2,
    pub pointer: Option<Vec2>,
    pub pointer_delta: Vec2,
    pub hovered: bool,
    pub left_down: bool,
    pub right_down: bool,
    pub middle_down: bool,
    pub left_clicked: bool,
    /// Scroll in notches; positive = away from the user (zoom in).
    pub scroll: f32,
    pub alt: bool,
    pub shift: bool,
    pub keys: FlyKeys,
    pub f_pressed: bool,
    /// A text field has keyboard focus: ignore viewport input.
    pub typing: bool,
}

/// Editor (scene view) camera: orbits a pivot at `distance`.
#[derive(Clone, Copy, Debug)]
pub struct EditorCamera {
    pub pivot: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub fly_speed: f32,
}

impl Default for EditorCamera {
    /// Pivot at the origin, eye at (0, 4, 8).
    fn default() -> Self {
        let offset = Vec3::new(0.0, 4.0, 8.0);
        let forward = -offset.normalize();
        Self {
            pivot: Vec3::ZERO,
            distance: offset.length(),
            yaw: forward.z.atan2(forward.x),
            pitch: forward.y.asin(),
            fly_speed: 5.0,
        }
    }
}

impl EditorCamera {
    pub fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.cos() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.sin() * self.pitch.cos(),
        )
    }

    pub fn eye(&self) -> Vec3 {
        self.pivot - self.forward() * self.distance
    }

    /// Apply one frame of Unity-style navigation.
    pub fn apply(&mut self, input: &ViewportInput, dt: f32) {
        let dragging = input.left_down || input.right_down || input.middle_down;
        if input.typing || !(input.hovered || dragging) {
            return;
        }
        let delta = input.pointer_delta;
        if input.right_down {
            // Look and fly around the eye; the pivot follows in front of it.
            let mut eye = self.eye();
            self.turn(delta);
            let forward = self.forward();
            let right = forward.cross(Vec3::Y).normalize_or_zero();
            let k = input.keys;
            let axis = |pos: bool, neg: bool| f32::from(u8::from(pos)) - f32::from(u8::from(neg));
            let dir = forward * axis(k.w, k.s) + right * axis(k.d, k.a) + Vec3::Y * axis(k.e, k.q);
            let boost = if input.shift { SHIFT_MULTIPLIER } else { 1.0 };
            eye += dir.normalize_or_zero() * self.fly_speed * boost * dt;
            if input.scroll != 0.0 {
                self.fly_speed = (self.fly_speed * 1.2f32.powf(input.scroll))
                    .clamp(MIN_FLY_SPEED, MAX_FLY_SPEED);
            }
            self.pivot = eye + forward * self.distance;
        } else if input.alt && input.left_down {
            self.turn(delta);
        } else if input.middle_down {
            let forward = self.forward();
            let right = forward.cross(Vec3::Y).normalize_or_zero();
            let up = right.cross(forward);
            let scale = self.distance * PAN_SENSITIVITY;
            self.pivot += (-right * delta.x + up * delta.y) * scale;
        } else if input.hovered && input.scroll != 0.0 {
            self.distance = (self.distance * 0.9f32.powf(input.scroll)).max(MIN_DISTANCE);
        }
    }

    fn turn(&mut self, delta: Vec2) {
        self.yaw += delta.x * LOOK_SENSITIVITY;
        self.pitch = (self.pitch - delta.y * LOOK_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Centre the pivot on `center` and back off until a sphere of `radius` fits.
    pub fn frame(&mut self, center: Vec3, radius: f32) {
        let half_fov = Camera::default().fov_y_radians / 2.0;
        self.pivot = center;
        self.distance = (radius / half_fov.tan()).max(MIN_DISTANCE);
    }

    /// Copy the pose into the render camera (fov, aspect and clip planes stay).
    pub fn write(&self, camera: &mut Camera) {
        camera.eye = self.eye();
        camera.center = self.pivot;
        camera.yaw = self.yaw;
        camera.pitch = self.pitch;
    }
}

/// Nearest entity with a mesh hit by the ray, tested against the mesh's local
/// bounds in the entity's own space, so rotated and scaled boxes are exact.
pub fn pick(world: &mut World, origin: Vec3, dir: Vec3) -> Option<Entity> {
    let mut query = world.query_filtered::<(Entity, &Transform), With<MeshRef>>();
    query
        .iter(world)
        .filter_map(|(entity, transform)| {
            let inverse = transform.to_mat4().inverse();
            if !inverse.is_finite() {
                return None; // zero scale
            }
            // Affine map: the hit distance t is the same in local and world space.
            let o = inverse.transform_point3(origin);
            let d = inverse.transform_vector3(dir);
            ray_unit_box(o, d).map(|t| (t, entity))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, entity)| entity)
}

/// Slab test against the box `[-0.5, 0.5]^3`. Returns the entry distance
/// (0 if the origin is inside), or `None` on a miss or a hit behind the origin.
fn ray_unit_box(o: Vec3, d: Vec3) -> Option<f32> {
    let (mut t_min, mut t_max) = (0.0f32, f32::INFINITY);
    for axis in 0..3 {
        let (o, d) = (o[axis], d[axis]);
        if d.abs() < 1e-12 {
            if o.abs() > MESH_HALF_EXTENT {
                return None;
            }
            continue;
        }
        let (t0, t1) = ((-MESH_HALF_EXTENT - o) / d, (MESH_HALF_EXTENT - o) / d);
        t_min = t_min.max(t0.min(t1));
        t_max = t_max.min(t0.max(t1));
        if t_min > t_max {
            return None;
        }
    }
    Some(t_min)
}

/// Centre and bounding-sphere radius of an entity's mesh, for framing.
/// `None` if the entity no longer exists.
pub fn frame_target(world: &World, entity: Entity) -> Option<(Vec3, f32)> {
    let transform = world.get::<Transform>(entity)?;
    Some((
        transform.translation,
        (transform.scale.abs() * MESH_HALF_EXTENT).length(),
    ))
}

/// Input for the camera: while a gizmo handle is being dragged the left button
/// belongs to the gizmo, so pressing Alt mid-drag must not also orbit.
pub fn camera_input(input: &ViewportInput, gizmo_dragging: bool) -> ViewportInput {
    ViewportInput {
        left_down: input.left_down && !gizmo_dragging,
        ..*input
    }
}

/// What a left-button click in the viewport does, by priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftAction {
    /// Alt held: the left button orbits the camera.
    Orbit,
    /// The pointer is on a gizmo handle (or dragging one).
    Gizmo,
    /// A plain click: pick the entity under the pointer.
    Select,
    None,
}

pub fn left_action(alt: bool, on_gizmo_handle: bool, clicked: bool) -> LeftAction {
    if alt {
        LeftAction::Orbit
    } else if on_gizmo_handle {
        LeftAction::Gizmo
    } else if clicked {
        LeftAction::Select
    } else {
        LeftAction::None
    }
}

/// Texture size for a viewport of `size_points` UI points: rounded to pixels,
/// never 0 on either side (a 0-sized wgpu texture is invalid).
pub fn viewport_pixel_size(
    size_points: Vec2,
    pixels_per_point: f32,
) -> winit::dpi::PhysicalSize<u32> {
    let px = (size_points * pixels_per_point).round().max(Vec2::ONE);
    winit::dpi::PhysicalSize::new(px.x as u32, px.y as u32)
}

fn line(a: Vec3, b: Vec3, color: [f32; 3]) -> [LineVertex; 2] {
    [
        LineVertex {
            position: a.to_array(),
            color,
        },
        LineVertex {
            position: b.to_array(),
            color,
        },
    ]
}

/// Ground grid on y = 0: 1 m spacing to ±50 m, every 10th line brighter, the
/// X axis red and the Z axis blue.
pub fn grid_lines() -> Vec<LineVertex> {
    let extent = GRID_HALF_SIZE as f32;
    let mut vertices = Vec::with_capacity(((2 * GRID_HALF_SIZE + 1) * 4) as usize);
    for i in -GRID_HALF_SIZE..=GRID_HALF_SIZE {
        let c = i as f32;
        let shade = if i % 10 == 0 {
            GRID_MAJOR_COLOR
        } else {
            GRID_MINOR_COLOR
        };
        let (along_x, along_z) = if i == 0 {
            (AXIS_X_COLOR, AXIS_Z_COLOR)
        } else {
            (shade, shade)
        };
        vertices.extend(line(
            Vec3::new(-extent, GRID_Y, c),
            Vec3::new(extent, GRID_Y, c),
            along_x,
        ));
        vertices.extend(line(
            Vec3::new(c, GRID_Y, -extent),
            Vec3::new(c, GRID_Y, extent),
            along_z,
        ));
    }
    vertices
}

/// Orange wire box around the mesh bounds under `transform` (12 edges).
pub fn selection_box_lines(transform: &Transform) -> Vec<LineVertex> {
    let m = transform.to_mat4();
    let h = MESH_HALF_EXTENT;
    let corner = |i: usize| {
        let sign = |bit: usize| if i & bit != 0 { h } else { -h };
        m.transform_point3(Vec3::new(sign(1), sign(2), sign(4)))
    };
    let mut vertices = Vec::with_capacity(24);
    for a in 0..8 {
        for bit in [1, 2, 4] {
            if a & bit == 0 {
                vertices.extend(line(corner(a), corner(a | bit), SELECTION_COLOR));
            }
        }
    }
    vertices
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::glam::Quat;
    use melon_core::{MeshRef, Name};

    fn approx(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    fn hovered() -> ViewportInput {
        ViewportInput {
            size: Vec2::new(800.0, 600.0),
            pointer: Some(Vec2::new(400.0, 300.0)),
            hovered: true,
            ..Default::default()
        }
    }

    #[test]
    fn default_camera_matches_spec() {
        let cam = EditorCamera::default();
        assert!(approx(cam.pivot, Vec3::ZERO));
        assert!(
            approx(cam.eye(), Vec3::new(0.0, 4.0, 8.0)),
            "eye {}",
            cam.eye()
        );
        assert_eq!(cam.fly_speed, 5.0);
    }

    #[test]
    fn orbit_keeps_distance_to_pivot() {
        let mut cam = EditorCamera::default();
        let (eye0, d0) = (cam.eye(), cam.distance);
        let input = ViewportInput {
            alt: true,
            left_down: true,
            pointer_delta: Vec2::new(40.0, 25.0),
            ..hovered()
        };
        cam.apply(&input, 0.016);
        assert!((cam.eye().distance(cam.pivot) - d0).abs() < 1e-4);
        assert!(!approx(cam.eye(), eye0));
        assert!(approx(cam.pivot, Vec3::ZERO));
    }

    #[test]
    fn pan_moves_eye_and_pivot_equally() {
        let mut cam = EditorCamera::default();
        let (eye0, piv0) = (cam.eye(), cam.pivot);
        let input = ViewportInput {
            middle_down: true,
            pointer_delta: Vec2::new(30.0, 0.0),
            ..hovered()
        };
        cam.apply(&input, 0.016);
        assert!(!approx(cam.pivot, piv0));
        assert!(((cam.eye() - eye0) - (cam.pivot - piv0)).length() < 1e-5);
    }

    #[test]
    fn dolly_shrinks_distance_with_floor() {
        let mut cam = EditorCamera::default();
        let d0 = cam.distance;
        cam.apply(
            &ViewportInput {
                scroll: 1.0,
                ..hovered()
            },
            0.016,
        );
        assert!((cam.distance - d0 * 0.9).abs() < 1e-4);
        cam.apply(
            &ViewportInput {
                scroll: 200.0,
                ..hovered()
            },
            0.016,
        );
        assert_eq!(cam.distance, 0.2);
    }

    #[test]
    fn fly_moves_along_view_and_shift_triples() {
        let fly = |shift: bool| {
            let mut cam = EditorCamera::default();
            let (eye0, fwd) = (cam.eye(), cam.forward());
            let input = ViewportInput {
                right_down: true,
                shift,
                keys: FlyKeys {
                    w: true,
                    ..Default::default()
                },
                ..hovered()
            };
            cam.apply(&input, 0.1);
            (cam.eye() - eye0, fwd)
        };
        let (moved, fwd) = fly(false);
        assert!(approx(moved, fwd * 0.5), "moved {moved}");
        let (moved, fwd) = fly(true);
        assert!(approx(moved, fwd * 1.5), "moved {moved}");
    }

    #[test]
    fn right_scroll_changes_fly_speed_within_bounds() {
        let mut cam = EditorCamera::default();
        let d0 = cam.distance;
        let scroll = |cam: &mut EditorCamera, notches: f32| {
            cam.apply(
                &ViewportInput {
                    right_down: true,
                    scroll: notches,
                    ..hovered()
                },
                0.016,
            )
        };
        scroll(&mut cam, 1.0);
        assert!((cam.fly_speed - 6.0).abs() < 1e-4);
        scroll(&mut cam, 100.0);
        assert_eq!(cam.fly_speed, 100.0);
        scroll(&mut cam, -100.0);
        assert_eq!(cam.fly_speed, 0.5);
        assert!((cam.distance - d0).abs() < 1e-4);
    }

    #[test]
    fn pitch_clamps() {
        let mut cam = EditorCamera::default();
        cam.apply(
            &ViewportInput {
                right_down: true,
                pointer_delta: Vec2::new(0.0, -1e6),
                ..hovered()
            },
            0.016,
        );
        assert!(cam.pitch.abs() <= 89f32.to_radians() + 1e-6);
    }

    #[test]
    fn no_change_when_typing_or_not_hovered() {
        let busy = ViewportInput {
            right_down: false,
            middle_down: false,
            alt: true,
            left_down: false,
            scroll: 3.0,
            pointer_delta: Vec2::new(50.0, 50.0),
            keys: FlyKeys {
                w: true,
                ..Default::default()
            },
            ..hovered()
        };
        let mut cam = EditorCamera::default();
        let before = (cam.eye(), cam.distance, cam.fly_speed);
        cam.apply(
            &ViewportInput {
                typing: true,
                right_down: true,
                ..busy
            },
            0.1,
        );
        assert_eq!(before, (cam.eye(), cam.distance, cam.fly_speed));
        cam.apply(
            &ViewportInput {
                hovered: false,
                ..busy
            },
            0.1,
        );
        assert_eq!(before, (cam.eye(), cam.distance, cam.fly_speed));
    }

    fn spawn_box(world: &mut World, t: Transform) -> Entity {
        world
            .spawn((Name("Box".to_string()), t, MeshRef::Cube))
            .id()
    }

    #[test]
    fn pick_nearest_of_two_boxes() {
        let mut world = World::new();
        let near = spawn_box(&mut world, Transform::from_translation(Vec3::ZERO));
        let _far = spawn_box(
            &mut world,
            Transform::from_translation(Vec3::new(0.0, 0.0, -3.0)),
        );
        let hit = pick(&mut world, Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z);
        assert_eq!(hit, Some(near));
    }

    #[test]
    fn pick_miss_returns_none() {
        let mut world = World::new();
        spawn_box(&mut world, Transform::default());
        assert_eq!(
            pick(&mut world, Vec3::new(3.0, 0.0, 5.0), Vec3::NEG_Z),
            None
        );
    }

    #[test]
    fn pick_rotated_scaled_box_exact() {
        // 4 long in local X, rotated 90° about Y: spans world z in [-2, 2].
        let mut world = World::new();
        let e = spawn_box(
            &mut world,
            Transform {
                translation: Vec3::ZERO,
                rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                scale: Vec3::new(4.0, 1.0, 1.0),
            },
        );
        let down = Vec3::NEG_Y;
        assert_eq!(pick(&mut world, Vec3::new(0.0, 5.0, 1.9), down), Some(e));
        assert_eq!(pick(&mut world, Vec3::new(0.0, 5.0, 2.1), down), None);
    }

    #[test]
    fn pick_ignores_entities_without_meshref() {
        let mut world = World::new();
        world.spawn((Name("Empty".to_string()), Transform::default()));
        assert_eq!(
            pick(&mut world, Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z),
            None
        );
    }

    #[test]
    fn frame_target_none_for_despawned_entity() {
        let mut world = World::new();
        let e = spawn_box(&mut world, Transform::default());
        world.despawn(e);
        assert!(frame_target(&world, e).is_none());
    }

    #[test]
    fn frame_fits_box_in_view() {
        let mut world = World::new();
        let e = spawn_box(
            &mut world,
            Transform {
                translation: Vec3::new(3.0, 1.0, -2.0),
                scale: Vec3::splat(2.0),
                ..Default::default()
            },
        );
        let (center, radius) = frame_target(&world, e).unwrap();
        let mut cam = EditorCamera::default();
        cam.frame(center, radius);
        assert!(approx(cam.pivot, Vec3::new(3.0, 1.0, -2.0)));
        let half_fov = melon_core::Camera::default().fov_y_radians / 2.0;
        assert!(cam.distance * half_fov.tan() >= radius - 1e-4);
    }

    #[test]
    fn grid_lines_count_and_axis_colors() {
        let lines = grid_lines();
        assert_eq!(lines.len(), 404);
        let red = lines
            .iter()
            .find(|v| v.color == AXIS_X_COLOR)
            .expect("x axis line");
        assert!(red.position[2].abs() < 1e-6);
        let blue = lines
            .iter()
            .find(|v| v.color == AXIS_Z_COLOR)
            .expect("z axis line");
        assert!(blue.position[0].abs() < 1e-6);
    }

    #[test]
    fn alt_during_gizmo_drag_does_not_orbit() {
        let vp = ViewportInput {
            alt: true,
            left_down: true,
            pointer_delta: Vec2::new(40.0, 0.0),
            ..hovered()
        };
        let mut cam = EditorCamera::default();
        let eye0 = cam.eye();
        cam.apply(&camera_input(&vp, true), 0.016);
        assert!(approx(cam.eye(), eye0), "camera moved during a gizmo drag");
        cam.apply(&camera_input(&vp, false), 0.016);
        assert!(!approx(cam.eye(), eye0), "orbit works without a gizmo drag");
    }

    #[test]
    fn alt_wins_over_gizmo_and_click() {
        assert_eq!(left_action(true, true, true), LeftAction::Orbit);
    }

    #[test]
    fn gizmo_handle_wins_over_select() {
        assert_eq!(left_action(false, true, true), LeftAction::Gizmo);
    }

    #[test]
    fn plain_click_selects() {
        assert_eq!(left_action(false, false, true), LeftAction::Select);
    }

    #[test]
    fn no_click_no_action() {
        assert_eq!(left_action(false, false, false), LeftAction::None);
    }

    #[test]
    fn viewport_pixel_size_never_zero() {
        let size = viewport_pixel_size(Vec2::ZERO, 2.0);
        assert_eq!((size.width, size.height), (1, 1));
    }

    #[test]
    fn viewport_pixel_size_scales_by_ppp() {
        let size = viewport_pixel_size(Vec2::new(400.5, 300.0), 2.0);
        assert_eq!((size.width, size.height), (801, 600));
    }

    #[test]
    fn selection_box_has_12_edges() {
        assert_eq!(selection_box_lines(&Transform::default()).len(), 24);
    }
}
