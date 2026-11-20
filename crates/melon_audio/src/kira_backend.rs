//! `Backend` on kira. Buses are mixer sub-tracks under the main track;
//! short clips are decoded once and cached per file version, long ones
//! stream from disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kira::{
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
    sound::{
        FromFileError, PlaybackState,
        static_sound::{StaticSoundData, StaticSoundHandle},
        streaming::{StreamingSoundData, StreamingSoundHandle},
    },
    track::{TrackBuilder, TrackHandle},
};

use crate::director::{Backend, PlayParams, VoiceId};
use crate::spatial::should_stream;

enum Loaded {
    Static(StaticSoundData),
    Stream(StreamingSoundData<FromFileError>),
}

enum Handle {
    Static(StaticSoundHandle),
    Stream(StreamingSoundHandle<FromFileError>),
}

struct Playing {
    handle: Handle,
    last: (f32, f32, f32),
}

pub struct KiraBackend {
    /// `None` when there is no output device (or it failed): all silent.
    manager: Option<AudioManager<DefaultBackend>>,
    tracks: HashMap<String, TrackHandle>,
    gains: HashMap<String, f32>,
    cache: HashMap<PathBuf, (u32, StaticSoundData)>,
    voices: HashMap<VoiceId, Playing>,
}

/// Linear amplitude to kira decibels; at or under 0.001 (-60 dB) is silence.
fn decibels(volume: f32) -> Decibels {
    if volume <= 0.001 {
        Decibels::SILENCE
    } else {
        Decibels(20.0 * volume.log10())
    }
}

impl KiraBackend {
    /// Opens the default output device. Without one it logs a single
    /// warning and the backend stays silent.
    #[tracing::instrument(level = "info")]
    pub fn new() -> Self {
        let manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(manager) => Some(manager),
            Err(error) => {
                tracing::warn!("no audio output ({error}); running silent");
                None
            }
        };
        Self::with_manager(manager)
    }

    /// No device; clips still load, so broken files are reported.
    pub fn silent() -> Self {
        Self::with_manager(None)
    }

    fn with_manager(manager: Option<AudioManager<DefaultBackend>>) -> Self {
        Self {
            manager,
            tracks: HashMap::new(),
            gains: HashMap::new(),
            cache: HashMap::new(),
            voices: HashMap::new(),
        }
    }

    fn load(&mut self, path: &Path, version: u32) -> Result<Loaded, String> {
        if let Some((v, data)) = self.cache.get(path)
            && *v == version
        {
            return Ok(Loaded::Static(data.clone()));
        }
        let stream = StreamingSoundData::from_file(path).map_err(|e| e.to_string())?;
        if should_stream(stream.duration()) {
            return Ok(Loaded::Stream(stream));
        }
        let data = StaticSoundData::from_file(path).map_err(|e| e.to_string())?;
        self.cache
            .insert(path.to_path_buf(), (version, data.clone()));
        Ok(Loaded::Static(data))
    }

    /// The bus's track, made on first use; an unknown bus is `Master`.
    fn track(&mut self, bus: &str) -> Option<&mut TrackHandle> {
        let manager = self.manager.as_mut()?;
        let name = if self.gains.contains_key(bus) {
            bus
        } else {
            "Master"
        };
        if !self.tracks.contains_key(name) {
            let gain = self.gains.get(name).copied().unwrap_or(1.0);
            let track = manager
                .add_sub_track(TrackBuilder::new().volume(decibels(gain)))
                .map_err(|e| tracing::warn!("audio bus {name}: {e}"))
                .ok()?;
            self.tracks.insert(name.to_string(), track);
        }
        self.tracks.get_mut(name)
    }
}

impl Default for KiraBackend {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! on_handle {
    ($handle:expr, $h:ident => $body:expr) => {
        match $handle {
            Handle::Static($h) => $body,
            Handle::Stream($h) => $body,
        }
    };
}

impl Backend for KiraBackend {
    fn play(&mut self, voice: VoiceId, p: &PlayParams) -> Result<(), String> {
        let loaded = self.load(&p.path, p.version)?;
        let Some(track) = self.track(&p.bus) else {
            return Ok(());
        };
        let (volume, pan, rate) = (decibels(p.volume), p.pan, f64::from(p.pitch));
        let handle = match loaded {
            Loaded::Static(data) => {
                let data = data.volume(volume).panning(pan).playback_rate(rate);
                let data = if p.looping {
                    data.loop_region(..)
                } else {
                    data
                };
                track
                    .play(data)
                    .map(Handle::Static)
                    .map_err(|e| e.to_string())?
            }
            Loaded::Stream(data) => {
                let data = data.volume(volume).panning(pan).playback_rate(rate);
                let data = if p.looping {
                    data.loop_region(..)
                } else {
                    data
                };
                track
                    .play(data)
                    .map(Handle::Stream)
                    .map_err(|e| e.to_string())?
            }
        };
        self.voices.insert(
            voice,
            Playing {
                handle,
                last: (p.volume, p.pan, p.pitch),
            },
        );
        Ok(())
    }

