# Volumetric Clouds Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Raymarched volumetric clouds in the sky, look controlled by a `clouds` section of the `.env` asset, cost by `RenderQuality::clouds`, under 1 ms on High.

**Architecture:** Core carries the data (`CloudsSection` -> `Environment::clouds: CloudSettings`, `CloudQuality`, wall-clock `Time::real_seconds` -> `FrameData::time`). `melon_render::clouds` owns GPU noise (built once), per-view march + history textures (`CloudView` in `ViewportTarget`) and the march pass before the 3D pass. The sky pipeline gets a second bind group (cloud composite) and draws `sky * T + scatter`; with clouds off it binds a stand-in group whose flag skips the cloud math, so the image is bit-identical. Lighting goes through one WGSL function (`cloud_light`) that the physical sky replaces later.

**Tech Stack:** Rust 2024, wgpu (WGSL, also compiled to HLSL), bevy_ecs, egui.

**Spec:** `docs/superpowers/specs/2026-11-20-volumetric-clouds-design.md`

## Global Constraints

- `clouds.coverage == 0` or `CloudQuality::Off`: no cloud pass, no per-view cloud textures, image bit-identical to before the feature.
- `EnvironmentAsset::default()` / `CloudSettings::default()` have coverage 0; every existing GPU test stays untouched.
- One main `render_scene` per submit; thumbnails (`View::Auxiliary`) draw no clouds and touch no cloud history.
- High <= 1 ms GPU `clouds` timing at `--size 2880x1659` on the dev GPU (min of 3 back-to-back runs). Over budget: cut primary steps, then light steps, then update fraction; resolution stays 1/4.
- Clouds composite only where the sky shader draws (sky only).
- `RenderQuality` stays `#[serde(default)]`; old `quality.ron` loads with `clouds` = preset default.
- Every cargo / just command: `CARGO_BUILD_JOBS=12 RUSTC_WRAPPER=sccache lowprio <cmd>`, foreground only. Shader changes: run the renderer GPU tests under DX12 and Vulkan (`MELON_BACKEND=vulkan`).
- Clippy `-D warnings`; tests at the bottom of each file; Conventional Commits, no co-author / tool credit lines.

## Review Focus

1. Sun exactly at / below the horizon and behind the camera: no NaN or Inf (bloom and SSGI spread one bad texel) — Task 2 test `clouds_are_finite_for_any_sun`.
2. Window resize and Scene view hidden/shown: history recreated, no stale smear — Task 3 test `resize_resets_the_history`.
3. Toggling coverage 0 <-> 0.5 and quality Off <-> High frame to frame: no validation error, textures created / dropped — Task 3 test `switching_clouds_on_and_off_is_clean`.
4. A `.env` written before this feature (no `clouds` section): coverage 0, no clouds — Task 1 test `env_without_clouds_section_has_none`.
5. Editor left open for hours: wind offset stays precise (f64 time, offset wrapped on the CPU) — Task 1 test `wind_offset_wraps_without_precision_loss`.

---

### Task 1: Core data (look, cost, time)

**Files:** Modify `crates/melon_core/src/environment_asset.rs`, `crates/melon_core/src/resources.rs` (`Environment`, `Time`), `crates/melon_core/src/quality.rs`, `crates/melon_core/src/lib.rs` (`Core::update`, type registration), `crates/melon_render/src/extract.rs` (`FrameData::time`), `crates/melon_editor/src/quality_settings.rs` (Clouds row).

**Interfaces (produces):**
- `#[derive(Clone, Copy, Debug, PartialEq)] pub struct CloudSettings { pub coverage: f32, pub density: f32, pub altitude: f32, pub thickness: f32, pub scale: f32, pub wind_direction: f32 /*degrees, compass like sun azimuth*/, pub wind_speed: f32 /*m/s*/ }`, `Default` = (0.0, 1.0, 1500.0, 2500.0, 1.0, 0.0, 10.0); `Environment::clouds: CloudSettings` (default).
- `CloudsSection` with the same fields in `EnvironmentAsset::clouds` (serde default, Reflect, `EditHint`s: coverage 0..1 /0.005; density 0..4 /0.01; altitude 0..10000 /10 " m"; thickness 100..8000 /10 " m"; scale 0.1..4 /0.01; wind_direction -180..180 /0.5 "°"; wind_speed 0..60 /0.1 " m/s"); `environment()` copies it into `Environment::clouds` (coverage clamped 0..1).
- `pub fn wind_offset(settings: &CloudSettings, time: f64, period: f64) -> glam::Vec2` (core, next to `CloudSettings`): displacement in metres along the wind, computed in f64 and wrapped modulo `period`.
- `Time::real_seconds: f64`, advanced by `delta_seconds` in `Core::update` in every `SimulationState`.
- `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)] pub enum CloudQuality { Off, Low, #[default] Medium, High }`; `RenderQuality::clouds`; presets Low->Low, Medium->Medium, High->High, Max->High; `QualitySetting::Clouds` (live, no rebuild); `RenderTimings::clouds: Option<f32>`.
- `FrameData::time: f64` (from `Time::real_seconds`; 0 when absent).

