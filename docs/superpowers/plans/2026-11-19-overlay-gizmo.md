# Overlay Pass and Unreal-Style Gizmo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A reusable GPU overlay pass for solid 3D editor shapes, and a thick Unreal-style transform gizmo (arrows, rings, cube tips, plane handles, centre handles) built on it.

**Architecture:** `melon_render::overlay` owns unit shape meshes, `OverlayDraw` and a pass that renders after tonemapping into a 4x MSAA target (scene-tested layer, then on-top layer) and composites onto the viewport's display image. `melon_editor::gizmo` becomes pure functions: build draws, pick handles, drag math. `build_gizmo` wires them to input; the host passes the draws to `render_scene`.

**Tech Stack:** Rust 2024, wgpu 29 (WGSL), glam 0.33, egui 0.35, bevy_ecs 0.19.

**Spec:** `docs/superpowers/specs/2026-11-19-overlay-gizmo-design.md`

## Global Constraints

- `just gate` before every commit, `just test` at the end of each task; Conventional Commits, no trailers or tool credits, commit on `main`.
- Shaders in `assets/shaders/*.wgsl`, loaded with `wgpu::include_wgsl!`. GPU assertions go in the single `gpu_tests` runner in `melon_render/src/lib.rs`.
- Colours: X `theme::colors::AXIS_X` (red), Y `AXIS_Y` (green), Z `AXIS_Z` (blue); hover / active yellow; centre handles light grey; plane handles semi-transparent in the colour of the axis they don't move along. Overlay colours are display (sRGB) colours, never tonemapped.
- Gizmo keeps constant screen size: `GIZMO_SIZE_PX` = 80 px shaft length. Snaps: `SNAP_MOVE` 0.25, `SNAP_SCALE` 0.25, `SNAP_ROTATE_DEG` 15; scale clamped to `SCALE_MIN..SCALE_MAX` (0.01..100).
- Every gizmo write goes through `set_world_transform`, `write_to_physics_body` and `snap_previous_transform`. No `Response` / `Context` calls inside `ctx.input(..)` closures.
- `melon_render` must not depend on `melon_editor`.

## Review Focus

