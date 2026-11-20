# MelonEngine Roadmap

State of the engine as of 2026-11-19 (commit `d16cea2`), and what is still
missing for a core 3D engine. Update this file when an item lands.

## Done

| Crate | What exists |
|---|---|
| `melon_core` | `Core` wraps a bevy_ecs `World` + `Schedule`. Components: `Transform`, `Name`, `MeshRef`, `Aabb`, `BodyInfo`, `Spin`, `Bob`, `PendingDelete`. Resources: `Camera` (fly camera, `world_to_screen`, `screen_to_world_ray`), one directional `Light`, `Time`, `SimulationState`, `Input`, `NameCounter`. |
| `melon_render` | wgpu forward renderer. Frame: sun shadow pass (directional shadow map, 3x3 PCF), SSAO (depth/normal prepass, AO, blur), 3D pass into an HDR target, ACES tonemap pass onto the display. glTF metallic-roughness PBR mesh pipeline (normal maps, emissive, hemisphere and sky ambient), line pipeline (gizmos), depth buffer, instanced draws from a per-instance storage buffer with CPU frustum culling, checkerboard fallback texture, glTF loader (positions, normals, uv0, tangents, indices, metallic-roughness materials), offscreen `ViewportTarget`, surface lost/outdated recovery. |
| `melon_physics` | rapier3d wrapper: dynamic and fixed cuboids, ECS to rapier sync both ways, `PendingDelete` cleanup. |
| `melon_editor` | egui editor with dockable panels (Hierarchy, Scene, Inspector, Content Browser, Console), menu bar, toolbar, status bar, Move/Rotate/Scale gizmos with snapping, Play/Pause/Step/Stop, RON scene save/load (versioned, atomic write, validated). |
| `melon_game` | Hot-reloadable dylib with two demo systems. |
| `melon_host` | winit event loop that wires all crates together. |

Tooling: `just gate` (fmt + clippy `-D warnings` + check), `unsafe_code` denied
workspace-wide, unit tests plus GPU tests (including shadow and tonemap).

## Known bugs

1. ~~**Play/pause unreachable.**~~ Fixed: Play / Pause / Stop with a scene
   snapshot.
2. ~~**Physics speed tied to frame rate.**~~ Fixed: physics runs in
   `FixedUpdate` at 1/60 s.
3. ~~**Demo systems ignore `Time`.**~~ Fixed: spin and bob use `Time`, bob no
   longer drifts.
4. ~~**1024 entity cap.**~~ Fixed: per-instance records live in a storage
   buffer that doubles as needed; no model limit.
5. ~~**No asset system.**~~ Fixed: `Handle<Model>` + `AssetServer`; any glTF
   in `assets/models` draws whole.
6. ~~**glTF node tree flattened.**~~ Fixed: a dropped model unpacks into a
   root entity with one child entity per glTF node (`<file>#mesh<N>` handles).
7. ~~**`melon_host/src/main.rs` does too much.**~~ Fixed: `melon_render`
   extracts the world (`extract` → `FrameData`) and records the scene passes
   (`render_scene`, `ui_pass`); the editor runs File Save / Load / Exit. The
   host is the winit loop, input mapping and frame order.
8. **Game systems hardcoded in host.** `melon_game` cannot register its own
   systems; signatures are duplicated in the host `hot_module` imports.
9. **README is stale.** Links deleted LICENSE files, says "v0.1 — released",
   lists only egui as the editor.
10. ~~**Scene load crashes the engine.**~~ Fixed: `load_scene` despawns scene
   entities in place instead of swapping in a fresh `World`, so systems and
   all resources survive a load.

## Missing, in priority order

### Tier 0: foundation

- [x] Fixed-timestep accumulator and stages (`FixedUpdate` / `Update` /
      `PostUpdate`) with interpolated rendering. Fixes bugs 2–3. `PreUpdate`
      and `Extract` get added with their first system.
