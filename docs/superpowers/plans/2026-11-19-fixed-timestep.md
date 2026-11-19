# Fixed Timestep and Frame Stages Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Physics and game logic run at the same speed at any frame rate, in ordered stages, with smooth interpolated rendering of fixed-step entities.

**Architecture:** `Core` owns a bevy `Schedules` collection (outside the `World`) with three labels: `FixedUpdate`, `Update`, `PostUpdate`. `Core::update` accumulates frame time in `Time::overstep` and runs `FixedUpdate` once per whole `fixed_delta`. Entities with `PreviousTransform` are drawn blended between the last two fixed steps.

**Tech Stack:** Rust 2024, bevy_ecs 0.19 (`Schedules`, `ScheduleLabel`), glam 0.33, rapier3d 0.34.

**Spec:** `docs/superpowers/specs/2026-11-19-fixed-timestep-design.md`

## Global Constraints

- Fixed step: `1.0 / 60.0` s.
- Frame delta clamp: `0.25` s (at most 15 fixed steps per frame).
- No new dependencies. `unsafe_code` stays denied (except existing `no_mangle` in `melon_game`).
- `just gate` (fmt-check + clippy `-D warnings` + check) passes after every task.
- Commits: Conventional Commits, no co-author or tool trailers.

## Review Focus

1. **Scene load swaps the whole `World`** (`melon_editor/src/scene.rs` `load_scene`). Stages must survive: schedules live in `Core`, never as a `World` resource. Test in Task 1.
2. **`1/60` is not exact in `f32`.** One 1/30 s frame may run 1 step, not 2. The contract is the long-run rate: N frames covering 1 s run 59..=60 steps. Tests in Task 1 assert ranges, never exact per-frame counts.
3. **Resume after pause must not burst.** `overstep` is 0 while paused, so the first frame after resume runs at most 1 step. Test in Task 1.
4. **Hitch** (Windows blocks the event loop while a window is dragged). A 1 s frame runs at most 15 steps. Test in Task 1.
5. **Entity spawned mid-play** (editor add, scene load) must not blend from the origin. `add_previous_transforms_system` initializes `PreviousTransform` to the current `Transform`. Test in Task 3.

---

### Task 1: Stages and accumulator in `melon_core`

**Files:**
- Create: `crates/melon_core/src/stages.rs`
- Modify: `crates/melon_core/src/lib.rs` (`Core`, re-exports, tests)
- Modify: `crates/melon_core/src/resources.rs` (`Time`, tests)

**Interfaces:**
- Produces:
  - `stages.rs`: `#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]` unit structs `FixedUpdate`, `Update`, `PostUpdate`. Re-exported from `melon_core`.
  - `pub struct Core { pub world: World, pub schedules: Schedules }`. The old `schedule` field is removed.
  - `Core::add_systems<M>(&mut self, label: impl ScheduleLabel, systems: impl IntoScheduleConfigs<ScheduleSystem, M>) -> &mut Self`
  - `Core::new()` inserts `Time::default()` and `SimulationState::default()` if absent, creates all three schedules, and adds `frame_counter_system`, `log_frame_system` to `Update`.
  - `Core::update()` order: accumulate, run `FixedUpdate` 0..N times, `Update`, `PostUpdate`, `world.clear_trackers()`. A missing `SimulationState` counts as `Playing`.
  - `Time { delta_seconds, elapsed_seconds, pub fixed_delta: f32, pub overstep: f32 }`, `Time::MAX_FRAME_DELTA: f32 = 0.25`, `Time::alpha(&self) -> f32` = `overstep / fixed_delta`.
  - `Time::update(delta)` stores `delta.min(Time::MAX_FRAME_DELTA)` in `delta_seconds` and adds it to `elapsed_seconds`. It does not touch `overstep`.

- [ ] **Step 1: Write failing tests** in `lib.rs` `mod tests`. Helper `fn counting_core() -> Core`: `Core::new()` plus a `FixedCount(u32)` resource and a `FixedUpdate` system that increments it. Helper `fn run_frames(core: &mut Core, dt: f32, n: u32)` calls `world.resource_mut::<Time>().update(dt)` then `core.update()`, n times. `steps` below = `FixedCount` after the frames.

