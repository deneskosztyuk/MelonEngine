use bevy_ecs::{
    resource::Resource,
    schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel, Schedules},
    system::{Res, ResMut, ScheduleSystem},
    world::World,
};
use tracing::instrument;

pub mod actions;
pub mod assets;
pub mod components;
pub mod hierarchy;
pub mod input;
pub mod resources;
pub mod stages;

pub use actions::{
    Action, ActionKind, Actions, AxisBinding, InputMap, LoadOutcome, update_actions_system,
};
pub use assets::{AssetServer, Handle, Model};
pub use bevy_ecs;
pub use bevy_ecs::query::With;
pub use components::{
    Aabb, Bob, BodyInfo, BodyKind, Name, PendingDelete, PreviousTransform, Spin, Transform,
    store_previous_transforms_system,
};
pub use glam;
pub use hierarchy::{ChildOf, Children, GlobalTransform};
pub use input::{GameInputGate, Input, InputButton, InputFocus, KeyCode, MouseButton, RawPresses};
pub use resources::{Camera, Light, NameCounter, SimulationState, Time};
pub use stages::{FixedUpdate, PostUpdate, PreUpdate, Update};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Resource, Default)]
pub struct FrameCounter {
    pub frame: u64,
}

/// Request one fixed step while `Paused` (the editor's Step button).
/// `Core::update` consumes it.
#[derive(Resource, Default)]
pub struct StepOnce;

#[derive(Resource, Default)]
pub struct LogInterval {
    pub frames_per_log: u64,
}

/// End of frame: input edges, deltas and wheel start over, so a game system
/// sees only this frame's changes.
fn clear_input_frame_system(input: Option<ResMut<Input>>) {
    if let Some(mut input) = input {
        input.clear_frame();
    }
}

fn frame_counter_system(mut counter: ResMut<FrameCounter>) {
    counter.frame += 1;
}

fn log_frame_system(counter: Res<FrameCounter>, interval: Res<LogInterval>) {
    if interval.frames_per_log > 0 && counter.frame.is_multiple_of(interval.frames_per_log) {
        tracing::info!("frame {}", counter.frame);
    }
}