    fn set(&mut self, voice: VoiceId, volume: f32, pan: f32, pitch: f32) {
        let Some(playing) = self.voices.get_mut(&voice) else {
            return;
        };
        let last = playing.last;
        let tween = Tween::default();
        if volume != last.0 {
            on_handle!(&mut playing.handle, h => h.set_volume(decibels(volume), tween));
        }
        if pan != last.1 {
            on_handle!(&mut playing.handle, h => h.set_panning(pan, tween));
        }
        if pitch != last.2 {
            on_handle!(&mut playing.handle, h => h.set_playback_rate(f64::from(pitch), tween));
        }
        playing.last = (volume, pan, pitch);
    }

    fn pause(&mut self, voice: VoiceId) {
        if let Some(p) = self.voices.get_mut(&voice) {
            on_handle!(&mut p.handle, h => h.pause(Tween::default()));
        }
    }

    fn resume(&mut self, voice: VoiceId) {
        if let Some(p) = self.voices.get_mut(&voice) {
            on_handle!(&mut p.handle, h => h.resume(Tween::default()));
        }
    }

    fn stop(&mut self, voice: VoiceId) {
        if let Some(mut p) = self.voices.remove(&voice) {
            on_handle!(&mut p.handle, h => h.stop(Tween::default()));
        }
    }

    fn finished(&self, voice: VoiceId) -> bool {
        self.voices
            .get(&voice)
            .is_none_or(|p| on_handle!(&p.handle, h => h.state()) == PlaybackState::Stopped)
    }

    fn set_bus_gain(&mut self, bus: &str, gain: f32) {
        if self.gains.get(bus) == Some(&gain) {
            return;
        }
        self.gains.insert(bus.to_string(), gain);
        if let Some(track) = self.tracks.get_mut(bus) {
            track.set_volume(decibels(gain), Tween::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(frames: u32) -> Vec<u8> {
        let data_len = frames * 2;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // PCM
        b.extend(1u16.to_le_bytes()); // mono
        b.extend(8000u32.to_le_bytes());
        b.extend(16000u32.to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        b.extend(vec![0u8; data_len as usize]);
        b
    }

    fn params(path: PathBuf) -> PlayParams {
        PlayParams {
            path,
            version: 0,
            bus: "Sfx".into(),
            volume: 1.0,
            pan: 0.0,
            pitch: 1.0,
            looping: false,
        }
    }

    #[test]
    fn decibel_conversion() {
        assert_eq!(decibels(1.0), Decibels(0.0));
        assert!((decibels(0.5).0 + 6.0206).abs() < 1e-3);
        assert_eq!(decibels(0.0), Decibels::SILENCE);
    }

    #[test]
    fn missing_and_broken_files_are_errors_not_panics() {
        let dir = std::env::temp_dir().join("melon_audio_kira_broken");
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.wav");
        std::fs::write(&junk, b"this is not audio").unwrap();
        let mut backend = KiraBackend::silent();
        let id = VoiceId::Preview;
        assert!(backend.play(id, &params(dir.join("nope.ogg"))).is_err());
        assert!(backend.play(id, &params(junk)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn short_wav_decodes_and_is_cached_without_a_device() {
        let dir = std::env::temp_dir().join("melon_audio_kira_short");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("short.wav");
        std::fs::write(&path, wav(800)).unwrap();
        let mut backend = KiraBackend::silent();
        let id = VoiceId::Preview;
        assert_eq!(backend.play(id, &params(path.clone())), Ok(()));
        assert!(
            backend.cache.contains_key(&path),
            "short clips decode to memory"
        );
        assert!(backend.finished(id), "no device: nothing is playing");
        backend.set(id, 0.5, 0.0, 1.0);
        backend.stop(id);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn long_wav_streams() {
        let dir = std::env::temp_dir().join("melon_audio_kira_long");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.wav");
        std::fs::write(&path, wav(8000 * 6)).unwrap();
        let mut backend = KiraBackend::silent();
        assert!(matches!(backend.load(&path, 0), Ok(Loaded::Stream(_))));
        assert!(backend.cache.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