```rust
#[test] fn fixed_rate_at_30fps()  { /* 30 frames @ 1/30 */  assert!((59..=60).contains(&steps)); }
#[test] fn fixed_rate_at_144fps() { /* 144 frames @ 1/144 */ assert!((59..=60).contains(&steps)); }
#[test] fn hitch_clamped_to_15_steps() { /* 1 frame @ 1.0 */ assert!((14..=15).contains(&steps)); }
#[test] fn alpha_is_leftover_fraction() { /* 1 frame @ 0.025 */ assert_eq!(steps, 1); assert!((alpha - 0.5).abs() < 1e-3); }
#[test] fn paused_runs_no_fixed_steps() { /* Paused, 60 frames @ 1/60 */ assert_eq!(steps, 0); assert_eq!(overstep, 0.0); }
#[test] fn resume_does_not_burst() { /* Paused 60 frames @ 1/60, then Playing 1 frame @ 1/60 */ assert!(steps <= 1); }
#[test] fn stages_survive_world_swap() {
    // core.world = World::new() with Time + FixedCount inserted; run 2 frames @ 1/60
    assert!(fixed_steps >= 1); // FixedUpdate still registered
}
#[test] fn update_runs_after_fixed() { /* Update system records FixedCount it saw; after 1 frame @ 1/30 it saw >= 1 */ }
```

Update existing `core_new_has_frame_counter` and `core_update_increments_frame` only if they touch `schedule`. Add to `resources.rs`: `time_update_clamps_delta` (`update(1.0)` gives `delta_seconds == 0.25`), `time_default_fixed_delta` (`fixed_delta == 1/60`, `overstep == 0`).

- [ ] **Step 2: Run, expect compile failure** — `cargo test -p melon_core` fails: `FixedUpdate` not found.

- [ ] **Step 3: Implement** `stages.rs`, the `Time` fields and `Core` changes. The accumulator loop in `Core::update` is written by the user (`TODO(human)`); everything around it is scaffolded first. Borrow rule: read and write `Time` through `world.resource_mut::<Time>()` in a short scope, then run the schedule with `self.schedules.get_mut(FixedUpdate)` on `&mut self.world`.

- [ ] **Step 4: Run** `cargo test -p melon_core` — all pass.

- [ ] **Step 5: Commit** `feat(core): fixed-timestep accumulator and frame stages`

### Task 2: `PreviousTransform` and interpolation in `melon_core`

**Files:**
- Modify: `crates/melon_core/src/components.rs`, `crates/melon_core/src/lib.rs` (re-exports)

**Interfaces:**
- Produces:
  - `#[derive(Component, Debug, Clone, Copy)] pub struct PreviousTransform(pub Transform);` Not serialized.
  - `Transform::interpolate(&self, prev: &Transform, alpha: f32) -> Transform`: translation and scale `lerp(prev, self, alpha)`, rotation `prev.rotation.slerp(self.rotation, alpha)`.
  - `pub fn store_previous_transforms_system(mut query: Query<(&Transform, &mut PreviousTransform)>)`: copies `Transform` into `PreviousTransform`.

- [ ] **Step 1: Write failing tests** in `components.rs`:

```rust
#[test] fn interpolate_alpha_zero_is_prev() { /* translations (0,0,0) -> (2,0,0) */ assert_eq!(cur.interpolate(&prev, 0.0).translation, prev.translation); }
#[test] fn interpolate_alpha_one_is_current() { assert_eq!(cur.interpolate(&prev, 1.0).translation, cur.translation); }
#[test] fn interpolate_half_rotation() { /* prev IDENTITY, cur rotation_y(PI/2); alpha 0.5 */ assert!(r.angle_between(Quat::from_rotation_y(PI/4)) < 1e-5); }
#[test] fn store_previous_copies_transform() { /* entity (Transform at (1,2,3), PreviousTransform default); run system */ assert_eq!(prev.0.translation, Vec3::new(1.0, 2.0, 3.0)); }
```

