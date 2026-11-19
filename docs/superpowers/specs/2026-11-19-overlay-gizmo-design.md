# Overlay pass and Unreal-style transform gizmo

Editor polish. The transform gizmo is drawn with 1-pixel GPU lines and is hard
to see and grab. The user wants it to look and work like Unreal Engine's, and
wants it built on a reusable foundation ("north star"), not the cheapest thing
that works today.

## Intent

1. A general **overlay layer** in `melon_render`: solid, shaded 3D shapes
   drawn after tonemapping, in exact colours, anti-aliased, either on top of
   the scene or hidden behind scene geometry. The transform gizmo is its first
   user; light icons, camera frustums, colliders and other debug or editor
   shapes can use it later without new GPU code.
2. A **thick, Unreal-style transform gizmo** with Unreal's extra handles.

Reference: Epic's "Transforming Actors" page
(https://dev.epicgames.com/documentation/unreal-engine/transforming-actors-in-unreal-engine).

## Goals

- **Move:** three arrows (cylinder shaft + cone head), three plane squares
  (move in XY / YZ / XZ), one centre handle (move freely in the plane facing
  the camera).
- **Rotate:** three thick rings, one per axis.
- **Scale:** three cube-tipped shafts, three plane triangles (scale two axes
  at once), one centre cube (uniform scale).
- Colours: X red, Y green, Z blue (`theme::colors::AXIS_*`); centre handles
  light grey; plane handles in the colour of the axis they don't move along,
  semi-transparent. The hovered or dragged handle turns **yellow**.
- Shapes are lightly shaded (lit from the camera) so they read as solid.
- Constant size on screen, as today (`GIZMO_SIZE_PX`).
- Handles are picked against the same 3D shapes that are drawn, so what you
  see is what you grab.
- Shift snaps every handle (move, plane move, rotate, scale, uniform scale)
  with today's snap steps.

## Non-goals

- View-axis (white) rotate ring, arcball rotation, Alt-drag duplicate.
- A World / Local space toggle (the gizmo keeps today's axes: the entity's
  world rotation).
- User settings for gizmo size and thickness (sizes live in one place so a
  setting can be added later).
- Other overlay users (light icons, frustums): the API allows them, nothing
  else is drawn yet.
- Moving the grid, selection box or other line drawing onto the overlay pass.

## Anti-goals

- Axis colours washed out by the scene's tonemapper.
- Jagged gizmo edges.
- A handle that looks grabbable but can't be grabbed, or grabs a different
  handle than the one highlighted.
- Breaking today's move / rotate / scale behaviour (snapping, physics write,
  `PreviousTransform` snap, hierarchy world-space edits).

## Approaches

| Option | Trade-off | Picked |
|---|---|---|
| egui painter overlay (2D projected shapes) | Least work, smooth edges, exact colours; no depth against the scene, approximate shading, not reusable as an engine feature | No |
| **GPU overlay pass with real 3D meshes** | More work (own pass, MSAA, depth, compositing); reusable for future editor and debug shapes | **Yes** (user: "north star") |
| Shader-widened thick lines | GPU work like the mesh pass, still looks flat | No |

## Design

### Overlay layer (`melon_render`)

- **Shapes:** a fixed set of unit meshes built once at renderer start:
  `OverlayShape::{Cylinder, Cone, Cube, Sphere, Torus, Quad, Triangle}`, each
  with normals, centred and sized in a documented unit space (for example a
  cylinder of radius 1 from y = 0 to y = 1; a torus of major radius 1 in the
  XZ plane with a thin minor radius).
- **Draw list:** `OverlayDraw { shape: OverlayShape, transform: Mat4,
  color: [f32; 4], depth: OverlayDepth }`, with
  `OverlayDepth::{OnTop, SceneTested}`. Colours are display colours (sRGB,
  straight alpha), never tonemapped.
- **Pass:** after the tonemap pass, `render_scene` takes the overlay draws
  and renders them into a 4x MSAA colour target cleared to transparent:
  - `SceneTested` draws first, depth-tested against the scene's depth buffer
    (same size and sample count), so later shapes can hide behind geometry.
  - then the depth is cleared and `OnTop` draws are rendered with their own
    depth test, so a gizmo hides its own back faces but sits over the scene.
  - the MSAA target resolves and is alpha-blended onto the viewport's display
    image.
