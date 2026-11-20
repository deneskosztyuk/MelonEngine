# MelonEngine Roadmap

State of the engine as of 2026-11-20 (commit `e7df3e6`), and what is still
missing for a core 3D engine that can make 3D games. Update this file when
an item lands.

## Done

| Crate | What exists |
|---|---|
| `melon_core` | `Core` wraps a bevy_ecs `World` + stage schedules (`PreUpdate`, `FixedUpdate` at 1/60 s, `Update`, `PostUpdate`) with interpolated rendering. Transform hierarchy (`ChildOf` / `Children`, `GlobalTransform`). Assets: `Handle<Model>`, `AssetServer` (GUIDs from `.meta` sidecars, file hot reload). Input: all keys, mouse buttons and wheel, `just_pressed` / `just_released`, action mapping (`Actions` / `InputMap`), editor/game input focus. Plugin API (`App`, `Plugin`) with an ABI fingerprint. One `Camera` resource, one directional `Light`, `Time`, `SimulationState`. |
| `melon_render` | wgpu forward renderer: sun shadow map (fixed box, 3x3 PCF), HDR target, ACES tonemap, MSAA 4x, overlay pass (gizmos). Diffuse lighting with a base colour texture. glTF loading (node tree, multiple meshes, base colour texture), bounds from accessors, model thumbnails. `extract` → `FrameData`, offscreen `ViewportTarget`. |
| `melon_physics` | rapier3d: dynamic and fixed cuboids, ECS ↔ rapier sync, delete cleanup. |
| `melon_editor` | egui + egui_dock editor: dockable panels (Hierarchy, Scene, Inspector, Content Browser, Console) with a saved layout, menu bar, toolbar, status bar, theme. Move / Rotate / Scale gizmos with plane handles and snapping. Play / Pause / Step / Stop with a scene snapshot. RON scenes (version 4, GUID + path references). Project Settings (input mapping). Content Browser: folder tree, tiles/list, search, type filter, favorites, file operations carrying `.meta`, references check, live updates, Asset card in the Inspector, thumbnails, Ctrl+Space Content Drawer. |
| `melon_game` | Game plugin with two demo systems, live-reloaded in dev builds. |
| `melon_host` | winit event loop, frame order, game plugin loader (dev) / static plugin (ship). |

Tooling: `just gate` (fmt + clippy `-D warnings` + check), `unsafe_code` denied
outside the plugin loader, unit tests, headless egui tests and GPU tests.

## Known bugs

1. **1024 entity cap.** `MAX_ENTITIES` sizes the model uniform buffer. Model
   drops past it are refused; other spawns are not guarded. Removed by
   instancing / storage buffers (priority 3).
2. **README is stale.** Says "v0.1 — released" and links `LICENSE-MIT` /
   `LICENSE-APACHE`, which do not exist.

Fixed earlier: Play/Pause unreachable, physics tied to frame rate, demo
systems ignoring `Time`, no asset system, flattened glTF tree, host doing too
much, game systems in the host, scene load crash.

## Compared with Unreal, Unity and Godot

Core features all three share, and where MelonEngine stands (✅ done,
🟡 partial, ❌ missing).

