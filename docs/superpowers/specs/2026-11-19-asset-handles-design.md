# Asset handles and model assets

Roadmap Tier 0, item 3. Fixes bug 5 (only `cube.glb` usable, only its first
primitive drawn) and most of bug 6 (glTF node transforms ignored).

## Intent

Put any glTF model from `assets/models` into a scene: drag it from the
Content Browser, or pick it in the Inspector. Re-export a model and the
editor shows the change without restarting. Scene files store plain paths so
editing them by hand or by an agent keeps working.

## Goals

- Typed `Handle<Model>` on entities, resolved through an `AssetServer`.
- Multi-part glTF models draw whole, with each part's base-colour texture
  and node transform.
- Drag a model from the Content Browser into the Scene (placed on the ground
  under the cursor) or the Hierarchy (placed at the origin).
- Inspector **Mesh** field: dropdown of `builtin:cube` + every `.glb` /
  `.gltf` under `assets/models`.
- Hot reload: a changed model file reloads within about a second.
- Picking, selection box and F-framing use each model's real bounds.
- Old scene files (version 1, `mesh_ref`) still load.

## Non-goals

- `Handle<Texture>` / `Handle<Material>` and PBR materials (Tier 1).
- Background (async) loading, unloading / reference counting.
- glTF node hierarchy as entities (needs the transform hierarchy, item 4);
  node transforms are baked into the vertices instead.
- Skinning, animation, cameras or lights from glTF.

## Anti-goals

- A missing or broken model file crashes the editor or loses the entity.
- A failed hot reload replaces a working model with nothing.
- Scene files that store ids instead of paths.

## Approaches

| Decision | Options | Picked |
|---|---|---|
| Editor workflow | A paths only; B drag + Inspector field; **C B + hot reload** | **C** (user) |
| Reference model | A path component + path-keyed render cache; **B typed `Handle<T>` + `AssetServer`** | **B** (user): foundation for textures, materials and sounds later |
| Loading | **Synchronous on first use**; background thread | My call: first-use hitch accepted; async can come later behind the same handles |
| Change detection | **Poll file modified times ~2×/s**; `notify` crate | My call: std only, a handful of files |
| Built-in cube | **`builtin:cube` model**; replace with `cube.glb` | My call: Ground keeps its checker look; no file needed |

## Design

### `melon_core::assets` (new, no GPU)

- `pub struct Model;` asset type tag.
- `Handle<T>`: `Copy`, `Eq`, `Hash`, `Debug`, `Component`; holds a `u32` id
  and `PhantomData<fn() -> T>`. Not serialised; scenes store paths.
- `AssetServer` (resource), rooted at the project root:
  - `load<T>(&mut self, path: &str) -> Handle<T>`: normalises the path
    (forward slashes, relative to the root), returns the existing handle for
    a known path, otherwise registers a new entry. No file I/O.
  - `path<T>(&self, h) -> &str`, `full_path<T>(&self, h) -> Option<PathBuf>`
    (`None` for `builtin:` paths), `version<T>(&self, h) -> u32`.
  - `set_bounds<T>(&mut self, h, min: Vec3, max: Vec3)`,
    `bounds<T>(&self, h) -> Option<(Vec3, Vec3)>` (local space; written by
    the renderer after a load).
  - `poll_changes(&mut self)`: stats every file-backed entry; a changed
    modified time bumps its `version`.
  - `hot_reload_system`: calls `poll_changes` every 0.5 s of wall time,
    in every simulation state. Registered by `Core::new`.

### `melon_render`: models from glTF

- `load_gltf` walks the default scene's node tree. Each primitive becomes a
  `Mesh` with its node's world matrix baked into positions (and its
  inverse-transpose into normals). Its base-colour texture becomes a bind
  group, or `None` for the checkerboard. It also returns local bounds
  (min/max of the baked positions).
- `ModelCache` (owned by the host's renderer side), keyed by handle id:
  `get(&mut self, renderer, server: &mut AssetServer, h: Handle<Model>) ->
  Option<&GpuModel>`.
  - Loads on first use; on version change reloads.
  - On a load error: logs once per version, keeps the previous model if
    there was one, otherwise returns the `builtin:cube` model as a
    placeholder.
  - Writes bounds into the server.
- `builtin:cube`: the procedural cube with the checkerboard, bounds ±0.5.

### Components and scenes

- `MeshRef` is removed. Entities carry `Handle<Model>`.
- `SceneEntity` gains `model: Option<String>` (path). `SCENE_VERSION` = 2.
  Loading version 1: `mesh_ref: Cube` → `builtin:cube`,
  `mesh_ref: GltfModel` → `assets/models/cube.glb`.
- Saving resolves handles to paths through the `AssetServer`.

### Editor

- **Content Browser:** `.glb` / `.gltf` rows are drag sources carrying the
  project-relative path.
- **Scene viewport:** drop target. On release, the cursor ray hits y = 0
  (fallback: 5 m in front of the camera), and a new entity named after the
  file stem spawns there, selected.
- **Hierarchy:** drop target; spawns at the origin.
- **Inspector:** a **Mesh** row (combo box) in the Transform section's grid
  listing `builtin:cube` and every model under `assets/models` (recursive,
  sorted). Choosing one swaps the entity's handle.
- `pick`, `selection_box_lines` and `frame_target` take the model's bounds
  from the `AssetServer` (unit cube if not loaded yet).
- Create Cube / + Add use `builtin:cube`.

### Host

- Draw-list build: for each entity with `Handle<Model>`, `ModelCache::get`;
  every part is drawn with the entity's model matrix (shadow pass too).
- The startup `cube.glb` load and `loaded_model` field go away.

## Tests (headless unless noted)

- `AssetServer`: same path → same handle; path normalisation
  (`assets\\models\\a.glb` == `assets/models/a.glb`); `builtin:` has no
  full path; `poll_changes` bumps the version after the file's modified time
  changes (temp file) and not otherwise; bounds round-trip.
- Scene: version 2 round-trip keeps model paths; a version 1 file with
  `mesh_ref: Cube` loads as `builtin:cube`; a missing model path still loads
  (the entity keeps its handle).
- Picking: a model with bounds (0,0,0)–(2,1,1) is hit at x = 1.9, missed at
  x = 2.1; no bounds yet → unit cube.
- glTF (GPU test): a two-node test model (generated in the test with node
  transforms) loads as 2 meshes whose baked bounds include both translations.
- Editor: dropping a model path spawns an entity named after the file stem
  with that handle (logic function, no UI); the Inspector model list is
  sorted, starts with `builtin:cube`, and contains only `.glb` / `.gltf`.

## Review Focus (for the plan)

- Missing / corrupt model file at load and at hot reload.
- A model file deleted while in use.
- Scene saved with a handle whose path came from a drag of a file outside
  `assets/` (should be refused or made relative).
- Play → Stop with model entities (snapshot round-trip keeps handles).
- 1024-entity cap: multi-part models still use one uniform slot per entity.

## My calls (correct any)

- Synchronous loading; polling for changes (0.5 s); `builtin:cube`.
- Paths stored relative to the project root with forward slashes.
- Missing model → drawn as the checker cube placeholder + one Console
  warning, entity kept.
- Drag targets: Scene viewport (ground hit) and Hierarchy (origin).
- glTF node transforms baked into vertices (no child entities yet).

## Left to the builder

- Exact Console wording; drag-preview look; combo box width.
