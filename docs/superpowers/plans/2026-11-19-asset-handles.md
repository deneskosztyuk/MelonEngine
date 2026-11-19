# Asset Handles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Entities reference glTF models through typed `Handle<Model>` resolved by an `AssetServer`; multi-part models draw whole; the editor drags models in, picks them in the Inspector, and hot-reloads changed files.

**Architecture:** `melon_core::assets` holds `Handle<T>`, `Model` and the `AssetServer` resource (path ↔ id, version, bounds, change polling). `melon_render` loads glTF scenes into baked multi-part `LoadedModel`s and caches them per handle in `ModelCache`. Scenes store paths (format v2, v1 still loads).

**Tech Stack:** Rust 2024, bevy_ecs 0.19, wgpu 29, gltf 1.4, egui 0.35.

**Spec:** `docs/superpowers/specs/2026-11-19-asset-handles-design.md`

## Global Constraints

- Work on `main`, one commit per task, Conventional Commits, no co-author or tool trailers.
- No new dependencies. `unsafe_code` stays denied.
- Gate per task: clippy `-D warnings` clean, `cargo test --workspace` green, fmt on touched files. Visual checks use the **dev** build (`cargo run -p melon_host`), per the user's workflow.
- Paths: project-relative, forward slashes (`assets/models/x.glb`); built-in: `builtin:cube`. Hot-reload poll interval 0.5 s. `SCENE_VERSION = 2`. v1 mapping: `Cube` → `builtin:cube`, `GltfModel` → `assets/models/cube.glb`.
- egui rule: never call `Response` or `Context` methods inside `ctx.input(..)` / `ui.input(..)` closures.

## Review Focus

1. **Missing or corrupt model file**, at first load and at hot reload: no panic; first load → checker-cube placeholder + one warning; reload failure keeps the previous model. Tests in Task 2.
2. **Model file deleted while in use**: `poll_changes` must not panic on a missing file; the cache keeps drawing the last good model. Test in Task 1 (poll on deleted file).
3. **Play → Stop with model entities**: the snapshot keeps each entity's model path. Test in Task 3.
4. **Drop of a non-model file** (README.md, a shader) onto the viewport: ignored, nothing spawned. Test in Task 4 (`model_path_from_drop`).
5. **Windows paths** (`assets\models\a.glb`) from the file system: normalised to forward slashes before they become handles, so the same file never gets two handles. Test in Task 1.

---

### Task 1: `melon_core::assets`

**Files:** Create `crates/melon_core/src/assets.rs`; Modify `crates/melon_core/src/lib.rs` (module, exports, `Core::new` registers the resource + system).

