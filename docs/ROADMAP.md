# MelonEngine Roadmap

State of the engine as of 2026-11-20 (commit `e7df3e6`), and what is still
missing for a core 3D engine that can make 3D games. Update this file when
an item lands.

## Done

| Crate | What exists |
|---|---|
| `melon_core` | `Core` wraps a bevy_ecs `World` + stage schedules (`PreUpdate`, `FixedUpdate` at 1/60 s, `Update`, `PostUpdate`) with interpolated rendering. Transform hierarchy (`ChildOf` / `Children`, `GlobalTransform`). Assets: `Handle<Model>`, `AssetServer` (GUIDs from `.meta` sidecars, file hot reload). Input: all keys, mouse buttons and wheel, `just_pressed` / `just_released`, action mapping (`Actions` / `InputMap`), editor/game input focus. Plugin API (`App`, `Plugin`) with an ABI fingerprint. One `Camera` resource, one directional `Light`, `Environment` (sky, fog, GI/AO/bloom look), `RenderQuality` (presets, profiles in `config/quality.ron`), `Time`, `SimulationState`. |
| `melon_render` | wgpu forward renderer. Frame: sun shadow pass (directional shadow map, 3x3 PCF), GI (DDGI voxel probes or SSGI, modular), SSAO (depth/normal prepass), sky, 3D pass into an HDR target (MSAA 1/2/4 from `RenderQuality`), bloom, ACES tonemap, overlay pass (gizmos). glTF metallic-roughness PBR (normal maps, emissive), instanced draws from a storage buffer with CPU frustum culling, GPU pass timings. glTF loading (node tree, multiple meshes, materials), bounds from accessors, model thumbnails (auxiliary views). `extract` → `FrameData`, offscreen `ViewportTarget`. |
| `melon_physics` | rapier3d: dynamic and fixed cuboids, ECS ↔ rapier sync, delete cleanup. |
| `melon_audio` | kira playback: `AudioSource` / `AudioListener`, distance attenuation and pan, buses with volume and mute, one-shots, editor clip preview; silent without a device. |
| `melon_editor` | egui + egui_dock editor: dockable panels (Hierarchy, Scene, Inspector, Content Browser, Console) with a saved layout, menu bar, toolbar, status bar, theme. Move / Rotate / Scale gizmos with plane handles and snapping. Play / Pause / Step / Stop with a scene snapshot. RON scenes (version 4, GUID + path references). Project Settings (input mapping). Content Browser: folder tree, tiles/list, search, type filter, favorites, file operations carrying `.meta`, references check, live updates, Asset card in the Inspector, thumbnails, Ctrl+Space Content Drawer. |
| `melon_game` | Game plugin with two demo systems, live-reloaded in dev builds. |
| `melon_host` | winit event loop, frame order, game plugin loader (dev) / static plugin (ship). |

Tooling: `just gate` (fmt + clippy `-D warnings` + check), `unsafe_code` denied
outside the plugin loader, unit tests, headless egui tests and GPU tests.

## Known bugs

1. **README is stale.** Says "v0.1 — released" and links `LICENSE-MIT` /
   `LICENSE-APACHE`, which do not exist.

Fixed earlier: Play/Pause unreachable, physics tied to frame rate, demo
systems ignoring `Time`, no asset system, flattened glTF tree, host doing too
much, game systems in the host, scene load crash, 1024 entity cap (instance
storage buffer).

## Compared with Unreal, Unity and Godot

Core features all three share, and where MelonEngine stands (✅ done,
🟡 partial, ❌ missing).

