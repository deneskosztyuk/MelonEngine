# Editor Viewport Basics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Unity-style scene viewport: the 3D view renders inside its own panel, navigates like Unity, shows a grid/axes/selection box, picks by click, with MSAA 4x.

**Architecture:** Library-independent logic in `melon_editor/src/viewport.rs` (`ViewportInput`, `EditorCamera`, `pick`, line builders). The egui glue fills `ViewportInput` and shows the `ViewportTarget` texture. The host renders into `ViewportTarget` for both editor UIs.

**Tech Stack:** Rust 2024, wgpu 29, egui 0.35 + egui-wgpu, bevy_ecs 0.19, glam 0.33.

**Spec:** `docs/superpowers/specs/2026-11-19-viewport-basics-design.md`

## Global Constraints

- Work on `main`, one commit per task, Conventional Commits, no co-author or tool trailers.
- No new dependencies. `unsafe_code` stays denied.
- Gate per task: `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo test --workspace` green, fmt on touched files (the pre-existing diff at `melon_slint_poc.rs:127` stays).
- Values: MSAA 4 samples. Grid y = 0, 1 m spacing, ±50 m, every 10th line brighter. X axis red, Z axis blue, selection box orange. Default camera pivot (0,0,0), eye (0,4,8), fly speed 5 m/s. Fly speed ×1.2 per notch within 0.5–100 m/s. Dolly ×0.9 per notch, min distance 0.2. Shift ×3. Pitch clamp ±89°.
- Picking/selection bounds: unit cube ±0.5 under the entity `Transform`, for entities with `MeshRef`.

## Review Focus

1. **Zero-size viewport** (window minimized, panel squeezed): `ViewportTarget` must never be created 0×0. Test `viewport_pixel_size_never_zero` in Task 3.
2. **Drag that starts outside the viewport** (a right-drag beginning on the side panel) must not move the camera; a drag that starts inside keeps working when the cursor leaves it. Use the viewport widget's own drag response, not raw pointer state. Checked by reading in Task 4.
3. **Typing in the inspector** with the pointer over the viewport must not move the camera. Test in Task 2.
4. **F with a selection that no longer exists** (after Play/Stop rebuilt the scene) is a no-op. Test in Task 2.
5. **Left click on a gizmo handle** must drag the gizmo, not change the selection. Test in Task 4.

---

### Task 1: MSAA and depth-tested world lines in `melon_render`

**Files:** Modify `crates/melon_render/src/lib.rs` (reuses `assets/shaders/line.wgsl`; no new shader).

**Interfaces:**
- Produces:
  - `pub const MSAA_SAMPLES: u32 = 4;`
  - `HdrTarget` holds a 4-sample colour texture plus the existing single-sample resolve texture: `HdrTarget::msaa_view(&self) -> &wgpu::TextureView` (render here), `HdrTarget::view()` unchanged (resolve target, sampled by tonemap).
  - Scene depth targets (`Renderer::depth_view`, `ViewportTarget::depth_view`, `create_depth_target`) are 4-sample. Shadow map stays 1-sample.
  - Mesh pipeline: `multisample.count = MSAA_SAMPLES`.
  - New world-line pipeline: `line.wgsl`, `LineList`, target `HDR_FORMAT`, depth `Depth32Float` test `Less`, write off, 4 samples. `SceneRenderer::draw_world_lines(&mut self, pass: &mut wgpu::RenderPass<'_>, vertices: &[LineVertex], view_proj: Mat4)`, drawn inside the 3D pass. Existing `draw_lines` (no depth, display format) unchanged for the gizmo.
- The 3D pass descriptor everywhere (host + tests) uses `view: hdr.msaa_view()`, `resolve_target: Some(hdr.view())`.

