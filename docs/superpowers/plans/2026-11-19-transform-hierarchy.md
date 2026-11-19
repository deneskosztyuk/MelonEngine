# Transform Hierarchy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Parent/child entities with world-pose propagation everywhere, an editor tree with drag reparenting, scene format v3, and glTF files unpacked into entity trees on drop.

**Architecture:** bevy_ecs 0.19 `ChildOf` / `Children` hold the tree. A new `melon_core::hierarchy` module owns all world-pose math (`GlobalTransform`, propagation, on-demand world matrix, reparent keeping world pose, interpolated render matrices, subtree delete). Physics, renderer, editor and scenes switch from `Transform` to those helpers. glTF meshes get `<file>#mesh<N>` handle paths, and a CPU-only node reader in `melon_render` lets the editor spawn a file's node tree.

**Tech Stack:** Rust 2024, bevy_ecs 0.19, glam 0.33, rapier3d 0.34, egui 0.35, gltf 1.4.

**Spec:** `docs/superpowers/specs/2026-11-19-transform-hierarchy-design.md`

## Global Constraints

- `just gate` (fmt-check + clippy `-D warnings` + check) passes before every commit; `just test` at the end of each task.
- `unsafe_code` denied. Tests in `#[cfg(test)] mod tests` at the bottom of each file. ECS systems tested with a `World` + one-system `Schedule`.
- `melon_game` depends only on `melon_core`. Dependency direction `core <- render, physics <- editor <- host`.
- Never replace `core.world`; never `World::clear_entities()`.
- Delete physics entities through `PendingDelete`, never despawn them directly.
- Conventional Commits; no `Co-Authored-By` or tool credits. Commit straight to `main`.
- Colours / sizes in editor UI from `theme::colors` / `theme::sizes`. No placeholder controls. No `Response` / `Context` calls inside `ctx.input(..)` / `ui.input(..)` closures.
- `MAX_ENTITIES` = 1024 model uniform slots.
- Scene paths stay project-relative strings; never store handle ids or entity ids, parents are indices into the file's `entities` list.

## Review Focus

- Deleting a parent and one of its children in the same frame: both bodies removed, no panic (test in Task 2).
- Dynamic body under a parent moved by the gizmo while playing: the child stays put in world space (test in Task 2).
- Unpacked model larger than the free uniform slots: drop refused with a warning, nothing spawned (test in Task 7).
- Hot reload where `car.glb#mesh5` no longer exists: placeholder cube + one warning, no panic (GPU test in Task 6).
- Zero scale on a parent: `set_parent_keep_world` and dynamic sync must not write NaN (tests in Tasks 1 and 2).

---

### Task 1: `melon_core::hierarchy`

**Files:**
- Create: `crates/melon_core/src/hierarchy.rs`
- Modify: `crates/melon_core/src/lib.rs` (module, re-exports, register propagation in `PostUpdate`), `crates/melon_core/src/components.rs` (`Transform::from_mat4`)

