# Physics: shapes, queries, events, character controller, joints (ROADMAP priority 4)

## Intent

Make physics usable for real games, from the editor and from game code, matching what Unity (Rigidbody, Colliders, Physics.Raycast, OnTrigger/OnCollision, CharacterController, Joints), Godot (RigidBody3D, CollisionShape3D, ray queries, body_entered signals, CharacterBody3D.move_and_slide, Joint3D) and Unreal offer as the shared core.

## Today

`melon_physics` wraps rapier3d 0.34. Only cuboids. A body exists only as the scene file's special `body_info` (type + half extents) spawned by `scene.rs::spawn_body`; `RigidBodyComponent` / `ColliderComponent` hold rapier handles. Not editable in the Inspector, not usable from `melon_game` (which may depend only on `melon_core`).

## Goals

1. **Data components, registered in the component registry** (so the Inspector, Add Component and scene files handle them with no special code):
   - `RigidBody { kind: Dynamic | Fixed | Kinematic, mass, linear_damping, angular_damping, gravity_scale, lock_rotation (per axis), ccd }`
   - `Collider { shape: Box { half_extents } | Sphere { radius } | Capsule { half_height, radius } | Mesh (trimesh from the entity's model), friction, restitution, sensor (trigger), layers: membership + filter bit masks }`
   - A collider without a `RigidBody` is static (Unity / Godot behaviour). Several colliders per body: colliders on child entities attach to the nearest ancestor body.
2. **Sync systems** create / update / remove rapier bodies and colliders when these components are added, changed or removed (and on `PendingDelete`). Rapier handles become internal bookkeeping. Changing a component in the Inspector updates the live body.
3. **Scene format v6.** Old `body_info` (v1-v5) still loads, converted to `RigidBody` + `Collider { Box }`. `SCENE_VERSION` bumped, old versions keep loading.
4. **Queries for game code:** ray cast (first hit and all hits), shape cast (sphere), overlap (sphere, box), each with a layer filter and an optional excluded entity, returning entity, point, normal, distance. Usable from `melon_game` systems.
5. **Events for game code:** collision started / ended and trigger entered / exited, with both entities, readable by systems (bevy `Message`s or the 0.19 equivalent) in `FixedUpdate`.
6. **Character controller:** a component (offset, max slope, max step height, snap to ground) plus a way to request a move each fixed step; reports `grounded` and the applied movement. Rapier's `KinematicCharacterController` underneath. Godot's `move_and_slide` / Unity's `CharacterController.Move` feel.
7. **Joints:** `Joint { kind: Fixed | Hinge { axis, limits } | Ball { cone limit } | Slider { axis, limits }, anchors }` connecting the entity's body to its nearest ancestor body (or the world if none). No entity references in scene files.
8. **Editor:** wireframe outlines of the selected entity's colliders in the Scene view (world lines; green like Unity / Godot), and the collider shape follows Inspector edits live.

## Decisions

- **Game-facing types live in `melon_core`** (components, query API, event types), because `melon_game` may depend only on `melon_core` (AGENTS.md). `melon_physics` implements them. The query API can be a trait object or a resource `melon_core` defines and `melon_physics` fills; choose the simplest that lets a game system call `raycast` synchronously.
- Mesh colliders read positions and indices of the entity's glTF model (the `x.glb#meshN` paths included) inside `melon_physics` (the `gltf` crate is a workspace dependency); a broken or missing model logs a warning and builds no collider.
- Physics still steps only in `FixedUpdate` while Playing; edit mode shows colliders but simulates nothing.

## Constraints

AGENTS.md conventions (registry registration, `mark_for_delete`, hierarchy world poses via `melon_core::hierarchy`, `PreviousTransform` for dynamic bodies, tests at the bottom of each file, ECS systems tested by running a one-system `Schedule`). Keep the `core <- physics <- editor` direction.

## Done when

- Tests: each shape collides (a sphere and a capsule fall and rest on a box); kinematic bodies follow `Transform`; Inspector-style component change rebuilds the collider; v5 scene loads and saves as v6 with the same body; ray cast hit / miss / layer filter / excluded entity; overlap; collision and trigger events start and end once; character controller walks up a step under the limit, stops at a too-steep slope, reports grounded; hinge joint keeps its anchor distance; deleting a body entity removes its rapier objects.
- `just test` green; AGENTS.md physics notes and ROADMAP (priority 4 ticked) updated.
