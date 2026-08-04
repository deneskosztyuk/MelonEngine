use std::collections::HashSet;

use bevy_ecs::resource::Resource;
use bevy_ecs::system::{Res, ResMut};
use glam::{Vec2, Vec3};

use crate::Camera;
use crate::resources::{Time, look_target};

#[derive(Resource, Default, Debug, Clone)]
pub struct Input {
    pub keys_pressed: HashSet<KeyCode>,
    pub mouse_delta: Vec2,
    pub right_mouse_held: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    W,
    A,
    S,
    D,
    Space,
    LeftShift,
}

impl Input {
    pub fn key(&self, code: KeyCode) -> bool {
        self.keys_pressed.contains(&code)
    }

    pub fn take_mouse_delta(&mut self) -> Vec2 {
        let d = self.mouse_delta;
        self.mouse_delta = Vec2::ZERO;
        d
    }
}

const MOVE_SPEED: f32 = 3.0;
const LOOK_SENSITIVITY: f32 = 0.0025;
const PITCH_LIMIT: f32 = 1.55;

pub fn fly_camera_system(mut input: ResMut<Input>, mut camera: ResMut<Camera>, time: Res<Time>) {
    let dt = time.delta_seconds;

    if input.right_mouse_held {
        let delta = input.take_mouse_delta();
        if delta.length_squared() > 0.0 {
            camera.yaw += delta.x * LOOK_SENSITIVITY;
            camera.pitch -= delta.y * LOOK_SENSITIVITY;
            camera.pitch = camera.pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
        }
    } else {
        input.take_mouse_delta();
    }

    let forward = Vec3::new(
        camera.yaw.cos() * camera.pitch.cos(),
        camera.pitch.sin(),
        camera.yaw.sin() * camera.pitch.cos(),
    )
    .normalize();
    let right = forward.cross(camera.up).normalize();

    let mut move_dir = glam::Vec3::ZERO;
    if input.key(KeyCode::W) {
        move_dir += forward;
    }
    if input.key(KeyCode::S) {
        move_dir -= forward;
    }
    if input.key(KeyCode::D) {
        move_dir += right;
    }
    if input.key(KeyCode::A) {
        move_dir -= right;
    }
    if input.key(KeyCode::Space) {
        move_dir += camera.up;
    }
    if input.key(KeyCode::LeftShift) {
        move_dir -= camera.up;
    }
    if move_dir.length_squared() > 0.0 {
        move_dir = move_dir.normalize() * MOVE_SPEED * dt;
        camera.eye += move_dir;
    }

    camera.center = look_target(camera.eye, camera.yaw, camera.pitch);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::schedule::Schedule;

    fn world_with_input(input: Input) -> bevy_ecs::world::World {
        let mut world = bevy_ecs::world::World::new();
        world.insert_resource(input);
        world.insert_resource(Camera::default());
        world.insert_resource(Time::default());
        world
    }

    fn run_system(world: &mut bevy_ecs::world::World) {
        let mut schedule = Schedule::default();
        schedule.add_systems(fly_camera_system);
        schedule.run(world);
    }

    #[test]
    fn input_default_is_empty() {
        let i = Input::default();
        assert!(i.keys_pressed.is_empty());
        assert_eq!(i.mouse_delta, Vec2::ZERO);
        assert!(!i.right_mouse_held);
    }

    #[test]
    fn input_key_returns_pressed_state() {
        let mut i = Input::default();
        i.keys_pressed.insert(KeyCode::W);
        assert!(i.key(KeyCode::W));
        assert!(!i.key(KeyCode::A));
    }

    #[test]
    fn take_mouse_delta_clears() {
        let mut i = Input {
            mouse_delta: Vec2::new(10.0, 20.0),
            ..Default::default()
        };
        assert_eq!(i.take_mouse_delta(), Vec2::new(10.0, 20.0));
        assert_eq!(i.take_mouse_delta(), Vec2::ZERO);
    }

    #[test]
    fn fly_camera_w_moves_forward() {
        let mut input = Input::default();
        input.keys_pressed.insert(KeyCode::W);
        let mut world = world_with_input(input);
        let eye_before = world.get_resource::<Camera>().unwrap().eye;
        run_system(&mut world);
        let eye_after = world.get_resource::<Camera>().unwrap().eye;
        assert!(
            eye_after.z < eye_before.z,
            "W should move toward -Z (forward), got {eye_before:?} -> {eye_after:?}"
        );
    }

    #[test]
    fn fly_camera_no_input_no_movement() {
        let mut world = world_with_input(Input::default());
        let eye_before = world.get_resource::<Camera>().unwrap().eye;
        run_system(&mut world);
        let eye_after = world.get_resource::<Camera>().unwrap().eye;
        assert_eq!(eye_before, eye_after, "no input should not move camera");
    }

    #[test]
    fn fly_camera_pitch_clamps() {
        let input = Input {
            right_mouse_held: true,
            mouse_delta: Vec2::new(0.0, -10000.0),
            ..Default::default()
        };
        let mut world = world_with_input(input);
        run_system(&mut world);
        let pitch = world.get_resource::<Camera>().unwrap().pitch;
        assert!(
            pitch.abs() <= PITCH_LIMIT + 1e-6,
            "pitch {pitch} exceeds limit {PITCH_LIMIT}"
        );
    }
}
