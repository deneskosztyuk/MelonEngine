//! Per-scene sun, sky and fog: a registered component (saved in scenes,
//! edited in the Inspector) that drives the global `Light` and `Environment`
//! resources while an entity carries it, like Godot's WorldEnvironment or
//! Unity's per-scene lighting settings. Without one, both resources go back
//! to their defaults.

use bevy_ecs::component::Component;
use bevy_ecs::reflect::ReflectComponent;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use glam::Vec3;

use crate::reflect::Vec3Reflect;
use crate::{Environment, Light};

/// The scene's sun, ambient, sky and fog. The first entity with one wins.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq)]
#[reflect(Component, Default)]
pub struct SceneLighting {
    /// Sun height above the horizon, degrees (90 = straight overhead).
    pub sun_elevation: f32,
    /// Compass direction the sun is in, degrees around +Y (0 = toward +Z,
    /// 90 = toward +X).
    pub sun_azimuth: f32,
    /// Sun colour and strength (linear RGB; `Light::color`).
    #[reflect(remote = Vec3Reflect)]
    pub sun_color: Vec3,
    /// Share of light that is ambient (0..1); the sun gets the rest.
    pub ambient: f32,
    /// Ambient from above (linear RGB).
    #[reflect(remote = Vec3Reflect)]
    pub sky_ambient: Vec3,
    /// Ambient from below (linear RGB).
    #[reflect(remote = Vec3Reflect)]
    pub ground_ambient: Vec3,
    /// Sky colour straight up (linear HDR).
    #[reflect(remote = Vec3Reflect)]
    pub zenith_color: Vec3,
    /// Sky colour at the horizon (linear HDR), also the fog colour.
    #[reflect(remote = Vec3Reflect)]
    pub horizon_color: Vec3,
    /// Exponential fog density per metre; 0 = no fog.
    pub fog_density: f32,
    /// Bloom mix; 0 = off.
    pub bloom_intensity: f32,
}

impl Default for SceneLighting {
    /// Today's defaults: adding the component changes nothing on screen.
    fn default() -> Self {
        Self::from_resources(&Light::default(), &Environment::default())
    }
}

impl SceneLighting {
    /// The component that reproduces these resources.
    pub fn from_resources(light: &Light, environment: &Environment) -> Self {
        let to_sun = light.to_light().normalize_or(Vec3::Y);
        Self {
            sun_elevation: to_sun.y.clamp(-1.0, 1.0).asin().to_degrees(),
            sun_azimuth: to_sun.x.atan2(to_sun.z).to_degrees(),
            sun_color: light.color,
            ambient: light.ambient,
            sky_ambient: light.sky_color,
            ground_ambient: light.ground_color,
            zenith_color: environment.zenith_color,
            horizon_color: environment.horizon_color,
            fog_density: environment.fog_density,
            bloom_intensity: environment.bloom_intensity,
        }
    }

    /// The `Light` this describes.
    pub fn light(&self) -> Light {
        let (el, az) = (
            self.sun_elevation.to_radians(),
            self.sun_azimuth.to_radians(),
        );
        let to_sun = Vec3::new(az.sin() * el.cos(), el.sin(), az.cos() * el.cos());
        Light {
            direction: -to_sun,
            ambient: self.ambient.clamp(0.0, 1.0),
            color: self.sun_color,
            sky_color: self.sky_ambient,
            ground_color: self.ground_ambient,
        }
    }

    /// `base` with this scene's sky, fog and bloom (AO, GI and the sky
    /// texture stay as they are).
    pub fn environment(&self, base: Environment) -> Environment {
        Environment {
            zenith_color: self.zenith_color,
            horizon_color: self.horizon_color,
            fog_density: self.fog_density.max(0.0),
            bloom_intensity: self.bloom_intensity.max(0.0),
            ..base
        }
    }
}

/// Whether a `SceneLighting` drove the resources last frame.
#[derive(Resource, Default)]
struct Applied(bool);

