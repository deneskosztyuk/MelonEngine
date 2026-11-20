# Content Browser M2: Browser Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the one-folder list into a Content Browser with a folder tree, a tile/list asset view with a size slider, a clickable path bar, Back / Forward, name search, a type filter and Favorites, saved per project.

**Architecture:** `content.rs` keeps the headless state and logic (`ContentBrowser`: listing, kinds, history, search, filter, favorites, settings). A new `content_view.rs` holds only egui drawing (`ContentBrowser::ui`), so M6 can draw the same browser in the drawer. Settings live in `.melon/content_browser.ron`, loaded by `Editor::set_project_root` and saved with the dock layout.

**Tech Stack:** Rust 2024, egui 0.35 (`SidePanel::show_inside`, `dnd_drag_source`, `PointerButton::Extra1/Extra2`), serde + ron 0.8.

**Spec:** `docs/superpowers/specs/2026-11-20-content-browser-design.md` (section M2, plus Constraints and Anti-goals).

## Global Constraints

- Colours and sizes come from `theme::colors` / `theme::sizes`, not literals. Every button and menu item does something (no placeholders).
- Never call egui `Response` / `Context` methods inside `ctx.input` / `ui.input` closures: read input into locals first.
- `.meta` files are never listed (already true in `list_dir`).
- Type filter kinds and extensions (case-insensitive): Model `glb`, `gltf`; Texture `png`, `jpg`, `jpeg`, `hdr`; Shader `wgsl`; Scene `ron`; Other (anything else).
- Search: case-insensitive name match in the current folder and all its subfolders; results show their folder.
- Back / Forward also on mouse buttons 4 / 5 (`PointerButton::Extra1` / `Extra2`) while the pointer is over the browser.
- Saved in `.melon/content_browser.ron`: favorites, view size, last folder, split width. A missing or broken file means the defaults. Writes are atomic (temp + rename, as `layout::save_layout`).
- Navigation never leaves the root (`assets/`).
- Model files stay drag sources carrying their project-relative path (`String` payload), in both views and in search results.
- `just gate` before every commit; Conventional Commits; no co-author or tool credit lines.

## Review Focus

1. The current folder (or a folder in history) is deleted or renamed outside the editor: Refresh and Back / Forward land on the nearest existing folder instead of an empty broken view. Tests: `refresh_falls_back_to_existing_ancestor`, `back_skips_missing_folders` (Task 2).
2. A broken or hand-edited `content_browser.ron`, or favorites that no longer exist or point outside `assets/`: defaults, missing favorites dropped, never a panic. Tests: `settings_broken_file_gives_defaults`, `settings_drop_missing_and_outside_favorites` (Task 3).
3. Every filter chip turned off, or a search with no hits: the view says "No assets match" instead of the "Empty folder" text. Test: `visible_entries_all_filters_off_is_empty` (Task 1) plus the empty-state branch in Task 4.
4. Opening the folder you are already in, or clicking the last path segment, adds no history entry (Back would otherwise do nothing). Test: `open_same_folder_adds_no_history` (Task 2).
5. Search walks subfolders without following symlinks / junctions (no loop on a junction pointing at a parent). Rule in Task 1: walk with `DirEntry::file_type()`, which does not follow links.

---

### Task 1: Asset kinds, filter and search

