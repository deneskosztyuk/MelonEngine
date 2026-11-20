//! The decisions: which sounds start, pause, stop and at what volume and
//! pan, from the ECS world and the request queue. Talks to a `Backend`, so
//! tests run without a device.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_ecs::{entity::Entity, query::QueryState, query::With, world::World};
use glam::Vec3;
use melon_core::audio::UI;
use melon_core::hierarchy::world_matrix;
use melon_core::{
    AssetServer, AudioClip, AudioListener, AudioPreview, AudioRequest, AudioRequests,
    AudioSettings, AudioSource, Camera, SimulationState,
};

use crate::spatial::{attenuation, pan};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VoiceId {
    Source(Entity),
    OneShot(u64),
    Preview,
}

pub struct PlayParams {
    pub path: PathBuf,
    /// `AssetServer` version of the file: a change means the cached decode is stale.
    pub version: u32,
    pub bus: String,
    pub volume: f32,
    pub pan: f32,
    pub pitch: f32,
    pub looping: bool,
}

/// What plays the sound. `KiraBackend` is the real one.
pub trait Backend {
    /// A missing or undecodable file is an `Err`; no device is not.
    fn play(&mut self, voice: VoiceId, params: &PlayParams) -> Result<(), String>;
    fn set(&mut self, voice: VoiceId, volume: f32, pan: f32, pitch: f32);
    fn pause(&mut self, voice: VoiceId);
    fn resume(&mut self, voice: VoiceId);
    fn stop(&mut self, voice: VoiceId);
    fn finished(&self, voice: VoiceId) -> bool;
    /// Composed bus gain (bus times master, mute applied).
    fn set_bus_gain(&mut self, bus: &str, gain: f32);
    /// The buses that exist now; every other bus is dropped. Sounds on a
    /// dropped bus are cut, so the director restarts them first.
    fn retain_buses(&mut self, buses: &[&str]);
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Mix {
    volume: f32,
    pan: f32,
    pitch: f32,
}

enum Kind {
    Source,
    OneShot {
        position: Option<Vec3>,
        bus: String,
        volume: f32,
    },
    Preview,
}

struct Voice {
    kind: Kind,
    clip: String,
    /// Bus and looping the voice started with; a source edited since restarts.
    bus: String,
    looping: bool,
    version: u32,
    /// Paused by a `Pause` request: Play resume of the game leaves it paused.
    user_paused: bool,
}

#[derive(Default)]
pub struct Director {
    prev: SimulationState,
    voices: HashMap<VoiceId, Voice>,
    /// Sources `play_on_start` already started this play session.
    started: HashSet<Entity>,
    next_one_shot: u64,
    /// Bus names of the last frame, to notice a removed or renamed bus.
    buses: Vec<String>,
    /// Last failure warning per clip, so a spammed source cannot flood the Console.
    warned: HashMap<String, Instant>,
    listeners: Option<QueryState<Entity, With<AudioListener>>>,
    sources: Option<QueryState<(Entity, &'static AudioSource)>>,
}

/// One failure warning per clip per second.
const WARN_EVERY: Duration = Duration::from_secs(1);

fn should_warn(warned: &mut HashMap<String, Instant>, clip: &str, now: Instant) -> bool {
    warned.retain(|_, at| now.saturating_duration_since(*at) < WARN_EVERY);
    if warned.contains_key(clip) {
        return false;
    }
    warned.insert(clip.to_string(), now);
    true
}

/// Position and right vector the sound is heard from: an `AudioListener`
/// entity, else the camera.
fn listener(
    world: &mut World,
    query: &mut Option<QueryState<Entity, With<AudioListener>>>,
) -> Option<(Vec3, Vec3)> {
    let query = query.get_or_insert_with(|| QueryState::new(world));
    let entity = query.iter(world).next();
    if let Some(entity) = entity {
        let m = world_matrix(world, entity);
        return Some((m.w_axis.truncate(), m.x_axis.truncate()));
    }
    let camera = world.get_resource::<Camera>()?;
    Some((camera.eye, (camera.center - camera.eye).cross(camera.up)))
}

fn spatial_mix(ears: Option<(Vec3, Vec3)>, at: Vec3, min: f32, max: f32) -> (f32, f32) {
    match ears {
        Some((pos, right)) => (attenuation(pos.distance(at), min, max), pan(pos, right, at)),
        None => (1.0, 0.0),
    }
}

impl Director {
    /// One frame: bus gains, simulation state changes, requests, then every
    /// voice's volume and pan.
    pub fn update(&mut self, world: &mut World, backend: &mut dyn Backend) {
        let state = world
            .get_resource::<SimulationState>()
            .copied()
            .unwrap_or_default();
        let requests = world
            .get_resource_mut::<AudioRequests>()
            .map(|mut q| q.drain())
            .unwrap_or_default();
        self.sync_buses(world, backend);

        let prev = std::mem::replace(&mut self.prev, state);
        match (prev, state) {
            (SimulationState::Editing, _) => {}
            (_, SimulationState::Editing) => self.stop_game(backend),
            (SimulationState::Playing, SimulationState::Paused) => self.each_game(|id| {
                backend.pause(id);
            }),
            (SimulationState::Paused, SimulationState::Playing) => {
                for (id, voice) in &self.voices {
                    if !matches!(voice.kind, Kind::Preview) && !voice.user_paused {
                        backend.resume(*id);
                    }
                }
            }
            _ => {}
        }

        for request in requests {
            match request {
                AudioRequest::PreviewPlay(clip) => self.start_preview(world, backend, &clip),
                AudioRequest::PreviewStop => self.remove(backend, VoiceId::Preview),
                game if state == SimulationState::Playing => {
                    self.game_request(world, backend, game);
                }
                _ => {}
            }
        }
        if state == SimulationState::Playing {
            self.start_play_on_start(world, backend);
        }
        self.maintain(world, backend, state);
        if let Some(mut preview) = world.get_resource_mut::<AudioPreview>() {
            let now = self.voices.get(&VoiceId::Preview).map(|v| v.clip.clone());
            if preview.0 != now {
                preview.0 = now;
            }
        }
    }

