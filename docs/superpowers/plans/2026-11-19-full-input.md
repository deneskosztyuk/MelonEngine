# Full Input Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every key and mouse button with per-frame edges, data-driven actions, Unreal-style input focus between editor and game, and an Edit → Project Settings… window for input mapping.

**Architecture:** `melon_core::input` holds raw state (`Input`), actions (`InputMap`, `Actions`), the focus profile (`InputFocus`) and the unfiltered press stream (`RawPresses`). A new `PreUpdate` stage turns raw input into actions. The host records every winit event and forwards it to the game's `Input` only while the focus is `Game`; the editor owns focus transitions and gates its own controls.

**Tech Stack:** Rust 2024, bevy_ecs 0.19, winit 0.30, egui 0.35, serde + ron 0.8.

**Spec:** `docs/superpowers/specs/2026-11-19-full-input-design.md`

## Global Constraints

- `just gate` before every commit; `just test` at the end of each task. Conventional Commits, no trailers or tool credits, commit on `main`.
- Dependency direction `core <- render, physics <- editor <- host`; `melon_core` gets no winit or egui dependency. Add deps through `[workspace.dependencies]` (`ron.workspace = true` in `melon_core`).
- `unsafe_code` stays denied. No `Response` / `Context` method calls inside `ctx.input(..)` / `ui.input(..)` closures.
- Editor colours and sizes from `theme::colors` / `theme::sizes`; log with `tracing`; no placeholder controls.
- Mapping file: `config/input.ron` under the project root, RON with `version: 1`; a broken file is backed up to `config/input.ron.broken` before the first overwrite. Default actions: MoveForward (axis W / S), MoveRight (axis D / A), Jump (button Space), Fire (button mouse Left).
- Reserved for the editor while running a game: Esc, F5, F6, Shift+F5.

## Review Focus

- Holding a key while focus switches to `Editor` (Esc): no stuck key; the game sees `just_released` (tests in Task 1 `release_all` and Task 4).
- Key auto-repeat (winit `repeat: true`): never a second `just_pressed` (test in Task 4).
- Press and release inside one frame: both edges that frame, `pressed` false after (test in Task 1).
- Rebinding capture: Esc cancels, and a captured key never also triggers an editor shortcut (test in Task 5).
- Duplicate / empty action names in a hand-edited `input.ron`: load keeps the first, reports it, no panic (test in Task 2).

---

### Task 1: Raw input state and `PreUpdate`

**Files:**
- Modify: `crates/melon_core/src/input.rs`, `crates/melon_core/src/stages.rs`, `crates/melon_core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum KeyCode` — letters `A`..`Z`, `Digit0`..`Digit9`, `F1`..`F12`, `ArrowUp/Down/Left/Right`, `Space`, `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `ShiftLeft`, `ShiftRight`, `ControlLeft`, `ControlRight`, `AltLeft`, `AltRight`, `Minus`, `Equal`, `BracketLeft`, `BracketRight`, `Backslash`, `Semicolon`, `Quote`, `Comma`, `Period`, `Slash`, `Backquote`, `Numpad0`..`Numpad9`. Derives `Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize`; `pub const ALL: &[KeyCode]`.
  - `pub enum MouseButton { Left, Right, Middle, Back, Forward }` (same derives, `ALL`).
  - `pub enum InputButton { Key(KeyCode), Mouse(MouseButton) }` (same derives).
  - `Input` (resource): `press(InputButton)`, `release(InputButton)`, `release_all()`, `pressed(InputButton) -> bool`, `just_pressed(..)`, `just_released(..)`, `key(KeyCode) -> bool` (= pressed), `mouse_delta: Vec2`, `wheel: f32` (notches, + = away from user), `cursor: Option<Vec2>`, `clear_frame()` (edges, delta, wheel). Remove `right_mouse_held` and `take_mouse_delta` and update their callers.
  - `PreUpdate` stage: runs once per frame before the fixed steps; `clear_input_deltas_system` becomes `clear_input_frame_system` calling `Input::clear_frame()` in `PostUpdate`.

- [ ] **Step 1: Write the failing tests** (in `input.rs` and `lib.rs`)

```rust
#[test] fn press_gives_one_frame_just_pressed()          // press; just_pressed true; clear_frame(); pressed true, just_pressed false
#[test] fn release_gives_one_frame_just_released()
#[test] fn press_and_release_in_one_frame_keeps_both_edges() // press+release before clear: just_pressed && just_released, !pressed
#[test] fn pressing_a_held_key_again_is_not_a_new_edge()    // press, clear_frame, press again: just_pressed false
#[test] fn release_all_releases_with_edges()
#[test] fn clear_frame_resets_delta_and_wheel()
#[test] fn pre_update_runs_before_fixed_update()          // lib.rs: a PreUpdate system and a FixedUpdate system record order within one Core::update
```

- [ ] **Step 2: Run** `cargo test -p melon_core` — Expected: FAIL (missing API).
- [ ] **Step 3: Implement** as above; fix compile errors in host / editor callers with the smallest change (host right-drag delta reads `pressed(InputButton::Mouse(MouseButton::Right))`).
- [ ] **Step 4: Run** `just test` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): full raw input with per-frame edges and a PreUpdate stage`

