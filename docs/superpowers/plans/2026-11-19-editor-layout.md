# Editor Layout Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Unity-style egui editor: dockable tabbed panels (Hierarchy, Scene, Inspector, Content Browser, Console), menu bar, toolbar, status bar, the Slint mock-up's theme, plus the cheap real features behind them.

**Architecture:** `egui_dock` 0.20 holds a `DockState<EditorTab>`; an `EditorTabViewer` dispatches each tab to the existing panel builders. New small modules: `theme.rs` (tokens → egui style), `console.rs` (a `tracing` layer feeding a ring buffer), `content.rs` (asset folder listing). Single-step lives in `melon_core` + `play.rs`.

**Tech Stack:** Rust 2024, egui 0.35, egui_dock 0.20 (`serde` feature), tracing-subscriber 0.3, bevy_ecs 0.19.

**Spec:** No separate design doc: the user approved the scope in conversation (2026-11-19) and said to proceed. Scope: ADR-0001 action items 1–4. In: dock layout saved between runs + Window ▸ Reset Layout; menu bar, toolbar, status bar; theme from `crates/melon_editor/ui/theme.slint`; Console from `tracing`; Content Browser listing `assets/`; hierarchy search; Step (one fixed step while paused); remove the Dear ImGui and Slint experiments. Out (next project): Undo/Redo, rename, duplicate. Rule: no control without a working action behind it.

## Global Constraints

- Work on `main`, one commit per task, Conventional Commits, no co-author or tool trailers.
- One new dependency only: `egui_dock = { version = "0.20", features = ["serde"] }` (MIT, approved in ADR-0001).
- Gate per task: clippy `-D warnings` clean, `cargo test --workspace` green, fmt on touched files.
- Theme tokens (from `theme.slint`): chrome #111316, chrome-raised #181b20, surface #1c2026, surface-raised #242932, viewport #343b46, border #303640, border-strong #414956, text #e1e5eb, text-muted #89919d, accent #2f79cf, accent-hover #3e8be2, accent-soft #23476f, success #51b37f, warning #d9a441, error #e05656, info #6cb2e6. Sizes: menu 28, toolbar 38, panel header 30, status 23, spacing 6, radius 3, input height 24, row 26. Default dock widths: left 220, right 260, bottom 200 (points).
- Layout file: `.melon/editor_layout.ron` (already git-ignored).

## Review Focus

1. **Corrupt or old layout file** (a tab renamed, file half-written): the editor must start with the default layout, not panic. Test in Task 6.
2. **All tabs closed / Scene tab closed**: Window menu must reopen each panel; with no Scene tab the viewport target must not be resized to garbage. Task 6 test for reopen; Scene closed keeps the last viewport size.
3. **Shortcuts while typing**: W/E/R, Delete, Ctrl+S, F5 must not fire while a text field (hierarchy search, inspector) has focus. Task 7 test on the shortcut mapper.
4. **Console flooding**: a system logging every frame must not grow memory without bound. Ring buffer capped at 2,000 lines. Task 4 test.
5. **Missing assets folder** (running from another directory): the Content Browser shows an empty state, no panic. Task 5 test.

---

### Task 1: Remove the Dear ImGui and Slint experiments

**Files:** Delete `crates/melon_editor/src/imgui_poc.rs`, `crates/melon_editor/src/slint_poc.rs`, `crates/melon_editor/build.rs`, `crates/melon_host/src/bin/melon_slint_poc.rs`. Keep `crates/melon_editor/ui/theme.slint` until Task 2 has ported it, then delete `ui/` in Task 2. Modify: root `Cargo.toml` (drop `imgui`, `imgui-wgpu`, `imgui-winit-support`, `slint`, `slint-build`), `crates/melon_editor/Cargo.toml` and `crates/melon_host/Cargo.toml` (features, deps, `[[bin]]`), `crates/melon_editor/src/lib.rs` (module declarations), `crates/melon_host/src/main.rs` (every `cfg(feature = "imgui-poc")` branch → the egui path only), `README.md` (drop the imgui-poc run line).

- [ ] **Step 1:** Remove the above. `editor`/`editor_ui` fields stay `Option` but lose their `cfg`.
- [ ] **Step 2:** `cargo build -p melon_host`, `cargo build -p melon_host --release --no-default-features`, clippy, `cargo test --workspace` — all pass; `grep -ri "imgui\|slint" crates Cargo.toml` finds nothing but `ui/theme.slint`.
- [ ] **Step 3: Commit** `chore: remove Dear ImGui and Slint editor experiments`

### Task 2: Theme

**Files:** Create `crates/melon_editor/src/theme.rs`; delete `crates/melon_editor/ui/`.

