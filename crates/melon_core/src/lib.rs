use bevy_ecs::{
    resource::Resource,
    schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel, Schedules},
    system::{Res, ResMut, ScheduleSystem},
    world::World,
};
use tracing::instrument;

pub mod components;
pub mod input;
pub mod resources;
pub mod stages;

pub use bevy_ecs;
pub use bevy_ecs::query::With;
pub use components::{
    Aabb, Bob, BodyInfo, BodyKind, MeshRef, Name, PendingDelete, PreviousTransform, Spin,
    Transform, store_previous_transforms_system,
};
pub use glam;
pub use input::{Input, KeyCode};
pub use resources::{Camera, Light, NameCounter, SimulationState, Time};
pub use stages::{FixedUpdate, PostUpdate, Update};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Resource, Default)]
pub struct FrameCounter {
    pub frame: u64,
}

#[derive(Resource, Default)]
pub struct LogInterval {
    pub frames_per_log: u64,
}

fn frame_counter_system(mut counter: ResMut<FrameCounter>) {
    counter.frame += 1;
}

fn log_frame_system(counter: Res<FrameCounter>, interval: Res<LogInterval>) {
    if interval.frames_per_log > 0 && counter.frame.is_multiple_of(interval.frames_per_log) {
        tracing::info!("frame {}", counter.frame);
    }
}

/// The ECS world plus its frame stages. Schedules live here, not as a
/// `World` resource, so scene load (which swaps in a fresh `World`) keeps them.
pub struct Core {
    pub world: World,
    pub schedules: Schedules,
}

impl Core {
    #[instrument(skip_all, level = "info")]
    pub fn new() -> Self {
        let mut world = World::new();
        world.init_resource::<FrameCounter>();
        world.insert_resource(LogInterval { frames_per_log: 60 });
        world.insert_resource(NameCounter::default());
        world.init_resource::<Time>();
        world.init_resource::<SimulationState>();

        let mut schedules = Schedules::new();
        schedules.insert(Schedule::new(FixedUpdate));
        schedules.insert(Schedule::new(Update));
        schedules.insert(Schedule::new(PostUpdate));

        let mut core = Self { world, schedules };
        core.add_systems(Update, (frame_counter_system, log_frame_system));
        core
    }

    /// Register systems into a frame stage (`FixedUpdate`, `Update`, `PostUpdate`).
    pub fn add_systems<M>(
        &mut self,
        label: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.schedules.add_systems(label, systems);
        self
    }

    /// Run one frame: `FixedUpdate` once per whole `Time::fixed_delta` of
    /// accumulated time (none while paused), then `Update`, then `PostUpdate`.
    /// Call `Time::update(frame_delta)` before this.
    #[instrument(skip(self), level = "debug")]
    pub fn update(&mut self) {
        let playing = self
            .world
            .get_resource::<SimulationState>()
            .is_none_or(|state| *state == SimulationState::Playing);

        if let Some(mut time) = self.world.get_resource_mut::<Time>() {
            time.overstep = if playing {
                time.overstep + time.delta_seconds
            } else {
                0.0
            };
        }
        // Time is borrowed only for the check, so the stage can borrow the world.
        while let Some(mut time) = self.world.get_resource_mut::<Time>()
            && time.overstep >= time.fixed_delta
        {
            time.overstep -= time.fixed_delta;
            self.run_stage(FixedUpdate);
        }

        self.run_stage(Update);
        self.run_stage(PostUpdate);
        self.world.clear_trackers();
    }

    fn run_stage(&mut self, label: impl ScheduleLabel) {
        if let Some(schedule) = self.schedules.get_mut(label) {
            schedule.run(&mut self.world);
        }
    }
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_new_has_frame_counter() {
        let core = Core::new();
        assert!(core.world.contains_resource::<FrameCounter>());
    }

