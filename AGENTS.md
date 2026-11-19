# AGENTS.md

Guide for coding agents working on MelonEngine, a Rust 3D game engine.
Current state and planned work: `docs/ROADMAP.md`.

## Layout

| Path | Role |
|---|---|
| `crates/melon_core` | ECS bootstrap (`Core` = bevy_ecs `World` + `Schedule`), components, resources, input, fly camera. No GPU or window code. |
| `crates/melon_render` | wgpu renderer: pipelines, meshes, textures, glTF loading, offscreen viewport target. Frame order: sun shadow pass, 3D pass into HDR target, tonemap pass onto the display. |
| `crates/melon_physics` | rapier3d wrapper (`PhysicsWorld` resource) and ECS sync systems. |
| `crates/melon_editor` | egui editor (ADR-0001): dock layout (`layout.rs`, `EditorTab`), theme tokens (`theme.rs`), console (`console.rs`, fed by `tracing`), content browser (`content.rs`), viewport logic (`viewport.rs`), gizmo math (`gizmo.rs`), play/pause/stop (`play.rs`), RON scenes (`scene.rs`). |
| `crates/melon_game` | Game logic as an `rlib` + `dylib`, hot-reloaded in dev builds. |
| `crates/melon_host` | Binary: winit event loop, wires every crate, runs the frame. |
| `assets/models` | glTF models. `cube.glb` loads at startup. |
| `assets/shaders` | WGSL shaders, compiled in with `wgpu::include_wgsl!` (no runtime reload). |

Dependency direction: `core` <- `render`, `physics` <- `editor` <- `host`.
`melon_game` depends only on `melon_core`. Keep it that way.

## Commands

```powershell
just gate        # fmt-check + clippy -D warnings + check. Run before every commit.
just test        # gate, then cargo test --workspace
just dev-run     # debug build with melon_game hot reload
just fast-run    # release run, no gate, no hot reload
```

First build is slow (wgpu, rapier, egui). Run long builds in the background.

## Conventions

- Edition 2024. Add dependencies to `[workspace.dependencies]` in the root
  `Cargo.toml`, then reference them with `.workspace = true`.
- Every crate uses `[lints] workspace = true`. Clippy warnings fail the gate.
- `unsafe_code` is denied. The only exception is `#[unsafe(no_mangle)]` on hot
  reload entry points in `melon_game`, with `#[allow(unsafe_code)]`.
- Tests live in `#[cfg(test)] mod tests` at the bottom of each file. ECS
  systems are tested by building a `World`, running a one-system `Schedule`,
  and asserting on components.
- Use `tracing` for logs and `#[instrument]` on setup and heavy functions.
- Commit messages use Conventional Commits (`feat:`, `fix:`, `chore:`).

## Gotchas

- **Hot reload signatures.** A new system in `melon_game/src/lib.rs` needs
  `#[unsafe(no_mangle)]`, and every type it uses must be imported in the
  `hot_game` module in `melon_host/src/main.rs`. Then register it in
  `App::new`.
- **Stages.** Register systems with `core.add_systems(Stage, ...)`:
  `FixedUpdate` (1/60 s, 0..N times per frame, not while paused), `Update`
  (once per frame, read `Time::delta_seconds`), `PostUpdate`.
- **Never replace `core.world` after the first frame.** bevy systems are bound
  to the first `World` they run on and panic on another one. To reset a
  scene, despawn its entities (`load_scene` despawns entities with a
  `Transform`). Never call `World::clear_entities()`: in bevy 0.19 resources
  are entities, so it deletes them too.
- **Editor panels.** A new panel is a variant of `layout::EditorTab` (title,
  `ALL`) plus a match arm in `EditorTabs::ui`. Colours and sizes come from
  `theme::colors` / `theme::sizes`, not literals. Log with `tracing`; INFO and
  above shows in the Console panel. Menu items and buttons need a working
  action: no placeholder controls.
- **Viewport.** `melon_editor/src/viewport.rs` holds the UI-library-independent
  viewport logic (`EditorCamera`, `pick`, grid/selection lines). UI glue only
  fills a `ViewportInput` and shows the `ViewportTarget` image; the scene
  always renders into `ViewportTarget` (MSAA 4x HDR, then tonemap). egui must
  sample it through `ViewportTarget::ui_view()` (non-sRGB), or it darkens.
- **Edit mode.** The engine starts in `SimulationState::Editing`: no fixed
  steps, no game systems. Play / Pause / Stop live in `melon_editor::play`
  (F5 / F6 / Shift+F5). Game systems in `Update` need
  `.run_if(Playing)`. `melon_host --play` starts playing. The editor camera owns
  `Camera`; there is no fly-camera system.
- **Interpolation.** Add `PreviousTransform` to entities moved in
  `FixedUpdate` so they render smoothly. Dynamic physics bodies get it
  automatically. Do not add it to entities moved in `Update`.
- **Entity cap.** The model uniform buffer holds 1024 entities
  (`MAX_ENTITIES` in `melon_render/src/lib.rs`).
- **Deleting entities.** Insert `PendingDelete`; `cleanup_deleted_system`
  removes the rapier body and despawns. Do not despawn physics entities
  directly.
- **Assets.** Reference files through `AssetServer::load::<Model>(path)` →
  `Handle<Model>`; paths are project-relative with forward slashes
  (`assets/models/x.glb`), plus `builtin:cube`. Never store handle ids in
  files. `melon_render::ModelCache` turns handles into GPU models; changed
  files hot-reload (poll every 0.5 s); a broken file keeps the last good model.
- **Scene format.** RON, `SCENE_VERSION = 2` in `melon_editor/src/scene.rs`
  (version 1 `mesh_ref` still loads). Adding a saved component means changing
  `SceneEntity`, `scene_to_data` and `spawn_scene_entity`. Bump the version on
  breaking changes and keep reading the old one.
- **Light direction sign.** `Light::direction` is the direction light travels
  (from the light into the scene). Shaders want the opposite; pass
  `Light::to_light()`.
- **Camera conventions.** Right-handed, DirectX depth (near = 0, far = 1).
  Screen helpers take logical points.
- **GPU tests** open a window, so they need a display and adapter.