- **Shading:** simple "headlight" lighting: brightness from the angle
  between the surface normal and the direction to the camera, with a floor
  so edges never go black. Alpha < 1 draws are blended and don't write depth.
- The overlay draws travel in `render_scene` (beside today's world lines and
  overlay lines); the overlay targets live with the `ViewportTarget` and
  resize with it.

### Gizmo (`melon_editor`)

- **Handles:** `GizmoHandle::{Axis(0..3), Plane(0..3), Center}` per mode
  (`Plane(i)` is the plane perpendicular to axis i).
- **Proportions** (constants in `gizmo.rs`, in pixels at the gizmo's
  distance): shaft length = `GIZMO_SIZE_PX`, shaft radius about 2.5 px, cone
  about 9 px wide and 18 px long, scale cube about 10 px, ring band about 3 px
  thick, plane handles about 30% of the shaft length from the centre and
  20% of its length on a side, centre handle about 10 px. Exact values are
  the builder's to tune by eye against Unreal.
- **Building:** one function turns (mode, pose, size, hovered/active handle)
  into `Vec<OverlayDraw>`, all `OnTop`.
- **Picking:** one function intersects the mouse ray with each handle's 3D
  shape (capsules for shafts and arrows, oriented quad / triangle for plane
  handles, sphere / cube for the centre, a band around each ring), returning
  the nearest hit. A small pixel tolerance makes thin parts easy to grab.
- **Dragging:** today's axis move and per-axis scale keep their math.
  - **Rotate is replaced** (today's is broken: its drag plane contains the
    axis, so the angle is always about ±90° and the object jumps by 0° or
    180°). New: hit the plane *perpendicular* to the axis through the gizmo
    centre and rotate by the change in angle around the axis. When that
    plane is seen nearly edge-on (|ray · axis| < 0.1), fall back to
    Unreal-style screen drag: angle = pointer movement along the ring's
    screen tangent at the grab point, 0.01 rad per pixel.
  - New: plane move (ray–plane in that plane, both axes snapped), centre
    move (ray–plane facing the camera through the gizmo centre), plane scale
    (both axes by the same factor, from the pointer's projection on the plane
    diagonal relative to the press; the handle sits away from the centre so
    the ratio is stable).
  - **Uniform scale** uses Unreal-style screen drag, not a distance ratio
    (the centre cube is only ~10 px, so a ratio from the press would explode):
    factor = 1 + 0.01 × (pointer Δx − Δy) in pixels since the press, clamped
    to `SCALE_MIN..SCALE_MAX`.
  Every write keeps going through `set_world_transform`, the physics body
  write and `snap_previous_transform`.
- The old line gizmo code goes away.

## Tests

- **Overlay (GPU):** an `OnTop` red quad over an empty scene gives a red
  centre pixel; a `SceneTested` quad behind a cube is hidden while an `OnTop`
  one at the same place shows; the centre pixel of an `OnTop` draw has exactly
  the requested colour (no tonemapping).
- **Shape meshes (headless):** each unit shape has the documented bounds.
- **Gizmo picking (headless):** for each mode, a ray through each handle's
  centre picks that handle; a ray between handles picks nothing; the
  centre handle wins over the plane handles where they overlap.
- **Gizmo dragging (headless):** rotate by a ray sweeping 90° around the
  axis turns the entity 90° (not 0° or 180°); edge-on rotate uses the screen
  fallback; plane move changes exactly the two plane axes; centre move keeps the depth from the camera; uniform scale scales all
  axes by the same factor; plane scale leaves the third axis unchanged;
  snapping applies to each.
- **Gizmo building (headless):** the hovered handle's draws are yellow, the
  rest keep their axis colours; Move builds 3 shafts + 3 cones + 3 planes +
  1 centre.

## My calls

- Rotate drag replaced (see Dragging): the old one is broken.
- Uniform scale: screen drag, 100 px = ×2, 0.01 per pixel.
- Overlay composited after tonemapping with its own 4x MSAA target.
- `OnTop` / `SceneTested` as the two depth layers.
- Headlight shading.
- Full rings for rotate (Unreal shows camera-facing arcs; full rings are
  simpler to read and pick; arcs can come later).
- Plane handles semi-transparent, coloured by the axis they don't move along.

## Left to the builder

- Exact proportions and shading constants, tuned by eye.
- Mesh tessellation (segment counts), internal module layout.
