# WorldEnvironment (part 1 of 3: environment, physical sky, volumetric clouds)

## Intent

One place controls everything about how a world looks: sun, ambient, sky,
fog, AO, GI, bloom, and later the physical atmosphere and volumetric clouds.
Godot's model: a `WorldEnvironment` component points at a shared environment
asset. It replaces `SceneLighting` (added 2026-11-20). Parts 2 (physical
sky) and 3 (clouds) add sections to the asset; this part builds the
foundation they plug into. North star: data-driven, extensible, shareable
across levels, so a later time-of-day or weather system can blend assets.

## Goals

- `.env` asset (`EnvironmentAsset`, RON, `.meta` GUID, hot reload) holding
  every look setting, in sections.
- `WorldEnvironment` component (registered, saved in scenes) that references
  one `.env` by GUID + path; the first entity carrying one drives the
  world.
- The project ships `assets/environments/default.env`. New scenes get an
  entity with a `WorldEnvironment` pointing at it.
- Editor: create, edit (live), assign `.env` assets; no editor code needed
  for future sections.

## Non-goals

- Physical sky, clouds (parts 2 and 3).
- Blending between environments, time of day, weather.
- A thumbnail for `.env` tiles.
- Undo for `.env` edits (asset edits are not in `History`, same as `.mat`).
- A separate directional-light entity: the sun stays in the environment
  (elevation / azimuth), so time of day lives in one place.

## Anti-goals

- Any change on screen for a scene without a `WorldEnvironment`: it keeps
  today's defaults, and the many renderer GPU tests that build an
  `Environment` directly stay untouched.
- Load-time side effects (writing files while loading a scene; a packed game
  cannot write).
- Hand-written editor UI per field: adding a section in part 2 or 3 must
  not need new editor code.

## Constraints

- `melon_core` has no GPU code; the renderer keeps reading the `Light` and
  `Environment` resources (its contract does not change in this part).
- Assets are read through `AssetServer::read` (disk or `game.pak`); handles
  never stored in files; GUID wins over path.
- `melon_game` depends only on `melon_core`.
- Hot reload is polled (0.5 s); a packed game has none.

## Approaches

- A. Settings inline in the component. Least work; no sharing between
  levels.
- **B. Component points at a `.env` asset (chosen by the user).** Shared
  looks, one project default file, matches Godot. About 1.5x A's work.
- C. One component per feature (Unreal). Rejected: against the single
  control point the user asked for.

## Decided

1. **Rename** `SceneLighting` -> `WorldEnvironment`, module
   `melon_core::world_environment` (file renamed). `scene_lighting_system`
   becomes `world_environment_system`, same stage (PostUpdate) and same
   rules: writes `Light` / `Environment` only on change; when the component
   goes, both return to their defaults.
2. **No scene compatibility code, no version bump.** `SceneLighting` is a
   day old and only `assets/scenes/sich_demo.ron` uses it. Its values move
   into `assets/demo/sich/golden_hour.env` (+ `.meta`), the scene entity
   gets `WorldEnvironment` pointing there, and the generator
   (`crates/melon_editor/examples/sich_demo.rs`) writes the same. Any other
   old file keeps the unknown component in `KeptComponents` (existing
   behaviour). (This replaces the "v7 conversion" floated in chat.)
   `golden_hour.env` values: the scene's saved `SceneLighting` fields, plus
   `Environment::default()` for AO, GI and sky texture (what the old
   component left untouched, since nothing else sets them for that scene),
   so `--scene sich_demo.ron` renders the same image.
3. **`EnvironmentAsset`** (`melon_core::environment_asset` or inside
   `world_environment.rs`): `serde` + `Reflect`, `#[serde(default)]` on the
   asset and every section, so missing fields and sections take defaults
   and old files keep loading when parts 2 / 3 add sections. Sections hold
   today's values exactly:
   - `sun`: `elevation`, `azimuth` (degrees), `color` (linear RGB)
   - `ambient`: `share` (0..1, `Light::ambient`), `sky`, `ground`
   - `sky`: `zenith`, `horizon`, `texture: Option<TextureRef>`,
     `texture_intensity`
   - `fog`: `density`
   - `ao`: `strength`, `radius`
   - `gi`: `intensity`
   - `bloom`: `intensity`
   `EnvironmentAsset::default()` reproduces `Light::default()` +
   `Environment::default()` exactly (test). It maps to the whole
   `Environment` resource (no more "keep AO, GI and the sky texture"
   carve-out) and to `Light`.
4. **Sky texture becomes saveable**: `sky.texture` is a `TextureRef`
   (GUID + path), resolved to `Handle<Texture>` into
   `Environment::sky_texture` by the system.
5. **`WorldEnvironment` component**: `environment: EnvironmentRef { guid,
   path }` (String fields, reflect-friendly, like `MaterialSlot`). Inspector
   shows an asset picker + Content Browser drop target for it
   (`WidgetKind::Asset`, filtered to `.env`).
   - **No entity, or an empty ref** = `EnvironmentAsset::default()`, today's
     look (user decision: the new look comes with the component, never from
     an engine-wide fallback to `default.env`). The Inspector shows the empty
     picker and no card.
   - **Several entities** (call): the first in Hierarchy order (roots in
     `RootOrder`, depth first) wins; a warning is logged once each time the
     count of carriers or the winner changes. GameObject > World Environment
     still creates one (the warning names the winner).
