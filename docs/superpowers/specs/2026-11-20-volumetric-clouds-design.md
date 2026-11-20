# Volumetric clouds (part 3 of 3, built before part 2)

## Intent

Realistic, Unreal-style volumetric clouds, on by default for new scenes
(through `assets/environments/default.env`), controlled from a `clouds`
section of the WorldEnvironment `.env` asset, cheap enough to leave on.
Built before the physical sky (user decision): clouds are lit from today's
sun and ambient through one small lighting interface that the physical sky
replaces later without touching the cloud code. First target scene: the
Sich village at golden hour (low sun in view: silver lining and dark bases
make or break it).

## Goals

- Raymarched clouds in a layer on a curved shell, seen from below, inside
  or above (camera anywhere).
- Look controls in `.env` (`clouds` section), cost controls in
  `RenderQuality::clouds`.
- Under 1 ms on High at 2880x1659 on the dev GPU (Radeon 8050S), measured
  with the snapshot's GPU timings; a `clouds` stage in `RenderTimings`.
- Clouds drift with the wind in the editor too (wall-clock time).

## Non-goals

- Physical sky / aerial perspective (part 2).
- Cloud shadows on the ground, clouds dimming the sun or ambient, clouds in
  GI, the IBL or reflections (later; documented limits).
- Multiple layers, cloud types per region, weather painting tools.
- Clouds in thumbnails (auxiliary views draw none).
- Clouds over geometry (user decision: sky only for now). Clouds composite
  only where the sky shader draws; a mountain inside the layer or a camera
  flying inside a cloud shows geometry without cloud in front. The march
  itself still works from any camera position, so the later step adds a
  depth-limited march and a second composite, not a rewrite.

## Anti-goals

- Any change to an image whose environment has `clouds.coverage == 0`:
  bit-identical to today, so `EnvironmentAsset::default()` (coverage 0) and
  every existing GPU test are untouched.
- Smearing / ghosting that a user notices when orbiting the editor camera
  past a roof edge.
- Per-frame CPU work or allocation that scales with anything.

## Constraints

- One main `render_scene` per submit; shared uniforms are written with
  `queue.write_buffer` (AGENTS "hard rule").
- Look vs cost split: `Environment` / `.env` = look (no on/off switches;
  coverage 0 = off), `RenderQuality` = cost (`#[serde(default)]`, old
  `quality.ron` files keep loading).
- WGSL goes through naga to HLSL too (DX12 and Vulkan); test under both.
- `melon_core` has no GPU code.

## Approaches

1. **Per-pixel raymarch on screen, reduced resolution + temporal
   reprojection (chosen by the user).** Unreal / Horizon Zero Dawn. Works
   from any camera position; can later also fill a sky panorama for
   reflections.
2. Raymarch into a sky panorama around the camera, time-sliced. Cheaper and
   stable, but ground-level only (no parallax, no fly-through).
3. Scrolling 2D layers. Not volumetric; rejected.

## Decided

1. **Look data** (`EnvironmentAsset::clouds: CloudsSection`, serde default,
   `EditHint` on every field) mirrored into `Environment::clouds:
   CloudSettings` by `EnvironmentAsset::environment()`:
   - `coverage` 0..1 (0 = no clouds, the pass does not run)
   - `density` (extinction scale, default 1.0)
   - `altitude` m, cloud base (default 1500)
   - `thickness` m (default 2500)
   - `scale` (shape size multiplier, default 1.0)
   - `wind_direction` degrees (compass, like sun azimuth), `wind_speed` m/s
     (default 10)
   - `EnvironmentAsset::default()` has coverage 0; `default.env` and
     `golden_hour.env` get a pleasant coverage (builder tunes, ~0.45).
2. **Time.** `Time::real_seconds` (core): wall-clock seconds accumulated in
   `Core::update` every frame in every `SimulationState`. `extract` copies
   it to `FrameData::time`. The snapshot sets it from the frame index
   (1/60 s per frame) so runs are deterministic.
3. **Cost data.** `RenderQuality::clouds: CloudQuality { Off, Low, Medium,
   High }` (presets Low -> Low, Medium -> Medium, High and Max -> High).
   Each level maps to (march resolution, primary steps, light steps,
   update fraction) in one table in the renderer; the builder tunes the
   table to the budget, starting from Low (1/4 res, 32, 4, 1/16), Medium
   (1/4, 48, 5, 1/4), High (1/4, 64, 6, 1/1). A row in the Quality panel.
