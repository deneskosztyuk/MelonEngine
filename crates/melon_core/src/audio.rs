//! Audio types shared by the game, the editor and `melon_audio`: the
//! components, the bus settings (`config/audio.ron`) and the request queue
//! game systems write and the audio system drains each frame. No device code.
//!
//! Volumes are linear amplitude, 0.0 (silent) to 1.0 (full); 0.5 is about
//! -6 dB.

use std::path::Path;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::reflect::ReflectComponent;
use bevy_ecs::resource::Resource;
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use glam::Vec3;
use serde::{Deserialize, Serialize};

pub use crate::ron_file::LoadOutcome;

/// Asset type tag for sound files (`.wav`, `.ogg`, `.mp3`, `.flac`).
pub struct AudioClip;

/// File extensions that load as an `AudioClip`, lower case.
pub const AUDIO_EXTENSIONS: [&str; 4] = ["wav", "ogg", "mp3", "flac"];

pub const MASTER: &str = "Master";
pub const MUSIC: &str = "Music";
pub const SFX: &str = "Sfx";
pub const UI: &str = "Ui";

/// Plays a clip, from the entity's world position when `spatial`.
/// Linear volume 0..1; `pitch` is a playback-rate multiplier (0.1 to 4).
#[derive(Component, Reflect, Debug, Clone, PartialEq)]
#[reflect(Component, Default)]
pub struct AudioSource {
    /// Project-relative clip path, e.g. `assets/audio/door.ogg`.
    pub clip: String,
    pub volume: f32,
    pub pitch: f32,
    pub looping: bool,
    /// Start when Play starts (once), never in edit mode.
    pub play_on_start: bool,
    /// Attenuate and pan from the listener; off plays flat.
    pub spatial: bool,
    /// Full volume inside this distance (metres).
    pub min_distance: f32,
    /// Silent beyond this distance.
    pub max_distance: f32,
    /// Bus name from Project Settings > Audio.
    pub bus: String,
}

impl Default for AudioSource {
    fn default() -> Self {
        Self {
            clip: String::new(),
            volume: 1.0,
            pitch: 1.0,
            looping: false,
            play_on_start: false,
            spatial: false,
            min_distance: 1.0,
            max_distance: 30.0,
            bus: SFX.to_string(),
        }
    }
}

/// Marks the entity sounds are heard from. Without one the camera listens.
#[derive(Component, Reflect, Debug, Clone, Copy, Default)]
#[reflect(Component, Default)]
pub struct AudioListener;

/// One mixer bus: a volume and a mute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioBus {
    pub name: String,
    pub volume: f32,
    pub mute: bool,
}

impl Default for AudioBus {
    fn default() -> Self {
        Self {
            name: String::new(),
            volume: 1.0,
            mute: false,
        }
    }
}

impl AudioBus {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }
}

/// The buses, `Master` first. Saved as `config/audio.ron`.
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub buses: Vec<AudioBus>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            buses: [MASTER, MUSIC, SFX, UI].map(AudioBus::new).to_vec(),
        }
    }
}

impl AudioSettings {
    fn bus(&self, name: &str) -> Option<&AudioBus> {
        self.buses.iter().find(|b| b.name == name)
    }

    /// What a sound on `bus` is scaled by: its bus times `Master`, 0 when
    /// either is muted. An unknown bus plays on `Master` alone.
    pub fn gain(&self, bus: &str) -> f32 {
        let level = |b: Option<&AudioBus>| b.map_or(1.0, |b| if b.mute { 0.0 } else { b.volume });
        let master = level(self.bus(MASTER));
        if bus == MASTER {
            master
        } else {
            master * self.bus(bus).map_or(1.0, |b| level(Some(b)))
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.buses.first().map(|b| b.name.as_str()) != Some(MASTER) {
            return Err(format!("the first bus must be {MASTER}"));
        }
        for (i, bus) in self.buses.iter().enumerate() {
            if bus.name.trim().is_empty() {
                return Err("a bus has no name".into());
            }
            if !(0.0..=1.0).contains(&bus.volume) {
                return Err(format!(
                    "bus {} volume {} out of 0..1",
                    bus.name, bus.volume
                ));
            }
            if self.buses[..i].iter().any(|b| b.name == bus.name) {
                return Err(format!("bus {} appears twice", bus.name));
            }
        }
        Ok(())
    }

    /// Missing, unreadable or invalid files give the defaults; the file is
    /// left untouched.
    pub fn load(path: &Path) -> (Self, LoadOutcome) {
        let (settings, outcome) = crate::ron_file::load::<Self>(path);
        if outcome == LoadOutcome::Loaded
            && let Err(message) = settings.validate()
        {
            return (Self::default(), LoadOutcome::Broken(message));
        }
        (settings, outcome)
    }

    /// Atomic write; with `backup_broken` the old file is kept as `<path>.broken`.
    pub fn save(&self, path: &Path, backup_broken: bool) -> anyhow::Result<()> {
        crate::ron_file::save(self, path, backup_broken)
    }
}

/// What a game or the editor asks the audio system to do.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioRequest {
    OneShot {
        clip: String,
        bus: String,
        position: Option<Vec3>,
        volume: f32,
    },
    /// Start the entity's source, or resume it when paused.
    Play(Entity),
    Pause(Entity),
    Stop(Entity),
    /// Sets `AudioSource::volume` (linear 0..1).
    SetVolume(Entity, f32),
    /// Editor: play a clip on the `Ui` bus, in any simulation state.
    PreviewPlay(String),
    PreviewStop,
}

