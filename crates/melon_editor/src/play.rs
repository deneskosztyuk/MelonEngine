//! Play / Pause / Stop. Play snapshots the scene in memory, Stop restores it,
//! so anything changed while playing is thrown away.

use anyhow::{Context, Result};
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use melon_core::{SimulationState, Time};
use melon_physics::PhysicsWorld;

use crate::scene::{SceneData, apply_scene, scene_to_data};

/// The scene as it was when Play was pressed in edit mode.
#[derive(Resource)]
pub struct PlaySnapshot(SceneData);

fn state(world: &World) -> SimulationState {
    world
        .get_resource::<SimulationState>()
        .copied()
        .unwrap_or_default()
}

/// Rebuild the scene (and its physics bodies) from `data`.
fn rebuild(world: &mut World, data: &SceneData) -> Result<()> {
    world
        .try_resource_scope::<PhysicsWorld, _>(|world, mut physics| {
            apply_scene(world, &mut physics, data)
        })
        .context("no PhysicsWorld resource")?
}

/// From `Editing`: snapshot the scene, rebuild it from the snapshot so physics
/// matches every edit-mode change, then start playing. From `Paused`: resume.
/// On error nothing changes.
pub fn play(world: &mut World) -> Result<()> {
    match state(world) {
        SimulationState::Editing => {
            let snapshot = scene_to_data(world);
            if let Err(error) = rebuild(world, &snapshot) {
                tracing::error!(?error, "play refused");
                return Err(error);
            }
            world.insert_resource(PlaySnapshot(snapshot));
            world.insert_resource(SimulationState::Playing);
        }
        SimulationState::Paused => world.insert_resource(SimulationState::Playing),
        SimulationState::Playing => {}
    }
    Ok(())
}

/// Freeze a running game. Does nothing unless `Playing`.
pub fn pause(world: &mut World) {
    if state(world) == SimulationState::Playing {
        world.insert_resource(SimulationState::Paused);
    }
}

/// Advance one fixed step while `Paused`. Does nothing in other states.
pub fn step(world: &mut World) {
    if state(world) == SimulationState::Paused {
        world.insert_resource(melon_core::StepOnce);
    }
}

