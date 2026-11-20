use bevy_ecs::{
    component::Component,
    reflect::AppTypeRegistry,
    resource::Resource,
    schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel, Schedules},
    system::{Res, ResMut, ScheduleSystem},
    world::World,
};
use bevy_reflect::{GetTypeRegistration, Reflect, TypePath};
use tracing::instrument;

pub mod actions;
pub mod assets;
pub mod audio;
pub mod components;
pub mod hierarchy;
pub mod input;
pub mod pack;
pub mod physics;
pub mod physics_settings;
pub mod plugin;
pub mod quality;
pub mod reflect;
pub mod resources;
mod ron_file;
pub mod scene;
pub mod stages;

pub use actions::{
    Action, ActionKind, Actions, AxisBinding, InputMap, LoadOutcome, update_actions_system,
};
pub use assets::{AssetGuid, AssetMeta, AssetServer, Handle, META_VERSION, Model, meta_path};
pub use audio::{
    AudioBus, AudioClip, AudioListener, AudioPreview, AudioRequest, AudioRequests, AudioSettings,
    AudioSource,
};
pub use bevy_ecs;
pub use bevy_ecs::query::With;
pub use bevy_reflect;
pub use components::{
    Aabb, Bob, BodyInfo, BodyKind, Name, PendingDelete, PreviousTransform, Spin, Transform,
    store_previous_transforms_system,
};
pub use glam;
pub use hierarchy::{ChildOf, Children, GlobalTransform};
pub use input::{GameInputGate, Input, InputButton, InputFocus, KeyCode, MouseButton, RawPresses};
pub use physics::{
    CharacterController, Collider, ColliderShape, CollisionEvent, CollisionEventKind, Joint,
    JointKind, PhysicsQueries, PhysicsRequest, PhysicsRequests, QueryBackend, QueryFilter, RayHit,
    RigidBody, RigidBodyKind, Velocity,
};
pub use physics_settings::{LAYER_COUNT, PhysicsSettings};
pub use plugin::{App, Plugin, abi_fingerprint};
pub use quality::{
    GiMode, PresentPreference, QualityPreset, QualitySetting, RenderQuality, RenderTimings,
    SsgiResolution,
};
pub use reflect::{
    ComponentInfo, QuatReflect, ReflectVariantDefaults, RegistryGeneration, VariantDefaults,
    Vec2Reflect, Vec3Reflect, Vec4Reflect, display_name, registered_components,
};
pub use resources::{Camera, Environment, Light, NameCounter, SimulationState, Time};
pub use stages::{FixedPostUpdate, FixedUpdate, PhysicsStep, PostUpdate, PreUpdate, Update};

/// What a game crate needs to define and register components:
/// `use melon_core::prelude::*;` then `#[derive(Component, Reflect)]`.
pub mod prelude {
    pub use bevy_ecs::prelude::*;
    pub use bevy_reflect::prelude::*;

    // Crate names the derive macros expand to.
    pub use crate::{
        Aabb, Actions, App, Bob, BodyInfo, BodyKind, Core, FixedUpdate, Name, Plugin, PostUpdate,
        PreUpdate, SimulationState, Spin, Time, Transform, Update,
    };
    pub use crate::{bevy_ecs, bevy_reflect};
}

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
    /// Plugin systems, kept apart from `schedules` so `rebuild_plugins`
    /// replaces exactly them. Each runs right after its engine stage.
    pub plugin_schedules: Schedules,
}

fn stage_schedules() -> Schedules {
    let mut schedules = Schedules::new();
    schedules.insert(Schedule::new(PreUpdate));
    schedules.insert(Schedule::new(FixedUpdate));
    schedules.insert(Schedule::new(PhysicsStep));
    schedules.insert(Schedule::new(FixedPostUpdate));
    schedules.insert(Schedule::new(Update));
    schedules.insert(Schedule::new(PostUpdate));
    schedules
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
        world.init_resource::<RenderQuality>();
        world.init_resource::<AudioSettings>();
        world.init_resource::<AudioRequests>();
        world.init_resource::<AudioPreview>();
        world.init_resource::<AppTypeRegistry>();
        world.init_resource::<RegistryGeneration>();
        world.init_resource::<bevy_ecs::message::Messages<CollisionEvent>>();
        world.init_resource::<PhysicsQueries>();
        world.init_resource::<PhysicsRequests>();
        world.init_resource::<PhysicsSettings>();

        let mut core = Self {
            world,
            schedules: stage_schedules(),
            plugin_schedules: stage_schedules(),
        };
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
        core.register_component::<Transform>()
            .register_component::<Aabb>()
            .register_component::<RigidBody>()
            .register_component::<Collider>()
            .register_component::<CharacterController>()
            .register_component::<Joint>()
            .register_component::<Spin>()
            .register_component::<Bob>()
            .register_component::<AudioSource>()
            .register_component::<AudioListener>();
        core
    }

    /// Make an engine component saved and editable (scene files, Inspector).
    /// Plugins use `App::register_component`, which checks the layout.
    pub fn register_component<T>(&mut self) -> &mut Self
    where
        T: Component + Reflect + TypePath + GetTypeRegistration,
    {
        reflect::register::<T>(&mut self.world);
        self
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

    /// Build a plugin into the plugin schedules.
    pub fn add_plugin(&mut self, plugin: &dyn Plugin) {
        let mut app = App {
            world: &mut self.world,
            schedules: &mut self.plugin_schedules,
            errors: Vec::new(),
        };
        plugin.build(&mut app);
        for error in app.errors {
            tracing::error!("Plugin build: {error}");
        }
    }

    /// Drop every plugin system and build again (game hot reload). The world,
    /// its entities and resources stay. If a plugin resource changed its
    /// layout, no plugin system is kept and the error says to restart.
    pub fn rebuild_plugins(&mut self, build: impl FnOnce(&mut App)) -> Result<(), String> {
        self.plugin_schedules = stage_schedules();
        let mut app = App {
            world: &mut self.world,
            schedules: &mut self.plugin_schedules,
            errors: Vec::new(),
        };
        build(&mut app);
        if app.errors.is_empty() {
            return Ok(());
        }
        let errors = app.errors.join("; ");
        // New systems must not run against old-layout storage.
        self.plugin_schedules = stage_schedules();
        Err(format!("plugin systems stopped: {errors}"))
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
            self.fixed_step();
        }
        // Time is borrowed only for the check, so the stage can borrow the world.
        while let Some(mut time) = self.world.get_resource_mut::<Time>()
            && time.overstep >= time.fixed_delta
        {
            time.overstep -= time.fixed_delta;
            self.fixed_step();
        }

        self.run_stage(Update);
        self.run_stage(PostUpdate);
        self.world.clear_trackers();
    }

    /// One fixed step: game and engine `FixedUpdate`, the physics step, then
    /// `FixedPostUpdate`.
    fn fixed_step(&mut self) {
        self.run_stage(FixedUpdate);
        self.run_stage(PhysicsStep);
        self.run_stage(FixedPostUpdate);
    }

    fn run_stage(&mut self, label: impl ScheduleLabel + Clone) {
        if let Some(schedule) = self.schedules.get_mut(label.clone()) {
            schedule.run(&mut self.world);
        }
        if let Some(schedule) = self.plugin_schedules.get_mut(label) {
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
