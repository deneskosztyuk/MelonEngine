# AGENTS.md

Guide for coding agents working on MelonEngine, a Rust 3D game engine.
Current state and planned work: `docs/ROADMAP.md`.

## Layout

| Path | Role |
|---|---|
| `crates/melon_core` | ECS bootstrap (`Core` = bevy_ecs `World` + `Schedule`), components, resources, input, fly camera. No GPU or window code. |
| `crates/melon_render` | wgpu renderer: pipelines, meshes, textures, glTF loading, offscreen viewport target. `extract(world)` → `FrameData`; `Renderer::render_scene` (owns the `ModelCache`) records sun shadow pass, SSAO passes, 3D pass into HDR target, tonemap pass; `ui_pass` for the editor. |
| `crates/melon_physics` | rapier3d wrapper (`PhysicsWorld` resource) and ECS sync systems. |
| `crates/melon_editor` | egui editor (ADR-0001): dock layout (`layout.rs`, `EditorTab`), theme tokens (`theme.rs`), console (`console.rs`, fed by `tracing`), content browser (`content.rs`), viewport logic (`viewport.rs`), gizmo math (`gizmo.rs`), play/pause/stop (`play.rs`), RON scenes (`scene.rs`), Project Settings window (`project_settings.rs`). |
| `crates/melon_game` | Game logic as an `rlib` + `dylib`, hot-reloaded in dev builds. |
| `crates/melon_host` | Binary: winit event loop, input mapping, frame order (core update, editor, extract, render_scene, ui pass). No render pass code. |
| `assets/models` | glTF models. `cube.glb` loads at startup. `review/` holds the snapshot scene's material test models (`grey_cube`, `material_spheres`, `normal_panel`), written by `cargo run -p melon_render --example gen_review_assets` from the repo root; commit them when regenerated. |
| `assets/shaders` | WGSL shaders, compiled in with `wgpu::include_wgsl!` (no runtime reload). |

Dependency direction: `core` <- `render`, `physics` <- `editor` <- `host`.
`melon_game` depends only on `melon_core`. Keep it that way.

## Commands

```powershell
just gate        # fmt-check + clippy -D warnings + check. Run before every commit.
just test        # gate, then cargo test --workspace
just dev-run     # debug build with melon_game hot reload
just fast-run    # release run, no gate, no hot reload
just snapshot    # render the review scene offscreen to target/review/snapshot.png
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
  (once per frame, read `Time::delta_seconds`), `PostUpdate`. `PreUpdate`
  runs first each frame and fills `Actions`.
- **Input.** Game code reads `Actions` (`pressed` / `just_pressed` /
  `just_released` / `axis` by action name), not raw keys; names and bindings
  live in `InputMap` (`config/input.ron`, edited in Edit ▸ Project Settings).
  The host feeds `Input` only while `GameInputGate` is true (the editor writes
  it from `InputFocus`: `Game` while playing, Esc gives it back to `Editor`).
  `RawPresses` sees every press whatever the focus and is cleared after the
  editor UI (rebinding capture).
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
- **Overlay shapes.** Solid editor shapes (gizmo now; icons and debug shapes
  later) are `melon_render::OverlayDraw`s passed to `render_scene(.., overlay)`:
  drawn after tonemapping in exact sRGB colours, `OnTop` or `SceneTested`.
  The gizmo builds its draws and picks handles in `melon_editor::gizmo`
  (`gizmo_draws`, `pick_handle`, `drag_*`).
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
- **Instancing (group 1).** Group 1 of the mesh, prepass and shadow
  pipelines is a read-only storage buffer of `InstanceData` (model and
  normal matrix), read at `@builtin(instance_index)`. `render_scene` culls
  each entity's world AABB (model bounds under its matrix) against the
  camera frustum (prepass, 3D) and the shadow box (shadow pass), groups the
  survivors by model handle (contiguous records, input order kept) and
  issues one `draw_indexed` per (model, part) with that instance range.
  `update_instances` rewrites all records each frame; the buffer doubles
  when it runs out (bind group rebuilt). No entity cap. `render_scene`
  returns `RenderStats` (draws, instances, culled).
- **Deleting entities.** Call `melon_core::hierarchy::mark_for_delete` (marks
  the subtree `PendingDelete`); `cleanup_deleted_system` removes the rapier
  bodies and despawns. Do not despawn physics entities directly.
- **Hierarchy.** Parents use bevy's `ChildOf` / `Children`. `Transform` is
  local to the parent; for world poses use `melon_core::hierarchy`
  (`world_matrix`, `set_world_transform`, `set_parent_keep_world`,
  `render_matrices`) or `GlobalTransform` (written in `PostUpdate` and before
  physics sync). Fixed bodies follow their parent; dynamic bodies don't.
- **Assets.** Reference files through `AssetServer::load::<Model>(path)` →
  `Handle<Model>`; paths are project-relative with forward slashes
  (`assets/models/x.glb`), plus `builtin:cube`; `x.glb#mesh2` draws one glTF
  mesh unbaked (used by unpacked models). Never store handle ids in
  files. `melon_render::ModelCache` turns handles into GPU models; changed
  files hot-reload (poll every 0.5 s); a broken file keeps the last good model.
