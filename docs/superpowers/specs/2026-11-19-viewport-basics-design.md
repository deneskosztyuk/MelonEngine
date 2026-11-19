# Editor viewport basics

First step toward an editor that looks and works like Unity / Unreal / Godot.
The UI library decision (egui / Dear ImGui / Slint) comes right after this,
so everything here except the panel glue must survive a library switch.

## Intent

A proper scene viewport: the 3D view lives inside its own panel, navigates
like Unity, shows where things are (grid, axes, selection), and lets you
select by clicking in the scene.

## Goals

- The 3D scene renders only inside the central viewport panel, centred on it,
  with the right aspect ratio.
- Unity camera controls (user's choice).
- Click an object in the viewport to select it; click empty space to deselect.
- Ground grid, world axis lines, orientation indicator, selection box.
- MSAA 4x. A useful default camera. FPS readout.

## Non-goals

- Panel layout redesign, docking, menus, top bar rework (after the UI
  library decision).
- A separate Game view / game camera.
- Gizmo upgrades (plane handles, local/world toggle).
- Selection outline shader, mesh-accurate picking.

## Anti-goals

- Navigation, picking or grid logic written inside egui code (would be
  rewritten when the UI library changes).
- Viewport input leaking: typing in an inspector field moves the camera, or
  a gizmo drag also orbits.

## Approaches

| Decision | Options | Picked (user) |
|---|---|---|
| Scene into panel | **A. Render into `ViewportTarget`, show as panel image**; B. draw to window clipped to panel rect; C. full-window draw with projection shift | **A**: same in all 3 UI libraries, Dear ImGui POC already does it, allows Game view / docking later |
| Click-to-select | **A. CPU ray vs entity box**; B. GPU id-buffer picking | **A**: uses existing `screen_to_world_ray`, exact for today's box meshes |
| Camera controls | **Unity**; Unreal; Godot | **Unity** |

## Design

### Library-independent core: `melon_editor/src/viewport.rs` (new)

- `ViewportInput`: filled each frame by the UI glue, in viewport-local
  points (UI logical units, origin top-left of the viewport): `size: Vec2`, `pointer: Option<Vec2>`, `pointer_delta: Vec2`,
  `hovered: bool`, button state (`left/right/middle` down + `left_pressed`,
  `left_released`, `left_clicked` = press and release without drag), `scroll: f32`,
  `alt`, `shift`, `ctrl`, keys held `w a s d q e`, `f_pressed`,
  `typing: bool` (a text field has focus).
- `EditorCamera { pivot: Vec3, distance: f32, yaw: f32, pitch: f32, fly_speed: f32 }`
  with `apply(&mut self, &ViewportInput, dt) ` and `write(&self, &mut Camera)`.
- `pick(world, ray_origin, ray_dir) -> Option<Entity>`: nearest hit over
  entities with `Transform` + `MeshRef`, against the mesh's local bounds
  (unit cube ±0.5 for both `Cube` and `GltfModel`; `cube.glb` is a unit cube),
  tested in the entity's local space (inverse transform of the ray), so
  rotated and scaled boxes are exact.
- `grid_lines() -> Vec<LineVertex>`, `selection_box_lines(&Transform) -> Vec<LineVertex>`
  (the same local bounds under the transform).

### Unity controls (`EditorCamera::apply`)

| Input (pointer over viewport, not typing) | Effect |
|---|---|
| Right drag | Look around (yaw/pitch about the eye) |
| Right held + W/A/S/D | Fly; Q down, E up; Shift ×3 speed |
| Right held + scroll | Change fly speed (×1.2 per notch, 0.5–100 m/s) |
| Alt + left drag | Orbit around `pivot` |
| Middle drag | Pan (moves eye and `pivot` together) |
| Scroll | Dolly toward `pivot` (distance ×0.9 per notch, min 0.2) |
| F | Frame selection: `pivot` = its centre, distance fits its box |

Look/fly keep `pivot` in front of the eye at the current `distance`, so
orbiting after flying orbits what you face. Pitch clamped to ±89°.

Default: `pivot` at origin, eye at (0, 4, 8) (pitch ≈ −26.6°, yaw facing −Z),
fly speed 5 m/s.

### Left-button priority

1. Alt held: orbit.
2. Pointer on a gizmo handle: gizmo drag (existing code, now in viewport-local
   coordinates).
3. Otherwise a click (no drag): `pick`; hit selects, miss deselects.

### Rendering (`melon_render`, `melon_host`)

- The egui path renders like the Dear ImGui path: shadow pass, 3D pass into
  the `ViewportTarget`'s HDR target, tonemap into its colour texture, gizmo
  lines on top, then the UI pass clears the window and draws the panels with
  the viewport image. One render path for both UIs.
- `ViewportTarget` resizes to the panel's size × pixels-per-point when it
  changes. The camera aspect follows it.
- MSAA 4x: multisampled HDR colour + depth with a resolve target. Mesh and the
  new depth-tested line pipeline use 4 samples. Shadow and tonemap passes
  unchanged.
- New depth-tested line pipeline variant (depth test on, write off) for the
  grid, axes and selection box inside the 3D pass. The existing no-depth line
  pipeline stays for the gizmo.
- Grid: y = 0, 1 m spacing to ±50 m, every 10th line brighter. X axis line red,
  Z axis line blue. Selection box: orange wire box.

### UI glue (egui, in `melon_editor`)

- The central panel shows the viewport texture
  (`egui_wgpu::Renderer::register_native_texture`, updated on resize) with
  click and drag sense, builds `ViewportInput` from egui, calls the core.
- Overlays painted on the viewport: FPS + frame time (top-right, smoothed),
  orientation axes indicator (top-right corner, X/Y/Z from the camera view).
- Top bar loses the "Frame N" counter.

### Removed

- `fly_camera_system` and its registration: `EditorCamera` owns the camera.
  `Input` stays as the game input resource.

## Tests (headless)

- Orbit keeps distance to `pivot`; pan moves eye and `pivot` equally; dolly
  shrinks distance, never below 0.2; fly moves along view; Shift triples it;
  right+scroll changes speed within bounds; pitch clamps; F frames a box
  (pivot = centre, whole box in view).
- No camera change when `typing` or the pointer is outside the viewport.
- `pick`: nearest of two boxes on the ray; miss → `None`; rotated and scaled
  box hit exactly (ray just inside / just outside a scaled face); entity
  without `MeshRef` ignored.
- `grid_lines` count and axis colours; `selection_box_lines` = 12 edges.
- Existing GPU tests pass with MSAA (render + readback).

## My calls (correct any)

- Q/E for down/up and Shift for speed, as in Unity (Space/Shift up/down go away).
- Grid always on (toggle comes with the toolbar rework).
- **Picking uses the drawn mesh's bounds, not the `Aabb` component.** `Aabb`
  is display-only and inconsistent today (Ground's is pre-scaled, and it goes
  stale when you scale in the inspector). Real mesh bounds come with the asset
  system (roadmap item 3).
- The camera stays navigable in every state (Editing, Playing, Paused), as in Unity's Scene view.
- FPS and orientation indicator are painted by the UI library (small; redone if the library changes).
- Viewport keys work only while the pointer is over the viewport.

## Left to the builder

- Exact smoothing for FPS. Line widths (1 px lines; wide lines are out of scope).
