use bevy_ecs::{
    resource::Resource,
    schedule::Schedule,
    system::{Res, ResMut},
    world::World,
};
use tracing::instrument;

pub mod components;
pub mod input;
pub mod resources;

pub use bevy_ecs;
pub use bevy_ecs::query::With;
pub use components::{
    Aabb, Bob, BodyInfo, BodyKind, MeshRef, Name, PendingDelete, Spin, Transform,
};
pub use glam;
pub use input::{Input, KeyCode};
pub use resources::{Camera, Light, NameCounter, SimulationState, Time};

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

pub struct Core {
    pub world: World,
    pub schedule: Schedule,
}

impl Core {
    #[instrument(skip_all, level = "info")]
    pub fn new() -> Self {
        let mut world = World::new();
        world.init_resource::<FrameCounter>();
        world.insert_resource(LogInterval { frames_per_log: 60 });
        world.insert_resource(NameCounter::default());

        let mut schedule = Schedule::default();
        schedule.add_systems((frame_counter_system, log_frame_system));

        Self { world, schedule }
    }

    #[instrument(skip(self), level = "debug")]
    pub fn update(&mut self) {
        self.schedule.run(&mut self.world);
        self.world.clear_trackers();
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
}