- [x] Play / Pause / Stop with scene snapshot and restore. Fixes bug 1.
- [x] Asset handles (`Handle<Model>`) and an `AssetServer`, hot reload,
      editor drag-in and Inspector mesh field. Fixes bug 5. (`Handle<Texture>` /
      `Handle<Material>` come with PBR materials.)
- [x] Transform hierarchy: `ChildOf` / `Children` + `GlobalTransform`
      propagation, world poses everywhere (render, physics, picking, gizmos),
      Hierarchy tree with drag reparenting, scene format v3. Fixes bug 6.
- [x] Render extraction moved from `melon_host` into `melon_render`. Fixes bug 7.
- [x] Full input: all keys, `just_pressed` / `just_released`, mouse buttons and
      wheel, action mapping (`Actions` / `InputMap`, Project Settings, Esc / click focus).
- [ ] Plugin API so `melon_game` registers its own systems. Fixes bug 8.
- [x] Decide on one editor UI: egui + `egui_dock` (ADR-0001); others removed.

### Tier 1: looks like a 3D game

- [x] PBR metallic-roughness materials, normal maps: glTF factors and
      textures per primitive (base colour, metallic-roughness, normal,
      emissive), tangents read or generated, Cook-Torrance sun plus sky
      ambient specular.
- [x] Directional (sun) shadow map with 3x3 PCF, no culling, slope bias and
      normal-offset sampling (no light leaks at contact seams).
- [ ] Point and spot lights, their shadows, cascaded sun shadows.
- [x] Hemisphere ambient: sky and ground colours on `Light`.
- [x] HDR target + ACES tonemap pass.
- [x] Bloom: no threshold, 13-tap downsample (Karis average first) and
      3x3 tent upsample over a half-resolution mip chain of up to 6
      levels, mixed into the HDR colour before tonemapping.
      `Environment::bloom_intensity` (default 0.08, 0 skips the passes).
      glTF emissive strength (`KHR_materials_emissive_strength`).
- [x] MSAA 4x.
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
- [x] GI modes: `Environment::gi_mode` (`Off`, `Ssgi`, `Ddgi`, default
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
      `RenderStats`. Fixes bug 4.
- [ ] `Camera` as a component; multiple cameras.
- [ ] glTF: node tree, multiple meshes, materials, skinning, animation. Fixes bug 6.
- [ ] Shader hot reload (shaders are compile-time `include_wgsl!` today).

### Tier 2: gameplay

- [ ] Physics: sphere / capsule / trimesh shapes, raycast and query API,
      collision events, character controller.
- [ ] Audio.
- [ ] Component reflection / registry so scenes and prefabs are not a
      hand-listed `SceneEntity`.
- [ ] Text rendering, in-game UI, debug-draw API.

### Tier 3: tooling

- [ ] Profiler integration (tracing spans already exist).
- [ ] Asset packaging for ship builds.

## Editor

Goal: an editor that looks and works like Unity / Unreal / Godot.

- [x] Viewport basics: scene renders inside its panel, Unity camera controls,
      click select, grid + axes + selection box, MSAA 4x, FPS and
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
- [ ] Quality settings part b: renderer reads `RenderQuality` (GI/AO/bloom
      switches, shadow map and MSAA rebuilds, present mode) and GPU
      timestamp timings fill `RenderTimings`.
- [ ] Undo / Redo (command history), rename, duplicate.
- [ ] Game view (game camera) next to the Scene view.
- [ ] Gizmo upgrades: plane handles, local/world toggle.

## Agent-friendliness

- [x] `AGENTS.md` with architecture, commands and conventions.
- [ ] Headless runner + screenshot capture command, so an agent can verify
      rendering. `Core` already runs without a window. Partly done: the
      `snapshot` example renders a fixed review scene to a PNG offscreen
      (no world or scene file yet).
- [ ] Component registry so an agent can build levels as RON data.
- [ ] Runtime console (stdin or socket) to query and edit world state.