/// Copy the scene's `SceneLighting` into `Light` and `Environment`; with
/// none (after one was applied), restore their defaults. Resources are
/// written only when a value changes.
pub fn scene_lighting_system(world: &mut World) {
    let scene = world.query::<&SceneLighting>().iter(world).next().copied();
    let applied = world.get_resource::<Applied>().is_some_and(|a| a.0);
    let base = world
        .get_resource::<Environment>()
        .copied()
        .unwrap_or_default();
    let (light, environment) = match scene {
        Some(s) => (s.light(), s.environment(base)),
        None if applied => (
            Light::default(),
            Environment {
                sky_texture: base.sky_texture,
                ..SceneLighting::default().environment(base)
            },
        ),
        None => return,
    };
    if world.get_resource::<Light>() != Some(&light) {
        world.insert_resource(light);
    }
    if world.get_resource::<Environment>() != Some(&environment) {
        world.insert_resource(environment);
    }
    world.insert_resource(Applied(scene.is_some()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Core, Name, PostUpdate};
    use bevy_ecs::change_detection::DetectChanges;
    use bevy_ecs::schedule::Schedule;

    fn run(world: &mut World) {
        let mut schedule = Schedule::new(PostUpdate);
        schedule.add_systems(scene_lighting_system);
        schedule.run(world);
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn defaults_reproduce_todays_light_and_environment() {
        let mut core = Core::new();
        core.world
            .spawn((Name("Lighting".into()), SceneLighting::default()));
        run(&mut core.world);
        let light = *core.world.resource::<Light>();
        let want = Light::default();
        assert!(close(light.direction, want.direction), "{light:?}");
        assert_eq!((light.ambient, light.color), (want.ambient, want.color));
        assert_eq!(
            *core.world.resource::<Environment>(),
            Environment::default()
        );
    }

    #[test]
    fn scene_values_drive_the_resources_and_keep_the_rest() {
        let mut core = Core::new();
        core.world.insert_resource(Environment {
            ao_strength: 0.3,
            gi_intensity: 0.5,
            ..Default::default()
        });
        core.world.spawn(SceneLighting {
            sun_elevation: 90.0,
            sun_color: Vec3::new(1.0, 0.5, 0.25),
            ambient: 0.4,
            horizon_color: Vec3::new(1.0, 0.6, 0.3),
            fog_density: 0.003,
            ..Default::default()
        });
        run(&mut core.world);
        let light = *core.world.resource::<Light>();
        assert!(close(light.direction, Vec3::NEG_Y), "{light:?}");
        assert_eq!(light.color, Vec3::new(1.0, 0.5, 0.25));
        assert_eq!(light.ambient, 0.4);
        let env = *core.world.resource::<Environment>();
        assert_eq!(env.horizon_color, Vec3::new(1.0, 0.6, 0.3));
        assert_eq!(env.fog_density, 0.003);
        assert_eq!((env.ao_strength, env.gi_intensity), (0.3, 0.5));
    }

    #[test]
    fn azimuth_turns_the_sun_around_y() {
        let low_east = SceneLighting {
            sun_elevation: 0.0,
            sun_azimuth: 90.0,
            ..Default::default()
        };
        // The sun sits toward +X, so its light travels toward -X.
        assert!(close(low_east.light().direction, Vec3::NEG_X));
    }

    #[test]
    fn removing_the_component_restores_the_defaults() {
        let mut core = Core::new();
        let e = core
            .world
            .spawn(SceneLighting {
                fog_density: 0.05,
                sun_elevation: 5.0,
                ..Default::default()
            })
            .id();
        run(&mut core.world);
        assert_eq!(core.world.resource::<Environment>().fog_density, 0.05);
        core.world.despawn(e);
        run(&mut core.world);
        assert_eq!(
            core.world.resource::<Environment>().fog_density,
            Environment::default().fog_density
        );
        let want = Light::default();
        assert!(close(
            core.world.resource::<Light>().direction,
            want.direction
        ));
    }

    #[test]
    fn unchanged_lighting_does_not_rewrite_the_resources() {
        let mut core = Core::new();
        core.world.spawn(SceneLighting::default());
        run(&mut core.world);
        let tick = core.world.change_tick();
        core.world.increment_change_tick();
        run(&mut core.world);
        let env = core
            .world
            .get_resource_ref::<Environment>()
            .expect("environment");
        assert!(
            !env.last_changed()
                .is_newer_than(tick, core.world.change_tick()),
            "rewritten although nothing changed"
        );
    }

    #[test]
    fn scene_lighting_is_a_registered_component() {
        let core = Core::new();
        let registry = core.world.resource::<bevy_ecs::reflect::AppTypeRegistry>();
        assert!(
            registry
                .read()
                .get(std::any::TypeId::of::<SceneLighting>())
                .is_some()
        );
    }
}
