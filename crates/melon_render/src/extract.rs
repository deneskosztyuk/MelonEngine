//! Extraction: the ECS world turned into plain data the renderer draws.

use glam::Mat4;
use melon_core::bevy_ecs::world::World;
use melon_core::hierarchy::render_matrices;
use melon_core::{Camera, Handle, Light, Model, SimulationState, Time};

/// Everything one view needs to draw a frame.
pub struct FrameData {
    pub view_proj: Mat4,
    pub light: Light,
    /// World matrix and model of every entity with a `Handle<Model>`.
    pub models: Vec<(Mat4, Handle<Model>)>,
}

/// Blend fixed-step entities while Playing and Paused (alpha is frozen while
/// paused, so the pose holds still). In edit mode draw the current pose.
pub fn blends(state: SimulationState) -> bool {
    state != SimulationState::Editing
}

/// Read the camera, the light and every model entity's world matrix
/// (interpolated along its parent chain) from `world`.
pub fn extract(world: &mut World) -> FrameData {
    let view_proj = world
        .get_resource::<Camera>()
        .map(Camera::view_proj)
        .unwrap_or_default();
    let light = world.get_resource::<Light>().copied().unwrap_or_default();
    let alpha = world.get_resource::<Time>().map_or(1.0, Time::alpha);
    let state = world
        .get_resource::<SimulationState>()
        .copied()
        .unwrap_or(SimulationState::Editing);
    let matrices = render_matrices(world, alpha, blends(state));
    let models = world
        .query::<(melon_core::bevy_ecs::entity::Entity, &Handle<Model>)>()
        .iter(world)
        .filter_map(|(e, h)| matrices.get(&e).map(|m| (*m, *h)))
        .collect();
    FrameData {
        view_proj,
        light,
        models,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use melon_core::bevy_ecs::world::World;
    use melon_core::{
        AssetServer, Camera, ChildOf, Handle, Light, Model, SimulationState, Transform,
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