- [ ] **Step 2: Run** `cargo test -p melon_core` — fails to compile.
- [ ] **Step 3: Implement** the type, method and system above.
- [ ] **Step 4: Run** `cargo test -p melon_core` — all pass.
- [ ] **Step 5: Commit** `feat(core): PreviousTransform and Transform::interpolate`

### Task 3: Fixed-step physics wiring in `melon_physics`

**Files:**
- Modify: `crates/melon_physics/src/lib.rs`

**Interfaces:**
- Consumes: `Core::add_systems`, `FixedUpdate`, `PostUpdate`, `Time::fixed_delta` (Task 1); `PreviousTransform`, `store_previous_transforms_system` (Task 2).
- Produces:
  - `step_system(mut world: ResMut<PhysicsWorld>, time: Res<Time>)`: sets `integration_parameters.dt = time.fixed_delta` before `step()`.
  - `pub fn add_previous_transforms_system(mut commands: Commands, query: Query<(Entity, &RigidBodyComponent, &Transform), Without<PreviousTransform>>)`: inserts `PreviousTransform(*transform)` on `Dynamic` bodies only.
  - `pub fn add_physics_systems(core: &mut Core)`: `FixedUpdate`: `(store_previous_transforms_system, sync_to_physics_system, step_system, sync_from_physics_system).chain()`. `PostUpdate`: `(cleanup_deleted_system, add_previous_transforms_system)`.

- [ ] **Step 1: Write failing tests.** Helper `fn drop_test_core() -> (Core, Entity)`: `Core::new()`, `PhysicsWorld` with a dynamic 0.5 m cuboid at y = 10 and its entity (`Transform`, `RigidBodyComponent`), `add_physics_systems`.

```rust
#[test] fn fall_height_independent_of_frame_rate() {
    // y30 = height after 30 frames @ 1/30; y144 = after 144 frames @ 1/144
    assert!((y30 - y144).abs() < 0.2, "{y30} vs {y144}"); // 0.2 m ~ one step of fall at 1 s
    assert!(y30 < 6.0 && y30 > 4.0); // ~4.9 m fall in 1 s, not 2.4x
}
#[test] fn paused_body_does_not_fall() { /* SimulationState::Paused, 60 frames */ assert_eq!(y, 10.0); }
#[test] fn dynamic_body_gets_previous_transform_equal_to_current() { /* 1 frame @ 0.001 (no fixed step) */ assert_eq!(prev.0.translation, transform.translation); }
#[test] fn fixed_body_gets_no_previous_transform() { assert!(world.get::<PreviousTransform>(fixed).is_none()); }
```

- [ ] **Step 2: Run** `cargo test -p melon_physics` — fails (`add_physics_systems` not found).
- [ ] **Step 3: Implement** the three items above. Existing single-system schedule tests keep using a plain `Schedule`; `step_system` tests now need a `Time` resource.
- [ ] **Step 4: Run** `cargo test -p melon_physics` — all pass, including existing tests.
- [ ] **Step 5: Commit** `fix(physics): step at a fixed rate independent of frame rate`

### Task 4: Frame-rate-independent demo systems in `melon_game`

**Files:**
- Modify: `crates/melon_game/src/lib.rs`

**Interfaces:**
- Consumes: `Time` (`delta_seconds`, `elapsed_seconds`).
- Produces (hot-reload signatures, used by Task 5):
  - `pub fn rotation_system(query: Query<&mut Transform, With<Spin>>, time: Res<Time>)`: rotate about Y by `ROTATION_SPEED * delta_seconds`, `ROTATION_SPEED = 0.5` rad/s.
  - `pub fn bob_system(query: Query<&mut Transform, With<Bob>>, time: Res<Time>)`: for entity index `i`, `y += BOB_AMPLITUDE * (sin(BOB_FREQ * t + i) - sin(BOB_FREQ * (t - dt) + i))`, with `t = elapsed_seconds`, `dt = delta_seconds`, `BOB_AMPLITUDE = 2.0` m, `BOB_FREQ = 0.6` rad/s.
  - Both keep `#[allow(unsafe_code)] #[unsafe(no_mangle)]`.