- Gizmo seen with an axis pointing at the camera (shaft and cone degenerate on screen): no NaN draws, that axis not pickable by accident (test in Task 4).
- Entity under a rotated / scaled parent: gizmo sits at the world pose and drags write the right local values (test in Task 5).
- Viewport resized: overlay targets resize with the `ViewportTarget`, no size-mismatch panic (test in Task 2).
- Transparent plane handles over opaque shafts: plane handles never hide the shafts that pass behind them (alpha draws don't write depth, drawn after opaque; test in Task 2).
- Selected entity deleted mid-drag or zero-scale entity: no gizmo, no panic (test in Task 6).

---

### Task 1: Overlay shapes and draw types

**Files:**
- Create: `crates/melon_render/src/overlay.rs`
- Modify: `crates/melon_render/src/lib.rs` (`mod overlay; pub use overlay::{OverlayDepth, OverlayDraw, OverlayShape};`)

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum OverlayShape { Cylinder, Cone, Cube, Sphere, Torus, Quad, Triangle }`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum OverlayDepth { OnTop, SceneTested }`
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub struct OverlayDraw { pub shape: OverlayShape, pub transform: Mat4, pub color: [f32; 4], pub depth: OverlayDepth }`
  - `pub(crate) fn shape_mesh(shape: OverlayShape) -> (Vec<OverlayVertex>, Vec<u16>)` with `OverlayVertex { position: [f32; 3], normal: [f32; 3] }` (Pod).
  - Unit spaces: Cylinder radius 1, y 0..1; Cone base radius 1 at y 0, tip at y 1; Cube −0.5..0.5; Sphere radius 1; Torus major radius 1 in the XZ plane, minor radius 0.1 (thickness comes from the transform); Quad 0..1 in X and Z at y 0; Triangle (0,0,0),(1,0,0),(0,0,1). Normals point outward; Quad / Triangle are double-sided (both windings).

- [ ] **Step 1: Write the failing tests** (headless, in `overlay.rs`)

```rust
#[test] fn shape_bounds_match_unit_spaces()   // per shape: min/max of positions == documented box (Torus: x,z ±1.1, y ±0.1)
#[test] fn normals_are_unit_length()          // every normal length within 1e-4 of 1
#[test] fn indices_are_in_range()             // every index < vertex count, count % 3 == 0
```

- [ ] **Step 2: Run** `cargo test -p melon_render --lib overlay` — Expected: FAIL (module missing).
- [ ] **Step 3: Implement** the interfaces (segment counts are the builder's call; 24 around is a fine default).
- [ ] **Step 4: Run** `cargo test -p melon_render` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(render): overlay shape meshes and draw types`

### Task 2: Overlay pass

**Files:**
- Create: `assets/shaders/overlay.wgsl`, `assets/shaders/composite.wgsl`
- Modify: `crates/melon_render/src/overlay.rs`, `crates/melon_render/src/lib.rs` (`ViewportTarget`, `render_scene`, `FrameData`), `crates/melon_render/src/extract.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `FrameData` gains `pub eye: Vec3` (from `Camera::eye`, `Vec3::ZERO` without a camera).
  - `render_scene(&mut self, encoder, frame: &FrameData, server, target, world_lines: &[LineVertex], overlay: &[OverlayDraw])` — the `overlay_lines` parameter is replaced; `draw_lines` and its pipeline are deleted if nothing else uses them.
  - `ViewportTarget` owns the overlay targets: a 4x MSAA `Rgba8UnormSrgb` colour texture and a 1-sample resolve texture of the same size, recreated with the target.
- Pass order after tonemap (only when `overlay` is non-empty):
  1. Pass A: colour = overlay MSAA (clear transparent, store), depth = the scene's MSAA depth (load). Draw `SceneTested` opaque, then `SceneTested` transparent.
  2. Pass B: colour = overlay MSAA (load), resolve into the overlay resolve texture; depth = scene depth (clear 1.0). Draw `OnTop` opaque, then `OnTop` transparent.
  3. Composite: fullscreen triangle sampling the resolve texture onto the display view, premultiplied-alpha blending.
- Overlay pipeline: instance data per draw (model mat4, normal mat3 as three vec4, sRGB colour converted to linear in the shader), blend colour `SrcAlpha / OneMinusSrcAlpha`, alpha `One / OneMinusSrcAlpha` (so the target holds premultiplied colour); opaque variant writes depth, transparent variant (alpha < 1) doesn't; depth compare `Less`. Shading: `light = 0.45 + 0.55 * max(dot(n, normalize(eye - world_pos)), 0)` applied to RGB.

- [ ] **Step 1: Write the failing GPU assertions** (in `gpu_tests`, 64×64 target, `Camera::default()` view):
  - `overlay_on_top_exact_colour` — empty scene, one `OnTop` opaque `Quad` facing the camera filling the centre, colour `[1.0, 0.0, 0.0, 1.0]` with the quad's normal toward the camera: centre pixel red channel ≥ 250 and green, blue ≤ 5 (lit full, not tonemapped).
  - `overlay_scene_tested_hidden_behind_cube` — `builtin:cube` at the origin, a `SceneTested` quad 2 m behind it: centre pixel equals the no-overlay render; the same quad `OnTop`: centre pixel differs.
  - `overlay_transparent_does_not_hide_opaque_behind` — opaque blue quad behind a 50%-alpha red quad, both `OnTop`: centre pixel has blue > 60.
  - `overlay_targets_follow_resize` — render with a 64×64 target, then a 32×16 target: no panic, both render.
- [ ] **Step 2: Run** `cargo test -p melon_render` — Expected: FAIL (no overlay parameter).
- [ ] **Step 3: Implement** as above. Update the host call (`render_scene(.., &[])` until Task 6).
- [ ] **Step 4: Run** `cargo test -p melon_render` and `just gate` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(render): overlay pass for solid editor shapes`

### Task 3: Gizmo handles and draws

**Files:**
- Modify: `crates/melon_editor/src/gizmo.rs`

**Interfaces:**
- Consumes: Task 1 types.
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum GizmoHandle { Axis(usize), Plane(usize), Center }` (`Plane(i)` ⟂ axis i).
  - `pub struct GizmoPose { pub center: Vec3, pub rotation: Quat, pub size: f32 }` (`size` = world length of `GIZMO_SIZE_PX` at the gizmo's distance).
  - `pub fn gizmo_draws(mode: GizmoMode, pose: &GizmoPose, highlight: Option<GizmoHandle>) -> Vec<OverlayDraw>` — all `OnTop`.
  - Proportion constants (fractions of `size`, from the spec's pixels at 80 px): `SHAFT_RADIUS` 2.5/80, `CONE_RADIUS` 4.5/80, `CONE_LENGTH` 18/80, `TIP_CUBE` 10/80, `RING_THICKNESS` 3/80, `PLANE_OFFSET` 0.3, `PLANE_SIZE` 0.2, `CENTER_SIZE` 10/80; `HIGHLIGHT` yellow `[1.0, 0.85, 0.1, 1.0]`, `CENTER_COLOR` light grey, plane alpha 0.5.
  - `GizmoMode` moves from `lib.rs` to `gizmo.rs` (re-exported from `lib.rs`, same name).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn move_builds_shafts_cones_planes_center()  // Cylinder×3, Cone×3, Quad×3, Sphere×1
#[test] fn rotate_builds_three_rings()               // Torus×3
#[test] fn scale_builds_shafts_cubes_triangles_center() // Cylinder×3, Cube×4 (3 tips + centre), Triangle×3
#[test] fn highlighted_handle_is_yellow_others_keep_axis_colours() // Move, highlight Axis(1): its shaft+cone HIGHLIGHT, Axis(0) parts AXIS_X
#[test] fn plane_handles_are_translucent()           // Quad alpha == 0.5
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib gizmo` — Expected: FAIL.
- [ ] **Step 3: Implement** (axis i direction = `rotation * AXIS[i]`; Move/Scale shafts from centre to `size`; Rotate ring radius = `size`).
- [ ] **Step 4: Run** `cargo test -p melon_editor` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): Unreal-style gizmo handles as overlay draws`

### Task 4: Gizmo picking

**Files:**
- Modify: `crates/melon_editor/src/gizmo.rs`

**Interfaces:**
- Consumes: Task 3.
- Produces: `pub fn pick_handle(mode: GizmoMode, pose: &GizmoPose, origin: Vec3, dir: Vec3, tolerance: f32) -> Option<GizmoHandle>` — nearest hit along the ray against: capsules (shaft + cone / tip, radius widened by `tolerance` world units) for `Axis`, plane quads / triangles for `Plane`, sphere / cube for `Center`, and for Rotate a band (`| |hit − center| − size | < RING_THICKNESS + tolerance` on the ring's plane). `Center` wins over `Plane` where both hit. The caller passes `tolerance` = 4 px in world units.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn ray_through_each_handle_picks_it()       // per mode, per handle centre: Some(that handle)
#[test] fn ray_between_handles_picks_nothing()      // e.g. diagonal at 0.9*size between X and Y shafts in Move: None
#[test] fn center_wins_over_planes()
#[test] fn rotate_ring_picked_on_band_only()        // at radius size: Some(Axis(i)); at 0.5*size: None
#[test] fn axis_pointing_at_camera_is_finite()      // ray along +Z at a Move gizmo: result is Some(Center) or Some(Axis(2)), never a NaN panic; gizmo_draws(..) transforms all finite
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib gizmo` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p melon_editor` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): pick gizmo handles against their 3D shapes`