| Area | Feature | MelonEngine |
|---|---|---|
| Runtime | Scene tree / ECS, transform hierarchy | ✅ |
| Runtime | Fixed-step game loop, interpolation | ✅ |
| Runtime | Gameplay code with hot reload | ✅ Rust plugin |
| Runtime | Input action mapping | ✅ |
| Runtime | Reflection / component registry | ✅ |
| Runtime | Scene save / load | ✅ |
| Runtime | Prefabs | ❌ |
| Assets | Stable asset IDs, watched folder | ✅ |
| Assets | Import pipeline with settings | 🟡 glTF only, no settings |
| Rendering | PBR materials, normal maps | ✅ |
| Rendering | Directional, point, spot lights | 🟡 one directional |
| Rendering | Shadows (cascades, point/spot) | 🟡 one fixed sun map |
| Rendering | Global illumination / ambient | ✅ DDGI or SSGI, hemisphere sky |
| Rendering | Sky / environment | ✅ procedural sky, fog |
| Rendering | Post-processing (bloom, AO, exposure) | 🟡 bloom, SSAO, ACES; no exposure |
| Rendering | Anti-aliasing | ✅ MSAA 1/2/4 |
| Rendering | Culling, instancing, LOD | 🟡 no LOD |
| Rendering | Particles | ❌ |
| Rendering | Shader hot reload | ❌ |
| Rendering | Cameras as components, several cameras | ❌ |
| Animation | Skeletal animation, blending | ❌ |
| Physics | Rigid bodies | 🟡 cuboids only |
| Physics | Ray casts, triggers, collision events | ❌ |
| Physics | Character controller, joints | ❌ |
| Audio | Playback, 3D sound, mixing | ✅ (no effects, no reverb zones) |
| UI | In-game UI and text | ❌ |
| Gameplay | Navigation mesh | ❌ |
| Gameplay | Networking | ❌ (not core for a first game) |
| Editor | Viewport, gizmos, snapping | ✅ (no local/world toggle) |
| Editor | Hierarchy, inspector | ✅ |
| Editor | Content browser | ✅ |
| Editor | Undo / redo | ❌ |
| Editor | Play in editor | ✅ |
| Editor | Game view | ❌ |
| Editor | Console / log | ✅ |
| Editor | Profiler | 🟡 GPU pass timings only |
| Shipping | Release build, asset packaging | 🟡 release build, no packaging |

## Missing, in priority order

Priorities 1–6 are the minimum to make any 3D game; 7–9 are needed to ship
one; 10 comes after.

1. - [x] **Component reflection / registry.** One registry drives the
      Inspector (cards, Add / Remove Component) and scene files (format v5,
      no hand-listed `SceneEntity`); an agent can build levels as RON data.
      Next step: prefabs, built on the same registry.
2. - [ ] **Undo / Redo** (command history), plus entity rename and duplicate.
3. - [ ] **Rendering:** `Handle<Texture>` / `Handle<Material>` assets; point
      and spot lights; image-based ambient; cascaded sun shadows, then
      point/spot shadows; LOD. (PBR, sky, GI, instancing and culling are
      done, see Graphics tier.)
4. - [ ] **Physics:** sphere / capsule / trimesh shapes, ray cast and query
      API, triggers and collision events, character controller, joints.
5. - [ ] **Animation:** glTF skinning, clips, blending.
6. - [x] **Audio:** `AudioSource` / `AudioListener` components, 3D
      attenuation and pan, buses (`config/audio.ron`), clip preview, kira.
7. - [ ] **In-game UI and text**, plus a debug-draw API.
8. - [ ] **Cameras as components**, several cameras, and a Game view next to
      the Scene view.
9. - [ ] **Asset packaging** for ship builds.
10. - [ ] **Later:** particles, navigation mesh, profiler integration
      (tracing spans exist), shader hot reload, exposure, gizmo local/world toggle, networking.

### Foundation (done)

- [x] Fixed-timestep stages with interpolated rendering.
- [x] Play / Pause / Stop with scene snapshot and restore.
- [x] Asset handles, `AssetServer`, hot reload, GUIDs in `.meta` files.
- [x] Transform hierarchy with world poses everywhere, scene format v4.
- [x] Render extraction in `melon_render`.
- [x] Full input with action mapping and editor/game focus.
- [x] Plugin API; the game plugin live-reloads in dev builds.
- [x] One editor UI: egui + `egui_dock` (ADR-0001).
- [x] Content Browser (`docs/superpowers/specs/2026-11-20-content-browser-design.md`):
      GUIDs, tree / tiles / search / filter / favorites, file operations,
      live updates and Inspector, thumbnails, Content Drawer.

### Graphics tier (done)

- [x] PBR metallic-roughness materials, normal maps: glTF factors and
      textures per primitive (base colour, metallic-roughness, normal,
      emissive), tangents read or generated, Cook-Torrance sun plus sky
      ambient specular.
- [x] Directional (sun) shadow map with 3x3 PCF, no culling, slope bias and
      normal-offset sampling (no light leaks at contact seams).
- [x] Hemisphere ambient: sky and ground colours on `Light`.
- [x] HDR target + ACES tonemap pass.
- [x] Bloom: no threshold, 13-tap downsample (Karis average first) and
      3x3 tent upsample over a half-resolution mip chain of up to 6
      levels, mixed into the HDR colour before tonemapping.
      `Environment::bloom_intensity` (default 0.08, 0 skips the passes).
      glTF emissive strength (`KHR_materials_emissive_strength`).
