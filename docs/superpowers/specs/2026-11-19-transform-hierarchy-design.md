# Transform hierarchy

Roadmap Tier 0: "Transform hierarchy: `Parent` / `Children` + `GlobalTransform`
propagation". Also fixes bug 6 (glTF node tree flattened).

## Intent

Group entities under other entities like in Unity: move a car and its wheels
come with it. Drop a glTF model into the scene and get its node tree as real
entities you can select, move, delete and reparent, saved in the scene.

## Goals

- Entities can have a parent. A child's `Transform` is local to its parent;
  its world pose is the parent's world pose times its local transform.
- Every system that today treats `Transform` as world space uses the world
  pose instead: rendering (with interpolation), shadows, picking, selection
  box, F-framing, gizmos, physics.
- Hierarchy panel shows a collapsible tree. Drag an entity onto another to
  make it a child; drag it onto empty space to make it a root. The entity keeps
  its world pose (Unity behaviour).
- Deleting an entity deletes its whole subtree, physics bodies included.
- Scenes save parents (format version 3); versions 1 and 2 still load.
  Play / Stop keeps the hierarchy (the snapshot is scene data).
- Dropping a `.glb` / `.gltf` from the Content Browser spawns the file's node
  tree as entities ("unpack on drop").

## Non-goals

- Linked instances / prefabs, overrides, "update placed models when the file's
  node structure changes".
- Undo / Redo, duplicate, multi-select reparent, reordering siblings by drag.
- Skinning, animation, glTF cameras / lights (those nodes become plain
  transform entities).

## Anti-goals

- Reparenting makes an entity jump.
- A cycle (an entity under its own descendant), in the editor or in a file.
- Deleting a parent leaks rapier bodies of its children or panics.
- A hand-edited scene with a bad parent reference silently restructures the
  scene or drops entities.
- Children jitter while playing (interpolation broken along the chain).

## Approaches

| Decision | Options | Picked |
|---|---|---|
| Scope | A engine only; B A + editor tree and reparent; **C B + glTF nodes as entities** | **C** (user) |
| What a model drop creates | **1 unpack: entities saved like any other**; 2 linked instance rebuilt from the file (read-only children); 3 both with an Unpack command | **1** (user): smallest, fully editable; 2 can come later on top without breaking scenes |
| Parent components | **bevy_ecs 0.19 `ChildOf` / `Children`**; own `Parent` type | My call: built in, kept in sync by bevy, `linked_spawn` despawn |
| World pose | **`GlobalTransform` component propagated by a system, plus an on-demand helper** | My call: roadmap names it; the helper covers edits made mid-frame by the editor |

## Design

### `melon_core::hierarchy` (new)

- Re-export bevy's `ChildOf`, `Children`.
- `GlobalTransform(pub Mat4)` component: world matrix. Entities with a
  `Transform` get one (inserted by the propagation system if missing).
- `propagate_transforms_system`: walks roots (entities with `Transform` and no
  `ChildOf`) down through `Children`, writing `GlobalTransform`. Runs in
  `PostUpdate`, and in `FixedUpdate` before `sync_to_physics_system`.
- `world_matrix(world: &World, entity) -> Mat4`: same math computed on demand
  by walking `ChildOf` up. Used by editor code right after it changes a
  transform in the same frame.
- `set_parent_keep_world(world: &mut World, child, new_parent: Option<Entity>)
  -> Result<()>`: new local = inverse(new parent world) × child world,
  decomposed to translation / rotation / scale. Errors (and changes nothing)
  if `new_parent` is the child or one of its descendants. `None` makes it a
  root. Shear from non-uniform parent scale is lost in the decomposition
  (accepted, as in Unity).

### Rendering (host draw list)

- Each drawn entity's matrix = parent's interpolated world matrix × its own
  interpolated local matrix (`render_matrix` per link), memoised per frame.
  Entities without `PreviousTransform` use their current local, as today.
- Entities that only group (no `Handle<Model>`) draw nothing but still
  contribute their matrix to their children.
- Shadow pass uses the same matrices.

### Physics

Rigid bodies live in world space.

- Fixed bodies follow their parent: `sync_to_physics_system` writes the body
  pose from `GlobalTransform`. (The engine has no kinematic body type; adding
  one is out of scope.)
- Dynamic bodies are owned by physics and do not follow their parent:
  `sync_from_physics_system` writes local = inverse(parent world) × body pose
  (translation and rotation; scale kept).
- Bodies are created from the full world pose, translation and rotation
  (scene load, Play snapshot). Today they spawn from translation only.
- Collider half extents ignore parent scale, as they already ignore the
  entity's own scale.

### Deletion

- Deleting marks the entity and every descendant `PendingDelete`.
- `cleanup_deleted_system` removes every marked body / collider, then
  despawns. It must not double-despawn: `linked_spawn` already despawns
  children of a despawned parent.

### Scenes (format version 3)

- `SceneEntity` gains `parent: Option<u32>`: index into `entities`.
  `transform` is local. Saved depth-first (parents before children, sibling
  order kept); `scene_to_data` is reordered to do this.
