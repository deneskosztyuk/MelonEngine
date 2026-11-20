//! Scene viewport logic that does not depend on the UI library: camera
//! navigation (Unity controls), click picking and the grid / selection lines.
//! The UI glue only fills a `ViewportInput` each frame.

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use melon_core::glam::{Mat4, Vec2, Vec3};
use melon_core::hierarchy::world_matrix;
use melon_core::{
    AssetServer, Camera, Collider, ColliderShape, Handle, Model, PointLight, SpotLight, Transform,
};
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
/// Where a model dropped at `pointer` lands: on the ground (y = 0) under the
/// cursor, or 5 m along the cursor ray when it doesn't point down at the ground.
pub fn drop_point(camera: &Camera, pointer: Vec2, size: Vec2) -> Vec3 {
    let (origin, dir) = camera.screen_to_world_ray(pointer, size);
    if dir.y < -1e-4 {
        let t = -origin.y / dir.y;
        if t > 0.0 {
            return origin + dir * t;
        }
    }
    origin + dir * 5.0
}

/// Bounds used until a model's real bounds are known (and for `builtin:cube`).
pub const UNIT_BOUNDS: (Vec3, Vec3) = (Vec3::splat(-0.5), Vec3::splat(0.5));

const GRID_HALF_SIZE: i32 = 50;
/// The grid sits a hair above y = 0 so it does not z-fight a floor whose top is y = 0.
const GRID_Y: f32 = 0.005;
const GRID_MINOR_COLOR: [f32; 3] = [0.25, 0.25, 0.25];
const GRID_MAJOR_COLOR: [f32; 3] = [0.45, 0.45, 0.45];
pub const AXIS_X_COLOR: [f32; 3] = [0.9, 0.15, 0.15];
pub const AXIS_Z_COLOR: [f32; 3] = [0.15, 0.35, 0.9];
const SELECTION_COLOR: [f32; 3] = [1.0, 0.55, 0.0];
/// Collider outlines, green like Unity and Godot.
const COLLIDER_COLOR: [f32; 3] = [0.2, 0.9, 0.3];

fn collider_rgb(sensor: bool) -> [f32; 3] {
    let c = if sensor {
        crate::theme::colors::COLLIDER_SENSOR
    } else {
        crate::theme::colors::COLLIDER
    };
    [c.r(), c.g(), c.b()].map(|v| v as f32 / 255.0)
}

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

    /// Take over the render camera's pose (eye and look target), e.g. where the
    /// game left it. Fly speed stays; a degenerate eye == center keeps the angles.
    pub fn adopt(&mut self, camera: &Camera) {
        let offset = camera.center - camera.eye;
        if let Some(forward) = offset.try_normalize() {
            self.yaw = forward.z.atan2(forward.x);
            self.pitch = forward.y.asin().clamp(-PITCH_LIMIT, PITCH_LIMIT);
            self.distance = offset.length().max(MIN_DISTANCE);
        }
        self.pivot = camera.eye + self.forward() * self.distance;
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
/// bounds in the entity's own space (world matrix, parents included), so
/// rotated and scaled boxes are exact.
pub fn pick(world: &mut World, origin: Vec3, dir: Vec3) -> Option<Entity> {
    let light_hit = pick_light(world, origin, dir);
    let mut query = world.query_filtered::<(Entity, &Handle<Model>), melon_core::With<Transform>>();
    let world: &World = world;
    let server = world.get_resource::<AssetServer>();
    query
        .iter(world)
        .filter_map(|(entity, model)| {
            let (min, max) = server.and_then(|s| s.bounds(*model)).unwrap_or(UNIT_BOUNDS);
            let inverse = world_matrix(world, entity).inverse();
            if !inverse.is_finite() {
                return None; // zero scale
            }
            // Affine map: the hit distance t is the same in local and world space.
            let o = inverse.transform_point3(origin);
            let d = inverse.transform_vector3(dir);
            ray_box(o, d, min, max).map(|t| (t, entity))
        })
        .chain(light_hit)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, entity)| entity)
}