    /// Push bus gains; when a bus was removed or renamed, restart its
    /// voices after the backend dropped it, so they fall back to Master.
    fn sync_buses(&mut self, world: &mut World, backend: &mut dyn Backend) {
        let default;
        let settings = match world.get_resource::<AudioSettings>() {
            Some(settings) => settings,
            None => {
                default = AudioSettings::default();
                &default
            }
        };
        for bus in &settings.buses {
            backend.set_bus_gain(&bus.name, settings.gain(&bus.name));
        }
        let same = self.buses.len() == settings.buses.len()
            && self
                .buses
                .iter()
                .zip(&settings.buses)
                .all(|(a, b)| *a == b.name);
        if same {
            return;
        }
        let names: Vec<&str> = settings.buses.iter().map(|b| b.name.as_str()).collect();
        let gone: Vec<String> = self
            .buses
            .iter()
            .filter(|old| !names.contains(&old.as_str()))
            .cloned()
            .collect();
        backend.retain_buses(&names);
        let new = settings.buses.iter().map(|b| b.name.clone()).collect();
        self.buses = new;
        let ids: Vec<VoiceId> = self
            .voices
            .iter()
            .filter(|(_, v)| gone.contains(&v.bus))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.restart(world, backend, id);
        }
    }

    /// Stop a voice and start it again from the top with the current values.
    fn restart(&mut self, world: &mut World, backend: &mut dyn Backend, id: VoiceId) {
        let Some(voice) = self.voices.remove(&id) else {
            return;
        };
        backend.stop(id);
        match (id, voice.kind) {
            // A source reads its clip, bus and looping from the component.
            (VoiceId::Source(entity), _) => self.start_source(world, backend, entity),
            (_, kind) => self.start(world, backend, id, kind, voice.clip),
        }
        if self.prev == SimulationState::Paused && id != VoiceId::Preview {
            backend.pause(id);
        }
    }

    fn each_game(&self, mut f: impl FnMut(VoiceId)) {
        for (id, voice) in &self.voices {
            if !matches!(voice.kind, Kind::Preview) {
                f(*id);
            }
        }
    }

    /// Back to `Editing`: every game sound stops, the preview plays on.
    fn stop_game(&mut self, backend: &mut dyn Backend) {
        let ids: Vec<_> = self
            .voices
            .iter()
            .filter(|(_, v)| !matches!(v.kind, Kind::Preview))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.remove(backend, id);
        }
        self.started.clear();
    }

