//! Point and spot lights: registered components (saved in scenes, edited in
//! the Inspector). Position and direction come from the entity's world
//! transform; a spot light shines along the entity's local -Z axis. The
//! sun and ambient are the `Light` resource.

use bevy_ecs::component::Component;
use bevy_ecs::reflect::ReflectComponent;
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use glam::Vec3;

use crate::reflect::Vec3Reflect;

/// An omnidirectional light. Radiance at distance `d` is
/// `color * intensity / d^2`, windowed to reach exactly 0 at `range`
/// (smooth, so a light never pops at its edge). `intensity` 3.14 at 1 m
/// lights a white surface as brightly as a full-strength sun.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq)]
#[reflect(Component, Default)]
pub struct PointLight {
    #[reflect(remote = Vec3Reflect)]
    pub color: Vec3,
    pub intensity: f32,
    /// Metres; beyond it the light adds nothing.
    pub range: f32,
    /// Casts shadows (the renderer shadows the nearest few, see
    /// `RenderQuality::shadowed_lights`).
    pub cast_shadows: bool,
}

impl Default for PointLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 20.0,
            range: 10.0,
            cast_shadows: false,
        }
    }
}

/// A cone light along the entity's local -Z axis: full strength inside
/// `inner_angle`, fading to 0 at `outer_angle` (radians from the axis).
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq)]
#[reflect(Component, Default)]
pub struct SpotLight {
    #[reflect(remote = Vec3Reflect)]
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    pub inner_angle: f32,
    pub outer_angle: f32,
    pub cast_shadows: bool,
}

impl Default for SpotLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 40.0,
            range: 12.0,
            inner_angle: 0.35,
            outer_angle: 0.6,
            cast_shadows: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Core;

    #[test]
    fn lights_are_registered_components_with_defaults() {
        let core = Core::new();
        let names: Vec<String> = crate::reflect::registered_components(&core.world)
            .into_iter()
            .filter(|c| c.addable)
            .map(|c| c.display_name)
            .filter(|n| n.ends_with("Light"))
            .collect();
        assert_eq!(names, ["Point Light", "Spot Light"]);
        let spot = SpotLight::default();
        assert!(spot.inner_angle < spot.outer_angle);
    }
}