4. **Lighting interface.** One WGSL function returns `CloudLight { sun_dir,
   sun_radiance, ambient_top, ambient_bottom }` from today's
   `LightUniform` (sun colour, hemisphere colours x ambient). The march
   reads only this. Part 2 swaps its body.
5. **Shape.** Shell between `altitude` and `altitude + thickness` above a
   6360 km planet. 3D Perlin-Worley base (128^3) and Worley detail (32^3)
   plus a 2D weather texture (coverage / height type), generated once on
   the GPU at first use (a counter proves "once"). Coverage remaps the
   base; a height gradient rounds bases and tops; detail erodes edges;
   wind offsets the sampling over `time`.
6. **Light transport.** Beer-Lambert, powder term, dual-lobe
   Henyey-Greenstein (forward + back lobe), a 3-octave multiple-scattering
   approximation, light march toward the sun with the last sample far
   away. Ray starts jittered (blue noise / interleaved gradient, changing
   per frame). Early out below 1% transmittance; large steps in empty air,
   small inside cloud.
7. **Temporal.** Per view (`ViewportTarget`, like `GiView`): a march target
   (RGB in-scatter + transmittance) and a ping-pong history with the
   representative cloud distance for reprojection; `prev_view_proj` per
   view. Each frame updates the pixels selected by the quality's update
   fraction (Bayer order inside each block) and reprojects the rest.
   History resets on a new view, a resize, a camera cut (the SSGI
   `is_camera_cut` rule) and any frame without clouds.
8. **Pass order and composite.** The cloud pass runs before the 3D pass.
   When this frame has the prepass (AO or SSGI on), texels fully covered by
   geometry are skipped; otherwise all texels march. `sky.wgsl` composites
   `sky * T + scatter` (bilinear upsample) where it already draws, so the
   sun disc and sky sit behind clouds, geometry in front. Distant clouds
   fade toward the horizon colour by distance (builder sets the range so
   the horizon does not turn into a solid band).
   - Both sky paths (procedural and the HDR image sky) get clouds; the
     horizon fade goes toward the sky colour in that direction (call).
   - The geometry skip is an optimisation only: a quarter-res texel is
     skipped when all 16 of its prepass pixels are covered (call).
   - Brightness (call): `sun_radiance = PI * light.color`, the same
     radiance the mesh shader lights a surface with, so a cloud's sunlit
     side sits next to a sunlit white wall; tuned against the scene's
     existing sky, judged by the user on the snapshot.
9. **Off means off.** `coverage == 0` or `CloudQuality::Off`: no pass, no
   per-view textures allocated, sky shader path identical to today (a
   shader override or uniform flag; the image must stay bit-identical). A
   shared 1x1 stand-in texture fills the sky's new binding while off
   (call).
   - Budget (call): 1 ms on High is firm. Cut primary steps first, then
     light steps, then the update fraction; resolution stays at 1/4.
     Measured as the minimum of 3 back-to-back runs.
10. **Timings and tooling.** `RenderTimings::clouds`; snapshot flags
    `--clouds off|low|medium|high` and `--cloud-coverage <0..1>`; the
    `--scene` path uses the scene's `.env`.
11. **Docs.** AGENTS.md gotcha "Volumetric clouds" (data, quality table,
    pass order, limits), ROADMAP row.

## Verification

- GPU tests: coverage 0 renders bit-identical to a frame without the
  feature; coverage 1 changes sky pixels above the horizon and no
  geometry pixel in front; CloudQuality Off skips the pass (timing `None`,
  no textures); noise generated once across frames; history resets on a
  camera cut; output finite (no NaN through bloom) with the sun behind the
  camera and at the horizon; runs under DX12 and Vulkan.
- Core tests: `Time::real_seconds` advances while Editing and Paused;
  `.env` clouds section round-trips and maps into `Environment`; a
  partial `.env` without `clouds` loads with coverage 0.
- Budget: snapshot `--size 2880x1659 --clouds high` GPU `clouds` timing
  under 1 ms (min of 3 runs, back to back).
- Look: snapshot of the Sich scene from the editor shot's angle before /
  after, judged by the user.
- Manual (user, dev build): orbit past a roof edge, no visible ghost;
  clouds drift in the editor; Quality > Clouds Off removes them.

## Left to the builder

- Exact noise construction, weather texture pattern, step sizes, phase
  parameters, horizon fade range, quality table values within the budget.
- Texture formats, workgroup sizes, compute vs fullscreen passes.
- Module split inside `melon_render` (`clouds.rs` + `clouds.wgsl` expected).
