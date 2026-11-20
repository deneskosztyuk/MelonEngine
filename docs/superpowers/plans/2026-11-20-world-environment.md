# WorldEnvironment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `SceneLighting` with a `WorldEnvironment` component that points at a shared `.env` asset holding every world-look setting, with editor create / edit / assign and a File > New Scene that starts with one.

**Architecture:** `melon_core` gets `AssetRef` (GUID + path, Reflect + serde), `EnvironmentAsset` (sectioned, serde + Reflect, `#[serde(default)]`) and `world_environment_system` (PostUpdate, exclusive) that resolves the first carrier's `.env`, caches it per file version and writes `Light` / `Environment` only on change. The renderer is untouched: its contract stays the two resources. The editor draws `.env` files with one reflection-driven card (sections = collapsible headers), previews in-progress edits through a core `EnvironmentPreview` resource and writes the file when the pointer is up.

**Tech Stack:** Rust 2024, bevy_ecs / bevy_reflect 0.19, glam 0.33 (`serde` on: `Vec3` is a RON tuple `(x, y, z)`), ron 0.12, egui.

**Spec:** `docs/superpowers/specs/2026-11-20-world-environment-design.md`

## Global Constraints

- `melon_core` has no GPU code; `melon_game` depends only on `melon_core`.
- Assets are read through `AssetServer::read` (disk or pack); GUID wins over path; handles are never stored in files.
- No scene format change: `SCENE_VERSION` stays 6, no `SceneLighting` compatibility code.
- A world with no `WorldEnvironment`, or one with an empty ref, looks exactly as today (`EnvironmentAsset::default()` == `Light::default()` + `Environment::default()`).
- `.env` RON: `#[serde(default)]` on the asset and every section struct.
- Every subagent prefixes cargo / just with `RUSTC_WRAPPER=sccache`; targeted `cargo test -p <crate>` while working, `just test` at the end of a task.
- Clippy `-D warnings`, `unsafe_code` denied, tests in `#[cfg(test)] mod tests` at the bottom of each file.
- Commits: Conventional Commits, no co-author or tool credit lines.

## Review Focus

1. A `.env` that is hand-edited into invalid RON while the editor runs: the world keeps the last good look, one warning per file version (Task 2 test `broken_file_keeps_the_last_good_asset`).
2. A `.env` written by an older build (missing sections / fields): loads with defaults (Task 1 test `partial_file_takes_defaults`).
3. Two entities with `WorldEnvironment` (duplicate via Ctrl+D is one keypress away): deterministic winner = first in Hierarchy order, warning once (Task 2 test `first_in_hierarchy_order_wins`).
4. The `.env` is renamed or moved in the Content Browser: the GUID still resolves (Task 2 test `ref_resolves_by_guid_after_a_rename`).
5. Selecting another entity mid-drag of a sun slider: the preview stops driving the world next frame and the file look returns (Task 5 test `preview_is_dropped_when_the_card_is_not_drawn`).

---

### Task 1: `AssetRef` and `EnvironmentAsset` (core data)

**Files:**
- Create: `crates/melon_core/src/environment_asset.rs`
- Modify: `crates/melon_core/src/lib.rs` (module + re-exports)

**Interfaces:**
- Produces:
  - `pub const ENVIRONMENT_EXTENSION: &str = "env";`
  - `#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)] #[reflect(Default)] pub struct AssetRef { pub guid: String, pub path: String }` with `pub fn for_path(server: &AssetServer, path: &str) -> Self`, `pub fn is_empty(&self) -> bool`, `pub fn resolve<T: Send + Sync + 'static>(&self, server: &mut AssetServer) -> Option<Handle<T>>` (GUID wins via `load_guid`, else non-empty path via `load`, else `None`).
  - Sections (each `#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)] #[reflect(Default)] #[serde(default)]`, Vec3 fields `#[reflect(remote = Vec3Reflect)]`):
    `SunSection { elevation: f32, azimuth: f32, color: Vec3 }` (degrees),
    `AmbientSection { share: f32, sky: Vec3, ground: Vec3 }`,
    `SkySection { zenith: Vec3, horizon: Vec3, texture: AssetRef, texture_intensity: f32 }` (empty `texture` = procedural sky),
    `FogSection { density: f32 }`, `AoSection { strength: f32, radius: f32 }`,
    `GiSection { intensity: f32 }`, `BloomSection { intensity: f32 }`.
  - `pub struct EnvironmentAsset { pub sun, pub ambient, pub sky, pub fog, pub ao, pub gi, pub bloom }` (same derives, field order as listed).
  - `impl EnvironmentAsset { pub fn parse(text: &str) -> anyhow::Result<Self>; pub fn to_ron(&self) -> String; pub fn read(path: &Path) -> anyhow::Result<Self>; pub fn save(&self, path: &Path) -> anyhow::Result<()> /* crate::ron_file::save(self, path, false) */; pub fn light(&self) -> Light; pub fn environment(&self, sky_texture: Option<Handle<Texture>>) -> Environment }`
  - `Default` values: computed from `Light::default()` / `Environment::default()` (sun elevation / azimuth from `Light::to_light()` exactly as `SceneLighting::from_resources` does today).

