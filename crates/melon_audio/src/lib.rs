//! Audio playback (ROADMAP priority 6). The game-facing types (components,
//! bus settings, request queue) live in `melon_core`; this crate reads them
//! each frame and plays sound through kira. No device: it logs one warning
//! and runs silent.

mod director;
mod kira_backend;
pub mod spatial;

use bevy_ecs::world::World;

pub use director::{Backend, Director, PlayParams, VoiceId};
pub use kira_backend::KiraBackend;

/// The audio device plus the decisions. The host owns one and calls
/// `update` once per frame.
pub struct AudioEngine {
    director: Director,
    backend: KiraBackend,
}

impl AudioEngine {
    pub fn new() -> Self {
        Self {
            director: Director::default(),
            backend: KiraBackend::new(),
        }
    }

    pub fn update(&mut self, world: &mut World) {
        self.director.update(world, &mut self.backend);
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}
