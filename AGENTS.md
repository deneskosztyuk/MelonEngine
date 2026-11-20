# AGENTS.md

Guide for coding agents working on MelonEngine, a Rust 3D game engine.
Current state and planned work: `docs/ROADMAP.md`.

## Layout

| Path | Role |
|---|---|
| `crates/melon_core` | ECS bootstrap (`Core` = bevy_ecs `World` + `Schedule`), components, resources, input, fly camera. No GPU or window code. |
| `crates/melon_render` | wgpu renderer: pipelines, meshes, textures, glTF loading, offscreen viewport target. `extract(world)` → `FrameData`; `Renderer::render_scene` (owns the `ModelCache`) records sun shadow pass, depth/normal prepass, SSAO passes, the GI mode's passes (`gi/`: DDGI voxel and probe passes or SSGI), 3D pass into HDR target, bloom passes, tonemap pass; `ui_pass` for the editor. |
| `crates/melon_physics` | rapier3d wrapper (`PhysicsWorld` resource) and ECS sync systems. |
| `crates/melon_editor` | egui editor (ADR-0001): dock layout (`layout.rs`, `EditorTab`), theme tokens (`theme.rs`), console (`console.rs`, fed by `tracing`), content browser (`content.rs`), viewport logic (`viewport.rs`), gizmo math (`gizmo.rs`), play/pause/stop (`play.rs`), RON scenes (`scene.rs`), Project Settings window (`project_settings.rs`). |
| `crates/melon_game` | Game logic as an `rlib` + `dylib`, hot-reloaded in dev builds. |
| `crates/melon_host` | Binary: winit event loop, input mapping, frame order (core update, editor, extract, render_scene, ui pass). No render pass code. |
| `assets/models` | glTF models. `cube.glb` loads at startup. `review/` holds the snapshot scene's material test models (`grey_cube`, `material_spheres`, `normal_panel`, `emissive_cube`, `red_panel`, `green_panel`), written by `cargo run -p melon_render --example gen_review_assets` from the repo root; commit them when regenerated. |
| `assets/shaders` | WGSL shaders, compiled in with `wgpu::include_wgsl!` (no runtime reload). Shared pieces are prepended with `concat!(include_str!(..))` (WGSL has no include): `mesh.wgsl` = `gi_common` + `ddgi_common` + `gi_ddgi` + `gi_ssgi` + `mesh`; `ddgi_update.wgsl` = `gi_common` + `ddgi_common` + it; `ssgi.wgsl` = `gi_common` + it. |

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
- **Frame pacing (`melon_host/src/pacing.rs`).** Raw device events are off
  (`DeviceEvents::Never`) and turn on (`WhenFocused`) only while
  `GameInputGate` is open: mouse look is their only user, and while the
  editor has input they were ~3,000 wake-ups/s that dropped frames on
  right-drag. The surface presents with `Mailbox` when offered, else `Fifo`
  (`MELON_PRESENT=fifo` forces FIFO, a debug A/B switch). Mailbox does not
  wait for vblank, so the host caps frames at the monitor refresh rate
  (60 Hz if unknown): `about_to_wait` sets `ControlFlow::WaitUntil` the next
  deadline (previous deadline + period, no drift; a hitch restarts the grid)
  and requests the redraw when it is due. FIFO keeps `Poll` + redraw.
