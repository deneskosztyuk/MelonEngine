# MelonEngine Roadmap

State of the engine as of 2026-11-20 (`roadmap` branch), and what is still
missing for a core 3D engine that can make and ship 3D games. Update this
file when an item lands. Details of how each feature works live in
`AGENTS.md`.

Each roadmap item keeps its priority number (P1-P10): P1-P6 are the minimum
to make any 3D game, P7-P9 are needed to ship one, P10 comes after.

Status marks:

- ✅ done: built, covered by automated tests, reviewed.
- 🧪 done, manual check pending: the 2026-11-20 orchestrated run (P2, P3,
  P4, P6, P9); every automated test passes, but nobody has used it in the
  editor yet.
- 🟡 partial: usable, with the named pieces missing.
- ❌ not started.

## Done

### Core & ECS

- ✅ `Core`: bevy_ecs `World` + stage schedules (`PreUpdate`, `FixedUpdate`
  at 1/60 s, `PhysicsStep`, `FixedPostUpdate`, `Update`, `PostUpdate`) with
  interpolated rendering (`PreviousTransform`).
- ✅ Transform hierarchy (`ChildOf` / `Children`, `GlobalTransform`, world
  poses everywhere), explicit root order (`RootOrder`).
- ✅ Input: all keys, mouse buttons and wheel, action mapping (`Actions` /
  `InputMap`, `config/input.ron`), editor / game input focus.
- ✅ Plugin API with an ABI fingerprint; the game plugin live-reloads in dev
  builds and links statically in ship builds.
- ✅ P1 Component reflection / registry: one registry drives the Inspector
  (cards, Add / Remove Component, payload enums with per-variant defaults)
  and scene files; an agent can build levels as RON data.
- 🧪 Runtime scenes (P9): scene save / load in `melon_core::scene` (format
  v6, per-entity `uid`, GUID + path references, v1-v5 still load), game
  code requests scene changes (`SceneRequests`), `KeepOnSceneLoad`, `Cursor`
  and `AppExit` for games.

### Editor

- ✅ egui + `egui_dock` (ADR-0001): dockable panels with a saved layout,
  menu bar, toolbar, status bar, theme tokens.
- ✅ Scene view: Unity camera controls, click select, grid, axes, selection
  box, Move / Rotate / Scale gizmos with plane handles and snapping, FPS and
  orientation overlays.
- ✅ Play / Pause / Step / Stop with a scene snapshot and restore.
- ✅ Hierarchy with search; Inspector built from the registry.
- ✅ Content Browser: GUID `.meta` files, folder tree, tiles / list, search,
  type filter, favorites, file operations, live updates, Asset card,
  thumbnails, Ctrl+Space Content Drawer.
- ✅ Console: readable rows, search, collapse, selection, copy, a log file
  (`.melon/logs/editor.log`).
- ✅ Project Settings: input mapping, Quality (presets, profiles, cost bar).
- 🧪 P2 Undo / Redo: snapshot history for every edit, History panel,
  Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z; inline rename (F2, double-click);
  duplicate (Ctrl+D, `Name (N)`); Hierarchy right-click menu (Rename,
  Duplicate, Delete, Create Child).
- ✅ Frame pacing: Mailbox with a refresh-rate cap (FIFO fallback), DX12
  first on Windows, frames drawn from `about_to_wait` and from overdue
  event callbacks (a moving 1000 Hz mouse no longer stalls frames),
  `MELON_FRAME_STATS=1` with per-frame stall timelines.

### Rendering

- ✅ wgpu forward renderer: HDR target, ACES tonemap, MSAA 1/2/4, overlay
  pass, GPU pass timings, instanced draws with CPU frustum culling (no
  entity cap), model thumbnails.
- ✅ PBR metallic-roughness (glTF factors and textures, normal maps,
  emissive with `KHR_materials_emissive_strength`).
- ✅ GI: DDGI (voxel probes) or SSGI, switchable per frame; SSAO; bloom;
  procedural sky with fog.
- ✅ Quality settings: `RenderQuality` presets and profiles, live switches
  and in-place rebuilds (shadow map size, MSAA), `Environment` is the look
  only.