- **Scene format.** RON, `SCENE_VERSION = 3` in `melon_editor/src/scene.rs`:
  `parent` is an index into `entities`, `transform` is local (versions 1 and 2
  still load, all roots). Adding a saved component means changing
  `SceneEntity`, `scene_to_data` and `spawn_scene_entity`. Bump the version on
  breaking changes and keep reading the old one.
- **Light direction sign.** `Light::direction` is the direction light travels
  (from the light into the scene). Shaders want the opposite; pass
  `Light::to_light()`.
- **Ambient.** A sky/ground hemisphere on `Light` (`sky_color`, `ground_color`, scaled by `ambient`), blended by the world normal's y.
- **Materials.** glTF metallic-roughness per primitive (`MaterialUniform`:
  base colour, metallic, roughness, normal scale, emissive). Group 2 of the
  mesh pipeline: 0 base colour (sRGB), 1 sampler, 2 metallic-roughness
  (linear, G = roughness, B = metallic), 3 normal (linear, tangent space),
  4 emissive (sRGB), 5 `MaterialUniform`. Missing textures bind 1x1 white /
  flat normal / black, so every material has this layout; build one with
  `create_material_bind_group`. `LoadedModel::material_bind_groups` has one
  per mesh; `builtin:cube` is the checkerboard on a rough white dielectric.
- **Shading.** Sun: Cook-Torrance (GGX, height-correlated Smith, Schlick).
  `Light::color` is scaled so a white Lambert surface facing the sun shows
  `(1 - ambient) * color` (the shader uses radiance `PI * color`); `ambient`
  still splits light between sun and hemisphere, which keeps the pre-PBR
  brightness. Ambient specular is the sky gradient in the mirror direction,
  faded toward the hemisphere as roughness rises. Shadows touch the sun
  term only; emissive is added before fog.
- **Tangents.** `Vertex` is 48 bytes: position, normal, uv, tangent
  (`[f32; 4]`, w = bitangent sign, glTF convention: bitangent =
  cross(normal, tangent) * w). glTF `TANGENT` is read when present, else
  `generate_tangents` builds them from positions and UVs. Node transforms
  are baked into tangents too (w flips under mirroring).
- **Per-view lighting (group 3).** Group 3 of the mesh pipeline holds
  per-view lighting inputs, created per `ViewportTarget` (rebuilt with it on
  resize): binding 0 = SSAO texture (`R8Unorm`, 1 = open, read with
  `textureLoad` at the fragment position). The DDGI probe data joins this
  group later. `draw_meshes_textured` takes the group; AO off binds a 1x1
  white one (`unoccluded_lighting`).
- **SSAO.** Before the 3D pass `render_scene` draws the camera's batches once,
  single-sample, into the view's prepass targets (`Depth32Float` depth,
  `Rgba16Float` world normal + eye distance), then `ssao.wgsl` `fs_ssao`
  (16 hemisphere samples, range check, 4x4 Bayer rotation) and `fs_blur`
  (4x4 depth-aware box, applies the strength). AO multiplies ambient
  diffuse and specular only, never the sun. `Environment::ao_strength`
  (default 1; 0 skips the passes and matches the pre-SSAO image exactly) and
  `ao_radius` (metres, default 0.5). GPU tests that are not about AO set
  `ao_strength: 0.0`.
- **Environment.** Optional `melon_core::Environment` resource (default when
  absent, not registered in `Core::new`): procedural sky (`zenith_color`,
  `horizon_color`) and exponential distance fog (`fog_density`). The fog
  colour is `horizon_color`. Mesh fog only; world lines and overlay shapes
  are not fogged.
- **Camera conventions.** Right-handed, DirectX depth (near = 0, far = 1).
  Screen helpers take logical points.
- **Checking rendering.** Agents verify rendering with the snapshot example
  (`cargo run -p melon_render --example snapshot -- <out.png> [overview|low|sun|materials|corner|crowd] [--ao-only|--no-ao]`;
  `--ao-only` saves the SSAO term as a grey image; `crowd` adds about
  10,000 instances; it prints the mean frame time and `RenderStats`),
  never by opening the editor or game window.
- **GPU tests** open a window, so they need a display and adapter.
