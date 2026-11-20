use std::alloc::Layout;
use std::any::{TypeId, type_name};
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
    /// Resources whose registered layout differs from the plugin's type.
    pub(crate) errors: Vec<String>,
}

impl App<'_> {
    /// Register systems into a frame stage; they run after the engine's
    /// systems of the same stage.
    pub fn add_systems<M>(
        &mut self,
        label: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        let label = label.intern();
        // An unknown label would create a schedule that never runs.
        if self.schedules.contains(label) {
            self.schedules.add_systems(label, systems);
        } else {
            tracing::warn!("Plugin systems for unknown stage {label:?} ignored");
        }
        self
    }

    /// Replaces the value on every hot reload (build runs again). For state
    /// that must survive a reload, use `init_resource`.
    pub fn insert_resource<R: Resource>(&mut self, r: R) -> &mut Self {
        if self.layout_ok::<R>() {
            self.world.insert_resource(r);
        }
        self
    }

    /// Insert `R::from_world` only if `R` is missing, so the value survives
    /// hot reloads.
    pub fn init_resource<R: Resource + FromWorld>(&mut self) -> &mut Self {
        if self.layout_ok::<R>() {
            self.world.init_resource::<R>();
        }
        self
    }

    /// A reloaded `R` with new fields keeps its `TypeId`, so bevy would keep
    /// the old-size storage; record an error instead of touching it.
    fn layout_ok<R: Resource>(&mut self) -> bool {
        let changed = changed_layout(self.world, TypeId::of::<R>(), Layout::new::<R>());
        if changed {
            self.errors.push(format!(
                "resource `{}` changed its fields; restart the editor",
                type_name::<R>()
            ));
        }
        !changed
    }

    pub fn world(&mut self) -> &mut World {
        self.world
    }
}

/// True if `type_id` is registered in `world` with a layout other than `layout`.
fn changed_layout(world: &World, type_id: TypeId, layout: Layout) -> bool {
    let components = world.components();
    components
        .get_valid_id(type_id)
        .and_then(|id| components.get_info(id))
        .is_some_and(|info| info.layout() != layout)
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
        core.rebuild_plugins(build_counter).unwrap();
        core.update();
        assert_eq!(core.world.resource::<Counter>().0, 1);
    }

    #[test]
    fn rebuild_keeps_entities_and_resources() {
        let mut core = Core::new();
        core.add_plugin(&CounterPlugin);
        core.update();
        let entity = core.world.spawn(crate::Transform::default()).id();
        core.rebuild_plugins(build_counter).unwrap();
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
        core.rebuild_plugins(build).unwrap();
        core.world.resource_mut::<Counter>().0 = 5;
        core.world.resource_mut::<Score>().0 = 9;
        core.rebuild_plugins(build).unwrap();
        assert_eq!(core.world.resource::<Counter>().0, 5);
        assert_eq!(core.world.resource::<Score>().0, 0);
    }

    #[test]
    fn changed_layout_compares_registered_layout() {
        let mut world = World::new();
        assert!(!changed_layout(
            &world,
            TypeId::of::<Marker>(),
            Layout::new::<u64>()
        ));
        world.insert_resource(Marker(1));
        let id = TypeId::of::<Marker>();
        assert!(!changed_layout(&world, id, Layout::new::<Marker>()));
        assert!(changed_layout(&world, id, Layout::new::<u64>()));
    }

    #[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
    struct Custom;

    #[test]
    fn add_systems_ignores_unknown_label() {
        let mut core = Core::new();
        core.rebuild_plugins(|app| {
            app.add_systems(Custom, |mut c: ResMut<Counter>| c.0 += 1);
        })
        .unwrap();
        assert!(!core.plugin_schedules.contains(Custom));
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
