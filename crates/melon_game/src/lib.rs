use melon_core::bevy_ecs::system::{Query, Res};
use melon_core::glam::Quat;
use melon_core::{Bob, FrameCounter, Spin, Transform, With};

/// Rotate all entities with the Spin marker around the Y axis.
/// Hot-reloadable: edit this function, rebuild the dylib, and see the speed
/// change without restarting the engine.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn rotation_system(mut query: Query<&mut Transform, With<Spin>>) {
    for mut transform in &mut query {
        let angle = 0.5 / 60.0;
        transform.rotation = Quat::from_rotation_y(angle) * transform.rotation;
    }
}

/// Bob entities with the Bob marker up and down using a sine wave based on frame count.
/// Hot-reloadable: edit amplitude/frequency, rebuild, see the bob change live.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn bob_system(mut query: Query<&mut Transform, With<Bob>>, counter: Res<FrameCounter>) {
    let t = counter.frame as f32 * 0.01;
    for (i, mut transform) in query.iter_mut().enumerate() {
        let offset = (t + i as f32).sin() * 0.02;
        transform.translation.y += offset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::bevy_ecs::schedule::Schedule;
    use melon_core::bevy_ecs::world::World;
    use melon_core::{Bob, FrameCounter, Spin, Transform};

    #[test]
    fn rotation_system_rotates_transform() {
        let mut world = World::new();
        let entity = world.spawn((Transform::default(), Spin)).id();
        let rot_before = world.get::<Transform>(entity).unwrap().rotation;

        let mut schedule = Schedule::default();
        schedule.add_systems(rotation_system);
        schedule.run(&mut world);

        let rot_after = world.get::<Transform>(entity).unwrap().rotation;
        assert_ne!(
            rot_before, rot_after,
            "rotation should change after one tick"
        );
    }

    #[test]
    fn rotation_system_ignores_non_spin() {
        let mut world = World::new();
        let entity = world.spawn(Transform::default()).id();
        let rot_before = world.get::<Transform>(entity).unwrap().rotation;

        let mut schedule = Schedule::default();
        schedule.add_systems(rotation_system);
        schedule.run(&mut world);

        let rot_after = world.get::<Transform>(entity).unwrap().rotation;
        assert_eq!(
            rot_before, rot_after,
            "rotation should not change for entities without Spin"
        );
    }

    #[test]
    fn bob_system_changes_y_position() {
        let mut world = World::new();
        world.insert_resource(FrameCounter { frame: 100 });
        let entity = world
            .spawn((
                Transform::from_translation(melon_core::glam::Vec3::new(0.0, 0.0, 0.0)),
                Bob,
            ))
            .id();
        let y_before = world.get::<Transform>(entity).unwrap().translation.y;

        let mut schedule = Schedule::default();
        schedule.add_systems(bob_system);
        schedule.run(&mut world);

        let y_after = world.get::<Transform>(entity).unwrap().translation.y;
        assert!(
            (y_after - y_before).abs() > 0.0,
            "bob should change Y position"
        );
    }

    #[test]
    fn bob_system_ignores_non_bob() {
        let mut world = World::new();
        world.insert_resource(FrameCounter { frame: 100 });
        let entity = world
            .spawn(Transform::from_translation(melon_core::glam::Vec3::new(
                0.0, 0.0, 0.0,
            )))
            .id();
        let y_before = world.get::<Transform>(entity).unwrap().translation.y;

        let mut schedule = Schedule::default();
        schedule.add_systems(bob_system);
        schedule.run(&mut world);

        let y_after = world.get::<Transform>(entity).unwrap().translation.y;
        assert_eq!(
            y_before, y_after,
            "bob should not change Y position for entities without Bob"
        );
    }
}