/// `pick`, plus the mesh part of the hit entity's model under the ray (the
/// nearest part box; `None` when the model has no known parts, or for a
/// light).
pub fn pick_part(world: &mut World, origin: Vec3, dir: Vec3) -> Option<(Entity, Option<usize>)> {
    let entity = pick(world, origin, dir)?;
    let part = (|| {
        let model = *world.get::<Handle<Model>>(entity)?;
        let inverse = world_matrix(world, entity).inverse();
        let o = inverse.transform_point3(origin);
        let d = inverse.transform_vector3(dir);
        let parts = world.get_resource::<AssetServer>()?.part_bounds(model);
        parts
            .iter()
            .enumerate()
            .filter_map(|(i, (min, max))| ray_box(o, d, *min, *max).map(|t| (t, i)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, i)| i)
    })();
    Some((entity, part))
}

/// Radius of a light's viewport icon, which is also its pick radius.
pub const LIGHT_ICON_RADIUS: f32 = 0.18;

/// Position, linear colour and range of the point or spot light on `e`.
fn light_of(world: &World, e: Entity) -> Option<(Vec3, Vec3, f32, Option<f32>)> {
    let at = world_matrix(world, e).w_axis.truncate();
    if let Some(l) = world.get::<PointLight>(e) {
        return Some((at, l.color, l.range, None));
    }
    let l = world.get::<SpotLight>(e)?;
    Some((at, l.color, l.range, Some(l.outer_angle)))
}

