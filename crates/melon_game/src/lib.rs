use melon_core::bevy_ecs::system::{Query, Res};
use melon_core::glam::Quat;
use melon_core::{Bob, Spin, Time, Transform, With};

/// Spin speed around Y, in rad/s.
const ROTATION_SPEED: f32 = 0.5;
/// Bob height, in m.
const BOB_AMPLITUDE: f32 = 2.0;
/// Bob angular frequency, in rad/s.
const BOB_FREQ: f32 = 0.6;

/// Rotate all entities with the Spin marker around the Y axis.
/// Hot-reloadable: edit this function, rebuild the dylib, and see the speed
/// change without restarting the engine.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn rotation_system(mut query: Query<&mut Transform, With<Spin>>, time: Res<Time>) {
    let angle = ROTATION_SPEED * time.delta_seconds;
    for mut transform in &mut query {
        transform.rotation = Quat::from_rotation_y(angle) * transform.rotation;
    }
}

/// Bob entities with the Bob marker up and down on a sine wave.
/// Adds this frame's change in the sine, so the motion never drifts.
/// Hot-reloadable: edit amplitude/frequency, rebuild, see the bob change live.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn bob_system(mut query: Query<&mut Transform, With<Bob>>, time: Res<Time>) {
    let (t, dt) = (time.elapsed_seconds, time.delta_seconds);
    for (i, mut transform) in query.iter_mut().enumerate() {
        let phase = i as f32;
        transform.translation.y +=
            BOB_AMPLITUDE * ((BOB_FREQ * t + phase).sin() - (BOB_FREQ * (t - dt) + phase).sin());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::bevy_ecs::schedule::Schedule;
    use melon_core::bevy_ecs::world::World;
    use melon_core::glam::Vec3;
    use melon_core::{Bob, Spin, Time, Transform};

    fn world_with_time(dt: f32) -> World {
        let mut world = World::new();
        let time = Time {
            delta_seconds: dt,
            ..Time::default()
        };
        world.insert_resource(time);
        world
    }

    #[test]
    fn rotation_uses_delta() {
        let mut world = world_with_time(0.2);
        let e = world.spawn((Transform::default(), Spin)).id();
        let mut schedule = Schedule::default();
        schedule.add_systems(rotation_system);
        schedule.run(&mut world);
        let rot = world.get::<Transform>(e).unwrap().rotation;
        assert!(rot.angle_between(Quat::from_rotation_y(0.1)) < 1e-5);
    }

    #[test]
    fn rotation_system_ignores_non_spin() {
        let mut world = world_with_time(0.2);
        let e = world.spawn(Transform::default()).id();
        let mut schedule = Schedule::default();
        schedule.add_systems(rotation_system);
        schedule.run(&mut world);
        assert_eq!(world.get::<Transform>(e).unwrap().rotation, Quat::IDENTITY);
    }

    #[test]
    fn bob_has_no_drift() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let e = world.spawn((Transform::default(), Bob)).id();
        let mut schedule = Schedule::default();
        schedule.add_systems(bob_system);
        for _ in 0..600 {
            let mut time = world.resource_mut::<Time>();
            time.delta_seconds = 1.0 / 60.0;
            time.elapsed_seconds += 1.0 / 60.0;
            schedule.run(&mut world);
        }
        let y = world.get::<Transform>(e).unwrap().translation.y;
        // Offsets telescope to A * sin(w * 10 s); a drifting bob would not match.
        assert!((y - 2.0 * (0.6f32 * 10.0).sin()).abs() < 1e-3, "y = {y}");
    }

    #[test]
    fn bob_has_no_drift_across_pause() {
        use melon_core::bevy_ecs::schedule::IntoScheduleConfigs;
        use melon_core::{Core, SimulationState, Update};
        let mut core = Core::new();
        let playing = |s: Res<SimulationState>| *s == SimulationState::Playing;
        core.add_systems(Update, bob_system.run_if(playing));
        let e = core.world.spawn((Transform::default(), Bob)).id();
        let run = |core: &mut Core, state: SimulationState, n: u32| {
            core.world.insert_resource(state);
            for _ in 0..n {
                core.world.resource_mut::<Time>().update(1.0 / 60.0);
                core.update();
            }
        };
        run(&mut core, SimulationState::Playing, 300);
        run(&mut core, SimulationState::Paused, 120);
        run(&mut core, SimulationState::Playing, 300);
        let y = core.world.get::<Transform>(e).unwrap().translation.y;
        assert!((y - 2.0 * (0.6f32 * 10.0).sin()).abs() < 1e-3, "y = {y}");
    }

    #[test]
    fn bob_system_ignores_non_bob() {
        let mut world = world_with_time(1.0 / 60.0);
        let e = world.spawn(Transform::default()).id();
        let mut schedule = Schedule::default();
        schedule.add_systems(bob_system);
        schedule.run(&mut world);
        assert_eq!(world.get::<Transform>(e).unwrap().translation, Vec3::ZERO);
    }
}
