# Render extraction into melon_render

Roadmap Tier 0: "Render extraction moved from `melon_host` into
`melon_render`". Fixes bug 7 (`melon_host/src/main.rs` does too much).

## Intent

The host should only wire crates together: window, input, frame timing, and
the order of calls. Deciding what to draw and recording the scene's render
passes belongs to `melon_render`; scene Save / Load belongs to
`melon_editor`. A second view (the roadmap's Game view) or a game build
without the editor then reuses the same calls instead of copying host code.

## Goals

- `melon_render::extract(world) -> FrameData`: plain data, no GPU, tested
  headless.
- `Renderer::render_scene(..)` records the shadow, 3D (HDR, MSAA) and
  tonemap passes for a `FrameData` into any `ViewportTarget`, plus the
  world lines (depth-tested) and overlay lines (gizmo, on top).
- The renderer owns the `ModelCache`.
- `Renderer::ui_pass(..)` gives the editor its egui pass; the host has no
  `wgpu::RenderPassDescriptor` left.
- `Editor::run_pending_operation(world)` does File Save / Load / Exit.
- The `RedrawRequested` handler shrinks from ~290 lines to a short sequence
  of calls; behaviour and look are unchanged.

## Non-goals

- Rendering as ECS systems / a render stage, render graph, Game view, a
  standalone game build, entity cap enforcement (bug 4), moving
  `spawn_default_scene` or input mapping out of the host.

## Anti-goals

- Any visible change in the editor (colours, shadows, gizmo, grid, MSAA).
- `melon_render` depending on `melon_editor` or `melon_physics`.
- `melon_game` depending on anything but `melon_core`.

## Approaches

| Option | Trade-off | Picked |
|---|---|---|
| **Extract to plain data + `render_scene` on the renderer** | Small, testable, a second view is a second call | **Yes** (user said "proceed") |
| Renderer as an ECS resource with render systems in a new stage | Bevy-like, but needs stage + resource plumbing nothing uses yet | No |
| Helper functions that only move the host code | Host still decides what gets drawn | No |

## Design

### `melon_render::extract` (new module)

- `pub struct FrameData { pub view_proj: Mat4, pub light: Light, pub models: Vec<(Mat4, Handle<Model>)> }`
  (`Light` is `melon_core::Light`, copied).
- `pub fn extract(world: &mut World) -> FrameData`: camera `view_proj`
  (default if no `Camera`), `Light` (default if none), and every entity with a
  `Handle<Model>` and a matrix from `render_matrices(world, alpha, blend)`.
- `blends(state)` (blend while Playing / Paused, not Editing) moves here
  from the host with its tests.

### `Renderer::render_scene`

`pub fn render_scene(&mut self, encoder: &mut wgpu::CommandEncoder, frame: &FrameData, server: &mut AssetServer, target: &ViewportTarget, world_lines: &[LineVertex], overlay_lines: &[LineVertex])`

- Uploads camera, light (shadow view-proj from the fixed `SHADOW_CENTER` /
  `SHADOW_RADIUS`, which move here) and model uniforms.
- `ModelCache::prepare` for every handle, then the draw list.
- Records: shadow pass; 3D pass into `target`'s HDR MSAA view (clear
  `CLEAR_COLOR`, depth 1.0) with meshes and world lines; tonemap pass onto
  `target`'s display view with overlay lines.
- `ModelCache` becomes a field of `Renderer`, created in `Renderer::new`.

### `Renderer::ui_pass`

`pub fn ui_pass<'a>(&self, frame: &'a mut Frame) -> wgpu::RenderPass<'static>`:
clears the swapchain image to `CLEAR_COLOR`, no depth (the current egui pass,
moved as is).

### `Editor::run_pending_operation`

`pub fn run_pending_operation(&mut self, world: &mut World) -> bool` (true =
exit requested). Save / Load and their status messages move from the host;
Load uses `world.resource_scope::<PhysicsWorld, _>` instead of the manual swap.
Exit saves the layout and returns true.

### Host frame order

`core.update()` → `begin_frame` → editor UI update → `run_pending_operation`
(exit if true) → viewport target resize + camera aspect → `extract` →
`render_scene` → `ui_pass` + `editor.render` → `end_frame`.

Extracting after the editor update means gizmo and Inspector edits show in the
same frame, and the camera is uploaded once.

## Tests

- `extract`: one model entity gives one entry with its world matrix; a child
  under a translated parent gets the composed matrix; an entity without a
  model is left out; `view_proj` equals the `Camera`'s; no `Camera` / `Light`
  gives defaults.
- `blends` tests move with it.
- `run_pending_operation`: Save writes the file and sets a success status;
  Load of a missing file sets an error status and keeps the world; Exit
  returns true.
- GPU: `render_scene` with a cube in front of the camera into a small
  `ViewportTarget` leaves the centre pixel different from the clear colour.

## My calls

- Short design doc, no separate design review: the approach is agreed and
  the refactor keeps behaviour.
- Extraction moves after the editor update (same-frame edits, one camera
  upload).
- The renderer owns the `ModelCache`.
- `run_pending_operation` lives on `Editor`.

## Left to the builder

- Whether `extract` lives in `lib.rs` or its own file; internal helper names.