- **Diagnosing stutter.** `$env:MELON_FRAME_STATS=1` makes the host print one
  stderr line every 2 s: FPS, average and worst frame interval, the worst
  frame's stages (update, acquire, editor, render, submit, other = events and
  waiting) and window/device events per frame. Off, it reads no clock.
  Redirect stderr to a file (`2> stats.txt`): a pipe filter can block it. Nothing in the engine runs on
  a 0.5 s beat except the asset poll, one `fs::metadata` per file-backed
  asset (built-ins skipped).
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
  4 emissive (sRGB), 5 `MaterialUniform`. Missing textures bind 1x1 white
  (normal: flat), so the factors apply alone and every material has this
  layout; build one with
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
  `textureLoad` at the fragment position); then every GI backend's
  bindings whatever the mode (`gi::Gi::layout_entries` / `entries`):
  1 to 5 DDGI (irradiance atlas, depth-moment atlas, linear clamp
  sampler, `GiUniform`, voxel buffer; world data shared by every view),
  6 to 8 SSGI (the view's output, `SsgiUniform`, output geometry). Each
  view (`ViewLighting`) has the group with its AO and with white AO (AO
  off); `draw_meshes_textured` takes it. `unoccluded_lighting` (white AO,
  SSGI stand-ins) serves `draw_meshes` and tests, `voxel_pass_lighting`
  the voxel passes.
- **SSAO.** Before the 3D pass `render_scene` draws the camera's batches once,
  single-sample, into the view's prepass targets (`Depth32Float` depth,
  `Rgba16Float` world normal + eye distance; `render_prepass`, also run
  for SSGI with AO off), then `ssao.wgsl` `fs_ssao`
  (16 hemisphere samples, range check, 4x4 Bayer rotation) and `fs_blur`
  (4x4 depth-aware box, applies the strength). AO multiplies ambient
  diffuse and specular only, never the sun. `Environment::ao_strength`
  (default 1; 0 skips the passes and matches the pre-SSAO image exactly) and
  `ao_radius` (metres, default 0.5). GPU tests that are not about AO set
  `ao_strength: 0.0`.
- **Emissive.** `MaterialUniform::emissive` is linear HDR radiance:
  `emissiveFactor` times `KHR_materials_emissive_strength` (the gltf
  crate feature is on), so glTF emitters can exceed 1.
- **Bloom.** After the 3D pass, before tonemapping: `render_scene` runs
  `bloom.wgsl` over the resolved HDR image into the view's bloom chain (one
  `Rgba16Float` texture at half resolution with up to `BLOOM_LEVELS` = 6
  mips, `bloom_mip_sizes`; levels under 1 px are dropped, a 1x1 view has
  none). Downsample with 13 taps (Karis average on the first), then 3x3
  tent upsample added (blend One + One) level by level into mip 0. The
  tonemap pass mixes `mix(hdr, mip0 / levels, bloom_intensity)`; no
  threshold, so uniform areas stay as they were. `Environment::bloom_intensity`
  (default 0.08; 0 skips the passes and binds black, the image matches the
  pre-bloom one exactly). Bloom textures live in `ViewportTarget` and are
  rebuilt with it. `tonemap_pass(hdr)` alone never blooms. Tradeoff: k
  lowers local contrast near every edge by about k (guarded at 87% kept
  by `bloom_adds_no_haze_keeps_edges`), so raise emitter brightness, not
  k, for more glow: lights want emissive strength well above 1 (the review
  cube is 12, about 10x a sunlit floor). GPU tests that
  are not about bloom set `bloom_intensity: 0.0`.
- **GI modes (`gi/`).** `Environment::gi_mode` (`Off`, `Ssgi`, `Ddgi`,
  default `Ddgi`) picks the technique per frame; `gi_intensity` scales it
  and 0 means `Off` (`gi::frame_mode`). Switching works frame to frame.
  `gi/mod.rs` is the front: mode dispatch (`Gi::update`, `Gi::render`,
  a `match` per mode), the group 3 entries of every backend, per-view
  resources (`GiView`, in `ViewportTarget`). Each backend owns its
  resources and passes behind the same few calls: `layout_entries`,
  `entries`, `create_view` (SSGI only), `update`, `render`. Shader side,
  mesh.wgsl `indirect_diffuse` / `gi_only` branch on the `GI_MODE`
  override; the mesh pipeline is built once per mode (`mesh_pipeline`,
  `mesh_ssgi_pipeline`, `mesh_ddgi_pipeline`, `gi::shader_mode`), so each
  compiles only its own path and Off (`GI_OFF`, every GI pass skipped)
  matches the renderer without GI exactly. Each backend's uniform holds
  its own `intensity` and `gi_only`. A third backend: `gi/<name>.rs`, a
  `GiMode` variant, its bindings after 8, arms in `Gi::update` /
  `Gi::render` / `shader_mode` / `draw_meshes_textured`, its pipeline
  variant, its shading in `gi_<name>.wgsl` (prepended in `MESH_WGSL`) and
  a branch in `indirect_diffuse`. Misses read `sky_light`
  (`gi_common.wgsl`) in every backend.
