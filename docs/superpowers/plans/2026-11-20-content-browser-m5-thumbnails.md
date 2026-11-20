# Content Browser M5: Thumbnails Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Model tiles in the Content Browser show a rendered thumbnail (a three-quarter view of the model), rendered on demand at most one per frame, only for tiles on screen, and re-rendered when the model file changes; other types keep their type icon.

**Architecture:** `melon_render` gets a pure `thumbnail_camera(min, max)` and `Renderer::render_thumbnail(server, handle, target) -> Option<TextureView>`, which draws one model into a shared 128 × 128 `ViewportTarget` with its own encoder and submission, then copies it into a new texture. `melon_editor::thumbnails::Thumbnails` (owned by the `ContentBrowser`) decides what to render next and remembers each model's egui `TextureId` and file version. The host drives it after `end_frame`; `EditorUi::register_thumbnail` registers the view with egui.

**Tech Stack:** Rust 2024, wgpu 29, egui 0.35 / egui-wgpu (`register_native_texture`, `update_egui_texture_from_wgpu_texture`), glam.

**Spec:** `docs/superpowers/specs/2026-11-20-content-browser-design.md` (section M5, plus Constraints and Anti-goals).

## Global Constraints

- `THUMBNAIL_SIZE = 128` (pixels, square), one shared target. Camera: fixed three-quarter view (direction from the model centre towards `(1, 0.8, 1)` normalised), 45° vertical field of view, fitted to the model's bounds; `Light::default()`.
- **One submission per thumbnail, recorded after the frame's `end_frame`.** `render_scene` writes shared uniform buffers with `queue.write_buffer`, which applies at the next submit; a thumbnail recorded into the frame's encoder (or before its submit) would give the main viewport the thumbnail's camera.
- At most one thumbnail per frame; only for model tiles drawn on screen in tile view (list view shows glyphs). Until a thumbnail exists, the tile shows its type icon.
- Re-render when the model's `AssetServer::version` changes. A model that fails to load gets no thumbnail and is not retried until its version changes (no render every frame).
- In-memory only (no disk cache), as the spec's Approaches table decides.
- Colours and sizes from theme tokens; never call egui `Response` / `Context` methods inside `input` closures.
- `just gate` must pass before every commit (never chain a commit after a failing gate); Conventional Commits; no co-author or tool credit lines.

## Review Focus

1. The main viewport must never show the thumbnail camera's view or lose its model matrices for a frame. Guard: `render_thumbnail` submits its own encoder and the host calls it only after `end_frame`. GPU test: `thumbnail_does_not_disturb_the_next_scene_frame` (Task 1).
2. A broken `.glb` (fails to load): no thumbnail, the type icon stays, and it is not re-rendered every frame. Test: `failed_model_not_retried_until_version_changes` (Task 2).
3. A folder with hundreds of models: only visible tiles are requested, one per frame. Test: `requests_only_wanted_paths_one_at_a_time` (Task 2).
4. A model re-exported while the browser shows it: its thumbnail updates (same `TextureId` reused). Test: `version_change_requests_again_with_existing_id` (Task 2).
5. A degenerate model (all vertices at one point, or bounds far from the origin): the camera still frames it (no NaN, the centre lands mid-image). Test: `thumbnail_camera_handles_flat_and_far_bounds` (Task 1).

---

### Task 1: Thumbnail rendering in `melon_render`

**Files:**
- Modify: `crates/melon_render/src/lib.rs` (`THUMBNAIL_SIZE`, `thumbnail_camera`, `Renderer::model_bounds`, `Renderer::render_thumbnail`, GPU tests)