/// Nearest ray hit on a light's icon sphere, as (distance, entity).
fn pick_light(world: &mut World, origin: Vec3, dir: Vec3) -> Option<(f32, Entity)> {
    let mut query = world.query_filtered::<Entity, melon_core::bevy_ecs::query::Or<(
        melon_core::With<PointLight>,
        melon_core::With<SpotLight>,
    )>>();
    let world: &World = world;
    query
        .iter(world)
        .filter_map(|e| {
            let (at, ..) = light_of(world, e)?;
            let to = at - origin;
            let along = to.dot(dir);
            let miss = (to - dir * along).length();
            (along > 0.0 && miss <= LIGHT_ICON_RADIUS).then_some((along, e))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Small coloured spheres at every point and spot light (hidden behind
/// geometry).
pub fn light_icons(world: &mut World) -> Vec<melon_render::OverlayDraw> {
    let mut query = world.query_filtered::<Entity, melon_core::bevy_ecs::query::Or<(
        melon_core::With<PointLight>,
        melon_core::With<SpotLight>,
    )>>();
    let world: &World = world;
    query
        .iter(world)
        .filter_map(|e| light_of(world, e))
        .map(|(at, color, _, _)| melon_render::OverlayDraw {
            shape: melon_render::OverlayShape::Sphere,
            transform: Mat4::from_scale_rotation_translation(
                Vec3::splat(LIGHT_ICON_RADIUS),
                melon_core::glam::Quat::IDENTITY,
                at,
            ),
            color: [color.x.min(1.0), color.y.min(1.0), color.z.min(1.0), 1.0],
            depth: melon_render::OverlayDepth::SceneTested,
        })
        .collect()
}

const LIGHT_COLOR: [f32; 3] = [1.0, 0.85, 0.2];

/// Range outline of the light on `entity`: three rings for a point light's
/// sphere, a cone (end ring and edges) for a spot light, along its -Z axis.
pub fn light_lines(world: &World, entity: Entity) -> Vec<LineVertex> {
    let Some((at, _, range, cone)) = light_of(world, entity) else {
        return Vec::new();
    };
    let (_, rotation, _) = world_matrix(world, entity).to_scale_rotation_translation();
    let m = Mat4::from_rotation_translation(rotation, at);
    let tau = std::f32::consts::TAU;
    let mut vertices = Vec::new();
    match cone {
        None => {
            for (a, b) in [(Vec3::X, Vec3::Y), (Vec3::X, Vec3::Z), (Vec3::Y, Vec3::Z)] {
                vertices.extend(arc(m, Vec3::ZERO, a * range, b * range, 0.0, tau));
            }
        }
        Some(outer) => {
            let angle = outer.clamp(0.01, 1.5);
            let (radius, depth) = (range * angle.sin(), -range * angle.cos());
            let end = Vec3::new(0.0, 0.0, depth);
            vertices.extend(arc(m, end, Vec3::X * radius, Vec3::Y * radius, 0.0, tau));
            for side in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y] {
                vertices.extend(line(
                    at,
                    m.transform_point3(end + side * radius),
                    COLLIDER_COLOR,
                ));
            }
        }
    }
    for v in &mut vertices {
        v.color = LIGHT_COLOR;
    }
    vertices
}

/// Slab test against the box `[min, max]`. Returns the entry distance
/// (0 if the origin is inside), or `None` on a miss or a hit behind the origin.
fn ray_box(o: Vec3, d: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let (mut t_min, mut t_max) = (0.0f32, f32::INFINITY);
    for axis in 0..3 {
        let (o, d, lo, hi) = (o[axis], d[axis], min[axis], max[axis]);
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (t0, t1) = ((lo - o) / d, (hi - o) / d);
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
    world.get::<Transform>(entity)?;
    let m = world_matrix(world, entity);
    let (min, max) = model_bounds(world, entity);
    Some((
        m.transform_point3((min + max) * 0.5),
        ((max - min) * 0.5 * Transform::from_mat4(m).scale.abs()).length(),
    ))
}

/// Local bounds of an entity's model: measured when loaded, else `UNIT_BOUNDS`.
pub fn model_bounds(world: &World, entity: Entity) -> (Vec3, Vec3) {
    world
        .get::<Handle<Model>>(entity)
        .zip(world.get_resource::<AssetServer>())
        .and_then(|(h, server)| server.bounds(*h))
        .unwrap_or(UNIT_BOUNDS)
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

/// Orange wire box around the model bounds under world matrix `m` (12 edges).
pub fn selection_box_lines(m: Mat4, (min, max): (Vec3, Vec3)) -> Vec<LineVertex> {
    let corner = |i: usize| {
        let pick = |bit: usize, axis: usize| if i & bit != 0 { max[axis] } else { min[axis] };
        m.transform_point3(Vec3::new(pick(1, 0), pick(2, 1), pick(4, 2)))
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

/// Green wire outlines of the colliders on `entity` and its descendants,
/// following the shape numbers and world pose (scale included) the physics
/// sync uses, so an Inspector edit shows at once. A mesh collider is drawn as
/// its model bounds.
pub fn collider_lines(world: &World, entity: Entity) -> Vec<LineVertex> {
    let mut vertices = Vec::new();
    let mut stack = vec![entity];
    while let Some(e) = stack.pop() {
        if let Some(children) = world.get::<melon_core::Children>(e) {
            stack.extend(children.iter());
        }
        let Some(c) = world.get::<Collider>(e) else {
            continue;
        };
        let (scale, rotation, translation) = world_matrix(world, e).to_scale_rotation_translation();
        let m = Mat4::from_rotation_translation(rotation, translation);
        let s = scale.abs();
        let start = vertices.len();
        match c.shape {
            ColliderShape::Box { half_extents } => {
                vertices.extend(box_lines(m, -half_extents * s, half_extents * s))
            }
            ColliderShape::Sphere { radius } => {
                let r = radius * s.max_element();
                for (a, b) in [(Vec3::X, Vec3::Y), (Vec3::X, Vec3::Z), (Vec3::Y, Vec3::Z)] {
                    vertices.extend(arc(m, Vec3::ZERO, a * r, b * r, 0.0, std::f32::consts::TAU));
                }
            }
            ColliderShape::Capsule {
                half_height,
                radius,
            } => {
                let r = radius * s.x.max(s.z);
                let h = Vec3::Y * half_height * s.y;
                // Rings where the cylinder meets the caps, the side lines, the cap arcs.
                for end in [h, -h] {
                    vertices.extend(arc(
                        m,
                        end,
                        Vec3::X * r,
                        Vec3::Z * r,
                        0.0,
                        std::f32::consts::TAU,
                    ));
                }
                for side in [Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z] {
                    vertices.extend(line(
                        m.transform_point3(h + side * r),
                        m.transform_point3(-h + side * r),
                        COLLIDER_COLOR,
                    ));
                }
                let pi = std::f32::consts::PI;
                for side in [Vec3::X, Vec3::Z] {
                    vertices.extend(arc(m, h, side * r, Vec3::Y * r, 0.0, pi));
                    vertices.extend(arc(m, -h, side * r, -Vec3::Y * r, 0.0, pi));
                }
            }
            ColliderShape::Mesh => {
                let (min, max) = model_bounds(world, e);
                vertices.extend(box_lines(m, min * s, max * s));
            }
        }
        let color = collider_rgb(c.sensor);
        for v in &mut vertices[start..] {
            v.color = color;
        }
    }
    vertices
}

/// Wire box of `min..max` under `m`, in the collider colour.
fn box_lines(m: Mat4, min: Vec3, max: Vec3) -> Vec<LineVertex> {
    let mut vertices = selection_box_lines(m, (min, max));
    for v in &mut vertices {
        v.color = COLLIDER_COLOR;
    }
    vertices
}

/// An arc of the ellipse `center + cos(t) * a + sin(t) * b` for `t` in
/// `from..to`, as line segments under `m`.
fn arc(m: Mat4, center: Vec3, a: Vec3, b: Vec3, from: f32, to: f32) -> Vec<LineVertex> {
    const SEGMENTS: usize = 24;
    let point = |i: usize| {
        let t = from + (to - from) * i as f32 / SEGMENTS as f32;
        m.transform_point3(center + a * t.cos() + b * t.sin())
    };
    (0..SEGMENTS)
        .flat_map(|i| line(point(i), point(i + 1), COLLIDER_COLOR))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use melon_core::Name;
    use melon_core::glam::Quat;

    #[test]
    fn lights_are_pickable_and_outline_their_range() {
        let mut world = World::new();
        let light = world
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)),
                PointLight::default(),
            ))
            .id();
        let spot = world
            .spawn((
                Transform::from_translation(Vec3::new(5.0, 2.0, 0.0)),
                SpotLight::default(),
            ))
            .id();
        assert_eq!(
            pick(&mut world, Vec3::new(0.0, 2.0, 5.0), Vec3::NEG_Z),
            Some(light)
        );
        assert_eq!(
            pick(&mut world, Vec3::new(5.0, 2.0, 5.0), Vec3::NEG_Z),
            Some(spot)
        );
        assert_eq!(
            pick(&mut world, Vec3::new(2.0, 2.0, 5.0), Vec3::NEG_Z),
            None
        );
        assert_eq!(light_icons(&mut world).len(), 2);
        // Point light range: every outline vertex lies on the range sphere.
        let range = PointLight::default().range;
        let lines = light_lines(&world, light);
        assert!(!lines.is_empty());
        for v in &lines {
            let d = (Vec3::from_array(v.position) - Vec3::new(0.0, 2.0, 0.0)).length();
            assert!((d - range).abs() < 1e-3, "{d}");
        }
        assert!(!light_lines(&world, spot).is_empty());
        let plain = world.spawn(Transform::default()).id();
        assert!(light_lines(&world, plain).is_empty());
    }

    #[test]
    fn pick_part_names_the_mesh_part_under_the_ray() {
        let mut world = World::new();
        let mut server = AssetServer::new(".".into());
        let model: Handle<Model> = server.load("assets/two.glb");
        server.set_bounds(model, Vec3::new(-2.0, -0.5, -0.5), Vec3::new(2.0, 0.5, 0.5));
        server.set_part_bounds(
            model,
            vec![
                (Vec3::new(-2.0, -0.5, -0.5), Vec3::new(0.0, 0.5, 0.5)),
                (Vec3::new(0.0, -0.5, -0.5), Vec3::new(2.0, 0.5, 0.5)),
            ],
        );
        world.insert_resource(server);
        let e = world.spawn((Transform::default(), model)).id();
        let mut from = |x: f32| pick_part(&mut world, Vec3::new(x, 0.0, 5.0), Vec3::NEG_Z);
        assert_eq!(from(-1.0), Some((e, Some(0))));
        assert_eq!(from(1.0), Some((e, Some(1))));
        assert_eq!(from(3.0), None);
    }

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
        let cube: Handle<Model> = world
            .get_resource_or_insert_with(|| AssetServer::new(".".into()))
            .load("builtin:cube");
        world.spawn((Name("Box".to_string()), t, cube)).id()
    }

    fn child_box(world: &mut World) -> Entity {
        let parent = world
            .spawn(Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)))
            .id();
        let child = spawn_box(world, Transform::default());
        world.entity_mut(child).insert(melon_core::ChildOf(parent));
        child
    }

    #[test]
    fn pick_child_at_world_position() {
        let mut world = World::new();
        let child = child_box(&mut world);
        assert_eq!(
            pick(&mut world, Vec3::new(10.0, 5.0, 0.0), Vec3::NEG_Y),
            Some(child)
        );
        assert_eq!(
            pick(&mut world, Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y),
            None
        );
    }

    #[test]
    fn frame_target_uses_world_position() {
        let mut world = World::new();
        let child = child_box(&mut world);
        let (centre, _) = frame_target(&world, child).unwrap();
        assert!(approx(centre, Vec3::new(10.0, 0.0, 0.0)), "{centre}");
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
    fn pick_uses_model_bounds() {
        let mut world = World::new();
        let mut server = AssetServer::new(".".into());
        let h: Handle<Model> = server.load("assets/models/wide.glb");
        server.set_bounds(h, Vec3::ZERO, Vec3::new(2.0, 1.0, 1.0));
        world.insert_resource(server);
        let e = world.spawn((Transform::default(), h)).id();
        let down = Vec3::NEG_Y;
        assert_eq!(pick(&mut world, Vec3::new(1.9, 5.0, 0.5), down), Some(e));
        assert_eq!(pick(&mut world, Vec3::new(2.1, 5.0, 0.5), down), None);
    }

    #[test]
    fn pick_without_bounds_uses_unit_cube() {
        let mut world = World::new();
        let mut server = AssetServer::new(".".into());
        let h: Handle<Model> = server.load("assets/models/unloaded.glb");
        world.insert_resource(server);
        let e = world.spawn((Transform::default(), h)).id();
        let down = Vec3::NEG_Y;
        assert_eq!(pick(&mut world, Vec3::new(0.4, 5.0, 0.0), down), Some(e));
        assert_eq!(pick(&mut world, Vec3::new(0.6, 5.0, 0.0), down), None);
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
    fn adopt_takes_the_camera_pose() {
        let mut camera = Camera {
            eye: Vec3::new(1.0, 3.0, 2.0),
            center: Vec3::new(4.0, 1.0, -2.0),
            ..Default::default()
        };
        let mut cam = EditorCamera::default();
        cam.adopt(&camera);
        let (eye, center) = (camera.eye, camera.center);
        cam.write(&mut camera);
        assert!(approx(camera.eye, eye), "{}", camera.eye);
        assert!(approx(camera.center, center), "{}", camera.center);
    }

    #[test]
    fn drop_point_hits_ground() {
        let mut camera = Camera::default();
        EditorCamera::default().write(&mut camera);
        let size = Vec2::new(800.0, 600.0);
        let p = drop_point(&camera, size * 0.5, size);
        assert!(p.y.abs() < 1e-3, "on the ground: {p}");
        assert!(p.length() < 0.1, "default camera looks at the origin: {p}");
    }

    #[test]
    fn drop_point_above_horizon_falls_back() {
        let camera = Camera::default(); // eye (0,0,4) looking level along -Z
        let size = Vec2::new(800.0, 600.0);
        let p = drop_point(&camera, Vec2::new(400.0, 0.0), size);
        let (origin, _) = camera.screen_to_world_ray(Vec2::new(400.0, 0.0), size);
        assert!(
            (p.distance(origin) - 5.0).abs() < 1e-3,
            "5 m along the ray: {p}"
        );
    }

    fn collider_world(collider: Collider, transform: Transform) -> (World, Entity) {
        let mut world = World::new();
        let e = world.spawn((transform, collider)).id();
        (world, e)
    }

    #[test]
    fn box_collider_outline_is_a_green_wire_box_at_the_scaled_size() {
        let (world, e) = collider_world(
            Collider {
                shape: ColliderShape::Box {
                    half_extents: Vec3::new(1.0, 2.0, 3.0),
                },
                ..Default::default()
            },
            Transform {
                translation: Vec3::new(5.0, 0.0, 0.0),
                scale: Vec3::splat(2.0),
                ..Default::default()
            },
        );
        let lines = collider_lines(&world, e);
        assert_eq!(lines.len(), 24);
        assert!(lines.iter().all(|v| v.color == collider_rgb(false)));
        let max_x = lines.iter().map(|v| v.position[0]).fold(f32::MIN, f32::max);
        let max_z = lines.iter().map(|v| v.position[2]).fold(f32::MIN, f32::max);
        assert!((max_x - 7.0).abs() < 1e-4 && (max_z - 6.0).abs() < 1e-4);
    }

    #[test]
    fn sensor_outlines_use_their_own_colour() {
        let rgb = |c: egui::Color32| [c.r(), c.g(), c.b()].map(|v| v as f32 / 255.0);
        let solid = collider_world(Collider::default(), Transform::default());
        let sensor = collider_world(
            Collider {
                sensor: true,
                ..Default::default()
            },
            Transform::default(),
        );
        let solid = collider_lines(&solid.0, solid.1);
        let sensor = collider_lines(&sensor.0, sensor.1);
        assert!(
            solid
                .iter()
                .all(|v| v.color == rgb(theme::colors::COLLIDER))
        );
        assert!(
            sensor
                .iter()
                .all(|v| v.color == rgb(theme::colors::COLLIDER_SENSOR))
        );
        assert_ne!(theme::colors::COLLIDER, theme::colors::COLLIDER_SENSOR);
    }

    #[test]
    fn sphere_and_capsule_outlines_stay_on_their_surface() {
        let (world, e) = collider_world(
            Collider {
                shape: ColliderShape::Sphere { radius: 2.0 },
                ..Default::default()
            },
            Transform::default(),
        );
        let lines = collider_lines(&world, e);
        assert!(!lines.is_empty());
        assert!(
            lines
                .iter()
                .all(|v| (Vec3::from_array(v.position).length() - 2.0).abs() < 1e-4)
        );
        let (world, e) = collider_world(
            Collider {
                shape: ColliderShape::Capsule {
                    half_height: 1.0,
                    radius: 0.5,
                },
                ..Default::default()
            },
            Transform::default(),
        );
        let top = collider_lines(&world, e)
            .iter()
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max);
        assert!((top - 1.5).abs() < 1e-4, "top {top}");
    }

    #[test]
    fn collider_outline_follows_an_edit_and_includes_children() {
        let (mut world, e) = collider_world(Collider::default(), Transform::default());
        let before = collider_lines(&world, e).len();
        world.get_mut::<Collider>(e).unwrap().shape = ColliderShape::Sphere { radius: 0.5 };
        let sphere = collider_lines(&world, e).len();
        assert_ne!(sphere, before);
        world.spawn((
            Transform::default(),
            Collider::default(),
            melon_core::ChildOf(e),
        ));
        let with_child = collider_lines(&world, e).len();
        assert_eq!(with_child, sphere + before);
        assert!(collider_lines(&World::new(), e).is_empty());
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
        assert_eq!(selection_box_lines(Mat4::IDENTITY, UNIT_BOUNDS).len(), 24);
    }
}
