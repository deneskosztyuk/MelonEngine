# Audio: playback, 3D sound, buses (ROADMAP priority 6)

## Intent

Sound for games and the editor, with the shared core of Unity (AudioSource, AudioListener, AudioClip import, Audio Mixer groups), Godot (AudioStreamPlayer / 3D, AudioListener3D, audio buses) and Unreal (Sound Wave, Audio component, Sound Classes / Submixes).

## Goals

1. **Audio clips as assets:** `.wav`, `.ogg`, `.mp3`, `.flac` under `assets/` load through `AssetServer` (`Handle<AudioClip>`), get `.meta` files like every asset, hot-reload when the file changes (poll), and show as an audio kind in the Content Browser.
2. **Components, registered in the component registry** (Inspector, Add Component, scene files):
   - `AudioSource { clip (path), volume (dB or linear 0..1, pick one and label it), pitch, looping, play_on_start, spatial, min_distance, max_distance, bus }`
   - `AudioListener` (marker). With none in the scene, the camera is the listener.
3. **Spatial sound:** distance attenuation between min and max distance and stereo panning from the listener's pose; sources follow their entity's world transform each frame.
4. **Buses:** `Master` plus `Music`, `Sfx`, `Ui` by default; names and volumes saved in `config/audio.ron`, edited in Edit ▸ Project Settings ▸ Audio (same pending-until-Apply pattern as Quality); every bus has volume and mute.
5. **Game API:** from `melon_game` systems: play a one-shot (clip, bus, optional position), play / pause / stop / set volume on an entity's source, all without depending on `melon_audio`.
6. **Play mode:** nothing plays while `Editing` (no surprise sound in the editor); `play_on_start` sources start on Play; Pause pauses all game sound; Stop stops it.
7. **Editor preview:** a play / stop button for the selected audio clip in the Content Browser (or the Inspector of an audio asset), on the `Ui` bus, working in edit mode.
8. **No device:** with no audio output device (or one that fails), the engine logs one warning and runs silently; nothing panics, and tests run without a device.

## Decisions

- **Library: `kira`** (game audio: mixer tracks for buses, spatial tracks with listener, tweens, streaming) with its default decoders (symphonia) for the formats above. Add it to `[workspace.dependencies]`.
- **New crate `melon_audio`** (depends on `melon_core`); the host uses it (the editor only pushes requests through the core queue, so it does not depend on it); the dependency direction becomes `core <- render, physics, audio; render, physics <- editor; editor, audio <- host`. Update AGENTS.md's layout table and dependency line.
- **Game-facing types live in `melon_core`** (components, the play-request API, bus names), because `melon_game` may depend only on `melon_core`. `melon_audio` reads them and drives kira. A message/command queue resource in core that the audio system drains each frame is the expected shape.
- Long clips (music) stream from disk; short clips (under a few seconds) decode to memory. Pick the threshold and document it.

## Constraints

AGENTS.md conventions (registry, `init_resource` for game state, `#[instrument]` on setup, tracing logs, tests at the bottom of each file, editor panels / theme tokens / working menu items). The audio device lives in the host process: the host creates the audio backend and runs its update each frame.

## Done when

- Tests (no device needed: test the decision logic and the request queue, not kira's output): attenuation and pan curve values; bus volume and mute composition; play_on_start fires once on Play and never while Editing; Pause / Stop stop game sound and leave editor preview alone; one-shot requests drained once; a missing or broken clip logs a warning and does not panic; `config/audio.ron` round-trips; a scene with an `AudioSource` saves and loads.
- `just test` green; a short manual check documented for the user (play a looping spatial source, walk the camera past it); AGENTS.md and ROADMAP (priority 6 ticked) updated.