| Area | Feature | MelonEngine |
|---|---|---|
| Runtime | Scene tree / ECS, transform hierarchy | ✅ |
| Runtime | Fixed-step game loop, interpolation | ✅ |
| Runtime | Gameplay code with hot reload | ✅ Rust plugin |
| Runtime | Input action mapping | ✅ |
| Runtime | Reflection / component registry | ❌ |
| Runtime | Scene save / load | 🟡 hand-listed components |
| Runtime | Prefabs | ❌ |
| Assets | Stable asset IDs, watched folder | ✅ |
| Assets | Import pipeline with settings | 🟡 glTF only, no settings |
| Rendering | PBR materials, normal maps | ❌ |
| Rendering | Directional, point, spot lights | 🟡 one directional |
| Rendering | Shadows (cascades, point/spot) | 🟡 one fixed sun map |
| Rendering | Global illumination / ambient | ❌ flat ambient |
| Rendering | Sky / environment | ❌ |
| Rendering | Post-processing (bloom, AO, exposure) | 🟡 tonemap only |
| Rendering | Anti-aliasing | ✅ MSAA 4x |
| Rendering | Culling, instancing, LOD | ❌ |
| Rendering | Particles | ❌ |
| Rendering | Shader hot reload | ❌ |
| Rendering | Cameras as components, several cameras | ❌ |
| Animation | Skeletal animation, blending | ❌ |
| Physics | Rigid bodies | 🟡 cuboids only |
| Physics | Ray casts, triggers, collision events | ❌ |
| Physics | Character controller, joints | ❌ |
| Audio | Playback, 3D sound, mixing | ❌ |
| UI | In-game UI and text | ❌ |
| Gameplay | Navigation mesh | ❌ |
| Gameplay | Networking | ❌ (not core for a first game) |
| Editor | Viewport, gizmos, snapping | ✅ (no local/world toggle) |
| Editor | Hierarchy, inspector | 🟡 hand-written inspector |
| Editor | Content browser | ✅ |
| Editor | Undo / redo | ❌ |
| Editor | Play in editor | ✅ |
| Editor | Game view | ❌ |
| Editor | Console / log | ✅ |
| Editor | Profiler | ❌ |
| Shipping | Release build, asset packaging | 🟡 release build, no packaging |

## Missing, in priority order

Priorities 1–6 are the minimum to make any 3D game; 7–9 are needed to ship
one; 10 comes after.

1. - [ ] **Component reflection / registry.** One registry drives the
      Inspector, scene files and prefabs (no hand-listed `SceneEntity`); an
      agent can build levels as RON data. Prefabs follow.
2. - [ ] **Undo / Redo** (command history), plus entity rename and duplicate.
3. - [ ] **Rendering:** PBR metallic-roughness materials with normal maps
      (`Handle<Texture>`, `Handle<Material>`); point and spot lights; skybox
      and image-based ambient; instancing / storage buffers with frustum
      culling (fixes bug 1); cascaded sun shadows, then point/spot shadows.
4. - [ ] **Physics:** sphere / capsule / trimesh shapes, ray cast and query
      API, triggers and collision events, character controller, joints.
5. - [ ] **Animation:** glTF skinning, clips, blending.
6. - [ ] **Audio:** playback, 3D sound, buses.
7. - [ ] **In-game UI and text**, plus a debug-draw API.
8. - [ ] **Cameras as components**, several cameras, and a Game view next to
      the Scene view.
9. - [ ] **Asset packaging** for ship builds.
10. - [ ] **Later:** particles, navigation mesh, profiler integration
      (tracing spans exist), shader hot reload, post-processing (bloom, AO,
      exposure), gizmo local/world toggle, networking.

### Foundation (done)

- [x] Fixed-timestep stages with interpolated rendering.
- [x] Play / Pause / Stop with scene snapshot and restore.
- [x] Asset handles, `AssetServer`, hot reload, GUIDs in `.meta` files.
- [x] Transform hierarchy with world poses everywhere, scene format v4.
- [x] Render extraction in `melon_render`.
- [x] Full input with action mapping and editor/game focus.
- [x] Plugin API; the game plugin live-reloads in dev builds.
- [x] One editor UI: egui + `egui_dock` (ADR-0001).
- [x] Directional shadow map, HDR + ACES tonemap, MSAA 4x.
- [x] Content Browser (`docs/superpowers/specs/2026-11-20-content-browser-design.md`):
      GUIDs, tree / tiles / search / filter / favorites, file operations,
      live updates and Inspector, thumbnails, Content Drawer.

## Agent-friendliness

- [x] `AGENTS.md` with architecture, commands and conventions.
- [ ] Headless runner + screenshot capture command, so an agent can verify
      rendering. `Core` already runs without a window.
- [ ] Component registry so an agent can build levels as RON data (priority 1).
- [ ] Runtime console (stdin or socket) to query and edit world state.