    fn remove(&mut self, backend: &mut dyn Backend, id: VoiceId) {
        if self.voices.remove(&id).is_some() {
            backend.stop(id);
        }
    }

    fn game_request(
        &mut self,
        world: &mut World,
        backend: &mut dyn Backend,
        request: AudioRequest,
    ) {
        match request {
            AudioRequest::OneShot {
                clip,
                bus,
                position,
                volume,
            } => {
                self.next_one_shot += 1;
                let id = VoiceId::OneShot(self.next_one_shot);
                let kind = Kind::OneShot {
                    position,
                    bus,
                    volume,
                };
                self.start(world, backend, id, kind, clip);
            }
            AudioRequest::Play(entity) => {
                let id = VoiceId::Source(entity);
                match self.voices.get_mut(&id) {
                    Some(voice) if voice.user_paused => {
                        voice.user_paused = false;
                        backend.resume(id);
                    }
                    Some(_) => {
                        self.remove(backend, id);
                        self.start_source(world, backend, entity);
                    }
                    None => self.start_source(world, backend, entity),
                }
            }
            AudioRequest::Pause(entity) => {
                let id = VoiceId::Source(entity);
                if let Some(voice) = self.voices.get_mut(&id) {
                    voice.user_paused = true;
                    backend.pause(id);
                }
            }
            AudioRequest::Stop(entity) => self.remove(backend, VoiceId::Source(entity)),
            AudioRequest::SetVolume(entity, volume) => {
                if let Some(mut source) = world.get_mut::<AudioSource>(entity) {
                    source.volume = volume.clamp(0.0, 1.0);
                }
            }
            AudioRequest::PreviewPlay(_) | AudioRequest::PreviewStop => {}
        }
    }

    fn start_source(&mut self, world: &mut World, backend: &mut dyn Backend, entity: Entity) {
        if let Some(clip) = world.get::<AudioSource>(entity).map(|s| s.clip.clone()) {
            self.start(world, backend, VoiceId::Source(entity), Kind::Source, clip);
        }
    }

    fn start_preview(&mut self, world: &mut World, backend: &mut dyn Backend, clip: &str) {
        self.remove(backend, VoiceId::Preview);
        self.start(
            world,
            backend,
            VoiceId::Preview,
            Kind::Preview,
            clip.to_string(),
        );
    }

    /// `play_on_start` sources start once per play session, also when they
    /// are spawned while playing.
    fn start_play_on_start(&mut self, world: &mut World, backend: &mut dyn Backend) {
        self.started.retain(|e| world.get_entity(*e).is_ok());
        let query = self.sources.get_or_insert_with(|| QueryState::new(world));
        let entities: Vec<Entity> = query
            .iter(world)
            .filter(|(e, s)| s.play_on_start && !self.started.contains(e))
            .map(|(e, _)| e)
            .collect();
        for entity in entities {
            self.started.insert(entity);
            self.start_source(world, backend, entity);
        }
    }

    fn start(
        &mut self,
        world: &mut World,
        backend: &mut dyn Backend,
        id: VoiceId,
        kind: Kind,
        clip: String,
    ) {
        let ears = listener(world, &mut self.listeners);
        let Some(mix) = mix_of(world, ears, id, &kind) else {
            return;
        };
        let (bus, looping) = match &kind {
            Kind::Source => world
                .get::<AudioSource>(match id {
                    VoiceId::Source(e) => e,
                    _ => return,
                })
                .map(|s| (s.bus.clone(), s.looping))
                .unwrap_or_default(),
            Kind::OneShot { bus, .. } => (bus.clone(), false),
            Kind::Preview => (UI.to_string(), false),
        };
        let (path, version) = {
            let mut server = world.resource_mut::<AssetServer>();
            let handle = server.load::<AudioClip>(&clip);
            (
                server.full_path(handle).unwrap_or_default(),
                server.version(handle),
            )
        };
        let params = PlayParams {
            path,
            version,
            bus: bus.clone(),
            volume: mix.volume,
            pan: mix.pan,
            pitch: mix.pitch,
            looping,
        };
        match backend.play(id, &params) {
            Ok(()) => {
                self.voices.insert(
                    id,
                    Voice {
                        kind,
                        clip,
                        bus,
                        looping,
                        version,
                        user_paused: false,
                    },
                );
            }
            Err(error) => {
                if should_warn(&mut self.warned, &clip, Instant::now()) {
                    tracing::warn!("audio: cannot play `{clip}`: {error}");
                }
            }
        }
    }

