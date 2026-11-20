use std::any::TypeId;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::{align_of, size_of};

use bevy_ecs::{
    resource::Resource,
    schedule::{IntoScheduleConfigs, ScheduleLabel, Schedules},
    system::ScheduleSystem,
    world::{FromWorld, World},
};

use crate::{Actions, Input, SimulationState, Time, Transform};

/// A bundle of systems and resources added to a `Core` (the game, later mods).
pub trait Plugin {
    fn build(&self, app: &mut App);
}

/// Registration handle a plugin builds into: the shared world plus the
/// plugin-owned schedules, so a rebuild replaces only plugin systems.
pub struct App<'a> {
    pub(crate) world: &'a mut World,
    pub(crate) schedules: &'a mut Schedules,
}

impl App<'_> {
    /// Register systems into a frame stage; they run after the engine's
    /// systems of the same stage.
    pub fn add_systems<M>(
        &mut self,
        label: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.schedules.add_systems(label, systems);
        self
    }

    /// Replaces the value on every hot reload (build runs again). For state
    /// that must survive a reload, use `init_resource`.
    pub fn insert_resource<R: Resource>(&mut self, r: R) -> &mut Self {
        self.world.insert_resource(r);
        self
    }

    /// Insert `R::from_world` only if `R` is missing, so the value survives
    /// hot reloads.
    pub fn init_resource<R: Resource + FromWorld>(&mut self) -> &mut Self {
        self.world.init_resource::<R>();
        self
    }

    pub fn world(&mut self) -> &mut World {
        self.world
    }
}

/// Hash of the types that cross the host / game dylib boundary (identity,
/// size, align) and of melon_core's sources. Host and game must report the
/// same value, or their layouts may differ.
pub fn abi_fingerprint() -> u64 {
    fn part<T: 'static>() -> (TypeId, usize, usize) {
        (TypeId::of::<T>(), size_of::<T>(), align_of::<T>())
    }
    fingerprint_of(
        &[
            part::<App<'static>>(),
            part::<World>(),
            part::<Schedules>(),
            part::<Transform>(),
            part::<Time>(),
            part::<Input>(),
            part::<Actions>(),
            part::<SimulationState>(),
        ],
        env!("MELON_CORE_SOURCE_HASH"),
    )
}

fn fingerprint_of(parts: &[(TypeId, usize, usize)], source_hash: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    source_hash.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Core, Update};
    use bevy_ecs::system::ResMut;

    #[derive(Resource, Default)]
    struct Order(Vec<&'static str>);

    struct OrderPlugin;

    impl Plugin for OrderPlugin {
        fn build(&self, app: &mut App) {
            app.add_systems(Update, |mut o: ResMut<Order>| o.0.push("plugin"));
        }
    }

    #[test]
    fn plugin_system_runs_in_its_stage_after_engine_systems() {
        let mut core = Core::new();
        core.world.init_resource::<Order>();
        core.add_plugin(&OrderPlugin);
        core.add_systems(Update, |mut o: ResMut<Order>| o.0.push("engine"));
        core.update();
        assert_eq!(core.world.resource::<Order>().0, ["engine", "plugin"]);
    }

    #[derive(Resource)]
    struct Marker(u32);

    struct ResourcePlugin;

    impl Plugin for ResourcePlugin {
        fn build(&self, app: &mut App) {
            app.insert_resource(Marker(7));
        }
    }

    #[test]
    fn plugin_resource_is_inserted() {
        let mut core = Core::new();
        core.add_plugin(&ResourcePlugin);
        assert_eq!(core.world.resource::<Marker>().0, 7);
    }

    #[derive(Resource, Default)]
    struct Counter(u32);

    fn build_counter(app: &mut App) {
        app.world().init_resource::<Counter>();
        app.add_systems(Update, |mut c: ResMut<Counter>| c.0 += 1);
    }

    struct CounterPlugin;

    impl Plugin for CounterPlugin {
        fn build(&self, app: &mut App) {
            build_counter(app);
        }
    }

    #[test]
    fn rebuild_runs_each_system_once() {
        let mut core = Core::new();
        core.add_plugin(&CounterPlugin);
        core.rebuild_plugins(build_counter);
        core.update();
        assert_eq!(core.world.resource::<Counter>().0, 1);
    }

    #[test]
    fn rebuild_keeps_entities_and_resources() {
        let mut core = Core::new();
        core.add_plugin(&CounterPlugin);
        core.update();
        let entity = core.world.spawn(crate::Transform::default()).id();
        core.rebuild_plugins(build_counter);
        core.update();
        assert!(core.world.get::<crate::Transform>(entity).is_some());
        assert_eq!(core.world.resource::<Counter>().0, 2);
        assert!(core.world.contains_resource::<crate::FrameCounter>());
    }

    #[derive(Resource)]
    struct Score(u32);

    #[test]
    fn rebuild_init_resource_keeps_value_insert_resource_replaces_it() {
        let mut core = Core::new();
        let build = |app: &mut App| {
            app.init_resource::<Counter>().insert_resource(Score(0));
        };
        core.rebuild_plugins(build);
        core.world.resource_mut::<Counter>().0 = 5;
        core.world.resource_mut::<Score>().0 = 9;
        core.rebuild_plugins(build);
        assert_eq!(core.world.resource::<Counter>().0, 5);
        assert_eq!(core.world.resource::<Score>().0, 0);
    }

    #[test]
    fn fingerprint_covers_type_id_size_align_and_source() {
        let base = [(TypeId::of::<u32>(), 4, 4)];
        let f = fingerprint_of(&base, "src");
        assert_ne!(f, fingerprint_of(&[(TypeId::of::<i32>(), 4, 4)], "src"));
        assert_ne!(f, fingerprint_of(&[(TypeId::of::<u32>(), 8, 4)], "src"));
        assert_ne!(f, fingerprint_of(&[(TypeId::of::<u32>(), 4, 8)], "src"));
        assert_ne!(f, fingerprint_of(&base, "src2"));
        assert!(!env!("MELON_CORE_SOURCE_HASH").is_empty());
    }

    #[test]
    fn abi_fingerprint_is_stable() {
        assert_eq!(abi_fingerprint(), abi_fingerprint());
        assert_ne!(abi_fingerprint(), 0);
    }
}