**Interfaces:**
- Produces:
  - `pub const THUMBNAIL_SIZE: u32 = 128;`
  - `pub fn thumbnail_camera(min: glam::Vec3, max: glam::Vec3) -> (glam::Mat4, glam::Vec3)`: `(view_proj, eye)`. Centre `c = (min+max)/2`, radius `r = max((max-min).length()/2, 0.01)`, distance `d = r / sin(22.5°) * 1.1`, eye `c + dir * d`, `look_at_rh(eye, c, Y)`, `perspective_rh(45°, 1.0, near = max(d - r * 1.5, 0.001), far = d + r * 1.5)` (DirectX depth 0..1, as the rest of the renderer).
  - `Renderer::model_bounds(&mut self, server: &mut AssetServer, handle: Handle<Model>) -> Option<(Vec3, Vec3)>`: loads through the `ModelCache` (`prepare`) and returns `server.bounds(handle)`; `None` when the file failed to load (the cache would draw the built-in cube in its place).
  - `Renderer::render_thumbnail(&mut self, server: &mut AssetServer, handle: Handle<Model>, target: &ViewportTarget) -> Option<wgpu::TextureView>`: `None` if `model_bounds` is `None`. Otherwise builds a `FrameData { view_proj, eye, light: Light::default(), models: vec![(Mat4::IDENTITY, handle)] }`, records `render_scene` (no lines, no overlay) into a **new** encoder, copies the target's colour texture into a new `THUMBNAIL_SIZE` texture (same format, `TEXTURE_BINDING | COPY_DST`, view format without the sRGB suffix like `ui_view`), submits, and returns that texture's non-sRGB view. `target` must be `THUMBNAIL_SIZE` square (debug-assert).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn thumbnail_camera_centres_and_contains_the_box() {
    let (min, max) = (glam::Vec3::new(-1.0, 0.0, -2.0), glam::Vec3::new(3.0, 2.0, 2.0));
    let (view_proj, eye) = thumbnail_camera(min, max);
    let centre = view_proj.project_point3((min + max) / 2.0);
    assert!(centre.x.abs() < 1e-4 && centre.y.abs() < 1e-4);
    for i in 0..8 {
        let corner = glam::Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        );
        let p = view_proj.project_point3(corner);
        assert!(p.x.abs() <= 1.0 && p.y.abs() <= 1.0 && (0.0..=1.0).contains(&p.z), "{p}");
    }
    assert!(eye.x > max.x && eye.y > max.y && eye.z > max.z, "three-quarter view from +x+y+z");
}

#[test]
fn thumbnail_camera_handles_flat_and_far_bounds() {
    let p = glam::Vec3::new(1000.0, -50.0, 3.0);
    let (view_proj, eye) = thumbnail_camera(p, p);
    assert!(view_proj.is_finite() && eye.is_finite());
    let centre = view_proj.project_point3(p);
    assert!(centre.x.abs() < 1e-3 && centre.y.abs() < 1e-3);
}
```

GPU tests, added to the existing `gpu_tests` assertion list (they need a display and adapter, like the others):

- `thumbnail_renders_the_model`: temp project with `assets/models/m.gltf` = `TWO_NODE_GLTF`; `AssetServer` on it; `target = renderer.create_viewport_target(THUMBNAIL_SIZE square)`; `render_thumbnail` returns `Some`; read back the copied texture (adapt the existing readback helper to a size parameter) and assert the centre pixel differs from the corner pixel (model in front of the clear colour).
- `thumbnail_of_a_broken_file_is_none`: `assets/models/bad.glb` containing `b"not a glb"` → `render_thumbnail` is `None`.
- `thumbnail_does_not_disturb_the_next_scene_frame`: render a scene frame into a viewport target with camera A and read a pixel; render a thumbnail; render the same scene frame again; the pixel is identical.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_render --lib -- thumbnail gpu_tests`
Expected: compile errors (`thumbnail_camera`, `render_thumbnail` not found).

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_render --lib`
Expected: all pass (GPU tests included).

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add crates/melon_render/src/lib.rs
git commit -m "feat(render): render model thumbnails in their own submission"
```

---

### Task 2: Thumbnail cache and tiles in the editor

**Files:**
- Create: `crates/melon_editor/src/thumbnails.rs`
- Modify: `crates/melon_editor/src/lib.rs` (`pub mod thumbnails;`; `Editor::thumbnail_request`, `Editor::thumbnail_done`; `EditorUi::register_thumbnail`)
- Modify: `crates/melon_editor/src/content.rs` (field `pub(crate) thumbnails: Thumbnails`)
- Modify: `crates/melon_editor/src/content_view.rs` (clear `wanted` each frame; visible model tiles push their path; paint the image when there is one)

**Interfaces:**
- Consumes: `AssetServer::{load, version}`, `content::is_model_path`.
- Produces:
  - `#[derive(Clone, Debug, PartialEq)] pub struct ThumbnailRequest { pub path: String /* project-relative */, pub handle: Handle<Model>, pub version: u32, pub existing: Option<egui::TextureId> }`
  - `#[derive(Default)] pub struct Thumbnails { done: HashMap<String, (u32, Option<egui::TextureId>)> /* None = failed at that version */, wanted: Vec<String> }`
  - `Thumbnails::begin_frame(&mut self)` (clears `wanted`), `want(&mut self, path: String)` (dedupes), `texture(&self, path: &str) -> Option<egui::TextureId>`.
  - `Thumbnails::request(&self, server: &mut AssetServer) -> Option<ThumbnailRequest>`: the first wanted path whose `done` entry is missing or has another version than `server.version(server.load(path))`; `existing` is the stored id if any.
  - `Thumbnails::finish(&mut self, request: &ThumbnailRequest, id: Option<egui::TextureId>)`: stores `(request.version, id.or(request.existing))` for a success; for `None` stores `(request.version, None)` unless an older id exists, which is kept (the last good thumbnail stays).
  - `Editor::thumbnail_request(&mut self, world: &mut World) -> Option<ThumbnailRequest>` and `Editor::thumbnail_done(&mut self, request: &ThumbnailRequest, id: Option<egui::TextureId>)`: thin wrappers over `self.content.thumbnails`.
  - `EditorUi::register_thumbnail(&mut self, device: &wgpu::Device, view: &wgpu::TextureView, existing: Option<egui::TextureId>) -> egui::TextureId`: `update_egui_texture_from_wgpu_texture` for an existing id, else `register_native_texture`, `FilterMode::Linear` (same as `set_viewport_target`).
  - View: in tile view, a model entry whose tile rect is visible (`ui.is_rect_visible`) calls `want(project-relative path)`; if `texture(path)` is `Some`, paint `egui::Image::new((id, square))` into the icon rect instead of the glyph.

