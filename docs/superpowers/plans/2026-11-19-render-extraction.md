# Render Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move "what to draw" and the scene render passes from `melon_host` into `melon_render`, and scene Save / Load into `melon_editor`, with no visible change.

**Architecture:** `melon_render::extract(world) -> FrameData` turns the ECS world into plain draw data. `Renderer::render_scene` owns the `ModelCache` and records shadow, 3D and tonemap passes into a `ViewportTarget`; `Renderer::ui_pass` hands the editor its egui pass. `Editor::run_pending_operation` does File Save / Load / Exit. The host becomes a short call sequence.

**Tech Stack:** Rust 2024, wgpu 29, bevy_ecs 0.19, egui 0.35.

**Spec:** `docs/superpowers/specs/2026-11-19-render-extraction-design.md`

## Global Constraints

- `just gate` passes before every commit; `just test` at the end of each task. Conventional Commits, no trailers or tool credits, commit on `main`.
- Dependency direction `core <- render, physics <- editor <- host`: `melon_render` must not depend on `melon_editor` or `melon_physics`.
- No visible change: same `CLEAR_COLOR` (0.06, 0.07, 0.09, 1.0), `SHADOW_CENTER` = `Vec3::ZERO`, `SHADOW_RADIUS` = 20.0, MSAA 4x HDR then tonemap, world lines depth-tested, gizmo lines on top.
- Never replace `core.world`; tests in `#[cfg(test)] mod tests` at file bottom; GPU assertions go in the single `gpu_tests` runner in `melon_render/src/lib.rs`.

## Review Focus

- No `Camera` or `Light` resource: `extract` returns defaults, no panic (test in Task 1).
- Failed Load must leave the `PhysicsWorld` resource in the world (test in Task 3).
- Save / Load / Exit must still clear the pending operation so it runs once (test in Task 3).
- Gizmo and Inspector edits show in the same frame because extraction runs after the editor update (checked by screenshot in Task 4).
- An entity with a missing model file still draws the placeholder through `render_scene` (existing `ModelCache` GPU tests; Task 2 keeps them green).

---

### Task 1: `extract` and `FrameData`

**Files:**
- Create: `crates/melon_render/src/extract.rs`
- Modify: `crates/melon_render/src/lib.rs` (`mod extract; pub use extract::{FrameData, blends, extract};`)

**Interfaces:**
- Produces:
  - `pub struct FrameData { pub view_proj: Mat4, pub light: melon_core::Light, pub models: Vec<(Mat4, Handle<Model>)> }`
  - `pub fn extract(world: &mut World) -> FrameData` — `Camera::view_proj()` or `Mat4::default()`; `Light` or `Light::default()`; models from `melon_core::hierarchy::render_matrices(world, Time::alpha(), blends(state))` for every entity with `Handle<Model>` (no `Time` → alpha 1.0; no `SimulationState` → Editing).
  - `pub fn blends(state: SimulationState) -> bool` (moved from the host, same rule: not Editing).

- [ ] **Step 1: Write the failing tests** (in `extract.rs`)

```rust
#[test] fn one_model_entity_gives_one_entry()      // T(1,2,3) + builtin:cube handle: models.len()==1, matrix translation (1,2,3)
#[test] fn child_matrix_is_composed()              // parent T(5,0,0), child T(0,1,0) with handle: translation (5,1,0)
#[test] fn entity_without_model_is_left_out()      // Transform only: models empty
#[test] fn view_proj_comes_from_camera()           // Camera::default(): frame.view_proj == camera.view_proj()
#[test] fn missing_camera_and_light_give_defaults()// bare World: view_proj == Mat4::default(), light.ambient == Light::default().ambient
#[test] fn blends_while_playing_and_paused()       // moved from host
#[test] fn no_blend_while_editing()                // moved from host
```

- [ ] **Step 2: Run** `cargo test -p melon_render --lib extract` — Expected: FAIL (module missing).
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo test -p melon_render` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(render): extract the ECS world into FrameData`

### Task 2: `render_scene`, `ui_pass`, renderer-owned `ModelCache`