**Interfaces:**
- Produces:
  - `pub use bevy_ecs::hierarchy::{ChildOf, Children};` from `melon_core`.
  - `#[derive(Component, Clone, Copy, Debug, PartialEq)] pub struct GlobalTransform(pub Mat4);` (`Default` = identity).
  - `Transform::from_mat4(m: Mat4) -> Transform` (`to_scale_rotation_translation`).
  - `pub fn propagate_transforms_system(world: &mut World)` — exclusive system: for every entity with `Transform`, writes / inserts `GlobalTransform` = parent world × local, walking roots down through `Children`.
  - `pub fn world_matrix(world: &World, entity: Entity) -> Mat4` — walks `ChildOf` up; identity for entities without `Transform`.
  - `pub fn set_world_transform(world: &mut World, entity: Entity, world_pose: Transform)` — writes local = inverse(parent world) × world_pose.
  - `pub fn set_parent_keep_world(world: &mut World, child: Entity, new_parent: Option<Entity>) -> anyhow::Result<()>`.
  - `pub fn depth_first(world: &mut World) -> Vec<(Entity, usize)>` — every entity with `Name` + `Transform` and no `PendingDelete`, roots in query order, children in `Children` order; usize = depth.
  - `pub fn mark_for_delete(world: &mut World, entity: Entity)` — inserts `PendingDelete` on the entity and every descendant.
  - `pub fn render_matrices(world: &mut World, alpha: f32, blend: bool) -> HashMap<Entity, Mat4>` — world matrix per `Transform` entity, each link interpolated with its `PreviousTransform` when `blend` (same rule as today's host `render_matrix`), memoised.
  - `pub fn safe_inverse(m: Mat4) -> Mat4` — inverse, or identity when `m.determinant().abs() < 1e-12`.

- [ ] **Step 1: Write the failing tests** (in `hierarchy.rs`)

```rust
#[test] fn grandchild_world_is_product_of_chain()
// root T(1,0,0), child rot_y(90°), grandchild T(0,0,2):
// world_matrix(grandchild).transform_point3(ZERO) ≈ (3,0,0)  [1 + rot_y90*(0,0,2) = (1+2,0,0)]
#[test] fn propagation_follows_moved_root()
// run propagate once, move root to (5,0,0), run again:
// GlobalTransform(grandchild).0.w_axis.truncate() ≈ (7,0,0)
#[test] fn reparent_keeps_world_pose()
// parent at (2,0,0), rot_y(45°), scale 2; child world pose (1,1,1) at root
// set_parent_keep_world(child, Some(parent)) is Ok; world_matrix(child) ≈ before (abs diff < 1e-4)
#[test] fn reparent_to_none_keeps_world_pose_and_removes_childof()
#[test] fn reparent_onto_self_or_descendant_errors_and_changes_nothing()
// both calls Err; ChildOf and Transform of both entities unchanged
#[test] fn reparent_under_zero_scale_parent_writes_finite_values()
// parent scale (0,1,1): Ok or Err both fine, but child Transform stays finite
#[test] fn depth_first_lists_parents_before_children_in_sibling_order()
// root A, children B then C, B has D: [(A,0),(B,1),(D,2),(C,1)]
#[test] fn mark_for_delete_marks_subtree_only()
#[test] fn render_matrices_interpolate_each_link()
// parent prev T(0,0,0) cur T(2,0,0); child prev T(0,0,0) cur T(0,2,0); alpha 0.5, blend:
// child translation ≈ (1,1,0); blend false ≈ (2,2,0)
```

- [ ] **Step 2: Run** `cargo test -p melon_core --lib hierarchy` — Expected: FAIL (module missing).
- [ ] **Step 3: Implement** the interfaces above. `set_parent_keep_world` refuses when `new_parent == child` or `new_parent` is a descendant (`Err("can't parent X under itself or its child")`), then computes the new local through `safe_inverse`, writes it, and inserts / removes `ChildOf`. `Core::new` adds `hierarchy::propagate_transforms_system` to `PostUpdate`.
- [ ] **Step 4: Run** `cargo test -p melon_core` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): transform hierarchy, world matrices and reparenting`

### Task 2: Physics in world space, subtree delete

**Files:**
- Modify: `crates/melon_physics/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 `GlobalTransform`, `propagate_transforms_system`, `safe_inverse`, `ChildOf`.
- Produces: `spawn_dynamic_cuboid(&mut self, translation: Vec3, rotation: Quat, half_extents: Vec3)` and `spawn_fixed_cuboid(...)` with the same new `rotation` parameter (update every caller, including tests).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn fixed_child_body_follows_moved_parent()
// parent at (0,0,0) with fixed child at local (1,0,0); move parent to (5,0,0); run one FixedUpdate chain:
// rapier body translation ≈ (6,0,0)
#[test] fn dynamic_child_ignores_parent_motion()
// dynamic child (gravity off or zero dt) under parent; move parent +5 x; step:
// rapier body translation unchanged; after sync_from_physics the child's world_matrix translation equals the body's
#[test] fn dynamic_sync_under_zero_scale_parent_stays_finite()
#[test] fn deleting_parent_and_child_same_frame_removes_both_bodies()
// parent + dynamic child, both marked PendingDelete (mark_for_delete): run cleanup in PostUpdate:
// physics.bodies.len() == 0, both entities gone, no panic
#[test] fn deleting_only_parent_with_mark_for_delete_removes_child_body()
```

- [ ] **Step 2: Run** `cargo test -p melon_physics` — Expected: FAIL.
- [ ] **Step 3: Implement.** `add_physics_systems`: FixedUpdate chain becomes `store_previous_transforms_system, propagate_transforms_system, sync_to_physics_system, step_system, sync_from_physics_system`. `sync_to_physics_system` reads `GlobalTransform` (decompose) for fixed bodies. `sync_from_physics_system` writes local = `safe_inverse(parent GlobalTransform)` × body pose for children (roots unchanged), keeping local scale. `cleanup_deleted_system` removes every marked body / collider, then despawns only marked entities whose parent is not also marked (`linked_spawn` takes the rest).
- [ ] **Step 4: Run** `cargo test -p melon_physics` and `cargo check --workspace` — Expected: PASS / no errors.
- [ ] **Step 5: Commit** `feat(physics): bodies follow world poses; delete removes whole subtrees`

### Task 3: Host draws world matrices

**Files:**
- Modify: `crates/melon_host/src/main.rs` (draw list), `crates/melon_render/src/lib.rs` (hoist `pub const MAX_ENTITIES: usize = 1024;` to crate level)

**Interfaces:**
- Consumes: Task 1 `render_matrices`.
- Produces: `melon_render::MAX_ENTITIES`.

- [ ] **Step 1:** No new test: `render_matrices_interpolate_each_link` (Task 1) covers the math. Replace the host's `render_matrix` / per-entity `t.to_mat4()` with one `render_matrices(&mut world, alpha, blends(state))` call, looking up each `Handle<Model>` entity. Delete `render_matrix` (now unused). The shadow pass already shares `draw_calls`.
- [ ] **Step 2: Run** `just gate` — Expected: PASS. Then release build screenshot via the scratchpad `melonwin.ps1`: Ground and cube render as before.
- [ ] **Step 3: Commit** `feat(host): draw entities at their world matrices`

### Task 4: Editor viewport and gizmo in world space

**Files:**
- Modify: `crates/melon_editor/src/viewport.rs` (`pick`, `frame_target`, `selection_box_lines`), `crates/melon_editor/src/lib.rs` (`build_gizmo`, `after_transform_edit`, `write_to_physics_body` callers, selection-box caller, `delete_selected`, Hierarchy right-click delete)

**Interfaces:**
- Consumes: Task 1 `world_matrix`, `set_world_transform`, `mark_for_delete`.
- Produces: `selection_box_lines(world: Mat4, bounds: (Vec3, Vec3)) -> Vec<LineVertex>` (was `&Transform`).

- [ ] **Step 1: Write the failing tests**

```rust
// viewport.rs
#[test] fn pick_child_at_world_position()
// parent T(10,0,0), child at local (0,0,0) with builtin:cube handle; ray down from (10,5,0) hits child; from (0,5,0) None
#[test] fn frame_target_uses_world_position()   // centre ≈ (10,0,0)
// lib.rs
#[test] fn delete_selected_marks_subtree()
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib` — Expected: the three new tests FAIL.
- [ ] **Step 3: Implement.** `pick` / `frame_target` / selection box use `world_matrix`. Gizmo: start pose = `Transform::from_mat4(world_matrix(e))`; axes, handle position and drag math stay in world space; each drag writes via `set_world_transform`, then `write_to_physics_body` with the world translation / rotation. `after_transform_edit` sends the world pose to physics. Deletes go through `mark_for_delete`.
- [ ] **Step 4: Run** `cargo test -p melon_editor` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): picking, framing and gizmos use world poses`