- [ ] **Step 0: Capture the parity baseline (before any code change)**

Run: `RUSTC_WRAPPER=sccache cargo run -p melon_render --example snapshot -- target/review/sich_before.png --scene assets/scenes/sich_demo.ron --eye " -26,22,26" --look 0,2,0`
Expected: file written. Also `just snapshot` and copy `target/review/snapshot.png` to `target/review/default_before.png`. Task 3 compares against both.

- [ ] **Step 1: Write the failing tests** (in `environment_asset.rs`)

```rust
#[test]
fn default_reproduces_todays_light_and_environment() {
    let a = EnvironmentAsset::default();
    let (l, want) = (a.light(), Light::default());
    assert!((l.direction - want.direction).length() < 1e-4);
    assert_eq!((l.ambient, l.color, l.sky_color, l.ground_color),
               (want.ambient, want.color, want.sky_color, want.ground_color));
    assert_eq!(a.environment(None), Environment::default());
}
#[test]
fn file_round_trips() { /* non-default value in every section, save to temp dir, read back, assert_eq */ }
#[test]
fn partial_file_takes_defaults() {
    let a = EnvironmentAsset::parse("(fog: (density: 0.02))").unwrap();
    assert_eq!(a.fog.density, 0.02);
    assert_eq!(a.sun, SunSection::default());
    assert!(EnvironmentAsset::parse("(fog: ").is_err());
}
#[test]
fn sections_map_onto_the_resources() {
    // sun elevation 90 -> direction -Y; ao.strength 0.3 -> Environment::ao_strength 0.3;
    // gi.intensity 0.5, bloom.intensity 0.12, sky.texture_intensity 2.0 all land;
    // environment(Some(h)).sky_texture == Some(h).
}
#[test]
fn asset_ref_prefers_guid_and_falls_back_to_path() { /* as material.rs texture_ref test; empty ref -> None */ }
```

- [ ] **Step 2: Run, expect compile failure**

Run: `RUSTC_WRAPPER=sccache cargo test -p melon_core environment_asset`
Expected: FAIL (types not defined).

- [ ] **Step 3: Implement the module** with the interfaces above; `light()` = today's `SceneLighting::light()` math; `environment()` sets every `Environment` field from the sections (`fog_density.max(0)`, `bloom.intensity.max(0)`), `ambient.share.clamp(0,1)`. Register every section type and `AssetRef` in `Core::new`'s type registry (`register_type`) so reflection UIs see them.

- [ ] **Step 4: Run tests, expect PASS** (`cargo test -p melon_core environment_asset`)

- [ ] **Step 5: Commit** `feat(core): EnvironmentAsset (.env) and AssetRef`

---

### Task 2: `WorldEnvironment` component and system (replaces `SceneLighting`)

**Files:**
- Create: `crates/melon_core/src/world_environment.rs`
- Delete: `crates/melon_core/src/scene_lighting.rs`
- Modify: `crates/melon_core/src/lib.rs:33,78,203,221` (module, re-export, PostUpdate system, `register_component`)

**Interfaces:**
- Consumes: Task 1 types.
- Produces:
  - `#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)] #[reflect(Component, Default)] pub struct WorldEnvironment { pub environment: AssetRef }`
  - `#[derive(Resource, Clone, Debug, Default)] pub struct EnvironmentPreview(pub Option<(String, EnvironmentAsset)>)` — project-relative `.env` path + in-progress asset; the system prefers it over the file when the path equals the winner's resolved path. Inserted by `Core::new`.
  - `pub fn world_environment_system(world: &mut World)` (PostUpdate, where `scene_lighting_system` was).
  - `pub fn active_environment(world: &mut World) -> Option<Entity>`: first carrier in Hierarchy order (`hierarchy::root_order`, then children depth first).

