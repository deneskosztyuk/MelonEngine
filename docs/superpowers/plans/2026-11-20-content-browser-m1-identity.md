# Content Browser M1: Asset Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every file under `assets/` gets a stable GUID in a `<file>.meta` sidecar, the `AssetServer` indexes GUIDs, and scenes (version 4) reference models by GUID with the path as fallback.

**Architecture:** `melon_core::assets` gains `AssetGuid`, `AssetMeta` (read-only parsing) and a GUID index on `AssetServer` (`scan_metas`, `guid_of`, `load_guid`, `rename_path`). A new `melon_editor::asset_db` writes `.meta` files (`reconcile`: create missing, delete orphans, split duplicates, follow moves). The host runs `reconcile` once at startup. `melon_editor::scene` saves `model_guid` beside `model`.

**Tech Stack:** Rust 2024, bevy_ecs 0.19, serde + ron 0.8, `uuid` 1 (`v4`, `serde`).

**Spec:** `docs/superpowers/specs/2026-11-20-content-browser-design.md` (section M1, plus Constraints and Anti-goals).

## Global Constraints

- `melon_core` only reads `.meta` files; writing them is editor code (`asset_db`).
- `.meta` files are committed with their assets (not git-ignored).
- `.meta` content is RON `(version: 1, guid: "<hyphenated lower-case uuid>")`, `META_VERSION = 1`, file name `<asset file name>.meta` beside the asset (`cube.glb.meta`).
- Every file under `assets/` gets a `.meta`, except `.meta` files and names starting with `.`; folders get none; folders whose name starts with `.` are skipped.
- GUIDs come from `uuid::Uuid::new_v4()`.
- Writes are atomic: write `<path>.tmp`, then rename (as `save_scene` does).
- `SCENE_VERSION = 4`; versions 1-3 keep loading. A scene with only a path (no GUID) must still load.
- Never two assets with one GUID. A failed write leaves the asset and its `.meta` as they were.
- Asset paths in the `AssetServer` are project-relative with forward slashes (`assets/models/x.glb`), `#mesh<N>` fragments as today.
- New crates go in `[workspace.dependencies]`, referenced with `.workspace = true`.
- `just gate` before every commit; Conventional Commits; no co-author or tool credit lines.

## Review Focus

1. A file copied with its `.meta` in Explorer keeps the modified time, and `cube - Copy.glb` sorts before `cube.glb`: the original (the path the server already maps, or on a cold start the older creation time) keeps the GUID. Test: `reconcile_duplicate_cold_start_original_keeps_guid` and `reconcile_duplicate_known_path_keeps_guid` (Task 3).
2. A broken `.meta` (merge conflict, garbage): logged and skipped; `reconcile` must not overwrite it with a new GUID (that silently breaks every reference). Test: `reconcile_leaves_broken_meta_alone` (Task 3), `scan_metas_skips_broken_meta` (Task 2).
3. A model in the open scene moved with its `.meta` outside the editor while it runs: `reconcile` sees the GUID's path change and calls `rename_path`, so loaded handles keep their id. Test: `reconcile_follows_moved_asset` (Task 3).
4. `rename_path` on a file whose `#mesh<N>` parts are loaded: every fragment entry moves and keeps its handle id. Test: `rename_path_moves_fragments_and_keeps_ids` (Task 2).
5. A scene whose GUID is unknown but whose path exists loads by path; one whose GUID is known but path is stale loads the current file. Tests: `v4_unknown_guid_falls_back_to_path`, `v4_guid_wins_over_stale_path` (Task 4).

---

### Task 1: `AssetGuid` and `AssetMeta`

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`: `uuid = { version = "1", features = ["v4", "serde"] }`)
- Modify: `crates/melon_core/Cargo.toml` (`uuid.workspace = true`)
- Modify: `crates/melon_core/src/assets.rs` (new types + tests)
- Modify: `crates/melon_core/src/lib.rs` (re-export `AssetGuid`, `AssetMeta`, `META_VERSION`, `meta_path`)

**Interfaces:**
- Produces:
  - `pub struct AssetGuid(uuid::Uuid)`: `Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize` with `#[serde(transparent)]`; `AssetGuid::new() -> Self` (v4); `impl Display` (hyphenated lower-case); `impl FromStr`.
  - `pub const META_VERSION: u32 = 1;`
  - `pub struct AssetMeta { pub version: u32, pub guid: AssetGuid }`: `Serialize, Deserialize, Clone, Debug, PartialEq`.
  - `AssetMeta::read(path: &Path) -> anyhow::Result<AssetMeta>`: parses RON; errors on a version other than `META_VERSION`.
  - `AssetMeta::to_ron(&self) -> String` (pretty or compact, must parse back).
  - `pub fn meta_path(asset: &Path) -> PathBuf`: appends `.meta` to the full file name.

