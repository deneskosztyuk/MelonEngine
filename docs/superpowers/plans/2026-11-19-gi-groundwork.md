# GI Groundwork Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prepare the renderer for GI. Free bind group 3, render in HDR with tonemapping, and add one directional shadow map.

**Architecture:**
- Camera, light and shadow data go into one "frame" bind group 0. The order becomes frame, model, material, and group 3 stays free for GI.
- Meshes render into an `Rgba16Float` HDR target. A fullscreen tonemap pass writes it to the swapchain sRGB view (or the editor viewport). Gizmo lines draw after tonemapping, so their colours stay exact.
- A depth-only shadow pass renders from the sun with an orthographic box, and the mesh shader samples it with 3x3 PCF.

**Tech Stack:** Rust 2024, wgpu 29, WGSL, glam 0.33, winit 0.30.

**Spec:** none. Decisions come from the 2026-11-19 GI chat. This plan is the spec.

## Global Constraints

- Keep `unsafe_code = "deny"` and `clippy -D warnings` (`just gate` must pass).
- No new dependencies.
- Use at most 4 bind groups per pipeline, the wgpu default `max_bind_groups`.
- `Light.direction` in `melon_core` means **the direction light travels**. The shader gets `to_light = -direction`.
- The HDR format is `wgpu::TextureFormat::Rgba16Float`. The shadow map is `Depth32Float`, 2048 x 2048.
- Feature builds must compile: default, `--features imgui-poc`, and `--features slint-poc --bin melon_slint_poc`.

## Review Focus

1. **Light straight down** (`to_light` parallel to Y) must not produce a NaN light matrix. Otherwise the whole scene goes black. Pinned in Task 3: `light_view_proj_vertical_light_is_finite`.
2. **Floor outside the shadow box** must stay lit, never be counted as shadowed. Pinned in Task 3: GPU assertion `outside_shadow_box_is_lit`.
3. **Shadow acne:** a lit floor must not self-shadow. Pinned in Task 3: lit pixel > 200.
4. **Zero-size window or viewport:** the HDR target clamps to 1x1 and does not panic. Pinned in Task 2: `hdr_target_clamps_zero_size`.
5. **Gizmo line colours** must not be tonemapped. Lines draw after tonemapping; checked manually in Task 2 Step 6 (red X axis stays pure red).

## Shared test scene (GPU tests, `melon_render/src/lib.rs` tests mod)

Tasks 1 and 3 use this setup through a test helper, `fn render_test_scene(r: &Renderer, shadow_radius: f32) -> Vec<u8>` (`shadow_radius` is ignored until Task 3, then feeds `directional_light_view_proj(to_light, Vec3::ZERO, shadow_radius)`). It returns 64x64 sRGB8 RGBA.

- **Camera:** `Mat4::orthographic_rh(-4.0, 4.0, -4.0, 4.0, 0.1, 20.0) * Mat4::look_at_rh(Vec3::new(0.0, 10.0, 0.0), Vec3::ZERO, Vec3::NEG_Z)`.
- **Floor:** `cube_mesh()` scaled `(8.0, 0.2, 8.0)` at `(0.0, -0.1, 0.0)`.
- **Occluder:** scaled `0.5` at `(0.0, 2.0, 0.0)`.
- **Albedo:** a 1x1 white texture (`create_texture_from_bytes` + `create_texture_bind_group`) drawn with `draw_meshes_textured`.
- **Light:** `to_light = Vec3::new(1.0, 1.0, 0.0).normalize()`, ambient `0.2`, colour `Vec3::ONE`.
- **Pixels to read:** **SHADOW_PX = (16, 32)** is world (-2, 0, 0), inside the shadow. **LIT_PX = (48, 32)** is world (2, 0, 0), in the open.
- **Readback:** render into a texture with `COPY_SRC`, copy it to a buffer (pad `bytes_per_row` to 256), map with `device.poll(wgpu::PollType::wait_indefinitely())` (match the wgpu 29 name), and strip the padding.

---

### Task 1: Frame bind group and light sign fix