- `apply_scene` despawns the old scene's entities. With `linked_spawn`,
  children may already be gone when their turn comes; it must not panic on
  them.
- Load is two-pass: spawn all, then attach parents. A parent index that is out
  of range, points at itself, or forms a cycle fails the load with an error
  naming the entity; the current scene stays as it was (same as other
  validation errors today).
- Versions 1 and 2 load with every entity at the root.

### glTF nodes as entities

- New handle path form `<file>#mesh<N>` (N = glTF mesh index): draws that
  mesh's primitives in mesh-local space, node transform not baked. Its bounds
  are mesh-local. A path without a fragment keeps today's behaviour (whole
  file, node transforms baked): v2 scenes and the Inspector Mesh field still
  use it.
- `AssetServer`: `full_path` and `poll_changes` use the file part, so all
  `car.glb#…` handles bump when `car.glb` changes. `ModelCache` loads the
  fragment's mesh (failure rules unchanged: warn once, keep previous, else
  placeholder cube).
- `melon_render` CPU-only reader (no GPU): the default scene's node tree as a
  list of `{ name, parent index, local translation / rotation / scale,
  mesh index }`.
- Drop (Scene or Hierarchy): a root entity named after the file stem at the
  drop point, with the file's root nodes as its children, recursively. Node
  name, or `Node <i>` if unnamed. A node with a mesh gets `Handle<Model>`
  `<file>#mesh<N>`; a node without one is a plain transform entity. Root is
  selected.
- If the file can't be read at drop time: spawn one entity with the
  whole-file handle (placeholder cube + one warning, as today), so hot reload
  can still fix it.
- Dropping a model onto a Hierarchy row makes the new root a child of that
  row's entity, at its local origin.
- A drop that would take the entity count past `MAX_ENTITIES` (1024) spawns
  nothing and shows a status warning.

### Editor

- **Hierarchy:** tree with expand / collapse arrows (leaves have none),
  indented. Drag a row onto another row: reparent keeping world pose. Drag onto
  the empty area below the rows: make root. An invalid drop (onto itself or a
  descendant) does nothing and shows a status warning. With a search query, the
  list shows matching entities flat.
- **Picking:** returns the entity that was hit (leaf), using its world matrix.
- **Gizmo:** drawn at the world pose, drags in world space, writes the result
  back as local through the parent's inverse.
- **Inspector:** shows and edits the local `Transform` (Unity behaviour). The
  Mesh field shows `car.glb › mesh2` for fragment handles.
- Selection box and F-framing use the world matrix.

## Tests (headless unless noted)

- Propagation: grandchild world = root × child × grandchild; moving the root
  moves the grandchild after one `PostUpdate`.
- `set_parent_keep_world`: world pose unchanged after reparenting under a
  rotated, scaled parent; reparent onto self / descendant errors and changes
  nothing; `None` makes a root with the same world pose.
- Render matrix chain: with interpolation, a child's matrix at alpha 0.5 equals
  interpolated parent × interpolated child (logic function, no GPU).
- Physics: a fixed child body follows its moved parent after one step; a
  dynamic child does not.
- Deletion: deleting a parent with a dynamic-body child removes both rapier
  bodies and despawns both entities without a panic.
- Scene: v3 round-trip keeps parents, local transforms and sibling order;
  v2 file loads with all roots; out-of-range / self / cyclic parent index
  errors and leaves the world unchanged.
- Play → Stop restores the hierarchy.
- glTF reader: the two-node test model gives two nodes with their
  translations and mesh index 0; drop spawns root + 2 children with
  `#mesh0` handles (logic function).
- AssetServer: `a.glb#mesh0` and `a.glb#mesh1` both bump when `a.glb` changes;
  `full_path` has no fragment.
- Picking a child under a translated parent hits at the world position.

## Review Focus (for the plan)

- Deleting a parent and one of its children in the same frame.
- Reparenting while playing, and a dynamic body under a parent moved by the
  gizmo or by game code.
- Entity cap: an unpacked model uses one uniform slot per node entity
  (`MAX_ENTITIES` = 1024); a too-large drop is refused, never overflows.
- Hot reload of an unpacked model whose file now has fewer meshes
  (`#mesh5` no longer exists): placeholder + warning, no panic.
- Zero scale on a parent (non-invertible matrix) during reparent or dynamic
  sync.

## My calls (correct any)

- bevy `ChildOf` / `Children`; `GlobalTransform` as a `Mat4`.
- Dynamic bodies ignore parent movement; fixed bodies follow it. No kinematic
  body type in this pass.
- Bodies spawn from the full world pose (rotation included).
- A model drop past the 1024-entity cap is refused with a warning.
- Inspector edits local values; gizmo works in world space.
- Drops always create a wrapper root named after the file.
- Bad parent references fail the scene load (no silent fix-up).
- Picking selects the leaf that was hit.
- Search shows a flat list of matches.

## Left to the builder

- Tree row look (indent size, arrow glyphs), drag highlight.
- Where the CPU glTF reader lives inside `melon_render`, and whether parsed
  files are shared between fragment handles of one file.
- Console / status wording.
