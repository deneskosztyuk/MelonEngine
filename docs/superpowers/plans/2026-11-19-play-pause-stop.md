# Play / Pause / Stop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The editor opens in an edit mode; Play snapshots the scene, Pause freezes it, Stop restores the snapshot.

**Architecture:** `SimulationState` gains `Editing` (default). `Core::update` advances play time and the accumulator only while `Playing`, and freezes them while `Paused`. Transitions are plain `&mut World` functions in `melon_editor/src/play.rs` that snapshot and restore through the scene module, split into in-memory `scene_to_data` / `apply_scene`.

**Tech Stack:** Rust 2024, bevy_ecs 0.19, egui 0.35, rapier3d 0.34, ron 0.8.

**Spec:** `docs/superpowers/specs/2026-11-19-play-pause-stop-design.md`

## Global Constraints

- States: `SimulationState { Editing (default), Playing, Paused }`.
- Shortcuts: F5 Play/Resume, F6 Pause, Shift+F5 Stop.
- Labels: `EDITING` grey, `PLAYING` green, `PAUSED` yellow. Buttons `▶ Play`, `⏸ Pause`, `⏹ Stop`.
- CLI flag: `--play`.
- No new dependencies. `unsafe_code` stays denied (existing `no_mangle` in `melon_game` excepted).
- Gate per task: `cargo clippy --workspace --all-targets -- -D warnings` clean and `cargo test --workspace` green. fmt-check on touched files (pre-existing diff in `melon_slint_poc.rs:127` stays).
- Commits: Conventional Commits, no co-author or tool trailers.
- Never replace `core.world`; never call `World::clear_entities()` (AGENTS.md).

## Review Focus

1. **Play refuses an invalid edit-mode scene.** The inspector can set a scale the scene validator rejects (zero scale). Play must return `Err`, stay `Editing`, and leave the scene untouched. Test in Task 3.
2. **Shortcut while typing.** F5 pressed while an inspector text field has focus must not start Play. Guard with egui `wants_keyboard_input()`. Checked by reading in Task 4 (UI, not unit-tested).
3. **Stale selection after Play/Stop.** Entity ids change; the editor must clear the selection so the inspector never looks up a despawned entity. Task 4.
4. **Stop twice / Play twice.** A second `stop` with no snapshot, or `play` while `Playing`, changes nothing and returns `Ok`. Test in Task 3.
5. **Unnamed scene entities.** `apply_scene` despawns every entity with a `Transform`, but scenes save only entities with `Name` + `Transform`. Anything unnamed would vanish on Play. Today every spawn path adds `Name`; Task 2 asserts it in a test so a future change trips it.

---

### Task 1: States and play time in `melon_core`

**Files:**
- Modify: `crates/melon_core/src/resources.rs` (`SimulationState`, `Time::update`, tests)
- Modify: `crates/melon_core/src/lib.rs` (`Core::update`, tests)
- Modify: `crates/melon_physics/src/lib.rs` (test helper only), `crates/melon_game/src/lib.rs` (tests only)

**Interfaces:**
- Produces:
  - `SimulationState::{Editing, Playing, Paused}`, `Editing` is `#[default]`.
  - `Time::update(delta)`: sets `delta_seconds = delta.min(MAX_FRAME_DELTA)` only; no longer touches `elapsed_seconds`.
  - `Core::update` per state: `Playing`: `elapsed_seconds += delta_seconds`, `overstep += delta_seconds`, run fixed steps. `Paused`: nothing changes. `Editing`: `overstep = 0`. Missing state counts as `Playing` (unchanged).

- [ ] **Step 1: Write / adapt failing tests.**
  - `resources.rs`: `simulation_state_default_is_editing`; rename `time_update_accumulates_elapsed` to `time_update_sets_delta_only` (`update(0.1)` then `update(0.2)`: `delta_seconds == 0.2`, `elapsed_seconds == 0.0`).
  - `lib.rs`: `counting_core()` inserts `SimulationState::Playing`. New tests:

```rust
#[test] fn elapsed_advances_only_while_playing() { /* 60 @1/60 Playing, 60 Paused, 60 Editing */ assert!((elapsed - 1.0).abs() < 1e-3); }
#[test] fn paused_keeps_overstep() { /* Playing 1 @0.025 -> o; Paused 10 @1/60 */ assert_eq!(overstep, o); assert_eq!(steps, 1); }
#[test] fn editing_zeroes_overstep() { /* Playing 1 @0.025; Editing 1 @1/60 */ assert_eq!(overstep, 0.0); }
```
  Change `paused_runs_no_fixed_steps` to assert `steps == 0` only (overstep is now frozen, not zeroed). Keep `resume_does_not_burst`.
  - `melon_physics` `drop_test_core()` inserts `SimulationState::Playing`.
  - `melon_game`: `bob_has_no_drift` and `rotation_uses_delta` set `elapsed_seconds` / `delta_seconds` on `Time` directly (no `Core`). New `bob_has_no_drift_across_pause` through `melon_core::Core` with `bob_system.run_if(|s: Res<SimulationState>| *s == SimulationState::Playing)` in `Update` (as the host registers it): Playing 300 frames @1/60, Paused 120, Playing 300; `assert!((y - 2.0 * (0.6f32 * 10.0).sin()).abs() < 1e-3)`.