**Files:**
- Modify: `crates/melon_render/src/lib.rs` (layouts in `SceneRenderer::new`, `build_mesh_pipeline`, `build_line_pipeline`, `draw_meshes*`, `draw_lines`, tests)
- Modify: `assets/shaders/mesh.wgsl` (group numbers)
- Modify: `assets/shaders/line.wgsl` (only if group 0's binding numbers change; camera stays binding 0)
- Modify: `crates/melon_core/src/resources.rs` (`Light` doc and `to_light`)
- Modify: `crates/melon_host/src/main.rs:373`, `crates/melon_host/src/bin/melon_slint_poc.rs:76`

**Interfaces:**
- Produces:
  - `Light::to_light(&self) -> Vec3`, which returns `-self.direction`.
  - `SceneRenderer.frame_bind_group_layout`: group 0, holding binding 0 camera (VERTEX | FRAGMENT) and binding 1 light (FRAGMENT).
  - The fields `frame_bind_group` and `frame_layout` replace `camera_bind_group`, `light_bind_group` and `light_bind_group_layout`.
  - New WGSL group map: 0 frame, 1 model, 2 material (albedo), 3 unused.

- [ ] **Step 1: Write failing tests.**
  - In `resources.rs`: `light_default_to_light_points_up`, asserting `Light::default().to_light().y > 0.0`.
  - In `gpu_tests`: assertion `lit_floor_is_bright`. Using `render_test_scene(r, 20.0)`, the red channel at LIT_PX is `> 200`. This is about 228 for linear 0.766.
- [ ] **Step 2:** Run `cargo test -p melon_core light_default_to_light` → FAIL (no method). `cargo test -p melon_render gpu_tests` → FAIL (helper missing).
- [ ] **Step 3: Implement.**
  - Merge the camera and light layouts into the frame layout. Move albedo to group 2.
  - The mesh pipeline layout becomes `[frame, model, material]`, and the line pipeline layout `[frame]`.
  - Add `Light::to_light`. The host and the Slint POC pass `light.to_light()` or `-dir` to `update_light`.
  - Add the `render_test_scene` helper.
- [ ] **Step 4:** Run `cargo test --workspace` → PASS. The existing `gpu_tests` assertions stay green.
- [ ] **Step 5:** `just gate`, plus `cargo check -p melon_host --features imgui-poc` and `cargo check -p melon_host --features slint-poc --bin melon_slint_poc` → clean.
- [ ] **Step 6:** Manual check with `just fast-run`: cube tops are now lit. Today they are dark because of the sign bug.
- [ ] **Step 7:** Commit `refactor(render): merge camera+light into frame bind group, fix light direction sign`.

### Task 2: HDR target and tonemap pass

**Files:**
- Create: `assets/shaders/tonemap.wgsl` (fullscreen triangle from `vertex_index`, `textureLoad` at `@builtin(position)`, no sampler)
- Modify: `crates/melon_render/src/lib.rs` (`HdrTarget`, the tonemap pipeline, `Renderer` and `ViewportTarget` own one each; remove `render_meshes`, which has no callers and would target the wrong format)
- Modify: `crates/melon_host/src/main.rs` (both cfg paths), `crates/melon_host/src/bin/melon_slint_poc.rs`
- Modify: `README.md`, `assets/README.md` (shader list, pass order)

**Interfaces:**
- Consumes: the Task 1 frame group and `render_test_scene`.
- Produces:
  - `pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;`
  - `pub struct HdrTarget`, with `fn view(&self) -> &wgpu::TextureView` and `fn size(&self) -> PhysicalSize<u32>`. It holds its own tonemap bind group (`texture_2d<f32>`, unfilterable).
  - `SceneRenderer::create_hdr_target(&self, size: PhysicalSize<u32>) -> HdrTarget`. It clamps to at least 1x1 and is recreated in `Renderer::resize`.
  - `Renderer::hdr_target(&self) -> &HdrTarget` and `ViewportTarget::hdr_target(&self) -> &HdrTarget`.
  - `SceneRenderer::draw_tonemap(&self, pass: &mut wgpu::RenderPass<'_>, hdr: &HdrTarget)`.
  - The mesh pipeline targets `HDR_FORMAT`. The tonemap and line pipelines target `surface_format.add_srgb_suffix()`. The line pipeline gets `depth_stencil: None` (it already used `Always` with no depth writes).
- New frame order in the host:
  1. 3D pass (HDR view + depth, clear).
  2. Tonemap and lines pass (swapchain or viewport colour, no depth).
  3. egui pass (unchanged).
- Tonemap curve: ACES fitted (Narkowicz), `x*(2.51x+0.03) / (x*(2.43x+0.59)+0.14)`, clamped to 0..1. The developer may write the curve by hand at execution time.

- [ ] **Step 1: Write failing tests** in `gpu_tests`:
  - `hdr_target_clamps_zero_size`: `create_hdr_target(PhysicalSize::new(0, 0)).size()` is 1x1.
  - `tonemap_compresses_highlights`: clear the HDR target to 1.0 and to 8.0, tonemap each to sRGB8, and read the centre red value. Require `px(1.0) < 250` and `px(8.0) > px(1.0)`. The test is curve-agnostic: untonemapped 1.0 would read 255.
  - Update `lit_floor_is_bright` to render through HDR and tonemap, threshold `> 200` (about 223 with ACES).
- [ ] **Step 2:** `cargo test -p melon_render gpu_tests` → FAIL.
- [ ] **Step 3: Implement** the interfaces above. Rewire the host's egui path, imgui path and Slint POC to the three-pass order.
- [ ] **Step 4:** `cargo test --workspace` → PASS.
- [ ] **Step 5:** `just gate` and both feature checks → clean.
- [ ] **Step 6:** Manual check with `just fast-run`, then `--features imgui-poc`:
  - The scene shows, slightly darker mid-tones (ACES) is expected.
  - Gizmo axes stay pure red, green and blue.
  - Resizing or minimizing the window does not panic.
- [ ] **Step 7:** Commit `feat(render): HDR target with ACES tonemap pass`.

### Task 3: Directional shadow map

**Files:**
- Create: `assets/shaders/shadow.wgsl` (vertex-only depth pass: group 0 light view-proj, group 1 model)
- Modify: `assets/shaders/mesh.wgsl` (light uniform gains `light_view_proj`, frame group gains the shadow map and comparison sampler, PCF)
- Modify: `crates/melon_render/src/lib.rs`
- Modify: `crates/melon_host/src/main.rs`, `crates/melon_host/src/bin/melon_slint_poc.rs`
- Modify: `README.md`, `assets/README.md`

**Interfaces:**
- Consumes: the Task 2 `HdrTarget` and frame order.
- Produces:
  - `pub const SHADOW_MAP_SIZE: u32 = 2048;`
  - `pub fn directional_light_view_proj(to_light: Vec3, center: Vec3, radius: f32) -> Mat4`:
    - Eye at `center + to_light * 2.0 * radius`.
    - `Mat4::orthographic_rh(-r, r, -r, r, 0.0, 4.0 * r)`.
    - Up vector is `Vec3::Z` when `to_light.y.abs() > 0.99`, else `Vec3::Y`.
  - `LightUniform` gains `light_view_proj: [[f32; 4]; 4]`, size 96.
  - `SceneRenderer::update_light(&self, to_light: Vec3, ambient: f32, color: Vec3, light_view_proj: Mat4)`. It also writes the shadow pass's own view-proj buffer.
  - `SceneRenderer::render_shadow_pass(&self, encoder: &mut wgpu::CommandEncoder, draw_calls: &[(usize, &Mesh)])`. It clears depth to 1.0 and draws every mesh. The pipeline uses a depth bias of `constant: 2, slope_scale: 2.0` and cull `Front`.
  - Frame group binding 2 is `texture_depth_2d` and binding 3 is `sampler_comparison` (`CompareFunction::LessEqual`, clamp-to-edge).
  - Host: `const SHADOW_CENTER: Vec3 = Vec3::ZERO; const SHADOW_RADIUS: f32 = 20.0;`, marked with a `ponytail:` comment ("fixed box; fit to camera frustum or cascades when scenes outgrow 40 m"). The host passes all draw calls, textured and cube, as `(usize, &Mesh)` before the 3D pass.
- PCF in `mesh.wgsl` (the y-flip is the usual trap):

```wgsl
let ls = light.light_view_proj * vec4f(world_pos, 1.0);
let uv = ls.xy * vec2f(0.5, -0.5) + 0.5;
if (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0)) || ls.z > 1.0) { return 1.0; } // outside box = lit
// 3x3 textureSampleCompare at offsets of 1.0 / SHADOW_MAP_SIZE, averaged
```

  `VsOut` gains `@location(2) world_pos: vec3f`. Shadow multiplies only the diffuse term, not ambient.

- [ ] **Step 1: Write failing tests.**
  - CPU:
    - `light_view_proj_center_maps_to_middle`: project `center` and get clip xy ≈ 0, z in (0, 1).
    - `light_view_proj_nearer_point_has_smaller_depth`: `center + to_light` has smaller z than `center - to_light`.
    - `light_view_proj_vertical_light_is_finite`: `to_light = Vec3::Y`, every matrix element `is_finite()`.
    - `light_uniform_size_is_96`, replacing the old 32 test.
  - GPU (in `gpu_tests`):
    - `occluder_casts_shadow`: with `render_test_scene(r, 20.0)`, SHADOW_PX red `< 170` (about 148) and LIT_PX red `> 200`.
    - `outside_shadow_box_is_lit`: with `render_test_scene(r, 0.5)`, LIT_PX red `> 200`.
- [ ] **Step 2:** `cargo test -p melon_render` → FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4:** `cargo test --workspace` → PASS.
- [ ] **Step 5:** `just gate` and both feature checks → clean.
- [ ] **Step 6:** Manual check with `just fast-run`:
  - The spinning cube casts a shadow on the floor.
  - There is no acne or peter-panning on flat faces.
  - Shadows stay correct while the cube moves (physics play mode).
- [ ] **Step 7:** Commit `feat(render): directional shadow map with 3x3 PCF`.

## Open questions (not tasks)

- **Ray queries for later DDGI:** wgpu exposes `Features::EXPERIMENTAL_RAY_QUERY`. Recent wgpu versions turn experimental features on through an `unsafe` call, which clashes with `unsafe_code = "deny"`. Check the wgpu 29 API before the DDGI plan, and decide whether to allow `unsafe` in that one spot.
- **README cleanup:** the README still lists `LICENSE-MIT` and `LICENSE-APACHE`, which commit `ddeb4c7` deleted. Out of scope here.