- [x] MSAA 1/2/4 from `RenderQuality::msaa` (default 4).
- [x] DDGI (dynamic diffuse GI), software voxel tracing: 128x64x128 voxel
      radiance volume (0.3125 m) voxelised each frame in three orthographic
      passes, 16x8x16 probes (2.5 m) tracing 64 rays each per frame (3D
      DDA), octahedral irradiance (8x8) and distance moments (16x16) with
      hysteresis 0.97, Chebyshev plus voxel-march visibility, multi-bounce
      through the voxels; ray misses read the hemisphere sky light, so open
      surfaces match GI off. Replaces the hemisphere diffuse inside the
      volume.
      `Environment::gi_intensity` (default 1, 0 skips the passes). Limits:
      fixed 40 x 20 x 40 m volume at the origin; walls under 0.35 m leak;
      hard edges where one probe dominates; about +2.2 to 2.4 ms on the dev GPU.
- [x] GI modes: `RenderQuality::gi_mode` (`Off`, `Ssgi`, `Ddgi`, default
      `Ddgi`), switchable frame to frame; `gi/` front with one module per
      backend and one mesh pipeline variant per mode.
- [x] SSGI: half-resolution screen-space diffuse GI, 6 cosine rays per
      pixel marching the prepass, hits read last frame's HDR colour
      (reprojected), misses the hemisphere sky light; reprojected history
      with disocclusion rejection, 5x5 depth/normal-aware denoise. About
      +0.5 to 0.9 ms on the dev GPU. Limits: only what is on screen
      bounces; one frame of lag.
- [x] SSAO: single-sample depth/normal prepass, 16-sample hemisphere AO
      with range check, 4x4 depth-aware blur, full resolution; occludes
      ambient diffuse and specular only. `Environment::ao_strength` /
      `ao_radius`; strength 0 skips the passes.
- [x] Skybox: procedural sky (horizon/zenith gradient, sun disc) from the
      `Environment` resource, plus exponential distance fog in the horizon
      colour.
- [x] Instancing: group 1 is a growable storage buffer of per-instance
      records; one instanced draw per (model, part) in the prepass, shadow
      and 3D passes. CPU frustum culling (camera planes for the camera
      passes, the shadow box for the shadow pass). `render_scene` returns
      `RenderStats`. Fixed the 1024 entity cap.

### Editor rendering and pacing

- [x] Viewport basics: scene renders inside its panel, Unity camera controls,
      click select, grid + axes + selection box, MSAA, FPS and
      orientation overlays.
- [x] Pick the UI library: egui + `egui_dock` (see `docs/adr/0001-editor-ui-library.md`).
- [x] Layout redesign: dockable tabbed panels (saved layout), menu bar,
      toolbar, status bar, theme, console from `tracing`, content browser,
      hierarchy search, Step.
- [x] Editor frame pacing: raw device events only while the game has
      input, Mailbox present with a refresh-rate frame cap (FIFO fallback,
      `MELON_PRESENT=fifo`), `MELON_FRAME_STATS=1` timing line; DX12
      first on Windows for Mailbox (`MELON_BACKEND=vulkan|dx12|auto`).
- [x] Quality settings (part a): `RenderQuality` presets, profiles,
      `config/quality.ron`, Project Settings ▸ Quality panel.
- [x] Quality settings part b: the renderer reads `RenderQuality` (GI/AO/bloom
      live; shadow map 1024/2048/4096 and MSAA 1/2/4 rebuilt in place,
      pipelines cached per sample count; present mode Vsync / Mailbox +
      cap); `Environment` is the look only; SSGI textures allocated only
      while SSGI runs; default = High, snapshots byte-identical.
- [x] GPU timings: timestamp queries between the passes, async readback
      ring, 1 s averages into `RenderTimings` (Quality panel cost bar).
- [x] Frame cap sleeps at the frame start instead of `WaitUntil` (drag
      stutter with Mailbox).
- [x] SSGI at editor size: `RenderQuality::ssgi_resolution` (Half / Quarter,
      Medium = Quarter), geometry pass + `R16Float` march texture; at
      2880x1659 GI 3.6-5.8 ms -> 1.5-2.4 (half), 0.35-0.55 (quarter).
      `MELON_FRAME_STATS` splits `other` into events / sleep / rest and
      prints the GPU total; snapshot `--size WxH`, `--ssgi-res`.
- [ ] Find the 100-330 ms SSGI stalls from a run with the split stats.

## Agent-friendliness

- [x] `AGENTS.md` with architecture, commands and conventions.
- [ ] Headless runner + screenshot capture command, so an agent can verify
      rendering. `Core` already runs without a window. Partly done: the
      `snapshot` example renders a fixed review scene to a PNG offscreen
      (no world or scene file yet).
- [x] Component registry so an agent can build levels as RON data (priority 1).
- [ ] Runtime console (stdin or socket) to query and edit world state.
