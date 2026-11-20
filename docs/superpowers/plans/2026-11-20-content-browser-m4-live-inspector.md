# Content Browser M4: Live Updates and the Inspector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The browser and the GUID index follow changes made outside the editor (a `notify` watcher on `assets/`), the Inspector shows the selected file's details, and the Inspector's Mesh field accepts a model dropped from the browser and can highlight its model in the browser.

**Architecture:** A new `melon_editor::asset_watch` owns the `notify` watcher and a small `Debounce` (300 ms of quiet). Each frame `Editor::build_ui` drains it; when it settles, `asset_db::reconcile` runs and the browser refreshes. A new `content::asset_details` turns a browser selection into an `AssetDetails` the Inspector draws as an "Asset" card. `ContentBrowser::ping` opens a file's folder, selects it, scrolls to it and flashes it.

**Tech Stack:** Rust 2024, `notify` 8, egui 0.35 (`Response::scroll_to_me`, `Context::request_repaint_after`), existing `melon_render::gltf_nodes`.

**Spec:** `docs/superpowers/specs/2026-11-20-content-browser-design.md` (section M4, plus Constraints, Anti-goals and Review focus).

## Global Constraints

- Watcher: `notify` on `<root>/assets`, recursive. Events go over a channel; the editor drains it each frame; after **300 ms** without new events it runs `reconcile` and refreshes the browser. A watcher that fails to start logs `tracing::warn!` and the editor runs without live updates (Refresh still works).
- The editor never blocks on the watcher (`try_recv` only) and never reconciles more than once per settle.
- Files moved outside the editor with their `.meta` keep their GUID and their loaded handles (`reconcile` already follows GUID path changes). A case-only rename outside the editor must too.
- Asset details shown: name, project-relative path, type, size, modified time, GUID; for models also mesh count and bounds (from the `AssetServer`, "not loaded yet" when unknown).
- Inspector Mesh field: also a drop target for `AssetDrag` with `model: Some(..)`. A "show" button next to it pings the model's file in the browser (open its folder, select it, scroll to it, flash for **1 s**).
- Colours and sizes from theme tokens; no placeholder controls; never call egui `Response` / `Context` methods inside `input` closures.
- `just gate` must pass before every commit (never chain a commit after a failing gate); Conventional Commits; no co-author or tool credit lines.

## Review Focus

1. A file and its `.meta` moved in Explorer arrive as separate events: no reconcile may run between them (it would delete the "orphan" `.meta` and give the file a new GUID). The 300 ms quiet period is the guard. Test: `debounce_waits_for_quiet` (Task 1) plus `editor_follows_outside_move_with_meta` (Task 1).
2. Case-only rename outside the editor (`cube.glb` → `Cube.glb` with its `.meta`): the loaded handle follows. Test: `reconcile_follows_case_only_rename` (Task 1).
3. The selected file is deleted outside the editor: the selection clears and the Inspector card disappears instead of showing stale data. Test: `refresh_drops_a_vanished_selection` (Task 1).
4. Pinging a model whose file is gone, or a `builtin:` model: the button is disabled (hover text says why), nothing panics. Test: `ping_missing_file_does_nothing` (Task 3).
5. Dropping a non-model asset (a `.png`) onto the Mesh field changes nothing. Test: `mesh_field_ignores_non_model_drop` (Task 3).

---

### Task 1: Watcher, debounce and live refresh

**Files:**
- Create: `crates/melon_editor/src/asset_watch.rs`
- Modify: `crates/melon_editor/src/lib.rs` (`pub mod asset_watch;`; `Editor` field `watcher: Option<asset_watch::AssetWatcher>`, started in `set_project_root`; drained at the start of `build_ui`)
- Modify: `crates/melon_editor/src/asset_db.rs` (case-only rename)
- Modify: `crates/melon_editor/src/content.rs` (`refresh` drops a vanished selection)
- Modify: `Cargo.toml` (`notify = "8"` in `[workspace.dependencies]`), `crates/melon_editor/Cargo.toml`

**Interfaces:**
- Produces:
  - `pub const QUIET: Duration = Duration::from_millis(300);`
  - `#[derive(Default)] pub struct Debounce { since: Option<Instant> }` with `pub fn note(&mut self, now: Instant)` (an event arrived; restarts the wait) and `pub fn ready(&mut self, now: Instant) -> bool` (true once, when `now - since >= QUIET`, then resets).
  - `pub struct AssetWatcher { _watcher: notify::RecommendedWatcher, events: std::sync::mpsc::Receiver<notify::Result<notify::Event>>, debounce: Debounce }`
  - `AssetWatcher::start(assets: &Path) -> anyhow::Result<AssetWatcher>` (recursive watch).
  - `AssetWatcher::poll(&mut self, now: Instant) -> bool`: drains with `try_recv`, `note`s each event (errors are logged and also `note`d), returns `ready(now)`.
  - `Editor`: in `build_ui`, `if watcher.poll(Instant::now())` → `asset_db::reconcile(&mut server)` (inserting the server like `run_content_request` does) and `self.content.refresh()`; while events are pending call `ui.ctx().request_repaint_after(QUIET)` so the settle happens without user input.
  - `asset_db::reconcile`: a GUID whose path changed only in letter case (`old.eq_ignore_ascii_case(new)`) is followed with `rename_path` even though the old path still "exists" on a case-insensitive file system.
  - `ContentBrowser::refresh`: if `selected` no longer exists on disk, set it to `None`.