- [ ] **Step 2: Run** `cargo test --workspace` — new tests fail (`Editing` not found).
- [ ] **Step 3: Implement** the variant, `Time::update`, and the per-state branch in `Core::update`.
- [ ] **Step 4: Run** `cargo test --workspace` — all pass. Host still compiles (`is_playing` closures unchanged).
- [ ] **Step 5: Commit** `feat(core): Editing state and play time that pauses`

### Task 2: In-memory scene snapshot in `melon_editor/src/scene.rs`

**Files:**
- Modify: `crates/melon_editor/src/scene.rs`

**Interfaces:**
- Produces:
  - `#[derive(Serialize, Deserialize, Clone)] pub struct SceneData` (fields stay private).
  - `pub fn scene_to_data(world: &mut World) -> SceneData`
  - `pub fn apply_scene(world: &mut World, physics: &mut PhysicsWorld, data: &SceneData) -> Result<()>`: version + entity validation first (no mutation on error), then the in-place clear and spawn (current `load_scene` phase 2).
  - `save_scene` = `scene_to_data` + RON + atomic write. `load_scene` = read + RON + `apply_scene`. Signatures unchanged.

- [ ] **Step 1: Write failing tests:**

```rust
#[test] fn scene_to_data_then_apply_restores_transforms() { /* data = scene_to_data; move "Floor" to y=5; apply_scene */ assert!((floor_y - (-1.0)).abs() < 1e-6); }
#[test] fn apply_scene_invalid_data_leaves_world_untouched() { /* data from a world with scale 0 on one entity; apply into world_with_entities() */ assert!(result.is_err()); assert_eq!(name_count, 2); }
#[test] fn every_transform_entity_in_default_spawn_paths_has_name() { /* world_with_entities(): count With<Transform> == count With<(Transform, Name)> */ }
```
- [ ] **Step 2: Run** `cargo test -p melon_editor` — fails (`scene_to_data` not found).
- [ ] **Step 3: Implement** the split. Existing file tests keep passing unchanged.
- [ ] **Step 4: Run** `cargo test -p melon_editor` — all pass.
- [ ] **Step 5: Commit** `refactor(editor): split scene save/load into in-memory halves`

### Task 3: Play / Pause / Stop transitions in `melon_editor/src/play.rs`

**Files:**
- Create: `crates/melon_editor/src/play.rs`; Modify: `crates/melon_editor/src/lib.rs` (`pub mod play;`)

**Interfaces:**
- Consumes: `scene_to_data`, `apply_scene`, `SceneData` (Task 2); `SimulationState`, `Time` (Task 1).
- Produces:
  - `#[derive(Resource)] pub struct PlaySnapshot(SceneData)`
  - `pub fn play(world: &mut World) -> anyhow::Result<()>`: `Editing`: snapshot, `apply_scene` from it (via `world.resource_scope::<PhysicsWorld, _>`), insert `PlaySnapshot`, state `Playing`; on `Err` nothing changes. `Paused`: state `Playing`. `Playing`: no-op `Ok`.
  - `pub fn pause(world: &mut World)`: `Playing` to `Paused`, else no-op.
  - `pub fn stop(world: &mut World) -> anyhow::Result<()>`: with a `PlaySnapshot`: `apply_scene` from it, remove it, `elapsed_seconds = 0`, `overstep = 0`, state `Editing`. Without one: no-op `Ok`.

- [ ] **Step 1: Write failing tests.** Helper `edit_world() -> World`: resources `PhysicsWorld::new()`, `Time::default()`, `SimulationState::Editing`, `NameCounter(0)`; entities `(Name("Floor"), Transform at y=-1, MeshRef::Cube, BodyInfo { Fixed, half (5,0.5,5) })` and `(Name("Box"), Transform at y=3, MeshRef::Cube, BodyInfo { Dynamic, half 0.5 })`, with no rapier bodies (the first `play` builds them from `BodyInfo`). "rapier y of Box" = `PhysicsWorld::body_translation(box's RigidBodyComponent.body_handle)`.