- [ ] **Step 1: Write the failing tests** (in `assets.rs` `mod tests`)

```rust
#[test]
fn meta_round_trips_and_guid_is_a_string() {
    let meta = AssetMeta { version: META_VERSION, guid: AssetGuid::new() };
    let text = meta.to_ron();
    assert!(text.contains(&format!("\"{}\"", meta.guid)), "{text}");
    let dir = temp_root("meta_rt");
    let file = dir.join("a.glb.meta");
    std::fs::write(&file, &text).unwrap();
    assert_eq!(AssetMeta::read(&file).unwrap(), meta);
}

#[test]
fn meta_with_other_version_is_an_error() {
    let dir = temp_root("meta_ver");
    let file = dir.join("a.glb.meta");
    std::fs::write(&file, format!("(version: 2, guid: \"{}\")", AssetGuid::new())).unwrap();
    assert!(AssetMeta::read(&file).is_err());
}

#[test]
fn meta_path_appends_meta() {
    assert_eq!(meta_path(Path::new("assets/models/cube.glb")),
               PathBuf::from("assets/models/cube.glb.meta"));
}

#[test]
fn guids_differ_and_parse_back() {
    let (a, b) = (AssetGuid::new(), AssetGuid::new());
    assert_ne!(a, b);
    assert_eq!(a.to_string().parse::<AssetGuid>().unwrap(), a);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_core -- meta_ guids_differ`
Expected: compile errors (`AssetMeta`, `AssetGuid`, `meta_path` not found).

- [ ] **Step 3: Add the dependency and implement the types and functions listed in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_core`
Expected: all pass.

- [ ] **Step 5: Gate and commit**

```powershell
just gate
git add Cargo.toml Cargo.lock crates/melon_core
git commit -m "feat(core): asset GUIDs and .meta sidecar format"
```

---

### Task 2: GUID index on `AssetServer`

**Files:**
- Modify: `crates/melon_core/src/assets.rs`

**Interfaces:**
- Consumes: `AssetGuid`, `AssetMeta::read`, `meta_path` (Task 1).
- Produces (all on `AssetServer`; paths are project-relative, forward slashes, no fragment):
  - `pub fn scan_metas(&mut self)`: clears and rebuilds the index from every `.meta` under `<root>/assets` (recursive, skipping folders starting with `.`). Read-only. A `.meta` that fails `AssetMeta::read` is logged with `tracing::warn!` (path + error) and skipped. A `.meta` whose asset file is missing is skipped. When two `.meta` files share a GUID, the index keeps the first in path order (`reconcile` fixes duplicates before scanning).
  - `pub fn guid_of(&self, path: &str) -> Option<AssetGuid>` (accepts a path with or without `#mesh<N>`).
  - `pub fn path_of_guid(&self, guid: AssetGuid) -> Option<&str>`.
  - `pub fn load_guid<T: Send + Sync + 'static>(&mut self, guid: AssetGuid, fragment: Option<usize>) -> Option<Handle<T>>`: `None` if unknown, else `load("<path>#mesh<N>")` or `load(path)`.
  - `pub fn rename_path(&mut self, old: &str, new: &str)`: every entry whose file part equals `old` (`old` and `old#mesh<N>`) gets `new` (keeping the fragment) and keeps its handle id; `by_path` and the GUID maps follow. Unknown `old` is a no-op.
  - `pub fn guid_index(&self) -> impl Iterator<Item = (AssetGuid, &str)>` (used by `reconcile` for the before/after comparison).
- Index storage: `guid_to_path: HashMap<AssetGuid, String>`, `path_to_guid: HashMap<String, AssetGuid>`.