- [ ] **Step 1: Failing tests:** `env_without_clouds_section_has_none` (parse a `.env` without `clouds` -> coverage 0); `clouds_section_round_trips_and_maps` (non-default values reach `Environment::clouds`); `real_seconds_advance_in_every_state` (Editing, Playing, Paused each add `delta_seconds`); `wind_offset_wraps_without_precision_loss` (time 1e6 s, speed 10, period 40000: result equals the f64 reference within 1e-3 m and lies in [0, period)); `quality_presets_pick_cloud_levels`; `old_quality_file_loads_with_default_clouds`; extract test `frame_data_carries_time`.
- [ ] **Step 2: Run, expect FAIL** (`cargo test -p melon_core`, `-p melon_render extract`).
- [ ] **Step 3: Implement**; Quality panel: Clouds dropdown row like the GI row.
- [ ] **Step 4: Run, expect PASS**; `just gate`.
- [ ] **Step 5: Commit** `feat(core): cloud settings in .env, CloudQuality, wall-clock time`

---

### Task 2: Noise, march and sky composite (no temporal yet)

**Files:** Create `crates/melon_render/src/clouds.rs`, `assets/shaders/clouds.wgsl` (+ `.meta` like the other shaders), `assets/shaders/clouds_noise.wgsl` (+ `.meta`). Modify `crates/melon_render/src/lib.rs` (module, `ViewportTarget`, `record_view` pass order, sky pipeline layout + draw), `assets/shaders/sky.wgsl`, `crates/melon_render/src/timings.rs` (`Mark::Clouds` between `Gi` and `Scene`; `clouds` = Gi..Clouds when the pass ran).

**Interfaces (produces):**
- `pub(crate) struct CloudParams { pub steps: u32, pub light_steps: u32, pub block: u32 /* 1, 2 or 4: update 1/block² of texels per frame */ }`; `pub(crate) fn cloud_params(q: CloudQuality) -> Option<CloudParams>` — the one table: Low (32, 4, 4), Medium (48, 5, 2), High (64, 6, 1); Off -> `None`. Resolution: 1/4 per axis (`cloud_size(w, h) = (ceil(w/4), ceil(h/4))`).
- `pub(crate) struct Clouds` on the renderer: noise textures (128³ base `Rgba8Unorm`, 32³ detail, 512² weather), built on first use; `pub noise_builds: u32`; the march pipeline; the composite layout (group 1 of the sky pipeline: cloud texture, linear sampler, `CloudCompositeUniform { enabled: u32, .. }`) and a stand-in group (1x1 texture, `enabled = 0`).
- `pub(crate) struct CloudView` in `ViewportTarget` (`Option`, created / dropped by `match_cloud_view(quality, coverage)` like `match_gi_view`).
- WGSL `fn cloud_light(world_pos: vec3f) -> CloudLight` (`sun_dir`, `sun_radiance = PI * light.color`, `ambient_top = ambient * sky_color`, `ambient_bottom = ambient * ground_color`): the only place the march reads lighting.
- `CloudUniform` (per view): inverse view-proj, eye, sun dir, wind offset (from `clouds::wind_offset` with the noise period in metres), settings, params, frame index, planet radius 6 360 000 m.

Algorithm notes the tests do not pin: shell intersection on the sphere (camera below, inside, above); density = remap(base, 1 - coverage) x height gradient - detail erosion; Beer-Lambert + powder; dual-lobe HG (forward g ≈ 0.8, back g ≈ -0.3, mixed); 3-octave multiple-scattering approximation; light march with the last sample far; jittered start (interleaved gradient noise + frame index); early out at transmittance < 0.01; empty-space steps larger than in-cloud steps; distant clouds fade toward the sky colour in that direction (both sky paths) so the horizon is not a band.