- 🧪 P3 Materials and textures: `Handle<Texture>` / `Handle<Material>`
  (`.mat` files, GUID texture references), mipmaps and 8x anisotropy,
  `MeshMaterials` per part, alpha Mask (alpha-to-coverage under MSAA) and
  sorted Blend; editor: Create > Material, live material card with texture
  pickers and colour pickers, material thumbnails, drag-drop assignment,
  glTF "Extract Materials".
- 🧪 P3 Point and spot lights: components, clustered forward (16x9x24
  froxels, per-cluster sphere culling, overflow reported), up to 256 / 512
  lights per quality, viewport icons and range outlines.
- 🧪 P3 Image-based ambient: SH9 irradiance, prefiltered specular, BRDF LUT,
  from the procedural sky or an HDR equirect.
- 🧪 P3 Shadows: 1-4 cascaded sun shadows (texel snapping, blended
  transitions, shadow distance); point / spot shadows in an atlas with a
  per-quality cap; DDGI injects the sun through its own volume-fitted
  shadow map and injects shadowed local lights.
- 🧪 P3 LOD: `Lod` component with screen-height levels, hysteresis and
  culling below the last level.

### Physics

- 🧪 P4 Data components kept in sync with rapier3d: `RigidBody`
  (dynamic / fixed / kinematic), `Collider` (box, sphere, capsule, mesh;
  friction, restitution, sensor, named layers), children colliders on the
  nearest ancestor body.
- 🧪 P4 Queries (ray cast, ray cast all, sphere cast, overlaps) and
  collision / trigger events for game code; `Velocity`, impulses, forces,
  teleport.
- 🧪 P4 Character controller (grounded, steps, slopes) and joints (fixed,
  hinge, ball, slider; auto anchors).
- 🧪 P4 Project Settings > Physics (gravity, 32 named layers); collider and
  sensor outlines in the Scene view.

### Audio

- 🧪 P6 kira playback: `AudioSource` / `AudioListener`, 3D attenuation and
  pan, buses with volume and mute (`config/audio.ron`, Project Settings >
  Audio), one-shots for game code, clip preview in the Content Browser,
  bus dropdown and clip picker in the Inspector; silent without a device.

### Assets & Packaging

- ✅ `AssetServer`: handles, GUIDs from `.meta` sidecars, file hot reload,
  glTF node trees and per-mesh unpacking.
- 🧪 FBX models (ufbx, binary and ASCII): metres / Y up conversion,
  materials and textures, node tree, thumbnails, drag-in, Extract Materials
  and `game.pak`, like glTF. No skinning or animation yet (the Animation
  area adds them for glTF and FBX alike). Manual check: the horse, see
  `AGENTS.md` (Assets).
- 🧪 P9 Ship builds: player mode (no editor linked), Project Settings >
  Player (`config/player.ron`), `game.pak` asset pack (`.gltf` with external
  files supported), File > Build (cancel, remembered output folder, Open
  Folder / Run) and `just package <out>`, FXC shaders and a static C
  runtime so the game runs without the Windows SDK, `--frames N` smoke run.

### Tooling & Agent-friendliness

- ✅ `just gate` (fmt, clippy `-D warnings`, check, also for the player
  build), `just test`, `just test-fxc`; `unsafe_code` denied outside the
  plugin loader; unit, headless egui and GPU tests.
- ✅ `AGENTS.md` with architecture, commands, conventions and gotchas.
- ✅ Render `snapshot` example (review scenes, quality flags, GPU timings).

## Partially done

### Rendering

- 🟡 Post-processing: bloom, SSAO, ACES; no exposure control (auto or
  manual). The default look is hazy (bright horizon colour, fog 0.01,
  ambient 0.2); scenes can now set their own sun, sky and fog with
  `SceneLighting` (the Sich demo uses a golden-hour setup).
- 🟡 Blended materials cast no shadows and take AO from the surface behind
  them; an HDR sky feeds GI misses as a two-colour approximation.
- 🟡 Local shadows: more than 512 casters in one shadow tile drop the rest;
  per-view instance lists still repeat matrices per view.

### Editor

- 🟡 Profiler: GPU pass timings and `MELON_FRAME_STATS`; no profiler view
  over the `tracing` spans.
- 🟡 Light components have no intensity units and no 3D gizmo handles for
  range or cone angle.

### Assets & Packaging

- 🟡 Import pipeline: glTF, FBX, images (png / jpg / hdr), audio with fixed rules; no per-asset
  import settings.
