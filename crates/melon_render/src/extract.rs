//! Extraction: the ECS world turned into plain data the renderer draws.

use std::collections::HashMap;

use crate::lights::LocalLight;
use glam::{Mat4, Vec3};
use melon_core::bevy_ecs::world::World;
use melon_core::hierarchy::render_matrices;
use melon_core::{
    AssetServer, Camera, Environment, Handle, Light, Material, MeshMaterials, Model, PointLight,
    RenderQuality, SimulationState, SpotLight, Time,
};

/// Everything one view needs to draw a frame.
#[derive(Default)]
pub struct FrameData {
    pub view_proj: Mat4,
    /// Camera position (overlay shading).
    pub eye: glam::Vec3,
    pub light: Light,
    /// The look: sky, fog, pass strengths.
    pub environment: Environment,
    /// Which passes run and at what cost (GI mode, AO, bloom, shadow map
    /// size, MSAA).
    pub quality: RenderQuality,
    /// World matrix and model of every entity with a `Handle<Model>`.
    pub models: Vec<(Mat4, Handle<Model>)>,
    /// Material per mesh part for the `models` entries (by index) that have
    /// `MeshMaterials`; `None` keeps the part's glTF material.
    pub overrides: HashMap<usize, Vec<Option<Handle<Material>>>>,
    /// Every point and spot light, in world space.
    pub lights: Vec<LocalLight>,
}

/// Blend fixed-step entities while Playing and Paused (alpha is frozen while
/// paused, so the pose holds still). In edit mode draw the current pose.
pub fn blends(state: SimulationState) -> bool {
    state != SimulationState::Editing
}