- [ ] **Step 1: Write the failing tests**

```rust
// asset_watch.rs
#[test]
fn debounce_waits_for_quiet() {
    let t0 = Instant::now();
    let mut d = Debounce::default();
    assert!(!d.ready(t0));
    d.note(t0);
    assert!(!d.ready(t0 + Duration::from_millis(299)));
    d.note(t0 + Duration::from_millis(200)); // a second event restarts the wait
    assert!(!d.ready(t0 + Duration::from_millis(450)));
    assert!(d.ready(t0 + Duration::from_millis(500)));
    assert!(!d.ready(t0 + Duration::from_millis(900)), "fires once per settle");
}

#[test]
fn watcher_sees_a_new_file() {
    // temp assets dir; AssetWatcher::start; write a file; poll in a loop
    // (sleep 50 ms) until it returns true; give up after 5 s -> panic
}

// asset_db.rs
#[test]
fn reconcile_follows_case_only_rename() {
    // a.glb + meta(g); reconcile; h = load("assets/models/a.glb");
    // rename a.glb -> A.glb and a.glb.meta -> A.glb.meta with std::fs; reconcile
    assert_eq!(server.path(h), "assets/models/A.glb");
}

// content.rs
#[test]
fn refresh_drops_a_vanished_selection() {
    // select x.glb; delete it on disk; refresh -> selected() == None
}

// lib.rs
#[test]
fn editor_follows_outside_move_with_meta() {
    // browser_project; h = load("assets/models/x.glb") on the world's server;
    // run one build_ui frame (starts nothing new: watcher was started by set_project_root);
    // move x.glb + x.glb.meta to assets/y.glb(+.meta) with std::fs;
    // loop build_ui frames (sleep 50 ms) until server.path(h) == "assets/y.glb", give up after 5 s;
    // assert the GUID is unchanged and assets/y.glb.meta exists
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- debounce watcher_sees reconcile_follows_case refresh_drops editor_follows`
Expected: compile errors (`Debounce`, `AssetWatcher` not found) and assertion failures for the case-only rename and the vanished selection.

- [ ] **Step 3: Add `notify` and implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add Cargo.toml Cargo.lock crates/melon_editor
git commit -m "feat(editor): watch assets/ and follow outside changes"
```

---

### Task 2: Asset details in the Inspector

**Files:**
- Modify: `crates/melon_editor/src/content.rs` (`AssetDetails`, `asset_details`)
- Modify: `crates/melon_editor/src/lib.rs` (`build_inspector` draws the Asset card; mesh-count cache)

**Interfaces:**
- Consumes: `AssetServer::{guid_of, bounds, load}`, `melon_render::gltf_nodes`, `content::{asset_kind, format_size}`.
- Produces:
  - `#[derive(Clone, Debug, PartialEq)] pub struct AssetDetails { pub name: String, pub path: String /* project-relative */, pub kind: AssetKind, pub size: u64, pub modified: Option<std::time::SystemTime>, pub guid: Option<AssetGuid> }`
  - `pub fn asset_details(server: &AssetServer, path: &Path) -> Option<AssetDetails>`: `None` if the file is gone or outside the project; folders give kind `Folder`, size 0.
  - `Editor` field `mesh_counts: HashMap<PathBuf, (Option<SystemTime>, Result<usize, String>)>`: mesh count = number of distinct `GltfNode::mesh` values from `gltf_nodes`, recomputed only when the modified time changes.
  - Inspector: when the browser has a selection, an "Asset" card comes first (then the entity's cards if an entity is selected; "Nothing selected" only when neither). Rows: Name, Path, Type, Size (`format_size`), Modified (local `YYYY-MM-DD HH:MM`, or "unknown"), GUID (or "none"); models add Meshes (count or the error text) and Bounds (`min … max` from `server.bounds(handle)` with the file's handle from `server.load(path)`, or "not loaded yet").
  - Modified time formatting: `fn format_modified(time: SystemTime) -> String`. No new crate: UTC from `UNIX_EPOCH` (civil-from-days), suffix " UTC".

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn asset_details_for_a_model() {
    // project with assets/models/x.glb (+meta g)
    let d = asset_details(&server, &x).unwrap();
    assert_eq!((d.name.as_str(), d.path.as_str(), d.kind), ("x.glb", "assets/models/x.glb", AssetKind::Model));
    assert_eq!(d.guid, Some(g));
    assert_eq!(d.size, 1);
}