**Interfaces (produces):**
- `pub struct Model;`
- `#[derive(Component)] pub struct Handle<T: Send + Sync + 'static> { id: u32, _marker: PhantomData<fn() -> T> }` with manual `Clone, Copy, PartialEq, Eq, Hash, Debug` impls (derives would wrongly require `T: Copy` etc.). `pub fn id(&self) -> u32`.
- `#[derive(Resource)] pub struct AssetServer` with:
  - `pub fn new(root: PathBuf) -> Self`
  - `pub fn load<T>(&mut self, path: &str) -> Handle<T>` (normalise: `\` → `/`, strip a leading `./`)
  - `pub fn path<T>(&self, h: Handle<T>) -> &str`
  - `pub fn full_path<T>(&self, h: Handle<T>) -> Option<PathBuf>` (`None` for `builtin:`)
  - `pub fn version<T>(&self, h: Handle<T>) -> u32`
  - `pub fn set_bounds<T>(&mut self, h: Handle<T>, min: Vec3, max: Vec3)`, `pub fn bounds<T>(&self, h: Handle<T>) -> Option<(Vec3, Vec3)>`
  - `pub fn poll_changes(&mut self)`: modified time differs from the last seen → `version += 1`; a missing file is not a change.
- `pub fn hot_reload_system(time: Res<Time>, server: Option<ResMut<AssetServer>>)`: accumulates `delta_seconds`, polls every 0.5 s.
- `Core::new` inserts `AssetServer::new(".".into())` and adds `hot_reload_system` to `Update`.

- [ ] **Step 1: Failing tests** (`assets.rs`): `same_path_same_handle`; `windows_separators_normalised` (`load("assets\\models\\a.glb") == load("assets/models/a.glb")`); `builtin_has_no_full_path`; `full_path_joins_root`; `poll_bumps_version_on_change` (temp file, poll → v0; rewrite file with a later mtime via `File::set_modified(now + 2 s)`, poll → v1; poll again → v1); `poll_ignores_deleted_file` (delete, poll, no panic, version unchanged); `bounds_round_trip`; `hot_reload_system_polls_every_half_second` (Core with a temp asset: 0.25 s frame → not polled, second 0.25 s → polled, via version bump).
- [ ] **Step 2:** `cargo test -p melon_core assets` → compile failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test --workspace` green; clippy clean.
- [ ] **Step 5: Commit** `feat(core): typed asset handles and asset server`

### Task 2: Model loading and `ModelCache` in `melon_render`

**Files:** `crates/melon_render/src/lib.rs` (loader; `ModelCache` may go in a new `crates/melon_render/src/models.rs`).

**Interfaces:**
- Consumes: `AssetServer`, `Handle<Model>` (Task 1).
- Produces:
  - `LoadedModel { pub meshes: Vec<Mesh>, pub albedo_bind_groups: Vec<Option<wgpu::BindGroup>>, pub bounds: (Vec3, Vec3) }`.
  - `SceneRenderer::load_gltf_bytes(&self, bytes: &[u8]) -> Result<LoadedModel>` (uses `gltf::import_slice`); `load_gltf(path)` reads the file and calls it. Walks `document.default_scene()` (or the first scene) recursively; each primitive's positions × node world matrix, normals × its inverse-transpose (renormalised); bounds over baked positions.
  - `SceneRenderer::builtin_cube_model(&self) -> LoadedModel` (procedural cube, `None` bind group, bounds ±0.5).
  - `pub struct ModelCache` with `pub fn new(scene: &SceneRenderer) -> Self` and `pub fn get(&mut self, scene: &SceneRenderer, server: &mut AssetServer, h: Handle<Model>) -> &LoadedModel`: load on first use / version change; on error keep the previous model, else return the built-in cube; warn once per (handle, version); write bounds to the server on success.

- [ ] **Step 1: Failing tests** (GPU runner assertions):
  - `gltf_two_nodes_load_whole`: a minimal `.gltf` JSON built in the test (one triangle mesh, base64 data-URI buffer, two nodes at x = −2 and x = +3) → 2 meshes; bounds.min.x ≤ −2, bounds.max.x ≥ +3.
  - `cache_missing_file_gives_placeholder`: server with `assets/models/nope.glb` → `get` returns a model with bounds ±0.5, no panic.
  - `cache_keeps_previous_on_broken_reload`: write a valid two-node `.gltf` to a temp root, `get` → 2 meshes; overwrite with `"garbage"`, bump the version (`poll_changes` after `set_modified`), `get` → still 2 meshes.
- [ ] **Step 2–4:** fail, implement, `cargo test -p melon_render` green.
- [ ] **Step 5: Commit** `feat(render): whole glTF scenes and a per-handle model cache`

### Task 3: Replace `MeshRef` with `Handle<Model>` (core, scenes, editor basics, host)

**Files:** `crates/melon_core/src/components.rs` + `lib.rs` (remove `MeshRef`), `crates/melon_editor/src/{scene.rs,play.rs,lib.rs,viewport.rs}`, `crates/melon_host/src/main.rs`.

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces:
  - `SceneEntity.model: Option<String>`; `SCENE_VERSION = 2`; v1 files with `mesh_ref` still deserialize (`#[serde(default)] mesh_ref: Option<LegacyMeshRef>` private enum `{ Cube, GltfModel }`) and map per Global Constraints.
  - `scene_to_data` resolves handles through the world's `AssetServer` (no server → skip `model`); `apply_scene` / `spawn_scene_entity` call `server.load(path)`.
  - `create_cube(world)` inserts `server.load::<Model>("builtin:cube")`.
  - `viewport::pick(world, origin, dir)` filters `With<Handle<Model>>` and reads bounds via `AssetServer` (fallback ±0.5); `selection_box_lines(t, bounds)`, `frame_target` use the same bounds. Signature: `pub fn model_bounds(world: &World, entity: Entity) -> (Vec3, Vec3)` shared helper.
  - Host: `App` holds `model_cache: Option<ModelCache>` (built in `resumed`); inserts `AssetServer::new(project_root)` after `Core::new`; default Ground uses `builtin:cube`; draw list: per entity, every part of `ModelCache::get(..)` with the entity's index (textured or checker), shadow pass included. Remove `cube_mesh`, `loaded_model`, the startup `cube.glb` load.

- [ ] **Step 1: Failing tests:** scene `v2_roundtrip_keeps_model_paths`; `v1_mesh_ref_cube_loads_as_builtin` (RON string with `version: 1` and `mesh_ref: Some(Cube)`); `missing_model_path_still_loads`; play `stop_keeps_model_handles` (entity with `assets/models/cube.glb` → play, stop → same path); viewport `pick_uses_model_bounds` (bounds (0,0,0)–(2,1,1): hit at x=1.9, miss at x=2.1); `pick_without_bounds_uses_unit_cube`.
- [ ] **Step 2:** fail. **Step 3:** implement; update every `MeshRef` use (grep must find none). **Step 4:** workspace tests, clippy, `cargo build -p melon_host`; launch the dev build: Ground + a Create Cube render as before.
- [ ] **Step 5: Commit** `feat: entities reference models through asset handles`

### Task 4: Editor: drag in, Inspector Mesh field

**Files:** `crates/melon_editor/src/{content.rs,lib.rs,viewport.rs}`.

**Interfaces:**
- Consumes: Task 3.
- Produces:
  - `pub fn is_model_path(path: &str) -> bool` (`.glb` / `.gltf`, case-insensitive).
  - `pub fn model_list(assets_root: &Path, project_root: &Path) -> Vec<String>`: `"builtin:cube"` first, then every model under `assets/models` recursively as project-relative forward-slash paths, sorted.
  - `pub fn spawn_model(world: &mut World, path: &str, at: Vec3) -> Entity`: name = file stem, `Transform` at `at`, `Handle<Model>`.
  - `pub fn drop_point(camera: &Camera, pointer: Vec2, size: Vec2) -> Vec3`: ray ∩ y = 0 if in front, else 5 m along the ray.
  - Content Browser: model rows are `dnd_drag_source` with payload `String` (project-relative path).
  - Scene viewport: `response.dnd_release_payload::<String>()` → `spawn_model(.., drop_point(..))`, select it. Hierarchy panel: drop → origin. Non-models ignored.
  - Inspector: a **Mesh** row (combo box) in the Transform grid; choosing swaps the handle (`after_transform_edit` not needed).

- [ ] **Step 1: Failing tests:** `is_model_path_accepts_glb_gltf_only` (`a.GLB` yes, `README.md` no); `model_list_sorted_builtin_first` (temp project with `assets/models/b.glb`, `assets/models/sub/a.gltf`, `assets/models/notes.txt` → `["builtin:cube", "assets/models/b.glb", "assets/models/sub/a.gltf"]`); `spawn_model_names_from_stem`; `drop_point_hits_ground` (camera default, centre pointer → y ≈ 0); `drop_point_above_horizon_falls_back` (pointer at top edge looking level → 5 m along the ray).
- [ ] **Step 2–4:** fail, implement, green. Dev-build check: Mesh field visible; Content Browser lists models.
- [ ] **Step 5: Commit** `feat(editor): drag models into the scene, Inspector mesh field`

### Task 5: Docs

**Files:** `docs/ROADMAP.md` (tick asset handles; bug 5 fixed; bug 6 partly: node transforms baked, hierarchy pending), `AGENTS.md` (assets: `AssetServer::load`, paths project-relative, `builtin:cube`, hot reload; adding a component to scenes now also bumps `SCENE_VERSION` handling), `README.md` (dragging models in, Inspector Mesh field, hot reload).

- [ ] **Step 1:** update. **Step 2:** tests + clippy. **Step 3: Commit** `docs: asset handles`