6. **Loading** (core, CPU): the system resolves the ref to a handle, reads
   and parses the file when its `AssetServer` version changes, caches the
   parsed asset in a resource. Missing or broken file: warn once per file
   version; keep the last good asset, or `EnvironmentAsset::default()` if
   none loaded yet. Hot reload follows the file version.
   `AssetServer::load` returns the same handle id for the same path, so the
   cache is keyed by (environment handle, its version, the sky texture
   handle) and resources are rewritten only when the resulting `Light` /
   `Environment` differ (existing rule; a test pins it).
7. **Live preview** (call on the lifecycle): while the editor edits a
   `.env`, its in-progress copy drives the world (an editor-written preview
   override the system prefers over the file for that asset), so dragging
   the sun elevation updates the viewport every frame. The file is written
   when the gesture ends (same rule as the `.mat` card). The override lives
   only during a gesture: cleared when the file is written, when the card
   was not drawn that frame (deselect, panel closed), and on Play, Stop,
   Open and New Scene. A missing or unreadable `.env` shows the error in
   the card, with no editable fields (the editor never runs from a pack).
8. **Editor**
   - `AssetKind::Environment` for `.env` (added to `FILE_KINDS` and the
     type filter), theme colour, Content Browser
     folder menu "New Environment" (`asset_ops::create_environment`, writes
     `EnvironmentAsset::default()` + `.meta`).
   - One reflect-driven card (`reflect_ui_with` + field widgets: colour
     pickers for colours, `WidgetKind::Degrees`-style for angles, asset
     picker for the texture), shown (a) when a `.env` is selected in the
     Content Browser and (b) under the `WorldEnvironment` component in the
     entity Inspector, editing the referenced file. Sections are collapsible
     headers. New sections in parts 2 / 3 appear with no editor code.
   - GameObject > World Environment: spawns an entity named "World
     Environment" with the component pointing at the project default
     (one undo step, `record_created`).
   - File > New Scene (call on the details): there is no dirty tracking,
     so it asks like the Content Browser's "Open <scene>?" modal
     (`open_scene_dialog` / `resolve_open`: Save / Don't Save / Cancel,
     always shown, only while Editing). Then: despawn every entity with a
     `Transform` (the `load_scene` rule; the editor camera is not an
     entity), clear selection, history and rename, set `current_scene` to
     `None` (Save then goes to Save As), clear the preview override, and
     spawn the "World Environment" entity. Nothing else: an empty scene,
     like Godot's. If `assets/environments/default.env` is missing, it is
     written first (with `.meta`).
9. **Shipped default**: `assets/environments/default.env` + `.meta`,
   today's look (`EnvironmentAsset::default()`). Parts 2 / 3 turn the
   physical sky and clouds on in this file.
10. **Docs**: AGENTS.md "Scene lighting" gotcha rewritten for
    WorldEnvironment / `.env`; ROADMAP row updated.

## Verification

- Core tests (pattern of the current `scene_lighting.rs` tests): defaults
  reproduce `Light` / `Environment`; values drive the resources; removal
  restores defaults; unchanged asset does not rewrite resources; a file edit
  (version bump) re-applies; broken file keeps the last good one; partial
  RON file loads with defaults; component is registered; sky texture ref
  resolves by GUID then path.
- Editor tests: `create_environment` writes a loadable default + `.meta`;
  New Scene leaves exactly one `WorldEnvironment` entity; GameObject menu
  step undoes.
- `just test` green. `just snapshot` unchanged. Snapshot `--scene
  assets/scenes/sich_demo.ron` matches the image before the change.
- Manual (user, dev build): select the World Environment entity, drag sun
  elevation: viewport follows live; release: `.env` file updated; edit the
  file in a text editor: viewport follows within 0.5 s.

## Left to the builder

- Exact module split, resource names, field widget wiring.
- How the preview override is keyed (asset path or handle id).
- Section header styling (theme tokens).

## Later parts (recorded so the decisions are not lost)

- **Part 2, physical sky**: Hillaire 2020 atmosphere (transmittance and
  multi-scattering LUTs rebuilt only on sun / atmosphere change, per-frame
  sky-view LUT, aerial perspective volume replacing exponential fog), sun
  colour from transmittance, sky light for GI misses and the IBL derived
  from it. Adds an `atmosphere` section.
- **Part 3, volumetric clouds**: raymarched layer on a curved shell
  (camera below, inside or above), Perlin-Worley / Worley noise generated
  on the GPU at startup, weather map, Beer-Lambert + powder, dual-lobe HG,
  multi-scatter approximation, lit from the atmosphere LUTs. Cost:
  quarter resolution, 1/16 of pixels per frame + temporal reprojection,
  empty-space skipping, early out at 1% transmittance, geometry-covered
  pixels skipped, pass off at 0 coverage, 6 light samples, amortised cloud
  shadow map; `RenderQuality::clouds` Off / Low / Medium / High and a
  `clouds` GPU timing stage; target under 1 ms on High on the dev GPU.
  Adds a `clouds` section; wall-clock time drives wind so clouds move in
  the editor.