- [ ] **Step 0: Baseline.** Before any edit in this task: `just snapshot`, copy `target/review/snapshot.png` to `target/review/clouds_baseline.png`.
- [ ] **Step 1: Failing GPU tests** (renderer GPU test harness, `gi_intensity: 0.0` per AGENTS):
  - `zero_coverage_is_bit_identical` (High + coverage 0 == Off + coverage 0.5 == Off + coverage 0, read back byte for byte; "identical to before the change" is covered by every existing GPU test staying green and by `just snapshot` matching a baseline PNG captured before Task 2's first edit)
  - `clouds_change_only_the_sky` (coverage 1, camera looking up at a slab: sky pixels above the horizon change, slab pixels identical)
  - `quality_off_runs_no_pass` (`timings().clouds` is `None`, `CloudView` absent)
  - `noise_is_built_once` (`noise_builds == 1` after 5 frames and a quality change)
  - `clouds_are_finite_for_any_sun` (sun elevation -10, 0, 0.5, 90 and behind the camera: every read-back pixel finite)
  - `thumbnail_draws_no_clouds` (auxiliary view with coverage 1 matches coverage 0)
- [ ] **Step 2: Run, expect FAIL** (`cargo test -p melon_render clouds`).
- [ ] **Step 3: Implement** (block = 1 for every level in this task; temporal comes in Task 3).
- [ ] **Step 4: Run, expect PASS**, also with `MELON_BACKEND=vulkan`; `just gate`.
- [ ] **Step 5: Commit** `feat(render): volumetric cloud march and sky composite`

---

### Task 3: Temporal update and reprojection

**Files:** Modify `crates/melon_render/src/clouds.rs`, `assets/shaders/clouds.wgsl`, `crates/melon_render/src/lib.rs` (history swap, prepass depth binding).

**Interfaces:** Consumes Task 2. Produces: `CloudView` holds ping-pong history (`Rgba16Float` scatter + transmittance, `R32Float` representative distance) and `prev: Option<(Mat4, Vec3)>`; resets (history invalid, all texels march) on create / resize, `ssgi::is_camera_cut`, and any frame without clouds on that view.

- Per frame, texel (x, y) marches when `bayer(x % block, y % block) == frame % (block * block)`; the others reproject: previous screen position of the point at the stored distance along the current ray; outside last frame's screen or invalid history -> march this frame too. Marched texels blend into the reprojected history (exponential, weight builder-tuned; full replace on reset).
- Geometry skip: when this frame has the prepass, a texel whose 4x4 full-resolution pixels all have prepass depth < 1 (covered) skips the march and writes transparent.

- [ ] **Step 1: Failing GPU tests:** `static_camera_converges_to_the_full_march` (block 4 after 64 frames vs block 1: mean abs diff < 2/255, max < 8/255 in sRGB); `camera_cut_resets_the_history` (cut -> next frame equals a fresh-view first frame); `resize_resets_the_history`; `switching_clouds_on_and_off_is_clean` (coverage 0 <-> 0.5 and Off <-> High over 6 frames, no validation error via `render_qualities`); `geometry_skip_changes_nothing` (AO on vs AO off at block 1: identical sky pixels).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS** (DX12 and Vulkan); `just gate`.
- [ ] **Step 5: Commit** `feat(render): temporal cloud updates with reprojection`

---

### Task 4: Snapshot tooling, budget, default look

**Files:** Modify `crates/melon_render/examples/snapshot.rs` (`--clouds off|low|medium|high`, `--cloud-coverage <f>`, fixed `time = frame / 60`, `clouds` in the printed timings), `assets/environments/default.env`, `assets/demo/sich/golden_hour.env` (add `clouds` sections).

- [ ] **Step 1: Budget.** `snapshot -- target/review/clouds_high.png overview --size 2880x1659 --clouds high --cloud-coverage 0.45` three times back to back; record the `clouds` ms (min). Over 1 ms: apply the Global Constraints cut order in `cloud_params`, re-run Task 2/3 tests, repeat. Record final table + timings for Low / Medium / High in the report.
- [ ] **Step 2: Default look.** Give `default.env` a daytime cloud section (coverage ≈ 0.45, rest default) and `golden_hour.env` one that reads well at sun elevation 6° (coverage ≈ 0.4–0.5, wind from the sun's side). Render `--scene assets/scenes/sich_demo.ron` from eye `" -2,3,18"` look `0,8,-20` (the user's roof-line shot toward the sun) and from the overview eye; save as `target/review/sich_clouds_*.png` for the user.
- [ ] **Step 3:** `just test`. Expected green.
- [ ] **Step 4: Commit** `feat(demo): clouds in the default and golden hour environments; snapshot cloud flags`

---

### Task 5: Docs

**Files:** `AGENTS.md` (new gotcha "Volumetric clouds": data, `cloud_params` table and measured costs, pass order, history rules, `cloud_light` seam, sky-only limit, thumbnails, snapshot flags; add `clouds` to the snapshot command line and `RenderTimings` list; `clouds.wgsl` in the shaders row), `docs/ROADMAP.md` (sky / clouds row).

- [ ] **Step 1:** Edit. **Step 2:** `just gate`. **Step 3: Commit** `docs: volumetric clouds`