/// The queue between game code and the audio system. Game systems push
/// (it only needs `melon_core`), `melon_audio` drains it once per frame.
/// Game requests are dropped unless the game is `Playing`.
#[derive(Resource, Default, Debug)]
pub struct AudioRequests(Vec<AudioRequest>);

impl AudioRequests {
    pub fn push(&mut self, request: AudioRequest) {
        self.0.push(request);
    }

    /// Fire and forget: `clip` on `bus` at full volume, flat.
    pub fn play_one_shot(&mut self, clip: &str, bus: &str) {
        self.one_shot(clip, bus, None);
    }

    /// As `play_one_shot`, heard from `position`.
    pub fn play_one_shot_at(&mut self, clip: &str, bus: &str, position: Vec3) {
        self.one_shot(clip, bus, Some(position));
    }

    fn one_shot(&mut self, clip: &str, bus: &str, position: Option<Vec3>) {
        self.push(AudioRequest::OneShot {
            clip: clip.to_string(),
            bus: bus.to_string(),
            position,
            volume: 1.0,
        });
    }

    pub fn play(&mut self, entity: Entity) {
        self.push(AudioRequest::Play(entity));
    }

    pub fn pause(&mut self, entity: Entity) {
        self.push(AudioRequest::Pause(entity));
    }

    pub fn stop(&mut self, entity: Entity) {
        self.push(AudioRequest::Stop(entity));
    }

    pub fn set_volume(&mut self, entity: Entity, volume: f32) {
        self.push(AudioRequest::SetVolume(entity, volume));
    }

    pub fn preview_play(&mut self, clip: &str) {
        self.push(AudioRequest::PreviewPlay(clip.to_string()));
    }

    pub fn preview_stop(&mut self) {
        self.push(AudioRequest::PreviewStop);
    }

    /// Take every queued request; each is seen once.
    pub fn drain(&mut self) -> Vec<AudioRequest> {
        std::mem::take(&mut self.0)
    }
}

/// The clip the editor preview is playing (project-relative path), written
/// by the audio system, read by the Content Browser's button.
#[derive(Resource, Default, Debug, PartialEq, Eq)]
pub struct AudioPreview(pub Option<String>);

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_audio_core_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set(settings: &mut AudioSettings, bus: &str, volume: f32, mute: bool) {
        let b = settings.buses.iter_mut().find(|b| b.name == bus).unwrap();
        (b.volume, b.mute) = (volume, mute);
    }

    #[test]
    fn default_buses() {
        let names: Vec<_> = AudioSettings::default()
            .buses
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(names, [MASTER, MUSIC, SFX, UI]);
    }

    #[test]
    fn gain_multiplies_bus_and_master() {
        let mut s = AudioSettings::default();
        set(&mut s, MASTER, 0.5, false);
        set(&mut s, SFX, 0.5, false);
        assert_eq!(s.gain(SFX), 0.25);
        assert_eq!(s.gain(MUSIC), 0.5);
        assert_eq!(s.gain(MASTER), 0.5);
    }

    #[test]
    fn mute_silences_bus_or_everything() {
        let mut s = AudioSettings::default();
        set(&mut s, MUSIC, 0.8, true);
        assert_eq!((s.gain(MUSIC), s.gain(SFX)), (0.0, 1.0));
        set(&mut s, MASTER, 1.0, true);
        assert_eq!((s.gain(SFX), s.gain(MASTER)), (0.0, 0.0));
    }

    #[test]
    fn unknown_bus_plays_on_master() {
        let mut s = AudioSettings::default();
        set(&mut s, MASTER, 0.5, false);
        assert_eq!(s.gain("Nope"), 0.5);
    }

    #[test]
    fn round_trip_and_missing() {
        let dir = temp_dir("round_trip");
        let path = dir.join("config/audio.ron");
        let mut s = AudioSettings::default();
        set(&mut s, MUSIC, 0.3, true);
        s.buses.push(AudioBus::new("Voice"));
        s.save(&path, false).unwrap();
        assert_eq!(AudioSettings::load(&path), (s, LoadOutcome::Loaded));
        let (d, outcome) = AudioSettings::load(&dir.join("none.ron"));
        assert_eq!(
            (d, outcome),
            (AudioSettings::default(), LoadOutcome::Missing)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_file_gives_default_and_stays() {
        let dir = temp_dir("invalid");
        let path = dir.join("audio.ron");
        for text in [
            "not ron {{",
            "(buses: [(name: \"Music\")])",
            "(buses: [(name: \"Master\", volume: 3.0)])",
            "(buses: [(name: \"Master\"), (name: \"Master\")])",
        ] {
            std::fs::write(&path, text).unwrap();
            let (s, outcome) = AudioSettings::load(&path);
            assert!(
                matches!(outcome, LoadOutcome::Broken(_)),
                "{text}: {outcome:?}"
            );
            assert_eq!(s, AudioSettings::default());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn requests_drain_once() {
        let mut q = AudioRequests::default();
        q.play_one_shot("a.wav", SFX);
        q.play_one_shot_at("b.wav", SFX, Vec3::X);
        q.preview_stop();
        assert_eq!(q.drain().len(), 3);
        assert!(q.drain().is_empty());
    }
}