- [ ] **Step 1: Write the failing tests**

```rust
fn write_asset_with_meta(root: &Path, rel: &str) -> AssetGuid {
    let file = root.join(rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"x").unwrap();
    let guid = AssetGuid::new();
    std::fs::write(meta_path(&file), AssetMeta { version: META_VERSION, guid }.to_ron()).unwrap();
    guid
}

#[test]
fn scan_metas_indexes_guids_both_ways() {
    let root = temp_root("scan");
    let g = write_asset_with_meta(&root, "assets/models/sub/a.glb");
    let mut server = AssetServer::new(root.clone());
    server.scan_metas();
    assert_eq!(server.guid_of("assets/models/sub/a.glb"), Some(g));
    assert_eq!(server.guid_of("assets/models/sub/a.glb#mesh1"), Some(g));
    assert_eq!(server.path_of_guid(g), Some("assets/models/sub/a.glb"));
}

#[test]
fn scan_metas_skips_broken_meta_and_writes_nothing() {
    let root = temp_root("scan_broken");
    let file = root.join("assets/models/a.glb");
    std::fs::write(&file, b"x").unwrap();
    std::fs::write(meta_path(&file), "not ron").unwrap();
    std::fs::write(root.join("assets/models/b.glb"), b"x").unwrap();
    let mut server = AssetServer::new(root.clone());
    server.scan_metas();
    assert_eq!(server.guid_of("assets/models/a.glb"), None);
    assert_eq!(std::fs::read_to_string(meta_path(&file)).unwrap(), "not ron");
    assert!(!meta_path(&root.join("assets/models/b.glb")).exists());
}

#[test]
fn load_guid_resolves_current_path_with_fragment() {
    let root = temp_root("load_guid");
    let g = write_asset_with_meta(&root, "assets/models/a.glb");
    let mut server = AssetServer::new(root.clone());
    server.scan_metas();
    let h: Handle<Model> = server.load_guid(g, Some(2)).unwrap();
    assert_eq!(server.path(h), "assets/models/a.glb#mesh2");
    assert!(server.load_guid::<Model>(AssetGuid::new(), None).is_none());
}

#[test]
fn rename_path_moves_fragments_and_keeps_ids() {
    let root = temp_root("rename");
    let g = write_asset_with_meta(&root, "assets/models/a.glb");
    let mut server = AssetServer::new(root.clone());
    server.scan_metas();
    let whole: Handle<Model> = server.load("assets/models/a.glb");
    let part: Handle<Model> = server.load("assets/models/a.glb#mesh1");
    server.rename_path("assets/models/a.glb", "assets/models/b.glb");
    assert_eq!(server.path(whole), "assets/models/b.glb");
    assert_eq!(server.path(part), "assets/models/b.glb#mesh1");
    assert_eq!(server.load::<Model>("assets/models/b.glb#mesh1"), part);
    assert_eq!(server.path_of_guid(g), Some("assets/models/b.glb"));
    assert_eq!(server.guid_of("assets/models/a.glb"), None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_core -- scan_metas load_guid rename_path`
Expected: compile errors (methods not found).

