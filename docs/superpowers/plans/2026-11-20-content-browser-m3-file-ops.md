# Content Browser M3: File Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create, rename, move, duplicate, delete, reveal and import files from the Content Browser (each `.meta` travelling with its asset), and open files on double-click: scenes load after a Save / Don't Save / Cancel prompt, everything else opens in the OS default app.

**Architecture:** A new UI-free `melon_editor::asset_ops` does the disk work and keeps the `AssetServer` in step (`rename_path` for moved paths, then `asset_db::reconcile`). The browser view queues `BrowserRequest`s; `Editor::apply_content_requests(world)` executes them (it owns the `AssetServer` access, the confirm modals and the status bar). All browser drags carry one payload type, `content::AssetDrag`, used for moving into folders and (for models) dropping into the Scene / Hierarchy.

**Tech Stack:** Rust 2024, egui 0.35 (`Modal`, `dnd_drag_source`, `dnd_release_payload`, `RawInput::dropped_files`), `trash` 5, rfd 0.14.

**Spec:** `docs/superpowers/specs/2026-11-20-content-browser-design.md` (sections M3 and "Opening files", plus Constraints and Anti-goals).

## Global Constraints

- No data loss: deletes go to the Recycle Bin (`trash` crate), a move or rename never overwrites an existing file, a failed operation leaves the file and its `.meta` where they were.
- Every operation moves / copies the `.meta` with its file; folders carry every `.meta` inside them. After every operation call `asset_db::reconcile(server)`.
- Names: reject empty or whitespace-only, path separators and `: * ? " < > |`, a trailing `.` or space, a leading `.`, a `.meta` suffix, and a name that already exists in the folder (case-insensitive; a case-only rename of the same item is allowed).
- New names on clash: `New Folder`, `New Folder 2`, ...; duplicates and imports `name 2.ext`, `name 3.ext`, ...
- Scenes are `.ron` files under `assets/`. File ▸ Save As starts in `assets/scenes/` (created if missing); saving elsewhere still works.
- Double-click / Enter: folder opens; scene asks Save / Don't Save / Cancel, then loads (always asks: no unsaved-change tracking); any other file opens in the OS default app. Opening a scene only in `SimulationState::Editing` (else status "Stop playing to open a scene").
- Show in Explorer: Windows `explorer /select,<path>`; macOS `open -R`; Linux `xdg-open <parent>`. Open in default app: Windows `explorer <path>`; macOS `open`; Linux `xdg-open`. No new crate for these.
- Keyboard F2 / Ctrl+D / Delete / Enter act on the browser only while it has focus (last click inside it, no text field focused); the editor's own Delete shortcut (delete entity) must not fire then.
- Errors go to the status bar (`set_status_error`) and `tracing::warn!`. Colours and sizes from theme tokens. No placeholder controls. Never call egui `Response` / `Context` methods inside `input` closures.
- `just gate` must pass before every commit (never chain a commit after a failing gate); Conventional Commits; no co-author or tool credit lines.

## Review Focus

1. Rename or move a model used by the open scene: its entities keep drawing and keep their handle; the next save writes the new path and the same GUID. Test: `rename_keeps_handle_path_and_guid` (Task 1).
2. Name clashes that differ only in case on Windows (`Cube.glb` next to `cube.glb`) and invalid names: rejected with a message, nothing changes on disk. Tests: `rename_rejects_case_insensitive_clash`, `validate_name_rejects_bad_names` (Task 1).
3. Moving a folder into itself or its own subfolder, or onto a folder that already has an item with that name: rejected, nothing moves. Test: `move_rejects_into_itself_and_clash` (Task 1).
4. Delete pressed while the browser has focus must not also delete the selected entity, and Delete with the Scene focused must not delete a file. Test: `delete_key_goes_to_the_focused_panel` (Task 3).
5. The `.meta` rename fails after the file was renamed (file locked): the file is renamed back, the error reported. Test: `rename_rolls_back_when_meta_is_locked` (Task 1, `#[cfg(windows)]`, lock with `OpenOptionsExt::share_mode(0)`).

---

### Task 1: `asset_ops` (disk work, no UI)

**Files:**
- Create: `crates/melon_editor/src/asset_ops.rs`
- Modify: `crates/melon_editor/src/lib.rs` (`pub mod asset_ops;`)
- Modify: `crates/melon_editor/src/asset_db.rs` (nothing public changes; `reconcile` is reused)
- Modify: `Cargo.toml` (`trash = "5"` in `[workspace.dependencies]`), `crates/melon_editor/Cargo.toml` (`trash.workspace = true`)