### Task 2: Actions and the mapping file

**Files:**
- Create: `crates/melon_core/src/actions.rs`
- Modify: `crates/melon_core/src/lib.rs` (exports; register `update_actions_system` in `PreUpdate`; insert default `InputMap` + `Actions` in `Core::new`), `crates/melon_core/Cargo.toml` (`ron.workspace = true`, `anyhow.workspace = true`)

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `pub enum ActionKind { Button, Axis }`; `pub struct AxisBinding { pub positive: InputButton, pub negative: InputButton }`; `pub struct Action { pub name: String, pub kind: ActionKind, pub buttons: Vec<InputButton>, pub axes: Vec<AxisBinding> }`; `#[derive(Resource, Clone, Serialize, Deserialize)] pub struct InputMap { pub version: u32, pub actions: Vec<Action> }` with `Default` = the four default actions.
  - `pub enum LoadOutcome { Loaded, Missing, Broken(String) }`; `InputMap::load(path: &Path) -> (InputMap, LoadOutcome)` (Missing / Broken → defaults; duplicate or empty names → keep the first, outcome `Broken(..)` naming them).
  - `InputMap::save(&self, path: &Path, backup_broken: bool) -> anyhow::Result<()>`: atomic write (temp + rename, creating `config/`); when `backup_broken` and the file exists, copy it to `<path>.broken` first.
  - `#[derive(Resource, Default)] pub struct Actions`: `pressed(&str)`, `just_pressed(&str)`, `just_released(&str)`, `axis(&str) -> f32`; unknown names → false / 0.0 + one `tracing::warn!` per name.
  - `pub fn update_actions_system(input: Res<Input>, map: Res<InputMap>, actions: ResMut<Actions>)` in `PreUpdate`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn any_binding_presses_a_button_action()       // Jump bound Space + Mouse(Left); press Mouse(Left) → pressed("Jump")
#[test] fn axis_sums_pairs_and_clamps()                 // W → 1.0; S → -1.0; both → 0.0; two pairs both positive → 1.0
#[test] fn unknown_action_is_false_and_zero()
#[test] fn ron_round_trip()
#[test] fn missing_file_gives_defaults()                // LoadOutcome::Missing, 4 actions
#[test] fn broken_file_gives_defaults_and_keeps_file()  // garbage text: Broken(_), file content unchanged
#[test] fn duplicate_names_keep_first()                 // two "Jump": one kept, Broken(msg) mentions "Jump"
#[test] fn save_backs_up_broken_file()                  // existing garbage + save(.., true): input.ron.broken == garbage, input.ron parses
```

- [ ] **Step 2: Run** `cargo test -p melon_core --lib actions` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): input actions and the input.ron mapping`

### Task 3: Input focus profiles in the editor

**Files:**
- Modify: `crates/melon_core/src/input.rs` (`InputFocus`, `GameInputGate`), `crates/melon_editor/src/lib.rs`, `crates/melon_editor/src/play.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)] pub enum InputFocus { #[default] Editor, Game }` in `melon_core`.
  - `#[derive(Resource, Default, Clone, Copy)] pub struct GameInputGate(pub bool)` in `melon_core`: true only when focus is `Game`, the simulation is Playing and no egui text field has keyboard focus. The editor writes it every frame.
  - Transitions: `play::play` / resume → `Game`; `play::pause`, `play::step`, `play::stop` → `Editor`; Esc (new `Shortcut::ReleaseGame`) → `Editor`; left-click on the Scene viewport while Playing → `Game`. In `Game`: editor camera does not move, W/E/R/Delete/F shortcuts and gizmo / click-select are off, gizmo tool buttons disabled; F5, F6, Shift+F5, Esc still work.
  - `pub fn focus_after_click(state: SimulationState, focus: InputFocus, clicked_viewport: bool) -> InputFocus` (pure helper).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn play_gives_game_focus_stop_and_pause_give_editor()