- [ ] **Step 1: Failing test** in the GPU test runner (`gpu_tests` assertions): `hdr_target_is_multisampled`: the texture behind `create_hdr_target(size).msaa_view()` has `sample_count() == 4`; and the existing lit/shadow pixel assertions still hold with the MSAA pass (update `render_test_scene` to the new attachment form).
- [ ] **Step 2: Run** `cargo test -p melon_render` — fails to compile (`msaa_view` missing).
- [ ] **Step 3: Implement.** Keep the multisampled texture in `HdrTarget` (store it, not just the view). The world-line pipeline uses the frame bind group's camera layout like `line.wgsl` expects; if `line.wgsl`'s group 0 does not match the frame layout, give the world-line pipeline its own camera uniform buffer + bind group as `draw_lines` does.
- [ ] **Step 4: Run** `cargo test -p melon_render` (GPU tests need a display) and `cargo build -p melon_host` — pass. Update the host's 3D pass attachment so it compiles.
- [ ] **Step 5: Commit** `feat(render): MSAA 4x and depth-tested world lines`

### Task 2: Library-independent viewport core `melon_editor/src/viewport.rs`

**Files:** Create `crates/melon_editor/src/viewport.rs`; Modify `crates/melon_editor/src/lib.rs` (`pub mod viewport;`)

**Interfaces:**
- Produces:
  - `#[derive(Default, Clone, Copy, Debug)] pub struct ViewportInput { pub size: Vec2, pub pointer: Option<Vec2>, pub pointer_delta: Vec2, pub hovered: bool, pub left_down: bool, pub right_down: bool, pub middle_down: bool, pub left_clicked: bool, pub scroll: f32, pub alt: bool, pub shift: bool, pub keys: FlyKeys, pub f_pressed: bool, pub typing: bool }` and `#[derive(Default, Clone, Copy, Debug)] pub struct FlyKeys { pub w, a, s, d, q, e: bool }`. Units: viewport-local points; `scroll` in notches (+ = away from user / zoom in). `left_down`/`right_down`/`middle_down` mean "a drag that started on the viewport is held".
  - `#[derive(Clone, Copy, Debug)] pub struct EditorCamera { pub pivot: Vec3, pub distance: f32, pub yaw: f32, pub pitch: f32, pub fly_speed: f32 }`, `Default` = spec defaults; `fn eye(&self) -> Vec3`; `fn apply(&mut self, input: &ViewportInput, dt: f32)`; `fn write(&self, camera: &mut melon_core::Camera)` (sets eye, center = pivot, yaw, pitch; leaves fov/aspect/near/far); `fn frame(&mut self, center: Vec3, radius: f32)` (pivot = center, distance = radius / tan(fov/2) with the default fov, min 0.2).
  - `pub fn pick(world: &mut World, origin: Vec3, dir: Vec3) -> Option<Entity>`
  - `pub fn frame_target(world: &World, entity: Entity) -> Option<(Vec3, f32)>` (centre, bounding-sphere radius of the unit cube under the transform; `None` if the entity is gone).
  - `pub fn grid_lines() -> Vec<LineVertex>`, `pub fn selection_box_lines(t: &Transform) -> Vec<LineVertex>`
  - Look sensitivity: 0.0025 rad per point (current fly camera value).

- [ ] **Step 1: Failing tests** (all in `viewport.rs`):

