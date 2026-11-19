use bevy_ecs::component::Component;
use bevy_ecs::system::Query;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    pub fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            ..Default::default()
        }
    }

    pub fn to_mat4(&self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Blend from `prev` (alpha 0) to `self` (alpha 1).
    pub fn interpolate(&self, prev: &Transform, alpha: f32) -> Transform {
        Transform {
            translation: prev.translation.lerp(self.translation, alpha),
            rotation: prev.rotation.slerp(self.rotation, alpha),
            scale: prev.scale.lerp(self.scale, alpha),
        }
    }
}

/// `Transform` at the start of the latest fixed step. Entities that have it
/// are drawn blended between it and `Transform` by `Time::alpha()`. Add it
/// only to entities moved in `FixedUpdate`.
#[derive(Component, Debug, Clone, Copy)]
pub struct PreviousTransform(pub Transform);

/// Run first in `FixedUpdate`: remember where each interpolated entity was.
pub fn store_previous_transforms_system(mut query: Query<(&Transform, &mut PreviousTransform)>) {
    for (transform, mut prev) in &mut query {
        prev.0 = *transform;
    }
}

#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Name(pub String);

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Aabb {
    pub half_extents: Vec3,
}

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Spin;

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Bob;

#[derive(Component, Debug, Clone, Copy, Default)]
pub struct PendingDelete;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BodyKind {
    #[default]
    None,
    Dynamic,
    Fixed,
}

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BodyInfo {
    pub body_type: BodyKind,
    pub half_extents: Vec3,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_default_is_identity() {
        let t = Transform::default();
        assert_eq!(t.translation, Vec3::ZERO);
        assert_eq!(t.rotation, Quat::IDENTITY);
        assert_eq!(t.scale, Vec3::ONE);
    }

    #[test]
    fn transform_from_translation_preserves_rotation_scale() {
        let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(t.translation, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(t.rotation, Quat::IDENTITY);
        assert_eq!(t.scale, Vec3::ONE);
    }

    #[test]
    fn transform_to_mat4_identity() {
        let t = Transform::default();
        assert_eq!(t.to_mat4(), glam::Mat4::IDENTITY);
    }

    #[test]
    fn transform_to_mat4_translation() {
        let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let expected = glam::Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(t.to_mat4(), expected);
    }

    #[test]
    fn transform_to_mat4_compose() {
        let t = Transform {
            translation: Vec3::new(1.0, 0.0, 0.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(2.0, 2.0, 2.0),
        };
        let expected =
            glam::Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        assert_eq!(t.to_mat4(), expected);
    }

    fn prev_and_cur() -> (Transform, Transform) {
        let prev = Transform::default();
        let cur = Transform {
            translation: Vec3::new(2.0, 0.0, 0.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            scale: Vec3::splat(3.0),
        };
        (prev, cur)
    }

    #[test]
    fn interpolate_alpha_zero_is_prev() {
        let (prev, cur) = prev_and_cur();
        let t = cur.interpolate(&prev, 0.0);
        assert_eq!(t.translation, prev.translation);
        assert_eq!(t.scale, prev.scale);
    }

    #[test]
    fn interpolate_alpha_one_is_current() {
        let (prev, cur) = prev_and_cur();
        let t = cur.interpolate(&prev, 1.0);
        assert_eq!(t.translation, cur.translation);
        assert_eq!(t.scale, cur.scale);
    }

    #[test]
    fn interpolate_half_rotation() {
        let (prev, cur) = prev_and_cur();
        let r = cur.interpolate(&prev, 0.5).rotation;
        assert!(r.angle_between(Quat::from_rotation_y(std::f32::consts::FRAC_PI_4)) < 1e-5);
    }

    #[test]
    fn store_previous_copies_transform() {
        let mut world = bevy_ecs::world::World::new();
        let e = world
            .spawn((
                Transform::from_translation(Vec3::new(1.0, 2.0, 3.0)),
                PreviousTransform(Transform::default()),
            ))
            .id();
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(store_previous_transforms_system);
        schedule.run(&mut world);
        let prev = world.get::<PreviousTransform>(e).unwrap();
        assert_eq!(prev.0.translation, Vec3::new(1.0, 2.0, 3.0));
    }
}