#[test] fn escape_returns_focus_to_editor_while_playing()       // shortcut_for(Escape, ..) == Some(Shortcut::ReleaseGame); state stays Playing
#[test] fn click_on_viewport_while_playing_takes_game_focus()   // focus_after_click(Playing, Editor, true) == Game; (Editing, Editor, true) == Editor
#[test] fn game_focus_blocks_editor_camera_and_tool_keys()      // headless frame with W pressed and Game focus: gizmo_mode unchanged, camera unchanged
#[test] fn gate_is_false_while_typing_or_editing()
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): editor / game input focus with Esc and click`

### Task 4: Host routing, all keys, cursor grab

**Files:**
- Modify: `crates/melon_host/src/main.rs`, `crates/melon_core/src/input.rs` (`RawPresses`)

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces:
  - `#[derive(Resource, Default)] pub struct RawPresses(pub Vec<InputButton>)` in `melon_core`: every press this frame (repeats excluded), regardless of focus, cleared in `PostUpdate`. Used by the rebinding capture.
  - Host: `map_key(&winit KeyCode) -> Option<KeyCode>` for every `KeyCode::ALL` variant; `map_mouse(winit MouseButton) -> Option<MouseButton>`. Every keyboard / mouse / wheel / cursor event is recorded into `RawPresses` and, when `GameInputGate` is true, into `Input`; egui still receives every event and its `consumed` flag no longer stops recording. Repeats (`event.repeat`) are ignored. `DeviceEvent::MouseMotion` adds to `Input::mouse_delta` while the gate is true. When the gate turns false, `Input::release_all()`.
  - Cursor: while `InputFocus::Game` and Playing, the window cursor is hidden and grabbed (`CursorGrabMode::Confined`, falling back to `Locked`); otherwise visible and free.
  - Testable pure helpers: `record_button(input: &mut Input, raw: &mut RawPresses, button: InputButton, pressed: bool, repeat: bool, gate: bool)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn map_key_covers_every_engine_key()       // for each KeyCode::ALL, some winit code maps to it (table in the test)
#[test] fn map_mouse_covers_every_button()
#[test] fn repeat_events_do_not_press_again()
#[test] fn gate_off_records_raw_only()
#[test] fn gate_turning_off_releases_held_keys()
```

- [ ] **Step 2: Run** `cargo test -p melon_host` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(host): route all keys and mouse input through the focus gate`

### Task 5: Project Settings window and mapping persistence

**Files:**
- Create: `crates/melon_editor/src/project_settings.rs`
- Modify: `crates/melon_editor/src/lib.rs` (menu item, window state), `crates/melon_host/src/main.rs` (load `config/input.ron` at startup via `InputMap::load`, log `Broken` as an error), `README.md`, `AGENTS.md`, `docs/ROADMAP.md`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces: `pub struct ProjectSettings { open: bool, section: Section, capture: Option<CaptureTarget>, broken_on_load: bool }` with `ui(&mut self, ctx: &egui::Context, world: &mut World, project_root: &Path)`; `enum Section { InputMapping }`. Edit menu: "Project Settings…". Window: left list of sections, right the Input Mapping table (name text field, kind combo, binding chips with ×, "+ binding", "+ Action", remove action). Capture: the next entry of `RawPresses` assigns the binding; Esc cancels; while capturing, editor shortcuts are suppressed. Every change: update the `InputMap` resource, then `save(path, backup_broken = broken_on_load)` and set `broken_on_load = false`. Duplicate / empty name: change refused, status warning.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn capture_assigns_next_raw_press()          // capturing for Jump; RawPresses = [Key(K)] → Jump buttons contain K, capture None
#[test] fn capture_escape_cancels()
#[test] fn captured_key_does_not_trigger_shortcut()  // capturing + Delete pressed: selected entity not deleted
#[test] fn rename_to_duplicate_is_refused()
#[test] fn edit_saves_file()                         // temp project root: after adding an action, config/input.ron parses and contains it
#[test] fn first_save_after_broken_load_backs_up()
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib project_settings` — Expected: FAIL.
- [ ] **Step 3: Implement.** README: Project Settings and focus keys (Esc / click). AGENTS.md: input gotcha (game code reads `Actions`; `InputFocus` / `GameInputGate`; `PreUpdate`). ROADMAP: tick the Full input item.
- [ ] **Step 4: Run** `just test` — Expected: PASS. Release-build screenshot of the Project Settings window.
- [ ] **Step 5: Commit** `feat(editor): Project Settings window with input mapping` and `docs: full input`