```rust
#[test] fn default_camera_matches_spec() { /* eye ≈ (0,4,8), pivot 0 */ }
#[test] fn orbit_keeps_distance_to_pivot() { /* alt+left_down, pointer_delta (40,25), apply */ assert!((cam.eye().distance(cam.pivot) - d0).abs() < 1e-4); assert_ne!(cam.eye(), eye0); }
#[test] fn pan_moves_eye_and_pivot_equally() { /* middle_down, delta (30,0) */ assert!(((eye1 - eye0) - (piv1 - piv0)).length() < 1e-5); }
#[test] fn dolly_shrinks_distance_with_floor() { /* scroll +1 -> d*0.9; scroll +200 -> 0.2 */ }
#[test] fn fly_moves_along_view_and_shift_triples() { /* right_down + w, dt 0.1: moved 0.5 along forward; with shift: 1.5 */ }
#[test] fn right_scroll_changes_fly_speed_within_bounds() { /* +1 -> 6.0; +100 -> 100.0; -100 -> 0.5; distance unchanged */ }
#[test] fn pitch_clamps() { /* right_down, delta (0, -1e6) */ assert!(cam.pitch.abs() <= 89f32.to_radians() + 1e-6); }
#[test] fn no_change_when_typing_or_not_hovered() { /* typing=true + all inputs -> unchanged; hovered=false + no drag held -> unchanged */ }
#[test] fn pick_nearest_of_two_boxes() { /* boxes at z=0 and z=-3, ray from (0,0,5) toward -Z */ }
#[test] fn pick_miss_returns_none() {}
#[test] fn pick_rotated_scaled_box_exact() { /* scale (4,1,1) rotated 90° about Y: ray at x=0, z=1.9 hits; z=2.1 misses */ }
#[test] fn pick_ignores_entities_without_meshref() {}
#[test] fn frame_target_none_for_despawned_entity() {}
#[test] fn grid_lines_count_and_axis_colors() { /* 101 lines per direction = 404 vertices; a red vertex lies on z=0; a blue vertex on x=0 */ }
#[test] fn selection_box_has_12_edges() { assert_eq!(selection_box_lines(&Transform::default()).len(), 24); }
```
- [ ] **Step 2: Run** `cargo test -p melon_editor viewport::` — fails (module missing).
- [ ] **Step 3: Implement.** `apply` order: if `typing` return; F handled by the glue (it needs the world), not here. Right held: look by delta, fly by keys, scroll changes speed. Else Alt+left held: orbit. Else middle held: pan (scale by distance so pan feels constant on screen). Else hovered: scroll dollies. Look/fly keep `pivot = eye + forward * distance`.
- [ ] **Step 4: Run** `cargo test -p melon_editor` — all pass.
- [ ] **Step 5: Commit** `feat(editor): library-independent viewport camera, picking and lines`

### Task 3: Render into the viewport panel (egui + host)

**Files:** Modify `crates/melon_editor/src/lib.rs` (`Editor`, `EditorOverlay`, central panel), `crates/melon_host/src/main.rs` (render path), `crates/melon_render/src/lib.rs` only if `ViewportTarget` needs a public `color_view` accessor change.

**Interfaces:**
- Consumes: `MSAA` attachment form (Task 1).
- Produces:
  - `pub fn viewport_pixel_size(size_points: Vec2, pixels_per_point: f32) -> winit::dpi::PhysicalSize<u32>` in `viewport.rs` (round, each side ≥ 1).
  - `Editor::requested_viewport_size(&self) -> PhysicalSize<u32>` (from the last central-panel rect).
  - `EditorOverlay::set_viewport_target(&mut self, device: &wgpu::Device, target: &ViewportTarget)`: registers the colour view with `register_native_texture` (Linear filter) the first time, `update_egui_texture_from_wgpu_texture` after.
  - The central panel shows the texture with `egui::Image` sized to the panel, `Sense::click_and_drag()`; the response is kept for Task 4.
  - Host: the egui path renders like the imgui path (shadow → 3D into `ViewportTarget` HDR → tonemap into its colour → gizmo lines on it → UI pass clears the swapchain and draws egui). After the frame, if `requested_viewport_size` changed: recreate the target, `set_viewport_target`, set camera aspect.

- [ ] **Step 1: Failing test** in `viewport.rs`: `viewport_pixel_size_never_zero` (`(0,0)` at 2.0 → 1×1) and `viewport_pixel_size_scales_by_ppp` (`(400.5, 300)` at 2.0 → 801×600).
- [ ] **Step 2: Run** `cargo test -p melon_editor viewport_pixel` — fails.
- [ ] **Step 3: Implement** the items above. Remove the `cfg(feature = "imgui-poc")` split for the scene targets in the host: both UIs now render into the `ViewportTarget`.
- [ ] **Step 4: Run** tests, clippy, `cargo build -p melon_host`, `cargo check -p melon_host --features imgui-poc`. Launch the editor and screenshot: the scene is inside the panel, centred, not under the side panel.
- [ ] **Step 5: Commit** `feat(editor): render the scene into the viewport panel`

