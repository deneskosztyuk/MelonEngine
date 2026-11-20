use melon_core::bevy_ecs::schedule::IntoScheduleConfigs;
use melon_core::bevy_ecs::system::{Query, Res};
use melon_core::glam::Quat;
use melon_core::{App, Bob, Plugin, SimulationState, Spin, Time, Transform, Update, With};

/// Spin speed around Y, in rad/s.
const ROTATION_SPEED: f32 = 0.5;
/// Bob height, in m.
const BOB_AMPLITUDE: f32 = 2.0;
/// Bob angular frequency, in rad/s.
const BOB_FREQ: f32 = 0.6;

/// The game's own systems and resources.
pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        let playing = |state: Res<SimulationState>| *state == SimulationState::Playing;
        app.add_systems(Update, (rotation_system, bob_system).run_if(playing));
    }
}

/// Hot reload entry: route this library's logs to the host's subscriber, then
/// build the plugin.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn melon_game_build(app: &mut App, log: &tracing::Dispatch) {
    // Err means already set (an earlier load); the first dispatcher stays.
    let _ = tracing::dispatcher::set_global_default(log.clone());
    GamePlugin.build(app);
}

/// Hot reload entry: the host refuses to load a library whose value differs.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub fn melon_game_abi() -> u64 {
    melon_core::abi_fingerprint()
}

/// Rotate all entities with the Spin marker around the Y axis.
pub fn rotation_system(mut query: Query<&mut Transform, With<Spin>>, time: Res<Time>) {
    let angle = ROTATION_SPEED * time.delta_seconds;
    for mut transform in &mut query {
        transform.rotation = Quat::from_rotation_y(angle) * transform.rotation;
    }
}

/// Bob entities with the Bob marker up and down on a sine wave.
/// Adds this frame's change in the sine, so the motion never drifts.
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
    use melon_core::{Bob, Core, SimulationState, Spin, Time, Transform};

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

    fn spin_after_one_frame(state: SimulationState) -> Quat {
        let mut core = Core::new();
        core.add_plugin(&GamePlugin);
        let e = core.world.spawn((Transform::default(), Spin)).id();
        core.world.insert_resource(state);
        core.world.resource_mut::<Time>().update(0.2);
        core.update();
        core.world.get::<Transform>(e).unwrap().rotation
    }

    #[test]
    fn game_plugin_spins_while_playing() {
        assert_ne!(
            spin_after_one_frame(SimulationState::Playing),
            Quat::IDENTITY
        );
    }

    #[test]
    fn game_plugin_idle_while_editing() {
        assert_eq!(
            spin_after_one_frame(SimulationState::Editing),
            Quat::IDENTITY
        );
    }

    #[test]
    fn build_entry_spins_while_playing_and_rebuilds() {
        let mut core = Core::new();
        let log = tracing::Dispatch::none();
        core.rebuild_plugins(|app| melon_game_build(app, &log));
        // A second build (a reload) replaces the first without panicking.
        core.rebuild_plugins(|app| melon_game_build(app, &log));
        let e = core.world.spawn((Transform::default(), Spin)).id();
        core.world.insert_resource(SimulationState::Playing);
        core.world.resource_mut::<Time>().update(0.2);
        core.update();
        let rot = core.world.get::<Transform>(e).unwrap().rotation;
        // One system copy, not two: 0.5 rad/s * 0.2 s.
        assert!(rot.angle_between(Quat::from_rotation_y(0.1)) < 1e-5);
    }

    #[test]
    fn abi_entry_matches_core() {
        assert_eq!(melon_game_abi(), melon_core::abi_fingerprint());
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