- **SSGI (`gi/ssgi.rs`, `ssgi.wgsl`, `gi_ssgi.wgsl`).** After the
  prepass, two fullscreen passes at half resolution (`half_size`, rounded
  up): `fs_trace` takes full pixel 2i of each texel, 6 cosine-weighted
  rays (interleaved-gradient noise, new every frame) of 12 quadratic steps
  up to 4 m; a sample hits when it is 1% to 0.5 m (`THICKNESS`) behind the
  prepass eye distance there, and reads last frame's resolved HDR colour
  where the hit point was last frame (the view's HDR target, read before
  the 3D pass overwrites it, so no copy; `prev_view_proj` per view); a
  miss or a hit off last frame's screen reads `sky_light`. The mean blends
  into the reprojected history (nearest texel; kept if eye distance is
  within 5% and normal within ~25 degrees, up to 32 frames), ping-ponged
  with the texel's normal and eye distance. `fs_denoise`: 5x5 blur over
  same-surface texels into the output + its geometry. The mesh shader
  (`ssgi_diffuse`) blends the 4 nearest texels that saw its surface
  (bilinear; eye distance within 10%, geometric normal within ~25
  degrees), else the hemisphere, times intensity, AO and
  `1 - metallic`. History resets on a new view (resize), a camera cut
  (`is_camera_cut`: eye jump over 2 m or a turn over 30 degrees) and any
  frame without SSGI on that view; the first frame after a reset is sky
  only; 100 frames are converged (300 differ by at most 2 sRGB steps).
  Limits: screen space only, so light from anything off screen or hidden
  disappears (a wall leaving the view stops bouncing); one frame of lag
  on moving lights; occluders are assumed 0.5 m thick (thin objects can
  shade too much, light leaks behind thick ones); half-resolution
  silhouettes and creases fall back to the hemisphere. About +0.5 to
  0.9 ms on the dev GPU with AO on (prepass shared; DDGI +2.2).
- **DDGI (`gi/ddgi.rs`, `ddgi_common.wgsl`, `ddgi_update.wgsl`,
  `gi_ddgi.wgsl`).** Per `render_scene` in mode `Ddgi`, after the shadow
  pass:
  (1) voxelise: clear the voxel buffer (3 u32 per voxel, 128x64x128 over
  `GI_ORIGIN` + 40 x 20 x 40 m, 0.3125 m), then draw every instance whose
  AABB touches the volume three times (mesh.wgsl `vs_voxel`/`fs_voxel`,
  orthographic along X, Y, Z, one pixel per voxel, no depth); (2) inject:
  each fragment writes albedo x (sun x shadow map + last frame's probe
  irradiance) + emissive into its voxel, per channel fixed point
  `u32(c * 4096) + 1` with `atomicMax` (0 = empty; the lit face of a
  thin surface wins); (3) trace + (4) blend: `cs_update`, one workgroup
  per probe (16x8x16, 2.5 m, at cell centres), 64 spherical-Fibonacci
  rays turned by a per-frame random rotation, 3D DDA to the first
  occupied voxel (a miss reads the sky light: `ambient * sky_color` above
  the horizon, `ambient * ground_color` below, never the drawn sky, which
  is several times brighter and washed the scene out; cosine-weighted
  this equals the GI-off hemisphere, so open surfaces keep their GI-off
  brightness), then every texel of the
  probe's octahedral tiles (irradiance 8x8, distance mean / mean square
  16x16, each plus a 1-texel border computed in place) blends the rays
  into last frame's value, hysteresis `min(0.97, n / (n + 1))` (a running
  mean for the first updates); the new atlases are copied over the
  sampled ones; (5) shade: mesh.wgsl `gi_irradiance` blends the 8
  surrounding probes (trilinear x backface x Chebyshev, DDGI's
  normal/view bias; Chebyshev allows one voxel of slack, since distances
  end at the hit voxel's near side; plus a voxel march from the point to
  each probe that drops probes behind geometry, skipping the point's own
  voxel and stopping a voxel short of the probe; if every probe is
  blocked the unmarched weights are used) and replaces the hemisphere
  diffuse (times AO and
  `1 - metallic`), fading back to it within one probe spacing outside the
  volume. Ambient specular stays sky-based. Turning DDGI on restarts
  convergence;
  about 30 frames converge. Limits: the volume is fixed at the origin
  (`ponytail` note: follow the camera); its corners stick out of the
  sun's shadow box (no sun shadows there, as for direct light); **walls
  must be at least 0.35 m thick** (a voxel plus the 1 cm inward push per
  face) or both faces share a voxel and the lit one leaks through; the
  binary voxel visibility gives hard edges where one probe dominates (an
  emitter's glow on the floor); each
  `render_scene` call updates the probes once, so several views per frame
  update them several times. Group 3 binding 5 is the voxel buffer
  (read-only, for the visibility march); the voxel passes bind
  `voxel_pass_lighting` with a dummy there, since they write the voxels.
  Debug with the snapshot's `--gi-only` (the indirect diffuse term alone,
  `set_gi_only`, either mode) and `--gi off`.