**Interfaces:**
- Consumes: `asset_db::reconcile(&mut AssetServer)`, `AssetServer::{rename_path, guid_index, root}`, `melon_core::meta_path`, `melon_core::assets::project_relative`.
- Produces (all `anyhow::Result`; paths absolute; `server.root()` is the project root):
  - `pub fn validate_name(name: &str) -> anyhow::Result<()>` (rules in Global Constraints).
  - `pub fn unique_path(folder: &Path, name: &str) -> PathBuf`: `name`, else `stem 2.ext`, `stem 3.ext`, ... (case-insensitive existence check).
  - `pub fn create_folder(server: &mut AssetServer, parent: &Path) -> anyhow::Result<PathBuf>` ("New Folder", unique).
  - `pub fn rename(server: &mut AssetServer, path: &Path, new_name: &str) -> anyhow::Result<PathBuf>`: validates; renames the file then its `.meta` (rolls the file back if the `.meta` rename fails); folders rename in one step; then `rename_prefix` and `reconcile`.
  - `pub fn move_to(server: &mut AssetServer, path: &Path, folder: &Path) -> anyhow::Result<PathBuf>` (one item: the browser selects one item at a time; the spec's `paths` list comes with multi-select): same folder → `Ok(path)` unchanged; rejects a folder into itself / its subfolder and a name clash in the target; moves file + `.meta` (rollback as `rename`); then `rename_prefix` and `reconcile`.
  - `pub fn duplicate(server: &mut AssetServer, path: &Path) -> anyhow::Result<PathBuf>`: copies the file (or the folder recursively, `.meta` files included) to `unique_path`, then `reconcile` (which gives the copies new GUIDs because the server already maps the originals' paths).
  - `pub fn delete(server: &mut AssetServer, path: &Path) -> anyhow::Result<()>`: `trash::delete_all` of the item and (for files) its `.meta`, then `reconcile`. Implemented over `fn delete_with(server, path, remove: impl FnOnce(&[PathBuf]) -> anyhow::Result<()>)` so tests remove with `std::fs` and never touch the user's Recycle Bin.
  - `pub fn import(server: &mut AssetServer, files: &[PathBuf], folder: &Path) -> Vec<anyhow::Result<PathBuf>>`: copies each OS file or folder (recursively) to `unique_path`; skips source `.meta` files; then one `reconcile`.
  - `fn rename_prefix(server: &mut AssetServer, old: &Path, new: &Path)`: `rename_path` for the file, or for every indexed asset under a renamed folder (so a case-only rename, which `reconcile` cannot see on Windows, keeps handles).
  - `pub fn reveal(path: &Path)` and `pub fn open_external(path: &Path)` (commands in Global Constraints; spawn, do not wait; log failures). Not unit-tested.

- [ ] **Step 1: Write the failing tests** (temp project root with `assets/`, `AssetServer::new(root)` + `reconcile`; helper writes an asset file)

```rust
#[test]
fn validate_name_rejects_bad_names() {
    for bad in ["", "  ", "a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a.", "a ", ".hidden", "x.glb.meta"] {
        assert!(validate_name(bad).is_err(), "{bad:?}");
    }
    assert!(validate_name("Crate 2.glb").is_ok());
}

#[test]
fn unique_path_counts_up() { /* a.glb, "a 2.glb" exist -> "a 3.glb"; "New Folder" exists -> "New Folder 2" */ }

#[test]
fn create_folder_names_new_folder_uniquely() { /* twice -> "New Folder", "New Folder 2" */ }

#[test]
fn rename_keeps_handle_path_and_guid() {
    // assets/models/a.glb (+ meta g); let h: Handle<Model> = server.load("assets/models/a.glb#mesh1");
    let new = rename(&mut server, &a, "b.glb").unwrap();
    assert_eq!(server.path(h), "assets/models/b.glb#mesh1");
    assert_eq!(server.guid_of("assets/models/b.glb"), Some(g));
    assert!(meta_path(&new).exists() && !meta_path(&a).exists());
}

#[test]
fn rename_case_only_keeps_handle() { /* a.glb -> A.glb: server.path(h) == "assets/models/A.glb" */ }

#[test]
fn rename_rejects_case_insensitive_clash() {
    // a.glb and b.glb ; rename(b, "A.GLB") is Err ; both files and metas unchanged
}

#[test]
fn rename_folder_moves_inner_handles() {
    // assets/models/sub/x.glb ; handle h ; rename(sub, "parts") -> server.path(h) == "assets/models/parts/x.glb"
}

#[test]
fn move_rejects_into_itself_and_clash() {
    // folders a/ a/b/ ; move_to(a, a) Err ; move_to(a, a/b) Err
    // x.glb in root and in c/ ; move_to(root/x.glb, c) Err ; nothing moved
}

#[test]
fn move_carries_meta_and_keeps_guid() { /* x.glb -> c/ ; guid_of("assets/c/x.glb") == Some(g) */ }

#[test]
fn duplicate_gets_new_guid_original_keeps_its_own() {
    // x.glb (g) -> "x 2.glb" exists with a .meta whose guid != g ; guid_of(x.glb) == Some(g)
}

#[test]
fn duplicate_folder_gives_inner_copies_new_guids() { /* d/x.glb (g) -> "d 2/x.glb" guid != g */ }

#[test]
fn delete_removes_file_and_meta() {
    // delete_with(.., |paths| { for p in paths { std::fs::remove_file(p)? } Ok(()) })
    // file and meta gone ; server.guid_of(..) == None
}

#[test]
fn import_copies_with_unique_names_and_metas() {
    // outside dir: a.glb, a.glb.meta ; folder has a.glb already
    // import -> "a 2.glb" with a fresh .meta ; the outside .meta is not copied as an asset
}

#[cfg(windows)]
#[test]
fn rename_rolls_back_when_meta_is_locked() {
    use std::os::windows::fs::OpenOptionsExt;
    // hold `std::fs::OpenOptions::new().read(true).share_mode(0).open(meta_path(&a))`
    // rename(a, "b.glb") is Err ; a.glb still exists ; b.glb does not
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- asset_ops::`
Expected: compile errors (functions not found).

- [ ] **Step 3: Add the `trash` dependency and implement the functions in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib -- asset_ops::`
Expected: all pass.

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add Cargo.toml Cargo.lock crates/melon_editor/Cargo.toml crates/melon_editor/src/asset_ops.rs crates/melon_editor/src/lib.rs
git commit -m "feat(editor): asset file operations that carry .meta files"
```

---

### Task 2: References and the `AssetDrag` payload

**Files:**
- Modify: `crates/melon_editor/src/asset_ops.rs` (references)
- Modify: `crates/melon_editor/src/content.rs` (`AssetDrag`)
- Modify: `crates/melon_editor/src/content_view.rs` (every item is a drag source carrying `AssetDrag`)
- Modify: `crates/melon_editor/src/lib.rs` (Scene drop at the `dnd_release_payload::<String>()` site and the Hierarchy drop site read `AssetDrag` and use `model` when it is `Some`; update the tests that set a `String` payload)

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, PartialEq)] pub struct AssetDrag { pub path: PathBuf, pub model: Option<String> }` in `content.rs`: `path` absolute; `model` = project-relative model path (`assets/models/x.glb`) for `.glb` / `.gltf`, else `None`.
  - `pub fn reference_needles(server: &AssetServer, target: &Path) -> Vec<String>`: for a file, its GUID string (if any) and its project-relative path; for a folder, the same for every file under it.
  - `pub fn scene_references(project_root: &Path, needles: &[String]) -> Vec<PathBuf>`: every `.ron` under the project root (skip `target`, `.git`, `.melon`, and names starting with `.`) whose text contains any needle; sorted.
  - `pub fn open_scene_uses(world: &mut World, server: &AssetServer, target: &Path) -> bool`: some entity's `Handle<Model>` path (fragment stripped) is the target file or under the target folder.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn scene_references_find_guid_and_path() {
    // project/assets/models/a.glb (g); project/assets/scenes/one.ron contains g ;
    // project/two.ron contains "assets/models/a.glb" ; project/target/x.ron contains g (skipped)
    let needles = reference_needles(&server, &a);
    assert_eq!(scene_references(&root, &needles), [root.join("assets/scenes/one.ron"), root.join("two.ron")]);
}

#[test]
fn open_scene_uses_sees_fragments_and_folders() {
    // entity with Handle for "assets/models/sub/a.glb#mesh1"
    assert!(open_scene_uses(&mut world, &server, &root.join("assets/models/sub/a.glb")));
    assert!(open_scene_uses(&mut world, &server, &root.join("assets/models/sub")));
    assert!(!open_scene_uses(&mut world, &server, &root.join("assets/models/other.glb")));
}
```