    #[test]
    fn core_update_increments_frame() {
        let mut core = Core::new();
        assert_eq!(core.world.get_resource::<FrameCounter>().unwrap().frame, 0);
        core.update();
        assert_eq!(core.world.get_resource::<FrameCounter>().unwrap().frame, 1);
        core.update();
        assert_eq!(core.world.get_resource::<FrameCounter>().unwrap().frame, 2);
    }

    #[test]
    fn version_returns_crate_version() {
        assert_eq!(version(), "0.1.0");
    }

    #[derive(Resource, Default)]
    struct FixedCount(u32);

    fn count_fixed_system(mut count: ResMut<FixedCount>) {
        count.0 += 1;
    }

    fn counting_core() -> Core {
        let mut core = Core::new();
        core.world.init_resource::<FixedCount>();
        core.add_systems(FixedUpdate, count_fixed_system);
        core
    }

    fn run_frames(core: &mut Core, dt: f32, n: u32) {
        for _ in 0..n {
            core.world.resource_mut::<Time>().update(dt);
            core.update();
        }
    }

    fn steps(core: &Core) -> u32 {
        core.world.resource::<FixedCount>().0
    }

    #[test]
    fn fixed_rate_at_30fps() {
        let mut core = counting_core();
        run_frames(&mut core, 1.0 / 30.0, 30);
        assert!((59..=60).contains(&steps(&core)), "got {}", steps(&core));
    }

    #[test]
    fn fixed_rate_at_144fps() {
        let mut core = counting_core();
        run_frames(&mut core, 1.0 / 144.0, 144);
        assert!((59..=60).contains(&steps(&core)), "got {}", steps(&core));
    }

    #[test]
    fn hitch_clamped_to_15_steps() {
        let mut core = counting_core();
        run_frames(&mut core, 1.0, 1);
        assert!((14..=15).contains(&steps(&core)), "got {}", steps(&core));
    }

    #[test]
    fn alpha_is_leftover_fraction() {
        let mut core = counting_core();
        run_frames(&mut core, 0.025, 1);
        assert_eq!(steps(&core), 1);
        let alpha = core.world.resource::<Time>().alpha();
        assert!((alpha - 0.5).abs() < 1e-3, "alpha {alpha}");
    }

    #[test]
    fn paused_runs_no_fixed_steps() {
        let mut core = counting_core();
        core.world.insert_resource(SimulationState::Paused);
        run_frames(&mut core, 1.0 / 60.0, 60);
        assert_eq!(steps(&core), 0);
        assert_eq!(core.world.resource::<Time>().overstep, 0.0);
    }

    #[test]
    fn resume_does_not_burst() {
        let mut core = counting_core();
        core.world.insert_resource(SimulationState::Paused);
        run_frames(&mut core, 1.0 / 60.0, 60);
        core.world.insert_resource(SimulationState::Playing);
        run_frames(&mut core, 1.0 / 60.0, 1);
        assert!(steps(&core) <= 1, "got {}", steps(&core));
    }

    #[test]
    fn stages_survive_world_swap() {
        let mut core = counting_core();
        let mut fresh = World::new();
        fresh.insert_resource(Time::default());
        fresh.init_resource::<FixedCount>();
        fresh.init_resource::<FrameCounter>();
        fresh.insert_resource(LogInterval { frames_per_log: 0 });
        core.world = fresh;
        run_frames(&mut core, 1.0 / 60.0, 2);
        assert!(steps(&core) >= 1, "FixedUpdate lost after world swap");
    }

    #[derive(Resource, Default)]
    struct SeenByUpdate(u32);

    fn record_fixed_count_system(count: Res<FixedCount>, mut seen: ResMut<SeenByUpdate>) {
        seen.0 = count.0;
    }

    #[test]
    fn update_runs_after_fixed() {
        let mut core = counting_core();
        core.world.init_resource::<SeenByUpdate>();
        core.add_systems(Update, record_fixed_count_system);
        run_frames(&mut core, 1.0 / 30.0, 1);
        assert!(core.world.resource::<SeenByUpdate>().0 >= 1);
    }
}