```rust
#[test] fn stop_restores_moved_entity() { /* play; set Box y=9; stop */ assert!((box_y - 3.0).abs() < 1e-6); assert_eq!(state, SimulationState::Editing); }
#[test] fn stop_removes_spawned_and_restores_despawned() { /* play; spawn "Extra"; despawn "Floor"; stop */ assert_eq!(names, ["Box", "Floor"]); }
#[test] fn play_applies_edit_mode_moves_to_physics() { /* Editing: set Box Transform y=7; play */ assert!((rapier_y_of_box - 7.0).abs() < 1e-6); }
#[test] fn stop_resets_play_time() { /* play; elapsed=5, overstep=0.01; stop */ assert_eq!((elapsed, overstep), (0.0, 0.0)); }
#[test] fn pause_then_play_resumes_without_new_snapshot() { /* play; move Box y=9; pause; play; stop */ assert!((box_y - 3.0).abs() < 1e-6); }
#[test] fn play_refuses_invalid_scene() { /* Editing: Box scale = ZERO; play */ assert!(r.is_err()); assert_eq!(state, Editing); assert_eq!(box_scale, Vec3::ZERO); }
#[test] fn wrong_state_calls_are_noops() { /* stop in Editing -> Ok, Editing; pause in Editing -> Editing; play twice -> Playing, snapshot from first */ }
```
- [ ] **Step 2: Run** `cargo test -p melon_editor play::` — fails (module missing).
- [ ] **Step 3: Implement** `play.rs`. On an `apply_scene` error inside `play`/`stop`, log with `tracing::error!` and return it.
- [ ] **Step 4: Run** `cargo test -p melon_editor` — all pass.
- [ ] **Step 5: Commit** `feat(editor): play, pause and stop with scene snapshot`

### Task 4: Editor toolbar, shortcuts, gizmo physics write

**Files:**
- Modify: `crates/melon_editor/src/lib.rs` (`build_ui` top bar, Save/Load enable, `write_to_physics_body`, tests)

**Interfaces:**
- Consumes: `play::{play, pause, stop}` (Task 3).

- [ ] **Step 1: Write failing test** in `lib.rs` `mod tests`:

```rust
#[test] fn gizmo_physics_write_works_while_paused() {
    // World: PhysicsWorld with dynamic body, entity with RigidBodyComponent, SimulationState::Paused
    // Editor::new().write_to_physics_body(&mut world, e, Some(Vec3::new(4.0, 0.0, 0.0)), None)
    assert!((rapier_x - 4.0).abs() < 1e-6);
}
```
- [ ] **Step 2: Run** `cargo test -p melon_editor gizmo_physics_write` — fails (early return when not `Playing`).
- [ ] **Step 3: Implement.**
  - `write_to_physics_body`: drop the `SimulationState` check.
  - Top bar: replace the state label with `▶ Play` (enabled in `Editing`/`Paused`), `⏸ Pause` (enabled in `Playing`), `⏹ Stop` (enabled in `Playing`/`Paused`), then the coloured state label. Each button calls the `play` function, then `self.clear_selection()` after `play` from `Editing` and after `stop`; errors go to `self.set_status`.
  - Shortcuts in the same place, only when `!ctx.wants_keyboard_input()`: F5 = Play/Resume, F6 = Pause, Shift+F5 = Stop.
  - Save and Load buttons: `add_enabled(!dialog_open && state == Editing, ...)`.
  - `imgui_poc.rs` needs no change (it prints `{state:?}`); confirm it compiles.
- [ ] **Step 4: Run** `cargo test -p melon_editor`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo check -p melon_host --features imgui-poc` — all pass.
- [ ] **Step 5: Commit** `feat(editor): play/pause/stop toolbar and shortcuts`

### Task 5: Host `--play`, blending while paused, docs

**Files:**
- Modify: `crates/melon_host/src/main.rs`, `docs/ROADMAP.md`, `AGENTS.md`, `README.md` (Controls section)

**Interfaces:**
- Consumes: `melon_editor::play::play` (Task 3), states (Task 1).
- Produces: `fn blends(state: SimulationState) -> bool` (true for `Playing` and `Paused`) in `main.rs`; the draw list passes `blends(state)` as `render_matrix`'s last argument.

- [ ] **Step 1: Write failing tests:**

```rust
#[test] fn blends_while_playing_and_paused() { assert!(blends(SimulationState::Playing) && blends(SimulationState::Paused)); }
#[test] fn no_blend_while_editing() { assert!(!blends(SimulationState::Editing)); }
```
  Rename `render_matrix_paused_uses_current` to `render_matrix_without_blend_uses_current` (same assertion).
- [ ] **Step 2: Run** `cargo test -p melon_host` — fails (`blends` missing).
- [ ] **Step 3: Implement.**
  - `blends`, wired into the draw list.
  - `App::new`: after the `PhysicsWorld` insert, if `std::env::args().any(|a| a == "--play")`, call `melon_editor::play::play(&mut core.world)` and log an error if it fails.
  - `docs/ROADMAP.md`: tick item 2, mark bug 1 fixed. `AGENTS.md`: one gotcha line: the editor starts in `Editing`; game systems run only while `Playing`; run `melon_host --play` to start playing. `README.md` Controls: F5 / F6 / Shift+F5 and `--play`.
- [ ] **Step 4: Verify.** `cargo test --workspace` green, clippy clean, `cargo build -p melon_host`, `cargo build -p melon_host --release --no-default-features`, `cargo check -p melon_host --features imgui-poc` all succeed.
- [ ] **Step 5: Commit** `feat(host): start in edit mode, --play flag, blend while paused`