### Task 5: Drag math

**Files:**
- Modify: `crates/melon_editor/src/gizmo.rs`

**Interfaces:**
- Consumes: Tasks 3–4.
- Produces (pure, world space; `start` = the world `Transform` at press):
  - `pub fn drag_move_axis(start: Transform, axis: Vec3, press_hit: Vec3, hit: Vec3, snap: bool) -> Transform` (today's math)
  - `pub fn drag_move_plane(start: Transform, normal: Vec3, press_hit: Vec3, hit: Vec3, axes: [Vec3; 2], snap: bool) -> Transform` — each plane axis snapped separately.
  - `pub fn drag_move_center(start: Transform, press_hit: Vec3, hit: Vec3, snap: bool) -> Transform` — hits on the camera-facing plane through the centre.
  - `pub fn rotate_angle(center: Vec3, axis: Vec3, origin: Vec3, dir: Vec3) -> Option<f32>` — angle of the ray's hit on the plane ⟂ `axis`, `None` when `|dir·axis| < 0.1` (edge-on).
  - `pub fn drag_rotate(start: Transform, axis: Vec3, delta_angle: f32, snap: bool) -> Transform`
  - Edge-on fallback: `delta_angle = 0.01 * pointer_delta_px.dot(ring_screen_tangent)` (tangent at the grab point).
  - `pub fn drag_scale_axis(start: Transform, axis_index: usize, factor: f32, snap: bool) -> Transform` (today's ratio math gives `factor`)
  - `pub fn drag_scale_plane(start: Transform, axes: [usize; 2], factor: f32, snap: bool) -> Transform`
  - `pub fn uniform_scale_factor(pointer_delta_px: Vec2) -> f32` = `1 + 0.01 * (delta.x - delta.y)`, clamped so the resulting scale stays in `SCALE_MIN..SCALE_MAX` by the caller.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn rotate_sweep_of_90_degrees_turns_90()    // rays hitting (1,0,0) then (0,0,-1) around +Y: delta ≈ +π/2 (sign matches right-hand rule), drag_rotate gives rot_y(90°)
#[test] fn rotate_edge_on_returns_none()            // dir ⟂ axis: None
#[test] fn plane_move_changes_only_plane_axes()     // XZ plane: y unchanged
#[test] fn center_move_keeps_depth_from_camera()
#[test] fn plane_scale_leaves_third_axis()
#[test] fn uniform_scale_factor_100px_doubles()     // Vec2(100, 0) -> 2.0; Vec2(-50, 0) -> 0.5; Vec2(0, -100) -> 2.0 (up grows)
#[test] fn snapping_applies_to_each_drag()          // move 0.3 -> 0.25; rotate 20° -> 15°; scale ×1.3 -> nearest 0.25 step
#[test] fn drags_under_rotated_scaled_parent_write_world_pose() // via set_world_transform: child world matches the drag result
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib gizmo` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p melon_editor` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): gizmo drag math (plane, centre, uniform scale; rotate fixed)`

### Task 6: Wire it up, remove the line gizmo, docs

**Files:**
- Modify: `crates/melon_editor/src/lib.rs` (`build_gizmo`, `GizmoDrag`, `hovered_axis` → `hovered: Option<GizmoHandle>`, `take_gizmo_vertices` → `take_overlay_draws(&mut self) -> Vec<OverlayDraw>`), `crates/melon_editor/src/gizmo.rs` (delete unused line helpers: `cube_handle_lines`, `circle_lines`, `point_to_segment_dist_2d`, `compute_drag_plane`, `plane_basis`, `rotate_angle_in_plane` and their tests if nothing uses them), `crates/melon_host/src/main.rs`, `AGENTS.md`, `README.md`

**Interfaces:**
- Consumes: Tasks 2–5.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn take_overlay_draws_after_gizmo_frame()   // headless egui frame with a selected cube: take_overlay_draws() non-empty, all OnTop
#[test] fn no_gizmo_for_deleted_or_zero_scale_entity() // selected entity despawned / scale (0,1,1): no draws, no panic
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib` — Expected: FAIL.
- [ ] **Step 3: Implement.** `build_gizmo`: pose from `world_matrix`; hover via `pick_handle` (not while a drag is active or the pointer is off the viewport); press starts a drag keyed by handle; drag dispatches to Task 5 functions; highlight = active drag handle else hover. Host passes `take_overlay_draws()` to `render_scene`.
- [ ] **Step 4: Run** `just test` — Expected: PASS. Release build + screenshots via `melonwin.ps1` of Move, Rotate (E) and Scale (R) on a cube: thick coloured handles, plane squares / triangles, centre handle.
- [ ] **Step 5:** Docs: README shortcuts mention plane and centre handles; AGENTS.md gotcha: editor shapes go through `OverlayDraw` / `render_scene(.., overlay)`.
- [ ] **Step 6: Commit** `feat(editor): Unreal-style gizmo on the overlay pass` and `docs: overlay gizmo`