#[test]
fn asset_details_none_for_missing_or_outside() { /* gone file -> None; temp_dir()/other.glb -> None */ }

#[test]
fn format_modified_is_utc_civil_time() {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_763_640_000); // 2025-11-20 12:00:00 UTC
    assert_eq!(format_modified(t), "2025-11-20 12:00 UTC");
}

#[test]
fn inspector_shows_asset_card_for_browser_selection() {
    // lib.rs: browser_project, nothing selected in the scene, content.select(x);
    // run build_inspector in a headless Ui; the output text (FullOutput shapes / read_response of the card header id)
    // contains "x.glb" and "assets/models/x.glb"; and no "Nothing selected"
}
```

For the last test give the card's header label a fixed id (`egui::Id::new("inspector_asset_card")`) and assert `ctx.read_response(..)` is `Some`, plus `!editor_shows_nothing_selected` by checking that id `egui::Id::new("inspector_nothing")` (the "Nothing selected" label, given that id) is absent.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- asset_details format_modified inspector_shows_asset`
Expected: compile errors (`asset_details`, `format_modified` not found).

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add crates/melon_editor
git commit -m "feat(editor): inspector shows the selected asset's details"
```

---

### Task 3: Mesh field drop and ping

**Files:**
- Modify: `crates/melon_editor/src/content.rs` (`ping`)
- Modify: `crates/melon_editor/src/content_view.rs` (scroll + flash)
- Modify: `crates/melon_editor/src/lib.rs` (Mesh field drop target, show button)
- Modify: `crates/melon_editor/src/theme.rs` (`sizes::PING_SECONDS: f64 = 1.0` only if a literal would otherwise appear)
- Modify: `README.md` (Inspector Mesh field: drop and show), `docs/ROADMAP.md` (Editor section: Content Browser line updated)

**Interfaces:**
- Consumes: `AssetDrag` (M3), `ContentBrowser::{open, select}`.
- Produces:
  - `ContentBrowser::ping(&mut self, file: &Path) -> bool`: `false` (no change) if the file does not exist or is outside the root; else opens its parent folder (history as `open`), clears the search, selects it, and records `pinged: Option<(PathBuf, Option<f64>)>` (start time filled in by the view on the first draw).
  - View: when drawing the pinged item, `response.scroll_to_me(Some(egui::Align::Center))` on the first draw, paint an `ACCENT` outline while less than 1 s has passed (`request_repaint` meanwhile), then clear `pinged`.
  - Inspector Mesh row: the combo box gets a drop target (`dnd_release_payload::<AssetDrag>()` on the combo's response) that sets the new model when `model` is `Some` and `content::is_model_path`; a small "show" button after it calls `self.content.ping(&file)` for the current model's file (`split_mesh_fragment`), disabled with hover text "No file to show" for `builtin:` models or missing files.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn ping_opens_folder_selects_and_clears_search() {
    // root/a/x.glb ; set_search("zzz") ; assert!(ping(root/a/x.glb))
    // current() == root/a ; selected() == Some(x) ; search() == ""
}

#[test]
fn ping_missing_file_does_nothing() {
    // assert!(!ping(root/nope.glb)) ; current unchanged ; selected unchanged
}

#[test]
fn mesh_field_takes_a_model_drop() {
    // lib.rs: entity with Handle for builtin:cube selected; draw inspector frame; read combo rect
    // (give it id "inspector_mesh" via from_id_salt already; read ctx.read_response(Id::new("inspector_mesh")))
    // set AssetDrag { model: Some("assets/models/x.glb") }, release over it
    // entity's handle path == "assets/models/x.glb"
}

#[test]
fn mesh_field_ignores_non_model_drop() {
    // same with AssetDrag { path: x.png, model: None } -> handle still builtin:cube
}
```

If `read_response` cannot find the ComboBox by its salt, wrap the combo in `ui.push_id("inspector_mesh_row", ..)` and drop on a fixed-id `ui.interact` over its rect instead (same pattern as `id_label` in `content_view.rs`); ledger the ruling.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- ping_ mesh_field`
Expected: compile errors (`ping` not found) / assertion failures.

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Check in the dev build**

Run: `cargo run -p melon_host`.
Expected: copy a `.glb` into `assets/models` in Explorer → it appears in the browser within about half a second with a `.meta`; move a model and its `.meta` in Explorer while a scene uses it → it keeps drawing; select a file → the Inspector shows its Asset card; drag a model onto the Mesh field → the entity's model changes; click "show" → the browser jumps to the model and flashes it.

- [ ] **Step 6: Docs, full test run, commit**

```powershell
just test
git add crates/melon_editor README.md docs/ROADMAP.md
git commit -m "feat(editor): drop models on the mesh field and show them in the browser"
```