### Task 4: Viewport interaction, overlays, removal of the fly camera

**Files:** Modify `crates/melon_editor/src/lib.rs` (`Editor`, `build_gizmo`, top bar), `crates/melon_core/src/input.rs` + `lib.rs` (remove `fly_camera_system`), `crates/melon_host/src/main.rs` (registration, world-line drawing)

**Interfaces:**
- Consumes: Task 2 core, Task 3 panel response.
- Produces:
  - `pub enum LeftAction { Orbit, Gizmo, Select, None }` and `pub fn left_action(alt: bool, on_gizmo_handle: bool, clicked: bool) -> LeftAction` in `viewport.rs`.
  - `Editor` owns `camera: EditorCamera`; each frame: build `ViewportInput` from egui (pointer relative to the image rect, drag state from the image response, `typing = ctx.text_edit_focused()`), `apply`, `write` into the `Camera` resource. F: `frame_target` of the selection → `EditorCamera::frame`.
  - Gizmo hover/drag in viewport-local points with `size` = image size.
  - `Editor::take_world_line_vertices(&mut self) -> Vec<LineVertex>` = `grid_lines()` + `selection_box_lines` of the selection; the host draws them with `draw_world_lines` in the 3D pass.
  - Overlays painted on the image: FPS and frame time (top-right, exponential smoothing factor 0.1), orientation indicator (three 30-point lines X red / Y green / Z blue from the camera rotation, top-right below the FPS).
  - Top bar: remove the "Frame N" label.
  - Remove `fly_camera_system` (+ its tests) from `melon_core::input`, its export, and its host registration. `Input` and `KeyCode` stay.

- [ ] **Step 1: Failing tests** in `viewport.rs`:

```rust
#[test] fn alt_wins_over_gizmo_and_click() { assert_eq!(left_action(true, true, true), LeftAction::Orbit); }
#[test] fn gizmo_handle_wins_over_select() { assert_eq!(left_action(false, true, true), LeftAction::Gizmo); }
#[test] fn plain_click_selects() { assert_eq!(left_action(false, false, true), LeftAction::Select); }
#[test] fn no_click_no_action() { assert_eq!(left_action(false, false, false), LeftAction::None); }
```
- [ ] **Step 2: Run** `cargo test -p melon_editor left_action` — fails.
- [ ] **Step 3: Implement** the items above. Select: `pick` with `Camera::screen_to_world_ray(pointer, size)`; hit selects, miss clears the selection.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, host builds. Launch and screenshot: grid, axis lines, orientation indicator, FPS visible; default view looks down at the ground from (0,4,8).
- [ ] **Step 5: Commit** `feat(editor): Unity viewport controls, click select, grid and overlays`

### Task 5: Docs

**Files:** `README.md` (Controls), `AGENTS.md`, `docs/ROADMAP.md`

- [ ] **Step 1:** README Controls: replace the WASD/Space/Shift/right-drag lines with the Unity table from the spec (right drag look, right+WASD fly, Q/E down/up, Shift faster, right+scroll speed, Alt+left orbit, middle pan, scroll dolly, F frame, left click select). AGENTS.md: `viewport.rs` holds library-independent viewport logic; UI glue only fills `ViewportInput`; scene renders into `ViewportTarget`. ROADMAP: add an "Editor" section: [x] viewport basics; [ ] pick the UI library (egui / Dear ImGui / Slint); [ ] layout redesign (docking, menus, toolbar); [ ] Game view; [ ] gizmo upgrades (plane handles, local/world).
- [ ] **Step 2:** `cargo test --workspace`, clippy, `cargo build -p melon_host --release --no-default-features`.
- [ ] **Step 3: Commit** `docs: viewport controls and editor roadmap`