- [ ] **Step 1: Port and write the failing tests** (in `world_environment.rs`; helper writes `.env` files into a temp project and builds `Core::new()` with `AssetServer::new(root)` + `scan_metas`)

Port the five `scene_lighting.rs` tests to the new types (defaults, values drive resources and AO / GI now come from the asset, removal restores defaults, unchanged asset does not rewrite resources, component is registered). New:

```rust
#[test] fn empty_ref_keeps_todays_defaults()          // Environment == default, Light == default
#[test] fn file_edit_reapplies()                      // rewrite file, bump version (set_modified / poll as AssetServer tests do), run: new fog density
#[test] fn broken_file_keeps_the_last_good_asset()    // valid then "(fog: " -> fog unchanged
#[test] fn missing_file_uses_defaults()               // ref to absent path -> defaults, no panic
#[test] fn first_in_hierarchy_order_wins()            // two carriers, RootOrder [b, a] -> b's file applies
#[test] fn ref_resolves_by_guid_after_a_rename()      // .meta GUID, ref path stale -> still applies
#[test] fn preview_overrides_the_file_for_its_path()  // EnvironmentPreview(Some((path, fog 0.3))) -> 0.3; None -> file value
#[test] fn sky_texture_ref_reaches_the_environment()  // sky.texture path -> Environment::sky_texture == Some(server.load(path))
```

- [ ] **Step 2: Run, expect FAIL** (`cargo test -p melon_core world_environment`)