**Files:**
- Modify: `crates/melon_render/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 `FrameData`.
- Produces:
  - `Renderer` field `model_cache: ModelCache`, built in `Renderer::new` (the host's `model_cache` field goes in Task 4).
  - `pub fn render_scene(&mut self, encoder: &mut wgpu::CommandEncoder, frame: &FrameData, server: &mut AssetServer, target: &ViewportTarget, world_lines: &[LineVertex], overlay_lines: &[LineVertex])`
  - `pub fn ui_pass(&self, frame: &mut Frame) -> wgpu::RenderPass<'static>` (clear `CLEAR_COLOR`, no depth, `forget_lifetime()`).
  - `pub const CLEAR_COLOR: wgpu::Color`, `SHADOW_CENTER`, `SHADOW_RADIUS` move here from the host.

- [ ] **Step 1: Write the failing GPU assertion** (in `gpu_tests`): `render_scene_draws_a_cube` — `FrameData` with `Camera::default()`'s `view_proj` (camera at its default pose looking at the origin), `Light::default()`, one `builtin:cube` at the origin; render into `create_viewport_target(64x64)`; read back the display view with the existing readback helper; the centre pixel differs from the clear colour.
- [ ] **Step 2: Run** `cargo test -p melon_render` — Expected: FAIL (no `render_scene`).
- [ ] **Step 3: Implement.** Body = the host's current camera / light / model-uniform upload, `prepare` loop, draw list, shadow pass, 3D pass and tonemap pass (`main.rs` `RedrawRequested`), unchanged in behaviour. `ui_pass` = the host's egui pass descriptor.
- [ ] **Step 4: Run** `cargo test -p melon_render` — Expected: PASS (all GPU assertions).
- [ ] **Step 5: Commit** `feat(render): render_scene and ui_pass; renderer owns the model cache`

### Task 3: `Editor::run_pending_operation`

**Files:**
- Modify: `crates/melon_editor/src/lib.rs`

**Interfaces:**
- Produces: `pub fn run_pending_operation(&mut self, world: &mut World) -> bool` — takes the pending operation (runs once); Save → `scene::save_scene`, status "Saved to {path}" + `set_current_scene`, or error status "Save failed: {e}"; Load → `world.resource_scope::<PhysicsWorld, _>(..)` around `scene::load_scene` (insert a fresh `PhysicsWorld` first if missing), on success `clear_selection` + `set_current_scene` + status "Loaded from {path}", else "Load failed: {e}"; Exit → `save_layout()`, return `true`. Same wording as the host today.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn pending_save_writes_file_and_reports()   // temp path: file exists, status_kind == Some(Success), second call does nothing (returns false, no rewrite)
#[test] fn pending_load_failure_keeps_world_and_physics() // missing file: status_kind == Some(Error), entity count unchanged, PhysicsWorld resource still present
#[test] fn pending_exit_returns_true()
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib pending_` — Expected: FAIL.
- [ ] **Step 3: Implement** as above (a test-only setter for `pending_operation` is fine).
- [ ] **Step 4: Run** `cargo test -p melon_editor` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): run File Save / Load / Exit inside the editor`

### Task 4: Host rewired, docs

**Files:**
- Modify: `crates/melon_host/src/main.rs`, `docs/ROADMAP.md`, `AGENTS.md`

**Interfaces:**
- Consumes: Tasks 1–3.

- [ ] **Step 1:** Rewrite `RedrawRequested` to the spec's frame order: `core.update()` → `begin_frame` → editor update → `run_pending_operation` (exit if true) → viewport resize + aspect → `extract` → `render_scene(encoder, &frame_data, &mut server, target, world_lines, gizmo)` (take the `AssetServer` with `resource_mut`) → `ui_pass` + `editor.render` → `end_frame`. Delete the host's `blends`, consts, `model_cache`, `entity_matrices` and the old pass code; keep `map_key` and its tests.
- [ ] **Step 2: Run** `just test` — Expected: PASS. Release build + screenshot via `melonwin.ps1`: Ground, shadow, grid and gizmo look as before; `+ Add` then a Hierarchy click shows the selection box the same frame.
- [ ] **Step 3:** ROADMAP: tick the item, bug 7 fixed (note the stale "three UI stacks" text). AGENTS.md layout rows: `melon_render` gains "extract → `FrameData`, `render_scene`, `ui_pass`, owns `ModelCache`"; `melon_host` is "winit loop, input mapping, frame order".
- [ ] **Step 4: Commit** `refactor(host): frame is extract, render_scene, ui_pass` and `docs: render extraction`