- [ ] **Step 3: Implement the methods listed in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_core`
Expected: all pass.

- [ ] **Step 5: Gate and commit**

```powershell
just gate
git add crates/melon_core
git commit -m "feat(core): GUID index, load by GUID and rename on the AssetServer"
```

---

### Task 3: `asset_db::reconcile` and startup wiring

**Files:**
- Create: `crates/melon_editor/src/asset_db.rs`
- Modify: `crates/melon_editor/src/lib.rs` (`pub mod asset_db;`)
- Modify: `crates/melon_editor/Cargo.toml` (`uuid` only if needed; prefer `AssetGuid::new()`)
- Modify: `crates/melon_host/src/main.rs` (after inserting `AssetServer::new(PROJECT_ROOT.into())`, call `melon_editor::asset_db::reconcile(&mut server)`)
- Modify: `AGENTS.md` (Assets gotcha: `.meta` sidecars, GUIDs, never move an asset without its `.meta`)
- Create: `.meta` files for the repo's assets (generated by the first run)

**Interfaces:**
- Consumes: Task 1 types; Task 2 `scan_metas`, `guid_index`, `path_of_guid`, `rename_path`, `root`.
- Produces:
  - `pub fn reconcile(server: &mut AssetServer)`, run in this order:
    1. `before`: snapshot of `server.guid_index()` (empty on a cold start).
    2. Walk `<root>/assets` (same skip rules as `scan_metas`). For each `.meta`: if its asset file is missing, delete it (`tracing::info!`). If it does not parse, `tracing::warn!` and leave it alone; its asset counts as having a `.meta`.
    3. Group the parsed `.meta` files by GUID. For a group of more than one, the keeper is: the path `before` maps to that GUID if it is in the group; else the asset with the earliest `std::fs::Metadata::created()` (a missing creation time sorts last); else the first in path order. Every other member gets a new GUID written atomically; `tracing::warn!` names the keeper and the re-assigned path.
    4. For each asset file with no `.meta`, write one with `AssetGuid::new()` atomically.
    5. `server.scan_metas()`.
    6. For each GUID in `before` whose path now differs and whose old file no longer exists, `server.rename_path(old, new)`.
  - `fn write_meta(asset: &Path, guid: AssetGuid) -> std::io::Result<()>`: atomic (`<meta>.tmp` then rename). On error, `tracing::warn!` and continue with the next file.
- Ruling for the executor: the spec says the scan "runs in `Core` setup"; `Core::new` has no project root (`"."`), so the host calls `reconcile` (which ends in `scan_metas`) right after it inserts the real `AssetServer`. `scan_metas` stays public for a future editor-less runtime.

- [ ] **Step 1: Write the failing tests** (in `asset_db.rs` `mod tests`; helper writes a file and optionally a `.meta`)

```rust
#[test]
fn reconcile_writes_missing_metas_but_not_for_meta_or_dot_files() {
    // assets/models/a.glb, assets/README.md, assets/.hidden, assets/.git/x
    // after reconcile: a.glb.meta and README.md.meta exist and parse;
    // no .hidden.meta, no .git/x.meta, no a.glb.meta.meta
}

#[test]
fn reconcile_removes_orphan_meta() {
    // assets/models/gone.glb.meta with no gone.glb -> deleted
}

#[test]
fn reconcile_leaves_broken_meta_alone() {
    // a.glb + a.glb.meta containing "<<<<<<< HEAD" -> file unchanged,
    // server.guid_of("assets/models/a.glb") == None
}

#[test]
fn reconcile_duplicate_cold_start_original_keeps_guid() {
    // write cube.glb + meta(g); sleep 50 ms; copy both to "cube - Copy.glb(.meta)"
    // with the same modified time (File::set_modified to the original's).
    // reconcile on a fresh server:
    assert_eq!(server.guid_of("assets/models/cube.glb"), Some(g));
    assert_ne!(server.guid_of("assets/models/cube - Copy.glb"), Some(g));
    assert!(server.guid_of("assets/models/cube - Copy.glb").is_some());
}

#[test]
fn reconcile_duplicate_known_path_keeps_guid() {
    // write a.glb (no meta); sleep 50 ms; write b.glb + meta(g); reconcile
    // (server now maps b.glb -> g, a.glb gets a fresh GUID).
    // Overwrite a.glb.meta with g. a.glb is older, so the cold-start rule
    // would pick it; the known-path rule must win. reconcile again:
    assert_eq!(server.guid_of("assets/models/b.glb"), Some(g));
    assert_ne!(server.guid_of("assets/models/a.glb"), Some(g));
}

#[test]
fn reconcile_follows_moved_asset() {
    // a.glb + meta(g); reconcile; let h: Handle<Model> = server.load("assets/models/a.glb#mesh0");
    // move a.glb and a.glb.meta to assets/models/sub/b.glb(.meta); reconcile:
    assert_eq!(server.path(h), "assets/models/sub/b.glb#mesh0");
    assert_eq!(server.path_of_guid(g), Some("assets/models/sub/b.glb"));
}
```

Fill the commented setups with real code; the assertions shown are required.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor reconcile_`
Expected: compile error (`asset_db::reconcile` not found).