/// Restore the scene from the Play snapshot and return to `Editing`, with play
/// time reset. Does nothing without a snapshot.
pub fn stop(world: &mut World) -> Result<()> {
    let Some(PlaySnapshot(snapshot)) = world.remove_resource::<PlaySnapshot>() else {
        return Ok(());
    };
    if let Err(error) = rebuild(world, &snapshot) {
        tracing::error!(?error, "stop could not restore the scene");
        world.insert_resource(PlaySnapshot(snapshot));
        return Err(error);
    }
    if let Some(mut time) = world.get_resource_mut::<Time>() {
        time.elapsed_seconds = 0.0;
        time.overstep = 0.0;
    }
    // A Step queued in the same frame must not fire on the next Pause.
    world.remove_resource::<melon_core::StepOnce>();
    world.insert_resource(SimulationState::Editing);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::entity::Entity;
    use melon_core::glam::Vec3;
    use melon_core::{BodyInfo, BodyKind, MeshRef, Name, NameCounter, Time, Transform};
    use melon_physics::RigidBodyComponent;

    fn edit_world() -> World {
        let mut world = World::new();
        world.insert_resource(PhysicsWorld::new());
        world.insert_resource(Time::default());
        world.insert_resource(SimulationState::Editing);
        world.insert_resource(NameCounter(0));
        world.spawn((
            Name("Floor".to_string()),
            Transform::from_translation(Vec3::new(0.0, -1.0, 0.0)),
            MeshRef::Cube,
            BodyInfo {
                body_type: BodyKind::Fixed,
                half_extents: Vec3::new(5.0, 0.5, 5.0),
            },
        ));
        world.spawn((
            Name("Box".to_string()),
            Transform::from_translation(Vec3::new(0.0, 3.0, 0.0)),
            MeshRef::Cube,
            BodyInfo {
                body_type: BodyKind::Dynamic,
                half_extents: Vec3::splat(0.5),
            },
        ));
        world
    }

    fn find(world: &mut World, name: &str) -> Entity {
        world
            .query::<(Entity, &Name)>()
            .iter(world)
            .find(|(_, n)| n.0 == name)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no entity named {name}"))
    }

    fn box_transform(world: &mut World) -> Transform {
        let e = find(world, "Box");
        *world.get::<Transform>(e).unwrap()
    }

    fn set_box_y(world: &mut World, y: f32) {
        let e = find(world, "Box");
        world.get_mut::<Transform>(e).unwrap().translation.y = y;
    }

    fn state(world: &World) -> SimulationState {
        *world.resource::<SimulationState>()
    }

    #[test]
    fn stop_restores_moved_entity() {
        let mut world = edit_world();
        play(&mut world).unwrap();
        set_box_y(&mut world, 9.0);
        stop(&mut world).unwrap();
        assert!((box_transform(&mut world).translation.y - 3.0).abs() < 1e-6);
        assert_eq!(state(&world), SimulationState::Editing);
    }

    #[test]
    fn stop_removes_spawned_and_restores_despawned() {
        let mut world = edit_world();
        play(&mut world).unwrap();
        world.spawn((Name("Extra".to_string()), Transform::default()));
        let floor = find(&mut world, "Floor");
        world.despawn(floor);
        stop(&mut world).unwrap();
        let mut names: Vec<String> = world
            .query::<&Name>()
            .iter(&world)
            .map(|n| n.0.clone())
            .collect();
        names.sort();
        assert_eq!(names, ["Box", "Floor"]);
    }

    #[test]
    fn play_applies_edit_mode_moves_to_physics() {
        let mut world = edit_world();
        set_box_y(&mut world, 7.0);
        play(&mut world).unwrap();
        let e = find(&mut world, "Box");
        let handle = world.get::<RigidBodyComponent>(e).unwrap().body_handle;
        let y = world
            .resource::<PhysicsWorld>()
            .body_translation(handle)
            .unwrap()
            .y;
        assert!((y - 7.0).abs() < 1e-6, "rapier y {y}");
    }

    #[test]
    fn stop_resets_play_time() {
        let mut world = edit_world();
        play(&mut world).unwrap();
        {
            let mut time = world.resource_mut::<Time>();
            time.elapsed_seconds = 5.0;
            time.overstep = 0.01;
        }
        stop(&mut world).unwrap();
        let time = world.resource::<Time>();
        assert_eq!((time.elapsed_seconds, time.overstep), (0.0, 0.0));
    }

    #[test]
    fn pause_then_play_resumes_without_new_snapshot() {
        let mut world = edit_world();
        play(&mut world).unwrap();
        set_box_y(&mut world, 9.0);
        pause(&mut world);
        assert_eq!(state(&world), SimulationState::Paused);
        play(&mut world).unwrap();
        assert_eq!(state(&world), SimulationState::Playing);
        assert!((box_transform(&mut world).translation.y - 9.0).abs() < 1e-6);
        stop(&mut world).unwrap();
        assert!((box_transform(&mut world).translation.y - 3.0).abs() < 1e-6);
    }

    #[test]
    fn play_refuses_invalid_scene() {
        let mut world = edit_world();
        let e = find(&mut world, "Box");
        world.get_mut::<Transform>(e).unwrap().scale = Vec3::ZERO;
        assert!(play(&mut world).is_err());
        assert_eq!(state(&world), SimulationState::Editing);
        assert!(world.get_resource::<PlaySnapshot>().is_none());
        assert_eq!(box_transform(&mut world).scale, Vec3::ZERO);
    }

    #[test]
    fn step_only_when_paused() {
        let mut world = edit_world();
        step(&mut world);
        assert!(!world.contains_resource::<melon_core::StepOnce>());
        play(&mut world).unwrap();
        step(&mut world);
        assert!(!world.contains_resource::<melon_core::StepOnce>());
        pause(&mut world);
        step(&mut world);
        assert!(world.contains_resource::<melon_core::StepOnce>());
    }

    #[test]
    fn stop_clears_pending_step() {
        let mut world = edit_world();
        play(&mut world).unwrap();
        pause(&mut world);
        step(&mut world);
        stop(&mut world).unwrap();
        assert!(!world.contains_resource::<melon_core::StepOnce>());
    }

    #[test]
    fn wrong_state_calls_are_noops() {
        let mut world = edit_world();
        stop(&mut world).unwrap();
        assert_eq!(state(&world), SimulationState::Editing);
        pause(&mut world);
        assert_eq!(state(&world), SimulationState::Editing);

        play(&mut world).unwrap();
        set_box_y(&mut world, 9.0);
        play(&mut world).unwrap(); // second play must not re-snapshot
        assert_eq!(state(&world), SimulationState::Playing);
        stop(&mut world).unwrap();
        assert!((box_transform(&mut world).translation.y - 3.0).abs() < 1e-6);
        stop(&mut world).unwrap();
        assert_eq!(state(&world), SimulationState::Editing);
    }
}