/// The ECS world plus its frame stages.
/// Never replace `world` after the first frame: bevy systems are bound to the
/// first `World` they run on, so `update` would panic.
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
        world.insert_resource(AssetServer::new(".".into()));
        world.init_resource::<Input>();
        world.init_resource::<InputFocus>();
        world.init_resource::<GameInputGate>();
        world.init_resource::<RawPresses>();
        world.init_resource::<InputMap>();
        world.init_resource::<Actions>();

        let mut schedules = Schedules::new();
        schedules.insert(Schedule::new(PreUpdate));
        schedules.insert(Schedule::new(FixedUpdate));
        schedules.insert(Schedule::new(Update));
        schedules.insert(Schedule::new(PostUpdate));

        let mut core = Self { world, schedules };
        core.add_systems(PreUpdate, update_actions_system);
        core.add_systems(Update, (frame_counter_system, log_frame_system));
        core.add_systems(
            PostUpdate,
            (
                clear_input_frame_system,
                hierarchy::propagate_transforms_system,
            ),
        );
        core.add_systems(Update, assets::hot_reload_system);
        core
    }

    /// Register systems into a frame stage (`PreUpdate`, `FixedUpdate`,
    /// `Update`, `PostUpdate`).
    pub fn add_systems<M>(
        &mut self,
        label: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.schedules.add_systems(label, systems);
        self
    }

    /// Run one frame: `PreUpdate`, then `FixedUpdate` once per whole `Time::fixed_delta` of
    /// accumulated time (only while `Playing`), then `Update`, then `PostUpdate`.
    /// Call `Time::update(frame_delta)` before this.
    #[instrument(skip(self), level = "debug")]
    pub fn update(&mut self) {
        let state = self
            .world
            .get_resource::<SimulationState>()
            .copied()
            .unwrap_or_default();

        if let Some(mut time) = self.world.get_resource_mut::<Time>() {
            match state {
                SimulationState::Playing => {
                    time.elapsed_seconds += time.delta_seconds;
                    time.overstep += time.delta_seconds;
                }
                // Frozen: the blended pose holds still and resume is smooth.
                SimulationState::Paused => {}
                SimulationState::Editing => time.overstep = 0.0,
            }
        }
        self.run_stage(PreUpdate);
        // Step button: exactly one fixed step while paused.
        if state == SimulationState::Paused && self.world.remove_resource::<StepOnce>().is_some() {
            if let Some(mut time) = self.world.get_resource_mut::<Time>() {
                time.elapsed_seconds += time.fixed_delta;
            }
            self.run_stage(FixedUpdate);
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
        core.world.insert_resource(SimulationState::Playing);
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
    }

    fn set_state(core: &mut Core, state: SimulationState) {
        core.world.insert_resource(state);
    }

    #[derive(Resource, Default)]
    struct Order(Vec<&'static str>);

    #[test]
    fn pre_update_runs_before_fixed_update() {
        let mut core = counting_core();
        core.world.init_resource::<Order>();
        core.add_systems(PreUpdate, |mut o: ResMut<Order>| o.0.push("pre"));
        core.add_systems(FixedUpdate, |mut o: ResMut<Order>| o.0.push("fixed"));
        run_frames(&mut core, 1.0 / 60.0, 1);
        assert_eq!(core.world.resource::<Order>().0, ["pre", "fixed"]);
    }

    #[test]
    fn input_mouse_delta_cleared_each_frame() {
        let mut core = Core::new();
        let mut input = crate::Input::default();
        input.mouse_delta = glam::Vec2::new(5.0, 5.0);
        core.world.insert_resource(input);
        core.update();
        assert_eq!(
            core.world.resource::<crate::Input>().mouse_delta,
            glam::Vec2::ZERO
        );
    }

    #[test]
    fn raw_presses_survive_the_frame_for_the_editor() {
        let mut core = Core::new();
        core.world
            .resource_mut::<crate::RawPresses>()
            .0
            .push(crate::InputButton::Key(crate::KeyCode::W));
        core.update();
        assert_eq!(core.world.resource::<crate::RawPresses>().0.len(), 1);
    }

    #[test]
    fn missing_state_runs_no_fixed_steps() {
        let mut core = counting_core();
        core.world.remove_resource::<SimulationState>();
        run_frames(&mut core, 1.0 / 30.0, 10);
        assert_eq!(
            steps(&core),
            0,
            "a missing state means Editing, like everywhere else"
        );
    }

    #[test]
    fn step_once_runs_one_fixed_step_while_paused() {
        let mut core = counting_core();
        set_state(&mut core, SimulationState::Paused);
        core.world.insert_resource(StepOnce);
        run_frames(&mut core, 1.0 / 60.0, 3);
        assert_eq!(steps(&core), 1);
        let elapsed = core.world.resource::<Time>().elapsed_seconds;
        assert!((elapsed - 1.0 / 60.0).abs() < 1e-6, "elapsed {elapsed}");
        assert!(!core.world.contains_resource::<StepOnce>());
    }

    #[test]
    fn step_once_ignored_while_editing() {
        let mut core = counting_core();
        set_state(&mut core, SimulationState::Editing);
        core.world.insert_resource(StepOnce);
        run_frames(&mut core, 1.0 / 60.0, 3);
        assert_eq!(steps(&core), 0);
    }

    #[test]
    fn elapsed_advances_only_while_playing() {
        let mut core = counting_core();
        run_frames(&mut core, 1.0 / 60.0, 60);
        set_state(&mut core, SimulationState::Paused);
        run_frames(&mut core, 1.0 / 60.0, 60);
        set_state(&mut core, SimulationState::Editing);
        run_frames(&mut core, 1.0 / 60.0, 60);
        let elapsed = core.world.resource::<Time>().elapsed_seconds;
        assert!((elapsed - 1.0).abs() < 1e-3, "elapsed {elapsed}");
    }

    #[test]
    fn paused_keeps_overstep() {
        let mut core = counting_core();
        run_frames(&mut core, 0.025, 1);
        let o = core.world.resource::<Time>().overstep;
        set_state(&mut core, SimulationState::Paused);
        run_frames(&mut core, 1.0 / 60.0, 10);
        assert_eq!(core.world.resource::<Time>().overstep, o);
        assert_eq!(steps(&core), 1);
    }

    #[test]
    fn editing_zeroes_overstep() {
        let mut core = counting_core();
        run_frames(&mut core, 0.025, 1);
        set_state(&mut core, SimulationState::Editing);
        run_frames(&mut core, 1.0 / 60.0, 1);
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

    /// Pins the rule that `Core::world` must never be replaced (scene load
    /// clears entities in place instead).
    #[test]
    #[should_panic(expected = "mismatched World")]
    fn world_swap_after_first_frame_panics() {
        let mut core = counting_core();
        run_frames(&mut core, 1.0 / 60.0, 1); // systems initialize on the first World
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