- [ ] **Step 3: Implement `reconcile` and `write_meta` as specified in Interfaces; wire the host call**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor reconcile_`
Expected: 6 passed.

- [ ] **Step 5: Generate the repo's `.meta` files**

Run: `cargo run -p melon_host`, wait for the editor window, close it.
Expected: `git status` shows new `assets/**/*.meta` files (one per asset file, e.g. `assets/models/cube.glb.meta`, `assets/README.md.meta`); the Console shows no `.meta` warnings. Run it a second time: no `.meta` file changes (`git status` unchanged).

- [ ] **Step 6: Update AGENTS.md** (Assets gotcha): every file under `assets/` has a `<file>.meta` with its GUID; commit it with the asset; move or rename an asset only together with its `.meta` (in the editor, or both files in Explorer); the editor writes missing `.meta` files at startup.

- [ ] **Step 7: Gate and commit**

```powershell
just gate
git add crates/melon_editor crates/melon_host AGENTS.md assets
git commit -m "feat(editor): write and reconcile asset .meta files at startup"
```

---

### Task 4: Scene version 4 with `model_guid`

**Files:**
- Modify: `crates/melon_editor/src/scene.rs`
- Modify: `AGENTS.md` (Scene format: `SCENE_VERSION = 4`, `model_guid`)

**Interfaces:**
- Consumes: `AssetServer::guid_of`, `load_guid`, `split_mesh_fragment` (existing).
- Produces:
  - `const SCENE_VERSION: u32 = 4;` with the doc line `/// 4: entities also store `model_guid`; it wins over `model` when it resolves.`
  - `SceneEntity.model_guid: Option<AssetGuid>` with `#[serde(default)]`.
  - `scene_to_data`: `model_guid = server.guid_of(path)` (None for `builtin:` and files without `.meta`).
  - `spawn_scene_entity`: if `model_guid` is `Some(g)` and `load_guid(g, fragment_of(model))` returns a handle, use it; else fall back to `model` / `mesh_ref` as today. When the GUID is set but unknown, `tracing::warn!` once per entity naming the GUID and the fallback path.

- [ ] **Step 1: Write the failing tests** (reuse the file's `world_with_entities` style; set up a temp root with `assets/models/a.glb` + `.meta` and an `AssetServer` on that root after `scan_metas`)

```rust
#[test]
fn v4_save_writes_model_guid() {
    // entity with Handle<Model> for "assets/models/a.glb#mesh1"
    let data = scene_to_data(&mut world);
    // serialize to RON and check it contains model_guid with g's string
}

#[test]
fn v4_guid_wins_over_stale_path() {
    // scene entity: model: Some("assets/models/old.glb#mesh1"), model_guid: Some(g)
    // where g -> assets/models/a.glb. After apply_scene:
    assert_eq!(server.path(handle), "assets/models/a.glb#mesh1");
}

#[test]
fn v4_unknown_guid_falls_back_to_path() {
    // model: Some("assets/models/a.glb"), model_guid: Some(AssetGuid::new())
    // -> handle path "assets/models/a.glb"
}

#[test]
fn v4_path_only_entity_loads() {
    // hand-written RON: (version: 4, entities: [(name: "A", transform: (...), model: Some("assets/models/a.glb"))], name_counter: 0)
    // loads; handle path "assets/models/a.glb"
}
```

The existing v1/v2/v3 tests stay and must still pass.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor v4_`
Expected: compile error (`model_guid` field not found).

- [ ] **Step 3: Implement the field, save and load changes listed in Interfaces; bump `SCENE_VERSION`**

- [ ] **Step 4: Run the scene tests**

Run: `cargo test -p melon_editor scene::`
Expected: all pass, including v1-v3 tests.

- [ ] **Step 5: Full test run, then commit**

```powershell
just test
git add crates/melon_editor/src/scene.rs AGENTS.md
git commit -m "feat(editor): scene version 4 references models by GUID"
```

---

## Manual check (dev build, after Task 4)

- `cargo run -p melon_host`: drop `cube.glb` into the Scene, File ▸ Save As, open the file: the entity has `model` and `model_guid`.
- Close the editor. In Explorer, rename `cube.glb` and `cube.glb.meta` to `box.glb` / `box.glb.meta`. Start again, load the scene: the cube draws. Rename back afterwards.