- 🟡 Packaging: Windows only; no custom exe icon or version stamp; each
  build also compiles the unused `melon_game` dylib.

### Tooling & Agent-friendliness

- 🟡 Headless runner + screenshot capture: the render `snapshot` example
  renders review scenes or any scene file (`--scene`, `--eye`, `--look`),
  and the player has `--frames N`; no simulation before the capture.

## Not done

### Core & ECS

- ❌ Prefabs (built on the component registry).

### Rendering

- ❌ P8 Cameras as components, several cameras, and a Game view next to the
  Scene view (needs per-view uniform and instance buffers first; see
  AGENTS.md "One main render_scene per submit").
- ❌ P10 Particles.
- ❌ P10 Shader hot reload.

### Animation

- ❌ P5 glTF and FBX skinning, animation clips, blending (ufbx keeps the
  skin and clip data reachable).

### UI

- ❌ P7 In-game UI and text, plus a debug-draw API.

### Editor

- ❌ P10 Gizmo local / world toggle.

### Gameplay

- ❌ P10 Navigation mesh.
- ❌ P10 Networking (not core for a first game).

### Tooling & Agent-friendliness

- ❌ Runtime console (stdin or socket) to query and edit world state.

### Docs & known bugs

- ❌ README is stale: says "v0.1 — released" and links `LICENSE-MIT` /
  `LICENSE-APACHE`, which do not exist.

Fixed earlier: Play/Pause unreachable, physics tied to frame rate, demo
systems ignoring `Time`, no asset system, flattened glTF tree, host doing too
much, game systems in the host, scene load crash, 1024 entity cap, editor
frames stalled 100-400 ms while the mouse moved (blamed on SSGI at first).

## Compared with Unreal, Unity and Godot

Core features all three share, and where MelonEngine stands.

| Area | Feature | MelonEngine |
|---|---|---|
| Runtime | Scene tree / ECS, transform hierarchy | ✅ |
| Runtime | Fixed-step game loop, interpolation | ✅ |
| Runtime | Gameplay code with hot reload | ✅ Rust plugin |
| Runtime | Input action mapping | ✅ |
| Runtime | Reflection / component registry | ✅ |
| Runtime | Scene save / load, runtime scene changes | 🧪 |
| Runtime | Prefabs | ❌ |
| Assets | Stable asset IDs, watched folder | ✅ |
| Assets | Import pipeline with settings | 🟡 fixed rules, no settings |
| Rendering | PBR materials, textures, material assets | 🧪 |
| Rendering | Directional, point, spot lights | 🧪 |
| Rendering | Shadows (cascades, point/spot) | 🧪 |
| Rendering | Global illumination / ambient | ✅ DDGI or SSGI, IBL 🧪 |
| Rendering | Sky / environment | ✅ procedural sky, fog; HDR sky 🧪 |
| Rendering | Post-processing (bloom, AO, exposure) | 🟡 no exposure |
| Rendering | Anti-aliasing | ✅ MSAA 1/2/4, alpha-to-coverage 🧪 |
| Rendering | Culling, instancing, LOD | 🧪 |
| Rendering | Particles | ❌ |
| Rendering | Shader hot reload | ❌ |
| Rendering | Cameras as components, several cameras | ❌ |
| Animation | Skeletal animation, blending | ❌ |
| Physics | Rigid bodies, shapes | 🧪 |
| Physics | Ray casts, triggers, collision events | 🧪 |
| Physics | Character controller, joints | 🧪 |
| Audio | Playback, 3D sound, mixing | 🧪 (no effects, no reverb zones) |
| UI | In-game UI and text | ❌ |
| Gameplay | Navigation mesh | ❌ |
| Gameplay | Networking | ❌ (not core for a first game) |
| Editor | Viewport, gizmos, snapping | ✅ (no local/world toggle) |
| Editor | Hierarchy, inspector | ✅ |
| Editor | Content browser | ✅ |
| Editor | Undo / redo | 🧪 |
| Editor | Play in editor | ✅ |
| Editor | Game view | ❌ |
| Editor | Console / log | ✅ |
| Editor | Profiler | 🟡 GPU pass timings only |
| Shipping | Release build, asset packaging | 🧪 player mode, `game.pak`, File > Build |