Plus: the existing Scene / Hierarchy drop tests in `lib.rs` switch their payload to `AssetDrag { path, model: Some(..) }` and still pass; add `non_model_asset_drag_spawns_nothing` (payload with `model: None` dropped on the Hierarchy spawns no entity).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- asset_ops:: non_model_asset_drag`
Expected: compile errors (`scene_references`, `AssetDrag` not found).

- [ ] **Step 3: Implement the items in Interfaces and switch the payload type**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add crates/melon_editor
git commit -m "feat(editor): asset reference search and one drag payload for browser items"
```

---

### Task 3: Browser operations UI

**Files:**
- Modify: `crates/melon_editor/src/content.rs` (requests, focus, rename state)
- Modify: `crates/melon_editor/src/content_view.rs` (menus, inline rename, keys, folder drop targets)
- Modify: `crates/melon_editor/src/lib.rs` (`apply_content_requests`, delete modal, shortcut gating)

**Interfaces:**
- Consumes: Task 1 ops, Task 2 `AssetDrag`, `reference_needles`, `scene_references`, `open_scene_uses`.
- Produces:
  - `pub enum BrowserRequest { NewFolder(PathBuf), Rename(PathBuf, String), Duplicate(PathBuf), AskDelete(PathBuf), Move(PathBuf, PathBuf), Import(Vec<PathBuf>), Reveal(PathBuf), Open(PathBuf) }` (`Open` is handled in Task 4; until then it opens folders only).
  - `ContentBrowser::take_requests(&mut self) -> Vec<BrowserRequest>`; `pub fn has_focus(&self) -> bool`; `pub fn start_rename(&mut self, path: PathBuf)`.
  - `Editor::apply_content_requests(&mut self, world: &mut World)`: runs each request through `asset_ops` with the world's `AssetServer`, refreshes the browser, selects the result (`NewFolder` also starts an inline rename on it), reports errors on the status bar. `AskDelete` computes references and opens the delete modal; its Delete button calls `asset_ops::delete`.
  - Called in `EditorTabs::ui` right after `self.editor.content.ui(ui)`.

