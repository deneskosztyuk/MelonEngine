use bevy_ecs::component::Component;
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
}

#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Name(pub String);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MeshRef {
    #[default]
    Cube,
    GltfModel,
}

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
}