    /// Drop finished or orphaned voices, restart ones whose file changed,
    /// push volume and pan. Game voices stand still while `Paused`.
    fn maintain(&mut self, world: &mut World, backend: &mut dyn Backend, state: SimulationState) {
        let ears = listener(world, &mut self.listeners);
        let ids: Vec<VoiceId> = self.voices.keys().copied().collect();
        for id in ids {
            let is_preview = id == VoiceId::Preview;
            if !is_preview && state != SimulationState::Playing {
                continue;
            }
            let Some(voice) = self.voices.get(&id) else {
                continue;
            };
            let orphan = matches!(id, VoiceId::Source(e) if world.get::<AudioSource>(e).is_none());
            if orphan || backend.finished(id) {
                self.remove(backend, id);
                continue;
            }
            let current = {
                let mut server = world.resource_mut::<AssetServer>();
                let handle = server.load::<AudioClip>(&voice.clip);
                server.version(handle)
            };
            let renamed = matches!(id, VoiceId::Source(e)
                if world.get::<AudioSource>(e).is_some_and(|s| s.clip != voice.clip));
            let edited = matches!(id, VoiceId::Source(e)
                if world.get::<AudioSource>(e).is_some_and(|s| s.looping != voice.looping || s.bus != voice.bus));
            if (renamed || edited || current != voice.version)
                && !matches!(voice.kind, Kind::OneShot { .. })
            {
                self.restart(world, backend, id);
                continue;
            }
            if let Some(mix) = mix_of(world, ears, id, &voice.kind) {
                backend.set(id, mix.volume, mix.pan, mix.pitch);
            }
        }
    }
}

/// Volume clamped to 0..1; NaN is silence.
fn unit(volume: f32) -> f32 {
    if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, 1.0)
    }
}