UI:
- Item right-click: Open, Rename (F2), Duplicate (Ctrl+D), Delete (Del), Show in Explorer. Empty-space right-click (and tree-row right-click, next to the Favorites entries): New Folder, Show in Explorer.
- Inline rename: a `TextEdit` in place of the name, focused, text pre-selected without the extension; Enter or focus lost commits (`Rename`), Esc cancels. An unchanged name sends nothing.
- Focus: set when a click lands inside the browser, cleared by a click outside it. Keys F2 / Ctrl+D / Delete / Enter act on the selection only with focus and no text field focused.
- `Editor::handle_shortcuts`: skip `Shortcut::Delete` while `self.content.has_focus()`.
- Drop targets: folder tiles / rows and tree rows accept an `AssetDrag` release → `Move(drag.path, folder)` (ignore a drop on itself). Hovering a valid target paints `ACCENT_SOFT`.
- Delete modal (`egui::Modal`): title "Delete <name>?", text "Moves it to the Recycle Bin.", then "Used by:" and each referencing scene (project-relative) plus "the open scene" when `open_scene_uses`; buttons Delete / Cancel; Esc cancels.

- [ ] **Step 1: Write the failing tests** (headless egui, as in `content_view.rs` tests; editor-level ones in `lib.rs` tests)

```rust
#[test]
fn f2_starts_rename_and_enter_requests_it() {
    // select a.glb, click inside browser (focus), press F2, type "b", Enter
    assert_eq!(browser.take_requests(), [BrowserRequest::Rename(a, "b.glb".into())]);
}

#[test]
fn escape_cancels_rename() { /* F2 then Esc -> no requests */ }

#[test]
fn keys_do_nothing_without_focus() { /* select, click outside the browser, press Delete -> no requests */ }

#[test]
fn dropping_an_item_on_a_folder_requests_a_move() {
    // set AssetDrag payload for x.glb, release over ("content_item", root/c)
    assert_eq!(browser.take_requests(), [BrowserRequest::Move(x, root.join("c"))]);
}

#[test]
fn delete_key_goes_to_the_focused_panel() {
    // lib.rs: entity selected AND a file selected in the browser with browser focus;
    // press Delete -> entity still alive, a delete modal is open for the file.
    // Then focus the Scene (click the viewport), press Delete -> entity marked for delete, file untouched.
}

#[test]
fn new_folder_request_creates_and_starts_rename() {
    // editor.apply_content_requests with NewFolder(assets) -> assets/New Folder exists, browser renaming it
}
```