- **No overlapping slabs.** Boxes that overlap share face planes, which
  z-fight into stripes at oblique views (the first cornell room's wall
  tops inside the ceiling). Let walls end where the next slab starts.
- **Sun shadow setup.** The shadow pass culls nothing (front faces of
  closed meshes store depth; single-sided quads cast from both sides) with
  a small slope-scaled bias (`constant: 1, slope_scale: 1.5`), and
  `shadow_factor` in `mesh.wgsl` offsets the lookup along the geometric
  normal by about 1.5 shadow texels, scaled by the sun's grazing angle.
  Do not go back to front-face culling: it stores a resting object's
  underside, level with the floor, and with the bias light leaks along
  every contact seam (`no_light_leak_at_contact`). Back faces alone without
  the normal offset bring acne on lit faces (`lit_floor_has_no_acne`).
- **Environment.** Optional `melon_core::Environment` resource (default when
  absent, not registered in `Core::new`): procedural sky (`zenith_color`,
  `horizon_color`) and exponential distance fog (`fog_density`). The fog
  colour is `horizon_color`. Mesh fog only; world lines and overlay shapes
  are not fogged.
- **Camera conventions.** Right-handed, DirectX depth (near = 0, far = 1).
  Screen helpers take logical points.
- **Checking rendering.** Agents verify rendering with the snapshot example
  (`cargo run -p melon_render --example snapshot -- <out.png> [overview|low|sun|materials|corner|crowd|cornell] [--ao-only] [--no-ao] [--no-shadow] [--no-bloom] [--bloom-only] [--gi off|ssgi|ddgi] [--no-gi] [--gi-only]`;
  `--ao-only` saves the SSAO term as a grey image; `--no-shadow` moves scene
  and camera outside the shadow box (no sun shadows, debug only); `--no-bloom`
  sets `bloom_intensity` 0, `--bloom-only` sets it to 1 (the image is the
  bloom alone); `--gi` picks `gi_mode` (default ddgi; `--no-gi` is
  `--gi off`), `--gi-only` saves that mode's indirect diffuse term alone; `crowd` adds about
  10,000 instances; `cornell` adds an open-front red/green room at
  (-13, 0, -13) seen through its opening; the 100 timed frames run
  before the capture, so GI has converged (SSGI too: 300 frames differ
  by at most 2 sRGB steps); it prints the mean frame time
  and `RenderStats`),
  never by opening the editor or game window.
- **GPU tests** open a window, so they need a display and adapter. Tests
  that are not about GI set `gi_intensity: 0.0` (and end GI tests on a
  GI-off frame: the mesh pipeline follows the last `render_scene`). GI
  tests render 60 frames (`render_converged`, which also drops cached
  models: handle ids restart with every `AssetServer`); `render_sequence`
  renders one frame per environment into one target (mode switching).
  SSGI tests need the bouncing surface on screen.