/// Volume, pan and pitch a voice has right now; `None` when its source is gone.
fn mix_of(world: &World, ears: Option<(Vec3, Vec3)>, id: VoiceId, kind: &Kind) -> Option<Mix> {
    match kind {
        Kind::Source => {
            let VoiceId::Source(entity) = id else {
                return None;
            };
            let source = world.get::<AudioSource>(entity)?;
            let (gain, pan) = if source.spatial {
                let at = world_matrix(world, entity).w_axis.truncate();
                spatial_mix(ears, at, source.min_distance, source.max_distance)
            } else {
                (1.0, 0.0)
            };
            Some(Mix {
                volume: unit(source.volume) * gain,
                pan,
                pitch: if source.pitch.is_nan() {
                    1.0
                } else {
                    source.pitch.clamp(0.1, 4.0)
                },
            })
        }
        Kind::OneShot {
            position, volume, ..
        } => {
            let defaults = AudioSource::default();
            let (gain, pan) = match position {
                Some(at) => spatial_mix(ears, *at, defaults.min_distance, defaults.max_distance),
                None => (1.0, 0.0),
            };
            Some(Mix {
                volume: unit(*volume) * gain,
                pan,
                pitch: 1.0,
            })
        }
        Kind::Preview => Some(Mix {
            volume: 1.0,
            pan: 0.0,
            pitch: 1.0,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::{Core, Transform};

    #[derive(Default)]
    struct Mock {
        log: Vec<String>,
        gains: HashMap<String, f32>,
        fail: bool,
        done: HashSet<VoiceId>,
        last: HashMap<VoiceId, (f32, f32, f32)>,
        retained: Vec<Vec<String>>,
        played_bus: Vec<String>,
    }

    impl Mock {
        fn count(&self, prefix: &str) -> usize {
            self.log.iter().filter(|l| l.starts_with(prefix)).count()
        }
    }

    impl Backend for Mock {
        fn play(&mut self, voice: VoiceId, params: &PlayParams) -> Result<(), String> {
            if self.fail {
                return Err("broken".into());
            }
            let name = match voice {
                VoiceId::Source(_) => "source",
                VoiceId::OneShot(_) => "oneshot",
                VoiceId::Preview => "preview",
            };
            self.log.push(format!("play {name} {}", params.bus));
            self.played_bus.push(params.bus.clone());
            self.last
                .insert(voice, (params.volume, params.pan, params.pitch));
            Ok(())
        }
        fn set(&mut self, voice: VoiceId, volume: f32, pan: f32, pitch: f32) {
            self.last.insert(voice, (volume, pan, pitch));
        }
        fn pause(&mut self, voice: VoiceId) {
            self.log.push(format!("pause {}", name(voice)));
        }
        fn resume(&mut self, voice: VoiceId) {
            self.log.push(format!("resume {}", name(voice)));
        }
        fn stop(&mut self, voice: VoiceId) {
            self.log.push(format!("stop {}", name(voice)));
        }
        fn finished(&self, voice: VoiceId) -> bool {
            self.done.contains(&voice)
        }
        fn retain_buses(&mut self, buses: &[&str]) {
            self.retained
                .push(buses.iter().map(|b| b.to_string()).collect());
        }
        fn set_bus_gain(&mut self, bus: &str, gain: f32) {
            self.gains.insert(bus.to_string(), gain);
        }
    }

    fn name(voice: VoiceId) -> &'static str {
        match voice {
            VoiceId::Source(_) => "source",
            VoiceId::OneShot(_) => "oneshot",
            VoiceId::Preview => "preview",
        }
    }

    fn world(state: SimulationState) -> World {
        let mut world = Core::new().world;
        world.insert_resource(state);
        world
    }

    fn source(world: &mut World, play_on_start: bool) -> Entity {
        world
            .spawn((
                AudioSource {
                    clip: "assets/a.ogg".into(),
                    play_on_start,
                    ..AudioSource::default()
                },
                Transform::default(),
            ))
            .id()
    }

    fn step(d: &mut Director, w: &mut World, m: &mut Mock) {
        d.update(w, m);
    }

    #[test]
    fn play_on_start_fires_once_on_play_never_while_editing() {
        let mut w = world(SimulationState::Editing);
        source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play"), 0, "editing is silent");
        w.insert_resource(SimulationState::Playing);
        step(&mut d, &mut w, &mut m);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 1);
        // The clip ended; it must not restart on its own.
        m.done.insert(*d.voices.keys().next().unwrap());
        step(&mut d, &mut w, &mut m);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 1);
    }

    #[test]
    fn a_new_play_session_starts_it_again() {
        let mut w = world(SimulationState::Playing);
        source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.insert_resource(SimulationState::Editing);
        step(&mut d, &mut w, &mut m);
        w.insert_resource(SimulationState::Playing);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 2);
    }

    #[test]
    fn pause_and_stop_leave_preview_alone() {
        let mut w = world(SimulationState::Playing);
        source(&mut w, true);
        w.resource_mut::<AudioRequests>()
            .play_one_shot("assets/b.ogg", "Sfx");
        w.resource_mut::<AudioRequests>()
            .preview_play("assets/p.ogg");
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play"), 3);
        assert_eq!(
            w.resource::<AudioPreview>().0.as_deref(),
            Some("assets/p.ogg")
        );

        w.insert_resource(SimulationState::Paused);
        step(&mut d, &mut w, &mut m);
        assert_eq!((m.count("pause source"), m.count("pause oneshot")), (1, 1));
        assert_eq!(m.count("pause preview"), 0);

        w.insert_resource(SimulationState::Playing);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("resume"), 2);

        w.insert_resource(SimulationState::Editing);
        step(&mut d, &mut w, &mut m);
        assert_eq!((m.count("stop source"), m.count("stop oneshot")), (1, 1));
        assert_eq!(m.count("stop preview"), 0);
        assert!(w.resource::<AudioPreview>().0.is_some(), "preview plays on");
    }

    #[test]
    fn preview_works_while_editing_and_ends() {
        let mut w = world(SimulationState::Editing);
        w.resource_mut::<AudioRequests>()
            .preview_play("assets/p.ogg");
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play preview Ui"), 1);
        m.done.insert(VoiceId::Preview);
        step(&mut d, &mut w, &mut m);
        assert_eq!(w.resource::<AudioPreview>().0, None);
        w.resource_mut::<AudioRequests>()
            .preview_play("assets/p.ogg");
        w.resource_mut::<AudioRequests>().preview_stop();
        m.done.clear();
        step(&mut d, &mut w, &mut m);
        assert_eq!(w.resource::<AudioPreview>().0, None);
    }

    #[test]
    fn one_shots_play_once_and_only_while_playing() {
        let mut w = world(SimulationState::Editing);
        w.resource_mut::<AudioRequests>()
            .play_one_shot("assets/b.ogg", "Sfx");
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play"), 0, "dropped in edit mode");
        w.insert_resource(SimulationState::Playing);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play"), 0, "and not kept for later");
        w.resource_mut::<AudioRequests>()
            .play_one_shot("assets/b.ogg", "Sfx");
        step(&mut d, &mut w, &mut m);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play oneshot Sfx"), 1);
    }

    #[test]
    fn broken_clip_warns_and_keeps_running() {
        let mut w = world(SimulationState::Playing);
        source(&mut w, true);
        w.resource_mut::<AudioRequests>()
            .play_one_shot("assets/missing.ogg", "Sfx");
        let (mut d, mut m) = (
            Director::default(),
            Mock {
                fail: true,
                ..Mock::default()
            },
        );
        step(&mut d, &mut w, &mut m);
        assert!(d.voices.is_empty());
    }

    #[test]
    fn request_api_controls_an_entitys_source() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, false);
        let (mut d, mut m) = (Director::default(), Mock::default());
        w.resource_mut::<AudioRequests>().play(e);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 1);
        w.resource_mut::<AudioRequests>().pause(e);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("pause source"), 1);
        w.resource_mut::<AudioRequests>().play(e);
        step(&mut d, &mut w, &mut m);
        assert_eq!((m.count("resume source"), m.count("play source")), (1, 1));
        w.resource_mut::<AudioRequests>().set_volume(e, 0.25);
        step(&mut d, &mut w, &mut m);
        assert_eq!(w.get::<AudioSource>(e).unwrap().volume, 0.25);
        assert_eq!(m.last[&VoiceId::Source(e)].0, 0.25);
        w.resource_mut::<AudioRequests>().stop(e);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("stop source"), 1);
    }

    #[test]
    fn despawned_source_stops() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.despawn(e);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("stop source"), 1);
    }

    #[test]
    fn spatial_source_follows_listener() {
        let mut w = world(SimulationState::Playing);
        let e = w
            .spawn((
                AudioSource {
                    clip: "assets/a.ogg".into(),
                    spatial: true,
                    play_on_start: true,
                    min_distance: 1.0,
                    max_distance: 11.0,
                    ..AudioSource::default()
                },
                Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)),
            ))
            .id();
        // Listener at the origin facing -Z: right is +X.
        w.spawn((AudioListener, Transform::default()));
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        let (volume, pan, _) = m.last[&VoiceId::Source(e)];
        assert!((pan - 1.0).abs() < 1e-5, "pan {pan}");
        assert!((volume - attenuation(5.0, 1.0, 11.0)).abs() < 1e-6);
        // Walk the listener past it: the sound moves to the left ear.
        let listener = w
            .query_filtered::<Entity, With<AudioListener>>()
            .single(&w)
            .unwrap();
        w.get_mut::<Transform>(listener).unwrap().translation.x = 10.0;
        step(&mut d, &mut w, &mut m);
        assert!(m.last[&VoiceId::Source(e)].1 < -0.99);
    }

    #[test]
    fn camera_listens_without_a_listener() {
        let mut w = world(SimulationState::Playing);
        w.insert_resource(Camera {
            eye: Vec3::ZERO,
            center: Vec3::new(0.0, 0.0, -1.0),
            ..Camera::default()
        });
        let e = w
            .spawn((
                AudioSource {
                    clip: "assets/a.ogg".into(),
                    spatial: true,
                    play_on_start: true,
                    ..AudioSource::default()
                },
                Transform::from_translation(Vec3::new(-3.0, 0.0, 0.0)),
            ))
            .id();
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert!(m.last[&VoiceId::Source(e)].1 < -0.99);
    }

    #[test]
    fn nan_values_reach_the_backend_finite() {
        let mut w = world(SimulationState::Playing);
        let e = w
            .spawn((
                AudioSource {
                    clip: "assets/a.ogg".into(),
                    play_on_start: true,
                    spatial: true,
                    volume: f32::NAN,
                    pitch: f32::NAN,
                    min_distance: f32::NAN,
                    max_distance: f32::NAN,
                    ..AudioSource::default()
                },
                Transform::from_translation(Vec3::new(f32::NAN, 0.0, 0.0)),
            ))
            .id();
        w.spawn((AudioListener, Transform::default()));
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        step(&mut d, &mut w, &mut m);
        let (v, p, r) = m.last[&VoiceId::Source(e)];
        assert!(
            v.is_finite() && p.is_finite() && r.is_finite(),
            "{v} {p} {r}"
        );
        assert!((0.0..=1.0).contains(&v) && (0.1..=4.0).contains(&r));
    }

    #[test]
    fn negative_and_huge_volume_clamp() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        w.get_mut::<AudioSource>(e).unwrap().volume = -2.0;
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.last[&VoiceId::Source(e)].0, 0.0);
        w.get_mut::<AudioSource>(e).unwrap().volume = 9.0;
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.last[&VoiceId::Source(e)].0, 1.0);
    }

    #[test]
    fn listener_at_source_position_is_full_and_centred() {
        let mut w = world(SimulationState::Playing);
        let at = Vec3::new(3.0, 1.0, 2.0);
        let e = w
            .spawn((
                AudioSource {
                    clip: "assets/a.ogg".into(),
                    spatial: true,
                    play_on_start: true,
                    ..AudioSource::default()
                },
                Transform::from_translation(at),
            ))
            .id();
        w.spawn((AudioListener, Transform::from_translation(at)));
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.last[&VoiceId::Source(e)], (1.0, 0.0, 1.0));
    }

    #[test]
    fn min_over_max_distance_does_not_panic() {
        let mut w = world(SimulationState::Playing);
        let e = w
            .spawn((
                AudioSource {
                    clip: "assets/a.ogg".into(),
                    spatial: true,
                    play_on_start: true,
                    min_distance: 50.0,
                    max_distance: 5.0,
                    ..AudioSource::default()
                },
                Transform::from_translation(Vec3::new(20.0, 0.0, 0.0)),
            ))
            .id();
        w.spawn((AudioListener, Transform::default()));
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert!(m.last[&VoiceId::Source(e)].0.is_finite());
    }

    #[test]
    fn changing_the_clip_while_playing_restarts_it() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.get_mut::<AudioSource>(e).unwrap().clip = "assets/b.ogg".into();
        step(&mut d, &mut w, &mut m);
        step(&mut d, &mut w, &mut m);
        assert_eq!((m.count("stop source"), m.count("play source")), (1, 2));
    }

    #[test]
    fn request_stop_then_play_restarts() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.resource_mut::<AudioRequests>().stop(e);
        step(&mut d, &mut w, &mut m);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 1, "play_on_start does not refire");
        w.resource_mut::<AudioRequests>().play(e);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 2);
    }

    #[test]
    fn user_paused_source_stays_paused_across_game_pause() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.resource_mut::<AudioRequests>().pause(e);
        step(&mut d, &mut w, &mut m);
        w.insert_resource(SimulationState::Paused);
        step(&mut d, &mut w, &mut m);
        w.insert_resource(SimulationState::Playing);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("resume"), 0);
    }

    #[test]
    fn game_requests_ignored_while_paused() {
        let mut w = world(SimulationState::Paused);
        let e = source(&mut w, false);
        w.resource_mut::<AudioRequests>().play(e);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play"), 0);
    }

    #[test]
    fn play_on_start_source_spawned_while_playing_fires() {
        let mut w = world(SimulationState::Playing);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        source(&mut w, true);
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 1);
    }

    #[test]
    fn removed_bus_falls_back_to_master_gain() {
        let mut w = world(SimulationState::Playing);
        w.resource_mut::<AudioSettings>().buses[0].volume = 0.5;
        w.resource_mut::<AudioSettings>()
            .buses
            .retain(|b| b.name != "Sfx");
        source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert!(!m.gains.contains_key("Sfx"));
        assert_eq!(w.resource::<AudioSettings>().gain("Sfx"), 0.5);
    }

    #[test]
    fn hot_reloaded_clip_restarts_the_source() {
        let root = std::env::temp_dir().join("melon_audio_hot_reload");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let file = root.join("assets/a.ogg");
        std::fs::write(&file, b"one").unwrap();
        let mut w = world(SimulationState::Playing);
        w.insert_resource(AssetServer::new(root.clone()));
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        std::fs::write(&file, b"two").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        w.resource_mut::<AssetServer>().poll_changes();
        step(&mut d, &mut w, &mut m);
        assert_eq!((m.count("stop source"), m.count("play source")), (1, 2));
        assert!(d.voices.contains_key(&VoiceId::Source(e)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn requests_for_missing_entities_or_clips_do_not_panic() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, false);
        let ghost = w.spawn_empty().id();
        w.despawn(ghost);
        let mut q = w.resource_mut::<AudioRequests>();
        q.play(e);
        q.play(ghost);
        q.pause(ghost);
        q.stop(ghost);
        q.set_volume(ghost, 1.0);
        let (mut d, mut m) = (
            Director::default(),
            Mock {
                fail: true,
                ..Mock::default()
            },
        );
        step(&mut d, &mut w, &mut m);
        assert!(d.voices.is_empty());
    }

    #[test]
    fn bus_gains_reach_the_backend() {
        let mut w = world(SimulationState::Editing);
        {
            let mut s = w.resource_mut::<AudioSettings>();
            s.buses[0].volume = 0.5;
            s.buses[2].mute = true;
        }
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert_eq!(
            (m.gains["Master"], m.gains["Music"], m.gains["Sfx"]),
            (0.5, 0.5, 0.0)
        );
    }

    fn on_music(w: &mut World) -> Entity {
        w.spawn((
            AudioSource {
                clip: "assets/a.ogg".into(),
                play_on_start: true,
                bus: "Music".into(),
                ..AudioSource::default()
            },
            Transform::default(),
        ))
        .id()
    }

    #[test]
    fn removed_bus_drops_it_and_restarts_its_voices() {
        let mut w = world(SimulationState::Playing);
        on_music(&mut w);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source Music"), 1);
        w.resource_mut::<AudioSettings>().buses.remove(1); // Music
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.retained.last().unwrap(), &["Master", "Sfx", "Ui"]);
        assert_eq!((m.count("stop source"), m.count("play source")), (1, 2));
        assert_eq!(d.voices.len(), 1, "still playing, not cut");
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 2, "restarted once");
    }

    #[test]
    fn voices_on_other_buses_survive_a_bus_removal() {
        let mut w = world(SimulationState::Playing);
        source(&mut w, true); // Sfx
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.resource_mut::<AudioSettings>().buses.remove(1);
        step(&mut d, &mut w, &mut m);
        assert_eq!((m.count("stop"), m.count("play")), (0, 1));
    }

    #[test]
    fn renamed_bus_restarts_voices_on_the_old_name() {
        let mut w = world(SimulationState::Playing);
        on_music(&mut w);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.resource_mut::<AudioSettings>().buses[1].name = "Tunes".into();
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 2);
        assert_eq!(
            m.played_bus.last().unwrap(),
            "Music",
            "unknown: backend uses Master"
        );
    }

    #[test]
    fn changing_looping_or_bus_restarts_the_source() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        w.get_mut::<AudioSource>(e).unwrap().looping = true;
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 2);
        w.get_mut::<AudioSource>(e).unwrap().bus = "Music".into();
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 3);
        assert_eq!(m.played_bus.last().unwrap(), "Music");
        step(&mut d, &mut w, &mut m);
        assert_eq!(m.count("play source"), 3, "stable afterwards");
    }

    #[test]
    fn started_set_forgets_despawned_entities() {
        let mut w = world(SimulationState::Playing);
        let e = source(&mut w, true);
        let (mut d, mut m) = (Director::default(), Mock::default());
        step(&mut d, &mut w, &mut m);
        assert!(d.started.contains(&e));
        w.despawn(e);
        step(&mut d, &mut w, &mut m);
        assert!(d.started.is_empty());
    }

    #[test]
    fn failure_warning_is_rate_limited_per_clip() {
        let t0 = Instant::now();
        let mut warned = HashMap::new();
        assert!(should_warn(&mut warned, "a.ogg", t0));
        assert!(!should_warn(
            &mut warned,
            "a.ogg",
            t0 + Duration::from_millis(900)
        ));
        assert!(should_warn(
            &mut warned,
            "b.ogg",
            t0 + Duration::from_millis(900)
        ));
        assert!(should_warn(
            &mut warned,
            "a.ogg",
            t0 + Duration::from_millis(1100)
        ));
    }
}
