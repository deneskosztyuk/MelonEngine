# Content Browser M6: Content Drawer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ctrl+Space and a status-bar "Content Drawer" button show the same `ContentBrowser` in a panel that slides up from the status bar over the dock area, hide it on Esc / Ctrl+Space / a click outside / a drag leaving it, and "Dock in Layout" moves it back into the Content Browser tab.

**Architecture:** A new `melon_editor/src/drawer.rs` holds `DrawerState` and an `impl Editor` block (child module, so it reaches `Editor`'s private fields). `build_ui` draws the drawer as a foreground `egui::Area` above the status bar after the dock, and treats it as a place the browser was shown (file drops, focus, thumbnails, requests). When the Content Browser tab was drawn last frame, the toggle focuses and flashes that tab instead, so one `ContentBrowser` is never drawn twice in a frame.

**Tech Stack:** Rust 2024, egui 0.35 (`Area`, `Order::Foreground`, `Context::animate_bool_with_time`, `DragAndDrop`), egui_dock 0.20.

**Spec:** `docs/superpowers/specs/2026-11-20-content-browser-design.md` (section M6, plus the builder-check ruling on Ctrl+Space with the tab visible).

## Global Constraints

- Toggle: Ctrl+Space (fresh press, not while typing, only with `InputFocus::Editor`) and a status-bar button "Content Drawer".
- Geometry: full window width, **40 %** of the window height, bottom edge on top of the status bar, above the dock area (`Order::Foreground`). Slides up in about **150 ms** (`animate_bool_with_time`).
- Hides on: Esc (not while a text field or an inline rename has focus), Ctrl+Space again, a press outside the drawer (the status-bar button itself does not count as outside), and an `AssetDrag` dragged out of the drawer (so the Scene / Hierarchy under it can take the drop). The spec says "when the drag starts"; hiding when the drag leaves the drawer keeps moving files between folders inside the drawer possible (planning ruling).
- Title bar: "Content Drawer" and a "Dock in Layout" button: opens or focuses the Content Browser tab (`layout::reopen`), hides the drawer.
- While the Content Browser tab was drawn last frame, the toggle (key or button) focuses that tab and flashes the browser for `sizes::PING_SECONDS` instead of opening the drawer.
- The drawer counts as "browser shown" for OS file drops, browser focus, thumbnails and `apply_content_requests`, exactly like the tab.
- Colours and sizes from theme tokens (`sizes::DRAWER_HEIGHT_SHARE = 0.4`, `sizes::DRAWER_SLIDE_SECONDS = 0.15`); no placeholder controls; never call egui `Response` / `Context` methods inside `input` closures.
- `just gate` must pass before every commit (never chain a commit after a failing gate); Conventional Commits; no co-author or tool credit lines.

## Review Focus

1. Pressing Esc while renaming a file inside the drawer cancels the rename only; the drawer stays open. Test: `esc_while_renaming_keeps_the_drawer` (Task 1).
2. Clicking the status-bar "Content Drawer" button while the drawer is open closes it (once), instead of the outside-click closing it and the button reopening it. Test: `button_toggles_closed_not_reopened` (Task 1).
3. Ctrl+Space while the game owns input (Playing, Game focus) does nothing. Test: `ctrl_space_ignored_with_game_focus` (Task 1).
4. Files dropped from Explorer while only the drawer shows the browser are imported into its folder; with neither shown, the status bar hint stays. Test: `drawer_takes_dropped_files` (Task 2).
5. The browser's keyboard focus and thumbnails keep working in the drawer (it is "shown"), and closing the drawer drops the browser's focus. Test: `closing_the_drawer_drops_browser_focus` (Task 2).

---

### Task 1: Drawer state, toggle and closing rules

**Files:**
- Create: `crates/melon_editor/src/drawer.rs`
- Modify: `crates/melon_editor/src/lib.rs` (`mod drawer;`; `Editor` field `drawer: drawer::DrawerState`; `Shortcut::ToggleDrawer` for Ctrl+Space in `shortcut_for` / `handle_shortcuts`; status-bar button; call `self.drawer_ui(world, ui)` after the dock)
- Modify: `crates/melon_editor/src/content_view.rs` / `content.rs` (`ContentBrowser::flash(&mut self)` reusing the ping outline around the whole view for `PING_SECONDS`)
- Modify: `crates/melon_editor/src/theme.rs` (`DRAWER_HEIGHT_SHARE`, `DRAWER_SLIDE_SECONDS`)

**Interfaces:**
- Produces:
  - `#[derive(Default)] pub(crate) struct DrawerState { pub(crate) open: bool, /// last drawn rect, for outside-click tests: pub(crate) rect: Option<egui::Rect> }`
  - `Editor::toggle_drawer(&mut self)`: if `self.content_shown` (the tab was drawn last frame) → focus the tab (`set_active_tab`) and `self.content.flash()`; else flip `drawer.open`.
  - `Editor::dock_drawer(&mut self)`: `layout::reopen(&mut self.dock, EditorTab::ContentBrowser)`, `drawer.open = false`.
  - `Editor::drawer_ui(&mut self, world: &mut World, ui: &egui::Ui)`: when open (or still animating closed), an `egui::Area::new(Id::new("content_drawer"))` with `Order::Foreground`, fixed position/size from the screen rect minus `STATUS_HEIGHT`; frame fill `colors::CHROME_RAISED`, `BORDER_STRONG` top stroke; title row ("Content Drawer", right-aligned "Dock in Layout" button with id-able response); then `self.content.ui(ui)` and `self.apply_content_requests(world)`; sets `self.content_shown = true` while drawn; stores `drawer.rect`.
  - Closing rules evaluated in `drawer_ui` before drawing, with input read into locals first:
    - Esc pressed, `!ui.ctx().text_edit_focused()` and `self.content.renaming.is_none()` → close.
    - A pointer press this frame whose position is outside `drawer.rect` and outside the status-bar button's rect (stored as `drawer_button: Option<egui::Rect>` on the state when the status bar draws it) → close.
    - `egui::DragAndDrop::has_payload_of_type::<content::AssetDrag>(ctx)` and the pointer is outside `drawer.rect` → close (the payload stays, so the drop lands on what is under the cursor).
  - `shortcut_for(key, ctrl, typing, flying)`: `Key::Space if ctrl` → `Shortcut::ToggleDrawer`; `handle_shortcuts` skips it with game focus (as all but `ReleaseGame`).

- [ ] **Step 1: Write the failing tests** (`lib.rs` tests or `drawer.rs` tests, headless `build_ui` frames on `browser_project`; helper `hide_content_tab(editor)` makes Console the active tab in the Content Browser's leaf)

```rust
#[test]
fn ctrl_space_toggles_the_drawer_when_the_tab_is_hidden() {
    // hide_content_tab ; warm frame ; Ctrl+Space frame (RawInput.modifiers = COMMAND!) -> drawer.open
    // another Ctrl+Space frame -> !drawer.open
}

#[test]
fn ctrl_space_with_the_tab_visible_flashes_it_instead() {
    // default layout ; warm frame (content_shown) ; Ctrl+Space -> !drawer.open and content flash set
}

#[test]
fn ctrl_space_ignored_with_game_focus() {
    // hide tab ; world InputFocus::Game (+ Playing) ; Ctrl+Space -> !drawer.open
}

#[test]
fn esc_closes_the_drawer() { /* open ; Esc frame -> closed */ }

#[test]
fn esc_while_renaming_keeps_the_drawer() {
    // open ; content.start_rename(x) ; frames until the rename field has focus ; Esc -> renaming cancelled, drawer.open
}

#[test]
fn press_outside_closes_inside_keeps() {
    // open ; frame ; press inside drawer.rect -> open ; press at the top of the screen -> closed
}

#[test]
fn button_toggles_closed_not_reopened() {
    // open via toggle ; frame ; click the status-bar button rect (drawer.drawer_button) -> closed after the frame
}

#[test]
fn dragging_out_of_the_drawer_hides_it() {
    // open ; set AssetDrag payload ; pointer moved above drawer.rect -> closed, payload still set
}

#[test]
fn dock_in_layout_opens_the_tab_and_closes() {
    // hide tab ; open ; editor.dock_drawer() ; next frame content_shown (tab drawn) and !drawer.open
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- drawer ctrl_space esc_ press_outside button_toggles dragging_out dock_in_layout`
Expected: compile errors (`drawer` field / `toggle_drawer` not found).

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add crates/melon_editor
git commit -m "feat(editor): Content Drawer (Ctrl+Space) over the dock area"
```

---

### Task 2: The drawer as a shown browser

**Files:**
- Modify: `crates/melon_editor/src/lib.rs` (`take_dropped_files` / focus clearing see the drawer; frame order so the drawer's `content_shown` counts next frame like the tab's)
- Modify: `README.md` (Ctrl+Space, Dock in Layout), `docs/ROADMAP.md` (Content Drawer checked; Content Browser line done), `AGENTS.md` (layout table: `drawer.rs`)

**Interfaces:**
- Consumes: Task 1 `DrawerState`, `drawer_ui`.
- Produces: no new names. `content_shown` is set by whichever of tab / drawer drew the browser; closing the drawer (any rule) also sets `self.content.focused = false`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn drawer_takes_dropped_files() {
    // hide tab ; open drawer ; warm frame ; frame with RawInput.dropped_files = [outside/crate.glb]
    // -> assets/crate.glb exists with a .meta (the browser's current folder is assets)
}

#[test]
fn closing_the_drawer_drops_browser_focus() {
    // hide tab ; open ; content.focused = true ; Esc frame -> !content.has_focus()
}

#[test]
fn drawer_tiles_want_thumbnails() {
    // hide tab ; open ; browser in assets/models with tile view ; frame -> thumbnails.wanted() contains "assets/models/x.glb"
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- drawer_takes closing_the_drawer drawer_tiles`
Expected: assertion failures.

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Check in the dev build**

Run: `cargo run -p melon_host`.
Expected: with the Content Browser tab behind the Console, Ctrl+Space slides the drawer up over the dock (40 % height); Esc, Ctrl+Space and a click on the Scene hide it; dragging a model out of the drawer hides it and drops into the Scene; Dock in Layout brings the tab to the front; with the tab in front, Ctrl+Space flashes the tab instead. Ctrl+Space while playing with game focus does nothing.

- [ ] **Step 6: Docs, full test run, commit**

```powershell
just test
git add crates/melon_editor README.md docs/ROADMAP.md AGENTS.md
git commit -m "feat(editor): the Content Drawer shows the browser like its tab"
```