- [ ] **Step 1: Write the failing tests** (`thumbnails.rs`; `AssetServer` on a temp root with model files so `version` works; `egui::TextureId::User(n)` as fake ids)

```rust
#[test]
fn requests_only_wanted_paths_one_at_a_time() {
    // want a.glb, b.glb ; request -> a.glb ; finish(a, Some(User(1))) ; request -> b.glb
    // finish(b, Some(User(2))) ; request -> None ; c.glb never wanted -> never requested
}

#[test]
fn version_change_requests_again_with_existing_id() {
    // a.glb done at version v with User(1) ; touch the file later + server.poll_changes()
    // request -> Some(req) with req.existing == Some(User(1)) and req.version == v + 1
}

#[test]
fn failed_model_not_retried_until_version_changes() {
    // finish(req, None) ; request -> None ; texture("a") -> None
    // after a version bump -> requested again
}

#[test]
fn failed_rerender_keeps_the_last_good_thumbnail() {
    // finish(v1, Some(User(1))) ; bump ; finish(v2, None) ; texture == Some(User(1)) ; request -> None
}

#[test]
fn begin_frame_forgets_tiles_no_longer_shown() {
    // want a ; begin_frame ; request -> None
}
```

View test (`content_view.rs`, headless): a folder with `a.glb` in tile view after one frame has `thumbnails.wanted == ["<root-relative to project>/a.glb"]`; with `*tile_size_mut() = TILE_MIN` (list view) after one frame `wanted` is empty.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p melon_editor --lib -- thumbnails:: content_view::`
Expected: compile errors (`Thumbnails` not found).

- [ ] **Step 3: Implement the items in Interfaces**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p melon_editor --lib`
Expected: all pass.

- [ ] **Step 5: Gate, then commit**

```powershell
just gate
git add crates/melon_editor
git commit -m "feat(editor): content browser thumbnail cache and tiles"
```

---

### Task 3: Host wiring

**Files:**
- Modify: `crates/melon_host/src/main.rs` (shared thumbnail target; one request per frame after `end_frame`)
- Modify: `README.md` (Content Browser: model thumbnails), `docs/ROADMAP.md` (thumbnails checked), `AGENTS.md` (Gotcha: thumbnails render in their own submission after `end_frame`)

**Interfaces:**
- Consumes: Task 1 `THUMBNAIL_SIZE`, `render_thumbnail`; Task 2 `Editor::{thumbnail_request, thumbnail_done}`, `EditorUi::register_thumbnail`.
- Produces: host field `thumbnail_target: Option<melon_render::ViewportTarget>` (created with the renderer, `THUMBNAIL_SIZE` square). After `renderer.end_frame(frame)`:
  `if let Some(request) = editor.thumbnail_request(world)` → `renderer.render_thumbnail(&mut server, request.handle, target)` → on `Some(view)`: `id = editor_ui_layer.register_thumbnail(device, &view, request.existing)`; then `editor.thumbnail_done(&request, result_id)`. (Use the host's actual names for the `Editor` and `EditorUi` values.)

- [ ] **Step 1: Check the build**

Run: `cargo build -p melon_host`
Expected: builds after the wiring.

- [ ] **Step 2: Check in the dev build**

Run: `cargo run -p melon_host`.
Expected: `assets/models` tiles show the cube's thumbnail within a frame or two; the Scene viewport looks unchanged while thumbnails render (no flicker, no camera jump); list view shows glyphs; re-export / touch a model and its tile updates; a broken `.glb` keeps the type icon and the frame rate stays steady.

- [ ] **Step 3: Docs, full test run, commit**

```powershell
just test
git add crates/melon_host/src/main.rs README.md docs/ROADMAP.md AGENTS.md
git commit -m "feat(host): render content browser thumbnails after each frame"
```