### Task 5: Scene format v3

**Files:**
- Modify: `crates/melon_editor/src/scene.rs`, `crates/melon_editor/src/play.rs` (test only)

**Interfaces:**
- Consumes: Task 1 `depth_first`, `world_matrix`, `ChildOf`; Task 2 spawn signatures.
- Produces: `SCENE_VERSION = 3`; `SceneEntity.parent: Option<u32>` (`#[serde(default)]`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn v3_roundtrip_keeps_parents_local_transforms_and_order()
// Car > (WheelA, WheelB), WheelA local (1,0,0): save→apply: same names under Car, WheelA before WheelB, local (1,0,0)
#[test] fn v2_scene_loads_all_roots()
#[test] fn bad_parent_index_fails_and_keeps_world()
// three cases: parent 99 (out of range), parent == own index, cycle 0->1->0:
// apply_scene is Err with the entity name in the message; previous entity count unchanged
#[test] fn reapplying_scene_with_children_does_not_panic()
#[test] fn child_body_spawns_at_world_pose()
// parent T(5,0,0) rot_y(90°), child fixed body local (1,0,0): rapier translation ≈ (5,0,-1), rotation ≈ rot_y(90°)
// play.rs
#[test] fn stop_restores_hierarchy()
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib scene play` — Expected: FAIL.
- [ ] **Step 3: Implement.** `scene_to_data` iterates `depth_first` and writes `parent` as the index of the parent's row. `apply_scene` validates parent indices (range, self, cycles) in phase 1; phase 2 despawns old entities with `get_entity_mut(..).map(|e| e.despawn())` (skip ones already gone), spawns all entities without bodies, attaches `ChildOf` in file order, then creates bodies from `world_matrix` (translation + rotation).
- [ ] **Step 4: Run** `cargo test -p melon_editor` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): scene format v3 saves the hierarchy`

### Task 6: Mesh fragments and the glTF node reader

**Files:**
- Modify: `crates/melon_core/src/assets.rs`, `crates/melon_render/src/lib.rs`, `crates/melon_render/src/models.rs`

**Interfaces:**
- Produces:
  - `melon_core::assets::split_mesh_fragment(path: &str) -> (&str, Option<usize>)` — `"a.glb#mesh2"` → `("a.glb", Some(2))`; anything else → `(path, None)`.
  - `AssetServer::full_path` and modification polling use the file part.
  - `pub struct GltfNode { pub name: Option<String>, pub parent: Option<usize>, pub translation: Vec3, pub rotation: Quat, pub scale: Vec3, pub mesh: Option<usize> }` and `pub fn gltf_nodes(path: &Path) -> Result<Vec<GltfNode>>`, `pub fn gltf_nodes_from_slice(bytes: &[u8]) -> Result<Vec<GltfNode>>` in `melon_render` (CPU only, `gltf::Gltf`, default scene or first scene, depth-first, parent before child).
  - `SceneRenderer::load_gltf_mesh(&self, path: &str, mesh: usize) -> Result<LoadedModel>` — that mesh's primitives, node transform not baked (`add_primitive` with `Mat4::IDENTITY`); errors if the index doesn't exist.
  - `ModelCache::prepare` loads fragment handles with `load_gltf_mesh`.

- [ ] **Step 1: Write the failing tests**

```rust
// assets.rs
#[test] fn split_mesh_fragment_parses_only_mesh_fragments()
// "a.glb#mesh2" -> ("a.glb", Some(2)); "a.glb" -> ("a.glb", None); "a.glb#meshX" -> ("a.glb#meshX", None)
#[test] fn fragments_bump_with_their_file()     // a.glb#mesh0 and a.glb#mesh1 both version 1 after touch
#[test] fn full_path_drops_fragment()
// melon_render lib.rs (headless, not in the GPU runner)
#[test] fn gltf_nodes_reads_two_node_tree()
// TWO_NODE_GLTF: 2 nodes, parent None, translations (-2,0,0) and (3,0,0), mesh Some(0)
// GPU runner assertions
// load_gltf_mesh(TWO_NODE file, 0): 1 mesh, bounds (0,0,0)-(1,1,0) (not baked)
// ModelCache on "<file>#mesh5": placeholder cube, one warning, no panic
```

- [ ] **Step 2: Run** `cargo test -p melon_core --lib assets` and `cargo test -p melon_render` — Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo test -p melon_core` and `cargo test -p melon_render` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(render): glTF mesh fragments and node tree reader`

### Task 7: Unpack models on drop

**Files:**
- Modify: `crates/melon_editor/src/lib.rs` (`spawn_model`, viewport / hierarchy drop sites, Inspector Mesh label)

**Interfaces:**
- Consumes: Task 6 `gltf_nodes`, fragment paths; Task 3 `MAX_ENTITIES`; Task 1 `ChildOf`.
- Produces: `pub fn spawn_model(world: &mut World, project_root: &Path, path: &str, at: Vec3, parent: Option<Entity>) -> Result<Entity, String>` — `Err` = status warning text, nothing spawned.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn unpacked_drop_spawns_root_and_node_children()
// TWO_NODE_GLTF written to temp project assets/models/two.gltf:
// root "two" at `at`, 2 children "Node 0" / "Node 1" with local (-2,0,0) / (3,0,0),
// handles "assets/models/two.gltf#mesh0"; root has no Handle<Model>
#[test] fn unreadable_file_spawns_single_whole_file_entity()
#[test] fn drop_onto_row_parents_new_root()      // parent Some(e): root's ChildOf is e, local translation ZERO
#[test] fn drop_past_entity_cap_is_refused()
// world already holds MAX_ENTITIES - 1 Handle<Model> entities: Err, entity count unchanged
#[test] fn mesh_label_shows_fragment()           // "assets/models/car.glb#mesh2" -> "car.glb › mesh2"
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib` — Expected: FAIL.
- [ ] **Step 3: Implement.** Root named by file stem; nodes by glTF name or `Node <i>`; `#mesh<N>` handles only on nodes with meshes. Cap check: count existing `Handle<Model>` entities + nodes with meshes > `MAX_ENTITIES` → `Err`. Drop sites show `Err` through `set_status_warning`. The Inspector Mesh combo shows `model_label` with ` › meshN` for fragments.
- [ ] **Step 4: Run** `cargo test -p melon_editor` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(editor): dropped glTF models unpack into entity trees`

### Task 8: Hierarchy tree panel and docs

**Files:**
- Modify: `crates/melon_editor/src/lib.rs` (`build_hierarchy`), `crates/melon_editor/src/layout.rs` (if helpers fit there), `AGENTS.md`, `docs/ROADMAP.md`, `README.md` (only if it describes the Hierarchy)

**Interfaces:**
- Consumes: Task 1 `depth_first`, `set_parent_keep_world`; Task 7 `spawn_model`.
- Produces: `pub fn hierarchy_drop(world: &mut World, dragged: Entity, target: Option<Entity>) -> Result<(), String>` — reparent keeping world pose, then pushes the world pose to the entity's physics body; `Err` = status warning.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn hierarchy_drop_onto_entity_reparents_keeping_world()
#[test] fn hierarchy_drop_onto_empty_space_makes_root()
#[test] fn hierarchy_drop_onto_descendant_is_refused()   // Err, tree unchanged
#[test] fn hierarchy_drop_moves_fixed_body_to_world_pose()
```

- [ ] **Step 2: Run** `cargo test -p melon_editor --lib hierarchy_drop` — Expected: FAIL.
- [ ] **Step 3: Implement.** Tree rows from `depth_first`, indented by depth, collapse arrow on entities with `Children` (state in `egui::collapsing_header::CollapsingState` or an editor `HashSet<Entity>`), collapsed subtrees skipped. Each row is a `dnd_drag_source` with an `Entity` payload and a drop target for `Entity` (→ `hierarchy_drop(Some(row))`) and `String` model paths (→ `spawn_model(.., parent: Some(row))`). The empty area below the rows takes `Entity` → `hierarchy_drop(None)` and `String` → root model drop. With a non-empty search query, rows are the matching entities flat (no indent, no drag). Docs: ROADMAP item ticked, bug 6 marked fixed; AGENTS.md gets a "Hierarchy" gotcha (local `Transform`, use `melon_core::hierarchy` helpers for world poses, delete with `mark_for_delete`, scene v3).
- [ ] **Step 4: Run** `just test` — Expected: PASS. Release build: drag Ground onto another entity in the Hierarchy via a screenshot check where possible; otherwise leave to the user's dev-build check.
- [ ] **Step 5: Commit** `feat(editor): Hierarchy tree with drag reparenting` and `docs: transform hierarchy`
