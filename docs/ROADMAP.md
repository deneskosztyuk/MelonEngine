# MelonEngine Roadmap

State of the engine as of 2026-11-19 (commit `d16cea2`), and what is still
missing for a core 3D engine. Update this file when an item lands.

## Done

| Crate | What exists |
|---|---|
| `melon_core` | `Core` wraps a bevy_ecs `World` + `Schedule`. Components: `Transform`, `Name`, `MeshRef`, `Aabb`, `BodyInfo`, `Spin`, `Bob`, `PendingDelete`. Resources: `Camera` (fly camera, `world_to_screen`, `screen_to_world_ray`), one directional `Light`, `Time`, `SimulationState`, `Input`, `NameCounter`. |
| `melon_render` | wgpu forward renderer. Frame: sun shadow pass (directional shadow map, 3x3 PCF), 3D pass into an HDR target, ACES tonemap pass onto the display. Textured Lambert + ambient mesh pipeline, line pipeline (gizmos), depth buffer, dynamic-offset model uniforms, checkerboard fallback texture, glTF loader (positions, normals, uv0, indices, base color texture), offscreen `ViewportTarget`, surface lost/outdated recovery. |
| `melon_physics` | rapier3d wrapper: dynamic and fixed cuboids, ECS to rapier sync both ways, `PendingDelete` cleanup. |
| `melon_editor` | egui overlay: hierarchy, add/delete entity, inspector, Move/Rotate/Scale gizmos with snapping, RON scene save/load (versioned, atomic write, validated). Experiments: `imgui-poc`, `slint-poc` features. |
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
4. **1024 entity cap.** `MAX_ENTITIES` in `melon_render` sizes the model uniform
   buffer. More entities overflow `write_buffer` and wgpu panics. No guard.
5. **No asset system.** `MeshRef` is a hardcoded enum (`Cube | GltfModel`).
   Only `assets/models/cube.glb` is loaded and only `meshes.first()` is drawn.
   Scene `model_path` is always `None`.
6. **glTF node tree ignored.** The loader walks `document.meshes()`, so node
   transforms and hierarchy are lost.
7. **`melon_host/src/main.rs` does too much.** It builds draw lists, render
   passes and scene I/O, with three editor UI stacks behind `cfg` branches.
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
- [ ] Asset handles (`Handle<Mesh>`, `Handle<Texture>`, `Handle<Material>`) and
      an asset server; replace the `MeshRef` enum. Fixes bug 5.
- [ ] Transform hierarchy: `Parent` / `Children` + `GlobalTransform` propagation.
- [ ] Render extraction moved from `melon_host` into `melon_render`. Fixes bug 7.
- [ ] Full input: all keys, `just_pressed` / `just_released`, mouse buttons and
      wheel, action mapping.
- [ ] Plugin API so `melon_game` registers its own systems. Fixes bug 8.
- [ ] Decide on one editor UI (egui, Dear ImGui or Slint); remove the others.

### Tier 1: looks like a 3D game

- [ ] PBR metallic-roughness materials, normal maps.
- [x] Directional (sun) shadow map with 3x3 PCF.
- [ ] Point and spot lights, their shadows, cascaded sun shadows.
- [x] HDR target + ACES tonemap pass.
- [ ] MSAA, skybox.
- [ ] Instancing or storage buffers (removes the 1024 cap, bug 4), frustum culling.
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
- [ ] Pick the UI library (egui / Dear ImGui / Slint).
- [ ] Layout redesign: docking panels, menu bar, toolbar, hierarchy and
      inspector as separate panels.
- [ ] Game view (game camera) next to the Scene view.
- [ ] Gizmo upgrades: plane handles, local/world toggle.

## Agent-friendliness

- [x] `AGENTS.md` with architecture, commands and conventions.
- [ ] Headless runner + screenshot capture command, so an agent can verify
      rendering. `Core` already runs without a window.
- [ ] Component registry so an agent can build levels as RON data.
- [ ] Runtime console (stdin or socket) to query and edit world state.