**Interfaces:** `pub mod colors { pub const CHROME: egui::Color32 = ...; ... }` (all 16 tokens), `pub mod sizes { pub const MENU_HEIGHT: f32 = 28.0; ... }`, `pub fn apply(ctx: &egui::Context)` (dark visuals: panel fill = SURFACE, window fill = SURFACE_RAISED, extreme bg = CHROME, faint bg = CHROME_RAISED, selection = ACCENT_SOFT with ACCENT stroke, hovered widget = ACCENT_HOVER, corner radius 3, item spacing 6, interact height 24, text colour TEXT, weak text TEXT_MUTED, error/warn colours). Called once from `EditorOverlay::new`.

- [ ] **Step 1: Failing tests:** `tokens_match_theme_slint` (spot-check CHROME, ACCENT, ERROR hex), `apply_sets_panel_fill_and_accent` (fresh `egui::Context`, `apply`, read `ctx.style().visuals.panel_fill == SURFACE`, `selection.bg_fill == ACCENT_SOFT`).
- [ ] **Step 2:** Run → fail. **Step 3:** Implement. **Step 4:** Run → pass; screenshot the editor.
- [ ] **Step 5: Commit** `feat(editor): port the Slint theme to egui`

### Task 3: Single-step while paused

**Files:** `crates/melon_core/src/lib.rs` (+ export), `crates/melon_editor/src/play.rs`.

**Interfaces:** `#[derive(Resource, Default)] pub struct StepOnce;` in core. `Core::update` while `Paused` with a `StepOnce` resource: remove it, `elapsed_seconds += fixed_delta`, run `FixedUpdate` exactly once (overstep untouched). `pub fn step(world: &mut World)` in `play.rs`: inserts `StepOnce` only when `Paused`.

- [ ] **Step 1: Failing tests:** core `step_once_runs_one_fixed_step_while_paused` (Paused, insert `StepOnce`, 3 frames → 1 step, elapsed += 1/60, resource gone); `step_once_ignored_while_editing`; play `step_only_when_paused`.
- [ ] **Step 2–4:** Run → fail, implement, run → pass.
- [ ] **Step 5: Commit** `feat: step one fixed frame while paused`

### Task 4: Console from `tracing`

**Files:** Create `crates/melon_editor/src/console.rs`; Modify `crates/melon_editor/Cargo.toml` (`tracing-subscriber.workspace = true`), `crates/melon_host/src/main.rs` (subscriber setup).

**Interfaces:** `#[derive(Clone, Default)] pub struct ConsoleLog(Arc<Mutex<VecDeque<LogLine>>>)` with `CAPACITY = 2000`, `fn push(&self, LogLine)`, `fn snapshot(&self) -> Vec<LogLine>`, `fn clear(&self)`. `pub struct LogLine { pub level: tracing::Level, pub target: String, pub message: String }`. `ConsoleLog::layer(&self) -> impl tracing_subscriber::Layer<S>` records event `message` field + other fields. `pub fn console_ui(ui, log: &ConsoleLog, filter: &mut ConsoleFilter)` with Clear button, level toggles (Error/Warn/Info/Debug), auto-scroll to bottom, level colours from theme. Host: `registry().with(fmt layer filtered by EnvFilter).with(console layer filtered to INFO)`; `Editor::set_console(ConsoleLog)`.

- [ ] **Step 1: Failing tests:** `ring_buffer_caps_at_capacity` (2,500 pushes → 2,000, oldest dropped), `layer_captures_message_and_level` (subscriber with only the layer, `tracing::warn!("hello {}", 1)` via `with_default` → one line, Level::WARN, message "hello 1").
- [ ] **Step 2–4:** fail, implement, pass.
- [ ] **Step 5: Commit** `feat(editor): console panel fed by tracing`

### Task 5: Content Browser

**Files:** Create `crates/melon_editor/src/content.rs`.

**Interfaces:** `pub struct ContentEntry { pub name: String, pub path: PathBuf, pub is_dir: bool }`; `pub fn list_dir(path: &Path) -> Vec<ContentEntry>` (dirs first, then files, each alphabetical, case-insensitive; unreadable/missing → empty); `pub struct ContentBrowser { root: PathBuf, current: PathBuf }` with `ui(&mut self, ui)`: breadcrumb (root-relative, clickable), "Up", Refresh, folder rows open on double-click, file rows show name + size. Root = `assets/` from the host (`Editor::set_assets_root`).

- [ ] **Step 1: Failing tests:** `list_dir_dirs_first_sorted` (temp dir with `b.txt`, `A/`, `a.txt`, `c/` → `A, c, a.txt, b.txt`), `list_dir_missing_is_empty`.
- [ ] **Step 2–4:** fail, implement, pass.
- [ ] **Step 5: Commit** `feat(editor): content browser for the assets folder`

### Task 6: Dock layout