`BrowserRequest` derives `Debug, PartialEq` for these assertions.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- content_view:: delete_key_goes new_folder_request`
Expected: compile errors (`take_requests`, `BrowserRequest` not found).

- [ ] **Step 3: Implement the items in Interfaces and the UI list above**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add crates/melon_editor
git commit -m "feat(editor): content browser create, rename, duplicate, move and delete"
```

---

### Task 4: Opening files and importing from the OS

**Files:**
- Modify: `crates/melon_editor/src/lib.rs` (open-scene modal, Save As directory, dropped files)
- Modify: `crates/melon_editor/src/content_view.rs` (double-click / Enter on files → `Open`)
- Modify: `README.md` (Content Browser controls), `AGENTS.md` (layout table: `asset_ops.rs`; gotcha: scenes live in `assets/scenes`, browser ops go through `asset_ops`)

**Interfaces:**
- Consumes: `BrowserRequest::Open`, `Import`; `asset_ops::{open_external, import}`; `dialog_operation`; `PendingOperation::{Save, Load}`.
- Produces:
  - `pub fn is_scene_path(project_root: &Path, path: &Path) -> bool`: `.ron` (case-insensitive) under `<root>/assets`.
  - `Editor` field `confirm_open: Option<PathBuf>` and its modal: text "Open <name>? Unsaved changes in the current scene are lost."; buttons Save (enabled only with a current scene; disabled hover text "Use File ▸ Save As first"), Don't Save, Cancel. Save → `scene::save_scene` to the current scene (status error and stop on failure), then `PendingOperation::Load`; Don't Save → `PendingOperation::Load`. Both through `dialog_operation(false, path, sim_state)`; `None` → status "Stop playing to open a scene".
  - `Open(path)`: folder → browser `open`; scene → `confirm_open`; other file → `asset_ops::open_external`.
  - Save As (`open_dialog(true)`): `create_dir_all(<root>/assets/scenes)`, then `rfd::FileDialog::set_directory` to it.
  - OS drops: `egui-winit` already puts winit `DroppedFile` into `RawInput::dropped_files`. In `Editor::build_ui`, read them (`ui.input(|i| i.raw.dropped_files.clone())`) and, if the Content Browser tab was drawn this frame, queue `Import(paths)` into the browser's current folder; otherwise status "Open the Content Browser to import files". (Ruling for the spec's "dropped over the browser": winit gives no drop position on Windows, so the visible browser takes the drop.)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn is_scene_path_only_ron_under_assets() {
    assert!(is_scene_path(&root, &root.join("assets/scenes/a.RON")));
    assert!(!is_scene_path(&root, &root.join("config/input.ron")));
    assert!(!is_scene_path(&root, &root.join("assets/models/a.glb")));
}

#[test]
fn opening_a_scene_asks_first_and_dont_save_loads() {
    // editor.apply_content_requests(Open(scene)) -> confirm_open == Some(scene), no pending operation
    // click "Don't Save" in the modal (headless frame) -> take_pending_operation() == Some(Load(scene))
}

#[test]
fn opening_a_scene_while_playing_is_refused() {
    // SimulationState::Playing ; Open(scene) then Don't Save -> no pending operation, status mentions "Stop playing"
}

#[test]
fn save_button_disabled_without_current_scene() {
    // confirm_open set, current_scene None -> read_response(Id::new("open_scene_save")).enabled() == false
}

#[test]
fn dropped_files_import_into_current_folder() {
    // browser tab drawn ; RawInput.dropped_files = [outside/a.glb] -> assets/a.glb exists with a .meta
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- is_scene_path opening_a_scene save_button_disabled dropped_files_import`
Expected: compile errors / failures (functions and modal missing).

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Check in the dev build**

Run: `cargo run -p melon_host`.
Expected: right-click New Folder (inline rename), F2 rename, Ctrl+D duplicate, drag a file onto a folder, Delete shows the modal with "Used by", the item lands in the Recycle Bin with its `.meta`; double-click a `.ron` in `assets/scenes` asks, then loads; double-click `README.md` opens it in the default app; Show in Explorer selects the file; drag a `.glb` from Explorer onto the window copies it into the current folder with a `.meta`; File ▸ Save As starts in `assets/scenes`.

- [ ] **Step 6: Docs, full test run, commit**

```powershell
just test
git add crates/melon_editor README.md AGENTS.md
git commit -m "feat(editor): open scenes and files from the content browser, import OS drops"
```