- [ ] **Step 3: Implement.** Private `Resource` cache: `{ applied: bool, handle: Option<u32>, version: u32, asset: Option<EnvironmentAsset>, failed: Option<(u32, u32)>, carriers: (usize, Option<Entity>) }`. Per run: winner = `active_environment`; none and not applied -> return; none and applied -> defaults (same as today's removal branch). Resolve `AssetRef::resolve::<EnvironmentFile>` (new marker type `pub struct EnvironmentFile;` next to `Material` in `assets.rs`), re-read via `AssetServer::read` + `parse` only when (handle, version) changed; parse error -> `tracing::warn!` once per (handle, version), keep cached asset. Preview wins when its path == `server.path(handle)`. Sky texture: `asset.sky.texture.resolve::<Texture>(server)`. Write `Light` / `Environment` only when different (existing rule). Warn once when the carrier count > 1 or the winner changes while > 1: `"{n} World Environments; {name} wins"`.

- [ ] **Step 4: Run tests, expect PASS**; then `RUSTC_WRAPPER=sccache cargo check --workspace --all-targets` and fix the two outside users (snapshot comment, `sich_demo.rs` is Task 3; make it compile by switching to `WorldEnvironment` with an empty ref for now if needed).

- [ ] **Step 5: Commit** `feat(core)!: WorldEnvironment component reads a .env asset (replaces SceneLighting)`

---

### Task 3: Shipped default, Sich demo migration, parity check

**Files:**
- Create: `assets/environments/default.env` (+ `.meta` via `asset_db::reconcile` or the editor), `assets/demo/sich/golden_hour.env` (+ `.meta`)
- Modify: `assets/scenes/sich_demo.ron` (the "Golden Hour Lighting" entity: rename to "World Environment", component `"melon_core::world_environment::WorldEnvironment": (environment: (guid: "<golden_hour guid>", path: "assets/demo/sich/golden_hour.env"))`), `crates/melon_editor/examples/sich_demo.rs:136-155` (write `golden_hour.env` with `EnvironmentAsset::save` and spawn `WorldEnvironment` pointing at it), `crates/melon_render/examples/snapshot.rs:546` (comment)

- [ ] **Step 1:** `default.env` = `EnvironmentAsset::default().to_ron()`. `golden_hour.env` = sun (7.0, -115.0, (1.35, 0.68, 0.32)), ambient (0.3, (0.55, 0.52, 0.85), (0.6, 0.4, 0.26)), sky zenith (0.09, 0.1, 0.3), horizon (1.05, 0.55, 0.3), fog 0.0025, bloom 0.12; AO / GI / sky texture left default.
- [ ] **Step 2: Parity.** Re-run the Step 0 commands to `sich_after.png` / `snapshot.png`; compare each pair pixel-wise (small Rust or Python `-I` script in the scratchpad). Expected: identical, or max channel delta ≤ 2 from GI noise; anything more is a bug.
- [ ] **Step 3:** `just test`. Expected: green.
- [ ] **Step 4: Commit** `feat(demo): Sich demo and default environment as .env assets`

---

### Task 4: Editor asset plumbing (kind, create, pickers)

**Files:**
- Modify: `crates/melon_editor/src/content.rs:12-62` (`AssetKind::Environment`, label "Environment", `FILE_KINDS: [AssetKind; 8]` with it after `Material`, `"env" => AssetKind::Environment`), `crates/melon_editor/src/content_view.rs:42,106` (kind colour from a new `theme::colors` token; "New Environment" button next to "New Material"), `crates/melon_editor/src/asset_ops.rs` (`create_environment`), `crates/melon_editor/src/inspect.rs` (`WidgetKind::AssetRef`), `crates/melon_editor/src/lib.rs:913` (`field_widgets` for `WorldEnvironment`)

**Interfaces:**
- Produces:
  - `pub fn create_environment(server: &mut AssetServer, parent: &Path) -> anyhow::Result<PathBuf>` — writes `New Environment.env` (`unique_path`), `EnvironmentAsset::default()`, then `reconcile`.
  - `WidgetKind::AssetRef { root: PathBuf, extensions: &'static [&'static str] }` editing a `melon_core::AssetRef`: same picker / drop as `asset_field` on its `path`, and on change sets `guid` from the `.meta` (the `material_assign::slot_for` lookup; empty when none).

- [ ] **Step 1: Failing tests:** `asset_kind(Path::new("a.ENV"), false) == AssetKind::Environment`; `create_environment_writes_a_loadable_default_with_a_meta` (mirror `create_material_writes_a_loadable_default_with_a_meta`: two calls -> `New Environment.env`, `New Environment 2.env`, both parse, `.meta` exists); an egui `run_ui` test that dropping / picking through `WidgetKind::AssetRef` fills both `path` and `guid`.
- [ ] **Step 2: Run, expect FAIL** (`cargo test -p melon_editor`)
- [ ] **Step 3: Implement** per Interfaces; `field_widgets`: `type_path == WorldEnvironment::type_path()` -> `"environment"`: label "Environment", `WidgetKind::AssetRef { root, extensions: &["env"] }`.
- [ ] **Step 4: Run, expect PASS**
- [ ] **Step 5: Commit** `feat(editor): .env asset kind, New Environment, asset-ref picker`

---

### Task 5: Environment card with live preview

**Files:**
- Create: `crates/melon_editor/src/environment_card.rs`
- Modify: `crates/melon_editor/src/lib.rs` (`build_inspector` ~2111: card for a selected `.env`; under the entity's components when it has a `WorldEnvironment` with a non-empty ref; clear `EnvironmentPreview` at the start of each editor UI frame)

**Interfaces:**
- Consumes: `EnvironmentAsset`, `EnvironmentPreview`, `inspect::reflect_ui_with`, `WidgetKind::{Color, AssetRef}`.
- Produces: `pub fn show(ui: &mut egui::Ui, world: &mut World, project_root: &Path, path: &str)` (`path` project-relative).

Behaviour (the algorithm the tests pin):
- Edited copy in egui memory keyed `("environment_edit", path)` like `material_card::show`; written with `EnvironmentAsset::save` when `should_write(dirty, pointer_down)` (reuse `material_card::should_write`, make it `pub(crate)`).
- While a copy is pending, set `EnvironmentPreview(Some((path, copy)))`; the editor clears it at the start of every UI frame, so it only lives while the card keeps drawing a pending copy.
- Sections: iterate `EnvironmentAsset`'s reflected struct fields; each is an `egui::CollapsingHeader` (title-cased field name, default open: `sun`, `sky`) around `reflect_ui_with(section, widgets)`, where `widgets` is built from the section's fields by type: `Vec3` -> `WidgetKind::Color`, `AssetRef` -> `WidgetKind::AssetRef { extensions: TEXTURE_EXTENSIONS }`. No per-field code: a new section or field shows up by itself.
- Missing or unparseable file: a `theme`-coloured error label with the reason, no fields.

- [ ] **Step 1: Failing tests** (`run_ui` with `RawInput`, as `material_card` tests do; never call `Response` / `ctx` inside `input` closures):
  - `the_file_is_written_when_the_pointer_is_up_not_while_dragging` (port from `material_card`)
  - `pending_edit_drives_the_preview` (pending copy -> `EnvironmentPreview` holds it with the path)
  - `preview_is_dropped_when_the_card_is_not_drawn` (frame with card pending, next editor frame without the card -> `EnvironmentPreview(None)`)
  - `every_section_gets_a_header` (headers for all 7 field names exist by id)
  - `broken_file_shows_the_error_and_no_fields`
- [ ] **Step 2: Run, expect FAIL**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run, expect PASS**
- [ ] **Step 5: Commit** `feat(editor): environment card with live preview`

---

### Task 6: GameObject > World Environment and File > New Scene

**Files:**
- Modify: `crates/melon_editor/src/lib.rs` (menu at 643-750, `PendingOperation`, modal next to `open_scene_dialog` / `resolve_open`), `crates/melon_editor/src/asset_ops.rs` (default file)

**Interfaces:**
- Produces:
  - `pub const DEFAULT_ENVIRONMENT: &str = "assets/environments/default.env";` (in `asset_ops`)
  - `pub fn ensure_default_environment(server: &mut AssetServer, root: &Path) -> anyhow::Result<()>` — writes `EnvironmentAsset::default()` + reconciles only when the file is missing.
  - `fn create_world_environment(world: &mut World, root: &Path) -> Entity` — spawns `(Name("World Environment"), Transform::default(), WorldEnvironment { environment: AssetRef::for_path(server, DEFAULT_ENVIRONMENT) })`, after `ensure_default_environment`.
  - `Editor::create_world_environment_step(&mut self, world)` — `record_created` + select (pattern of `create_light_step`).
  - `PendingOperation::NewScene`, run in `run_pending_operation`: despawn every entity with a `Transform` (the `load_scene` rule, with the physics wrapper), `clear_selection`, `history.clear()`, `renaming = None`, `current_scene = None`, `EnvironmentPreview(None)`, spawn via `create_world_environment`, status "New scene".
  - Menu: File > "New Scene" (first item, enabled while Editing and no dialog) opens a "New scene?" modal reusing the Save / Don't Save / Cancel layout and rules of `open_scene_dialog` (Save writes the current scene first; disabled without a current file). GameObject > "World Environment".

- [ ] **Step 1: Failing tests:** `new_scene_leaves_one_world_environment` (scene with cubes -> after NewScene: exactly one entity, it has `WorldEnvironment` pointing at `DEFAULT_ENVIRONMENT`, `current_scene` is `None`, history empty); `new_scene_writes_the_default_environment_when_missing`; `create_world_environment_undoes` (step, undo -> gone, redo -> back with its ref); `new_scene_cancel_changes_nothing`.
- [ ] **Step 2: Run, expect FAIL**
- [ ] **Step 3: Implement**
- [ ] **Step 4: Run, expect PASS**
- [ ] **Step 5: Commit** `feat(editor): File > New Scene and GameObject > World Environment`

---

### Task 7: Docs and final verification

**Files:**
- Modify: `AGENTS.md` (replace the "Scene lighting" gotcha with "World environment": component, `.env` sections, preview, New Scene, no-component = defaults; mention `EnvironmentFile` handles; update the `melon_core` layout row), `docs/ROADMAP.md` (Sky / environment row and the SceneLighting mentions)

- [ ] **Step 1:** Edit docs.
- [ ] **Step 2:** `just test` -> green; `just snapshot` -> matches `default_before.png` (Task 3 rule).
- [ ] **Step 3: Manual check in the dev build** (`cargo run -p melon_host`): File > New Scene -> one "World Environment" entity; select it, drag Sun > elevation: viewport follows live; release: `default.env` on disk changed; edit the file in a text editor: viewport follows within 0.5 s; open `sich_demo.ron`: golden hour as before; Ctrl+D the environment entity: Console warns which one wins.
- [ ] **Step 4: Commit** `docs: WorldEnvironment and .env assets`