- [ ] **Step 1: Rewrite tests** (they use `FrameCounter` today):

```rust
#[test] fn rotation_uses_delta() { /* Time.update(0.2) (under the 0.25 clamp); run */ assert!(rot.angle_between(Quat::from_rotation_y(0.1)) < 1e-5); }
#[test] fn bob_has_no_drift() {
    // one Bob entity (index 0), y0 = 0; 600 frames @ 1/60, Time.update before each run
    // offsets telescope, so y must equal A * sin(w * 10 s) exactly, not grow
    assert!((y - 2.0 * (0.6f32 * 10.0).sin()).abs() < 1e-3);
}
```

Keep `rotation_system_ignores_non_spin` and `bob_system_ignores_non_bob`, switched to a `Time` resource.

- [ ] **Step 2: Run** `cargo test -p melon_game` — fails.
- [ ] **Step 3: Implement** the two systems.
- [ ] **Step 4: Run** `cargo test -p melon_game` — all pass.
- [ ] **Step 5: Commit** `fix(game): make demo systems frame-rate independent`

### Task 5: Host wiring, interpolated draw, docs

**Files:**
- Modify: `crates/melon_host/src/main.rs` (`hot_game` imports, `App::new`, draw-list build, tests)
- Modify: `docs/ROADMAP.md`, `AGENTS.md`

**Interfaces:**
- Consumes: everything above.
- Produces: `fn render_matrix(t: &Transform, prev: Option<&PreviousTransform>, alpha: f32, playing: bool) -> Mat4` in `main.rs`: blended only when `playing` and `prev` is `Some`, else `t.to_mat4()`.

- [ ] **Step 1: Write failing tests** in `main.rs` `mod tests`:

```rust
#[test] fn render_matrix_paused_uses_current() { assert_eq!(render_matrix(&cur, Some(&prev), 0.0, false), cur.to_mat4()); }
#[test] fn render_matrix_playing_blends() { assert_eq!(render_matrix(&cur, Some(&prev), 0.0, true), prev.0.to_mat4()); }
#[test] fn render_matrix_without_prev_uses_current() { assert_eq!(render_matrix(&cur, None, 0.5, true), cur.to_mat4()); }
```

- [ ] **Step 2: Run** `cargo test -p melon_host` — fails.
- [ ] **Step 3: Implement.**
  - `hot_game` imports: `use melon_core::bevy_ecs::system::{Query, Res}; use melon_core::{Bob, Spin, Time, Transform, With};`
  - `App::new`: `core.add_systems(Update, fly_camera_system)`, `core.add_systems(Update, (hot_game::rotation_system, hot_game::bob_system).run_if(is_playing))`, `melon_physics::add_physics_systems(&mut core)`. Remove the old `core.schedule` calls and now-unused imports. `Core::new` already inserts `Time` and `SimulationState`; drop the duplicate inserts.
  - Draw-list build (query near `t.to_mat4()`): query `(Entity, &Transform, Option<&PreviousTransform>, &MeshRef)`, map through `render_matrix` with `Time::alpha()` and `SimulationState == Playing`.
  - `docs/ROADMAP.md`: tick the fixed-timestep item, mark bugs 2 and 3 fixed. `AGENTS.md`: replace the "Frame-rate dependence" gotcha with: register systems with `core.add_systems(Stage, …)`; fixed-rate logic in `FixedUpdate` (1/60 s), per-frame logic in `Update` reading `delta_seconds`; add `PreviousTransform` to entities moved in `FixedUpdate` for smooth rendering.
- [ ] **Step 4: Verify.** `just test` passes (gate + all tests). `cargo build -p melon_host` (hot-reload build) and `cargo build -p melon_host --release --no-default-features` both succeed.
- [ ] **Step 5: Commit** `feat(host): run systems in stages with interpolated rendering`