**Files:** Create `crates/melon_editor/src/layout.rs`; Modify root + editor `Cargo.toml` (`egui_dock`), `crates/melon_editor/src/lib.rs` (`build_ui` body, hierarchy search), `crates/melon_host/src/main.rs` (save on close).

**Interfaces:**
- `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)] pub enum EditorTab { Hierarchy, Scene, Inspector, ContentBrowser, Console }` with `title()`.
- `pub fn default_layout() -> egui_dock::DockState<EditorTab>`: Scene centre; Hierarchy left (fraction from 220 of a 1440 reference width); Inspector right (260); bottom Content Browser + Console tabs (200 of 900).
- `pub fn load_layout(path: &Path) -> DockState<EditorTab>` (missing/corrupt → default), `pub fn save_layout(state, path) -> anyhow::Result<()>` (atomic temp + rename).
- `pub fn reopen(state: &mut DockState<EditorTab>, tab: EditorTab)`: focus if open, else push into the main surface's focused leaf.
- `pub fn hierarchy_matches(name: &str, query: &str) -> bool` (case-insensitive substring; empty query matches all).
- `Editor::build_ui`: menu/toolbar/status bars are Task 7; here the body becomes `DockArea::new(&mut self.dock).style(Style::from_egui(ui.style())).show_inside(ui, &mut viewer)` where `viewer` borrows the editor parts + world. Scene tab calls `viewport_panel`; when the Scene tab is not visible, `viewport_size` keeps its last value. Hierarchy gets a search box above the list.
- `Editor::save_layout(&self)`, called by the host on `CloseRequested`.

- [ ] **Step 1: Failing tests:** `default_layout_has_all_tabs`, `layout_roundtrip` (save to temp, load, same tabs), `corrupt_layout_falls_back_to_default` (write `"garbage"`), `reopen_closed_tab` (remove Console, reopen → present), `hierarchy_filter` (`("Ground","gro")` true, `("Ground","x")` false, `("Ground","")` true).
- [ ] **Step 2–4:** fail, implement, pass. Screenshot: five panels docked in the default arrangement.
- [ ] **Step 5: Commit** `feat(editor): dockable tabbed panels with saved layout`

### Task 7: Menu bar, toolbar, status bar, shortcuts

**Files:** `crates/melon_editor/src/lib.rs`, `crates/melon_host/src/main.rs`.

**Interfaces:**
- `PendingOperation::Exit` (host: `event_loop.exit()` after saving the layout).
- `Editor::current_scene_path: Option<PathBuf>` set on successful Save As / Open (host reports success via existing `set_status` path → add `Editor::scene_saved(path)` / `scene_loaded(path)` calls).
- `pub enum Shortcut { Save, Delete, Move, Rotate, Scale, Frame }` and `pub fn shortcut_for(key: egui::Key, ctrl: bool, typing: bool, flying: bool) -> Option<Shortcut>` (Ctrl+S Save; Delete; W/E/R tool modes only when not flying; nothing when typing). F5/F6/Shift+F5 stay in `play_controls`.
- Menu bar (height 28): **File**: Open… (Load dialog), Save (Ctrl+S, enabled when a current path exists, else Save As), Save As…, Exit. **Edit**: Delete (enabled with a selection). **GameObject**: Create Cube. **Window**: Hierarchy / Scene / Inspector / Content Browser / Console (`reopen`), Reset Layout. **Help**: About MelonEngine (small window: version, egui/wgpu/rapier names). File items disabled outside `Editing`, as today.
- Toolbar (height 38): tool modes Move / Rotate / Scale (selectable, accent when active) left; Play / Pause / Step / Stop centred, Step enabled only when Paused; nothing on the right.
- Status bar (height 23): status message (left, success/warn/error colour), state label, entity count, FPS, tool mode (right).
- Remove the old top bar and left panel code.

- [ ] **Step 1: Failing tests:** `ctrl_s_saves`, `shortcuts_blocked_while_typing` (W, Delete, Ctrl+S with typing → None), `tool_keys_ignored_while_flying` (W with flying → None, without → Move).
- [ ] **Step 2–4:** fail, implement, pass. Screenshot the full editor.
- [ ] **Step 5: Commit** `feat(editor): menu bar, toolbar, status bar and shortcuts`

### Task 8: Docs

**Files:** `README.md` (editor layout + shortcuts: W/E/R, Delete, Ctrl+S, F5/F6/Shift+F5, Step), `AGENTS.md` (layout module, tabs via `EditorTab`, theme tokens in `theme.rs`, console via `tracing`), `docs/ROADMAP.md` (tick "Layout redesign"; add "Undo/Redo, rename, duplicate" as next editor item), `docs/adr/0001-editor-ui-library.md` (tick action items).

- [ ] **Step 1:** Update. **Step 2:** tests + clippy. **Step 3: Commit** `docs: editor layout, shortcuts and roadmap`