/// Read the camera, the light, the environment and every model entity's world matrix
/// (interpolated along its parent chain) from `world`.
pub fn extract(world: &mut World) -> FrameData {
    let view_proj = world
        .get_resource::<Camera>()
        .map(Camera::view_proj)
        .unwrap_or_default();
    let eye = world
        .get_resource::<Camera>()
        .map_or(glam::Vec3::ZERO, |c| c.eye);
    let light = world.get_resource::<Light>().copied().unwrap_or_default();
    let environment = world
        .get_resource::<Environment>()
        .copied()
        .unwrap_or_default();
    let quality = world
        .get_resource::<RenderQuality>()
        .copied()
        .unwrap_or_default();
    let alpha = world.get_resource::<Time>().map_or(1.0, Time::alpha);
    let state = world
        .get_resource::<SimulationState>()
        .copied()
        .unwrap_or(SimulationState::Editing);
    let matrices = render_matrices(world, alpha, blends(state));
    let mut models = Vec::new();
    let mut slots = Vec::new();
    let mut query = world.query::<(
        melon_core::bevy_ecs::entity::Entity,
        &Handle<Model>,
        Option<&MeshMaterials>,
    )>();
    for (e, h, materials) in query.iter(world) {
        if let Some(m) = matrices.get(&e) {
            if let Some(materials) = materials {
                slots.push((models.len(), materials.clone()));
            }
            models.push((*m, *h));
        }
    }
    let mut overrides = HashMap::new();
    if let Some(mut server) = world.get_resource_mut::<AssetServer>() {
        for (index, materials) in slots {
            let handles = materials.parts.iter().map(|s| s.resolve(&mut server));
            overrides.insert(index, handles.collect());
        }
    }
    let mut lights = Vec::new();
    let mut points = world.query::<(melon_core::bevy_ecs::entity::Entity, &PointLight)>();
    for (e, light) in points.iter(world) {
        if let Some(m) = matrices.get(&e) {
            lights.push(LocalLight {
                position: m.w_axis.truncate(),
                direction: Vec3::NEG_Z,
                color: light.color * light.intensity,
                range: light.range,
                spot: None,
                cast_shadows: light.cast_shadows,
            });
        }
    }
    let mut spots = world.query::<(melon_core::bevy_ecs::entity::Entity, &SpotLight)>();
    for (e, light) in spots.iter(world) {
        if let Some(m) = matrices.get(&e) {
            lights.push(LocalLight {
                position: m.w_axis.truncate(),
                direction: (-m.z_axis.truncate()).normalize_or(Vec3::NEG_Y),
                color: light.color * light.intensity,
                range: light.range,
                spot: Some((light.inner_angle, light.outer_angle.max(light.inner_angle))),
                cast_shadows: light.cast_shadows,
            });
        }
    }
    FrameData {
        view_proj,
        eye,
        light,
        environment,
        quality,
        models,
        overrides,
        lights,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use melon_core::bevy_ecs::world::World;
    use melon_core::{
        AssetServer, Camera, ChildOf, Environment, Handle, Light, Model, RenderQuality,
        SimulationState, Transform,
    };

    fn cube(world: &mut World) -> Handle<Model> {
        world
            .get_resource_or_insert_with(|| AssetServer::new(".".into()))
            .load("builtin:cube")
    }

    fn translation(m: glam::Mat4) -> Vec3 {
        m.w_axis.truncate()
    }

    #[test]
    fn one_model_entity_gives_one_entry() {
        let mut world = World::new();
        let h = cube(&mut world);
        world.spawn((Transform::from_translation(Vec3::new(1.0, 2.0, 3.0)), h));
        let frame = extract(&mut world);
        assert_eq!(frame.models.len(), 1);
        assert_eq!(translation(frame.models[0].0), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(frame.models[0].1, h);
    }

    #[test]
    fn mesh_materials_resolve_to_handles_by_model_index() {
        use melon_core::{MaterialSlot, MeshMaterials};
        let mut world = World::new();
        let h = cube(&mut world);
        world.spawn((Transform::default(), h));
        world.spawn((
            Transform::default(),
            h,
            MeshMaterials {
                parts: vec![
                    MaterialSlot::default(),
                    MaterialSlot {
                        guid: String::new(),
                        path: "assets/m.mat".into(),
                    },
                ],
            },
        ));
        let frame = extract(&mut world);
        assert_eq!(frame.overrides.len(), 1);
        let parts = frame.overrides.values().next().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].is_none());
        let server = world.resource::<AssetServer>();
        assert_eq!(server.path(parts[1].unwrap()), "assets/m.mat");
    }

    #[test]
    fn child_matrix_is_composed() {
        let mut world = World::new();
        let h = cube(&mut world);
        let parent = world
            .spawn(Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)))
            .id();
        world.spawn((Transform::from_translation(Vec3::Y), h, ChildOf(parent)));
        let frame = extract(&mut world);
        assert_eq!(frame.models.len(), 1);
        assert_eq!(translation(frame.models[0].0), Vec3::new(5.0, 1.0, 0.0));
    }

    #[test]
    fn entity_without_model_is_left_out() {
        let mut world = World::new();
        world.spawn(Transform::default());
        assert!(extract(&mut world).models.is_empty());
    }

    #[test]
    fn view_proj_comes_from_camera() {
        let mut world = World::new();
        let camera = Camera::default();
        world.insert_resource(camera);
        assert_eq!(extract(&mut world).view_proj, camera.view_proj());
    }

    #[test]
    fn missing_camera_and_light_give_defaults() {
        let mut world = World::new();
        let frame = extract(&mut world);
        assert_eq!(frame.view_proj, glam::Mat4::default());
        assert_eq!(frame.light.ambient, Light::default().ambient);
        assert_eq!(frame.environment, Environment::default());
    }

    #[test]
    fn environment_comes_from_resource() {
        let mut world = World::new();
        let env = Environment {
            fog_density: 0.5,
            ..Default::default()
        };
        world.insert_resource(env);
        assert_eq!(extract(&mut world).environment, env);
    }

    #[test]
    fn quality_comes_from_resource_or_default() {
        let mut world = World::new();
        assert_eq!(extract(&mut world).quality, RenderQuality::default());
        let low = RenderQuality::preset(melon_core::QualityPreset::Low);
        world.insert_resource(low);
        assert_eq!(extract(&mut world).quality, low);
    }

    #[test]
    fn blends_while_playing_and_paused() {
        assert!(blends(SimulationState::Playing) && blends(SimulationState::Paused));
    }

    #[test]
    fn no_blend_while_editing() {
        assert!(!blends(SimulationState::Editing));
    }
}