**Files:**
- Modify: `crates/melon_editor/src/content.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)] pub enum AssetKind { Folder, Model, Texture, Shader, Scene, Other }`
  - `AssetKind::FILE_KINDS: [AssetKind; 5]` (all but `Folder`, in that order) and `AssetKind::label(self) -> &'static str` ("Folder", "Model", "Texture", "Shader", "Scene", "Other").
  - `pub fn asset_kind(path: &Path, is_dir: bool) -> AssetKind` (extensions from Global Constraints).
  - `ContentEntry` gains `pub kind: AssetKind` (set by `list_dir`).
  - `ContentBrowser` fields `search: String`, `filter: HashSet<AssetKind>` (default: all `FILE_KINDS`), cached `visible: Vec<ContentEntry>`.
  - `pub fn set_search(&mut self, text: String)`, `pub fn search(&self) -> &str`, `pub fn toggle_kind(&mut self, kind: AssetKind)`, `pub fn kind_enabled(&self, kind: AssetKind) -> bool`.
  - `pub fn visible_entries(&self) -> &[ContentEntry]`: empty search → the current folder's folders (always) then its files whose kind is enabled; non-empty search → files only, under the current folder recursively, name contains the search (case-insensitive), kind enabled, sorted by lower-case name then path. Recomputed only by `refresh`, `open`, `set_search`, `toggle_kind` (not every frame).
  - `fn walk_files(dir: &Path, out: &mut Vec<ContentEntry>)`: recursive via `list_dir`, recursing only where `DirEntry::file_type().is_dir()` (no symlink following; change `list_dir`'s `is_dir` to use `file_type()` too).

- [ ] **Step 1: Write the failing tests** (in `content.rs` tests; build trees with `temp_dir`)

```rust
#[test]
fn asset_kind_by_extension_case_insensitive() {
    assert_eq!(asset_kind(Path::new("a/B.GLB"), false), AssetKind::Model);
    assert_eq!(asset_kind(Path::new("a/b.gltf"), false), AssetKind::Model);
    assert_eq!(asset_kind(Path::new("t.JPeG"), false), AssetKind::Texture);
    assert_eq!(asset_kind(Path::new("s.wgsl"), false), AssetKind::Shader);
    assert_eq!(asset_kind(Path::new("level.ron"), false), AssetKind::Scene);
    assert_eq!(asset_kind(Path::new("README.md"), false), AssetKind::Other);
    assert_eq!(asset_kind(Path::new("models"), true), AssetKind::Folder);
}

#[test]
fn filter_hides_files_but_keeps_folders() {
    // root: models/ (dir), a.glb, b.png
    browser.toggle_kind(AssetKind::Model);
    let names: Vec<_> = browser.visible_entries().iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, ["models", "b.png"]);
}

#[test]
fn search_is_recursive_case_insensitive_files_only() {
    // root: Crate.glb, models/sub/crate_big.glb, models/crates/ (dir), notes.md
    browser.set_search("CRATE".into());
    let names: Vec<_> = browser.visible_entries().iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, ["Crate.glb", "crate_big.glb"]);
}

#[test]
fn search_respects_filter_and_hides_meta() {
    // root: a.glb, a.glb.meta, a.png ; filter Model off ; search "a"
    assert_eq!(names, ["a.png"]);
}

#[test]
fn visible_entries_all_filters_off_is_empty() {
    // root: models/ (dir), a.glb ; turn off all FILE_KINDS, search "a"
    assert!(browser.visible_entries().is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- content::`
Expected: compile errors (`AssetKind`, `asset_kind`, `set_search` not found).

- [ ] **Step 3: Implement the items in Interfaces** (the current `ui` keeps compiling: switch it to `visible_entries()`)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib -- content::`
Expected: all pass.

- [ ] **Step 5: Gate and commit**

```powershell
just gate
git add crates/melon_editor/src/content.rs
git commit -m "feat(editor): content browser asset kinds, type filter and search"
```

---

### Task 2: History and safe navigation

**Files:**
- Modify: `crates/melon_editor/src/content.rs`

**Interfaces:**
- Consumes: Task 1 `refresh` recompute.
- Produces (on `ContentBrowser`; `back: Vec<PathBuf>`, `forward: Vec<PathBuf>`, `selected: Option<PathBuf>`):
  - `open(&mut self, dir: PathBuf)` (existing name): ignored outside the root or if `dir == current`; otherwise pushes `current` onto `back`, clears `forward`, clears `search` and `selected`, refreshes.
  - `up(&mut self)` (existing): same as `open(parent)`.
  - `pub fn go_back(&mut self)` / `pub fn go_forward(&mut self)`: pop entries that no longer exist as folders; move `current` between the stacks; refresh. No-op on an empty stack.
  - `pub fn can_go_back(&self) -> bool` / `pub fn can_go_forward(&self) -> bool`.
  - `refresh(&mut self)`: if `current` no longer exists, walk up to the nearest existing ancestor inside the root (the root itself if needed), then list.
  - `pub fn root(&self) -> &Path`, `pub fn path_segments(&self) -> Vec<(String, PathBuf)>`: root name then each folder down to `current`.
  - `pub fn select(&mut self, path: Option<PathBuf>)`, `pub fn selected(&self) -> Option<&Path>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn back_and_forward_walk_history() {
    // root/a/b ; open(a), open(a/b), go_back -> a, go_back -> root,
    // go_forward -> a, can_go_forward() == true; open(root) clears forward.
    assert!(!browser.can_go_forward());
}

#[test]
fn open_same_folder_adds_no_history() {
    browser.open(root.join("a"));
    browser.open(root.join("a"));
    browser.go_back();
    assert_eq!(browser.current(), root);
    assert!(!browser.can_go_back());
}

#[test]
fn back_skips_missing_folders() {
    // open(a), open(b), open(c); delete a/ and b/ on disk except root; go_back from c
    // b is gone -> skipped, a is gone -> skipped, lands on root
    assert_eq!(browser.current(), root);
}

#[test]
fn refresh_falls_back_to_existing_ancestor() {
    // open(a/b/c); remove_dir_all(a/b); refresh
    assert_eq!(browser.current(), root.join("a"));
}

#[test]
fn path_segments_from_root_to_current() {
    browser.open(root.join("a/b"));
    let names: Vec<_> = browser.path_segments().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, [root_name, "a".to_string(), "b".to_string()]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- content::`
Expected: compile errors (`go_back`, `path_segments` not found).

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib -- content::`
Expected: all pass, including `navigation_stays_inside_root`.

- [ ] **Step 5: Gate and commit**

```powershell
just gate
git add crates/melon_editor/src/content.rs
git commit -m "feat(editor): content browser back/forward history"
```

---

### Task 3: Favorites and saved settings

**Files:**
- Modify: `crates/melon_editor/src/content.rs`
- Modify: `crates/melon_editor/src/lib.rs` (`set_project_root` loads, `save_layout` also saves the browser; field `content_settings_file: PathBuf`)
- Modify: `crates/melon_editor/src/theme.rs` (`sizes::CONTENT_TREE: f32 = 200.0`, `sizes::TILE_MIN: f32 = 32.0`, `sizes::TILE_DEFAULT: f32 = 72.0`, `sizes::TILE_MAX: f32 = 128.0`, `sizes::TILE_LIST_BELOW: f32 = 48.0`)

**Interfaces:**
- Produces:
  - `pub fn settings_path() -> PathBuf` → `.melon/content_browser.ron`.
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct ContentSettings { version: u32, favorites: Vec<String>, tile_size: f32, last_folder: Option<String>, tree_width: f32 }`, `version = 1`; folder fields are root-relative with `/` (`""` = root). `Default`: no favorites, `TILE_DEFAULT`, `None`, `CONTENT_TREE`.
  - `ContentBrowser` fields `favorites: Vec<PathBuf>`, `tile_size: f32`, `tree_width: f32`.
  - `pub fn add_favorite(&mut self, dir: PathBuf)` (folders inside the root only, no duplicates), `pub fn remove_favorite(&mut self, dir: &Path)`, `pub fn favorites(&self) -> &[PathBuf]`.
  - `pub fn is_list(&self) -> bool` → `tile_size < TILE_LIST_BELOW`; `pub fn tile_size_mut(&mut self) -> &mut f32` (slider binds here; clamp to `TILE_MIN..=TILE_MAX` on save/load).
  - `pub fn load_settings(&mut self, file: &Path)`: missing / unreadable / unparsable / other version → defaults (broken file logs `tracing::warn!`). Favorites and `last_folder` that do not exist as folders under the root, or contain `..`, are dropped; `last_folder` that survives is opened without a history entry.
  - `pub fn save_settings(&self, file: &Path) -> anyhow::Result<()>`: atomic, creates `.melon/`.
  - `Editor::set_project_root` calls `load_settings(root.join(content::settings_path()))`; `Editor::save_layout` also calls `save_settings` (warn on error).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn settings_round_trip() {
    // root/a ; add_favorite(a) ; *tile_size_mut() = 100.0 ; open(a) ; tree_width = 250.0
    // save_settings(file) ; fresh ContentBrowser::new(root) ; load_settings(file)
    assert_eq!(fresh.favorites(), [root.join("a")]);
    assert_eq!(fresh.current(), root.join("a"));
    assert!(!fresh.can_go_back());
    assert!(!fresh.is_list());
}

#[test]
fn settings_broken_file_gives_defaults() {
    std::fs::write(&file, "(((not ron").unwrap();
    browser.load_settings(&file);
    assert!(browser.favorites().is_empty());
    assert_eq!(browser.current(), root);
}

#[test]
fn settings_drop_missing_and_outside_favorites() {
    // file with favorites ["a", "gone", "../outside"], last_folder Some("gone")
    assert_eq!(browser.favorites(), [root.join("a")]);
    assert_eq!(browser.current(), root);
}

#[test]
fn add_favorite_ignores_duplicates_and_files() {
    browser.add_favorite(root.join("a"));
    browser.add_favorite(root.join("a"));
    browser.add_favorite(root.join("x.glb"));
    assert_eq!(browser.favorites(), [root.join("a")]);
}
```

Also extend the existing `set_project_root` test in `lib.rs` (the one asserting `editor.content.current()`): write a `.melon/content_browser.ron` with `last_folder: Some("models")` first and assert `current()` is `root.join("assets/models")`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- content:: set_project_root`
Expected: compile errors (`load_settings`, `add_favorite` not found).

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Gate and commit**

```powershell
just gate
git add crates/melon_editor/src/content.rs crates/melon_editor/src/lib.rs crates/melon_editor/src/theme.rs
git commit -m "feat(editor): content browser favorites and saved settings"
```

---

### Task 4: Browser UI (tree, path bar, tiles / list)

**Files:**
- Create: `crates/melon_editor/src/content_view.rs` (move `ContentBrowser::ui` here from `content.rs`)
- Modify: `crates/melon_editor/src/lib.rs` (`mod content_view;`)
- Modify: `README.md` (Editor controls: Content Browser line), `AGENTS.md` (layout table: `content.rs` state, `content_view.rs` drawing)

**Interfaces:**
- Consumes: everything from Tasks 1-3.
- Produces: `impl ContentBrowser { pub fn ui(&mut self, ui: &mut egui::Ui) }` (same call site in `EditorTabs::ui`). Widget ids the test reads: `egui::Id::new(("content_crumb", index))` for path-bar buttons, `egui::Id::new(("content_item", &path))` for each item.

Layout, top to bottom:
- Toolbar row: Back, Forward (enabled per `can_go_*`), Up, Refresh, path bar (one small button per `path_segments()` entry, `/` separators), then right-aligned: search box (hint "Search"), filter chips (one `selectable_label` per `FILE_KINDS`, toggled via `toggle_kind`), size slider (`TILE_MIN..=TILE_MAX` on `tile_size_mut`).
- Body: `egui::SidePanel::left("content_tree").resizable(true).default_width(tree_width).show_inside(ui, ..)`; store the panel's width back into `tree_width`. Tree: Favorites section first (each favorite a row; right-click Remove from Favorites), then the root as a collapsing tree of folders only (expand state per path in egui memory; children listed through a `HashMap<PathBuf, Vec<PathBuf>>` cache cleared by `refresh`). Click a folder row → `open`. Right-click a folder → Add to Favorites / Remove from Favorites. The current folder row uses `ACCENT_SOFT`.
- Asset view (rest of the space, vertical scroll): `is_list()` → one row per entry (icon glyph, name, kind label, size; search results also a muted root-relative folder); else a wrapping grid of `tile_size` squares (glyph centred, name below, truncated with hover text). Single click → `select`; selected item uses `ACCENT_SOFT`. Double click on a folder → `open`. Model files wrap their item in `ui.dnd_drag_source(id, project_relative_path, ..)` as today.
- Empty states: listing empty and no search and all filters on → "Empty folder"; otherwise → "No assets match".
- Mouse 4 / 5: before drawing, read `let (back, fwd) = ui.input(|i| (i.pointer.button_pressed(PointerButton::Extra1), i.pointer.button_pressed(PointerButton::Extra2)));`, then outside the closure, if `ui.rect_contains_pointer(ui.max_rect())`, call `go_back` / `go_forward`.

- [ ] **Step 1: Write the failing test** (in `content_view.rs` tests, using a headless `egui::Context` like `hierarchy_frame` in `lib.rs` tests: run a frame, read a widget rect with `ctx.read_response`, then run a frame with a click there)

```rust
#[test]
fn clicking_a_path_segment_opens_that_folder() {
    // root/a/b ; browser.open(a/b) ; frame 1 draws ; click centre of ("content_crumb", 1)
    assert_eq!(browser.current(), root.join("a"));
}

#[test]
fn mouse_back_button_goes_back() {
    // open(a) ; frame with PointerButton::Extra1 pressed over the browser
    assert_eq!(browser.current(), root);
}

#[test]
fn double_click_folder_tile_opens_it() {
    // root/a ; double click centre of ("content_item", root/a)
    assert_eq!(browser.current(), root.join("a"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- content_view::`
Expected: compile error (module or ids missing).

- [ ] **Step 3: Implement `content_view.rs` as described; any size or colour it needs beyond the existing tokens goes into `theme::sizes` / `theme::colors`**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Check in the dev build**

Run: `cargo run -p melon_host`.
Expected: tree on the left with Favorites, path bar segments jump, Back / Forward and mouse 4 / 5 work, slider turns tiles into a list at the left end, search finds `cube.glb` from `assets`, filter chips hide files, dragging `cube.glb` from a tile into the Scene still spawns it, no `.meta` shown. Close and reopen: favorites, size, folder and split width are restored.

- [ ] **Step 6: Docs, full test run, commit**

```powershell
just test
git add crates/melon_editor README.md AGENTS.md
git commit -m "feat(editor): content browser tree, path bar and tile/list view"
```
