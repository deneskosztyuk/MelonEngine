# Rendering: materials, textures, point / spot lights, IBL, cascaded and local shadows, LOD (ROADMAP priority 3)

## Intent

The shared rendering core every 3D game needs, as Unity (Materials, Light components, reflection probes / skybox ambient, cascaded shadows, LOD Group), Godot (StandardMaterial3D, OmniLight3D / SpotLight3D, sky ambient, directional shadow splits, visibility ranges) and Unreal (material instances, point / spot lights, sky light, CSM, LODs) provide. PBR, sky, GI, instancing and culling already exist (see AGENTS.md).

## Goals (in build order; commit each as it works)

1. **Texture and material assets.**
   - `Handle<Texture>` (png, jpg, hdr through `AssetServer`, `.meta` GUIDs, hot reload, sRGB vs linear chosen by use).
   - `Handle<Material>` from `.mat` RON files: base colour + texture, metallic, roughness + metallic-roughness texture, normal texture + scale, emissive + texture + strength, alpha mode (`Opaque`, `Mask { cutoff }`, `Blend`), double sided. Textures referenced by GUID (path as fallback), like scene model references.
   - A registered component assigns materials per mesh part of the entity's model (unset parts keep the glTF material).
   - Editor: Content Browser Create ▸ Material, material and texture kinds with thumbnails (material on a sphere), selecting a `.mat` shows an Inspector that edits it live (saved to the file, undoable is not required for asset edits), drag a material onto an entity / part in the Scene view or the component, "Extract materials" for a glTF model (writes `.mat` + textures next to it, Unity-style).
   - Alpha: Mask in the opaque pass (and prepass / shadows), Blend in a sorted back-to-front pass after opaque (no shadows cast; documented).
2. **Point and spot lights** as registered components (`PointLight { color, intensity, range, cast_shadows }`, `SpotLight { .., inner_angle, outer_angle }`), physically plausible inverse-square falloff windowed to `range`. **Clustered forward**: a froxel grid and a light-culling pass, so cost scales with lights per cluster, not total lights; at least 256 visible lights per frame. Editor: Create ▸ Light ▸ Point / Spot, viewport icons for lights (pickable), range sphere / cone outline for the selected light.
3. **Image-based ambient (IBL).** Ambient diffuse (irradiance) and specular (prefiltered mips + BRDF LUT, split sum) come from the environment: the procedural sky by default, or an HDR equirect texture set on `Environment` (also drawn as the sky). Rebuilt when the sky changes. The default procedural sky's irradiance should stay close to today's hemisphere ambient so scenes keep their brightness; GI misses (`sky_light`) read the same irradiance.
4. **Cascaded sun shadows.** 1-4 cascades (`RenderQuality`), practical split scheme, stable (texel snapping, no shimmer when the camera moves or turns), blended cascade transitions, max shadow distance. DDGI injection keeps shadowed sun light.
5. **Point / spot shadows.** A shadow atlas (spot: one view; point: six faces), PCF, a per-quality cap on shadowed lights (nearest / most important first) and atlas resolution. DDGI injects point / spot lights that cast shadows (using their shadows); unshadowed local lights are not injected (documented, avoids light through walls).
6. **LOD.** A registered `Lod` component: levels of (model, screen-height threshold) like Unity's LOD Group, culled below the last level, hysteresis so a level does not flicker at a threshold. Instancing groups by the chosen level's model.

## Decisions

- Costs go in `RenderQuality` (cascades, shadow distance, shadowed-light cap, atlas size, max lights); looks go in `Environment` / components (AGENTS.md rule). Presets updated for Low / Medium / High / Max.
- Keep the one-main-`render_scene`-per-submit rule and the thumbnail rules (thumbnails see materials; they do not need local lights or cascades beyond one).
- Snapshots will change where the look changes (IBL, cascades). Update `assets/models/review` only if a test model is needed (`gen_review_assets`), add snapshot scenes `lights` (several point / spot lights, some shadowed) and `alpha` (mask + blend), and say in the report which existing snapshot images changed and why.
- Run the GPU tests under both backends when WGSL changes (AGENTS.md).

## Constraints

AGENTS.md rendering rules (MSAA pipelines per sample count, group layouts, GI backends, shadow setup gotchas, overlay shapes, frame timings). Tests that are not about a feature turn it off as existing tests do (`gi_intensity: 0.0`, etc.). Keep `melon_game` on `melon_core` only: light components and material assignment are in core.

## Done when

- Tests: material file round trip, missing texture binds the 1x1 fallback, texture hot reload, Mask discards below cutoff, Blend sorted far to near; light culling puts each light in the right clusters (CPU test of the cluster math + GPU test of the lit result); light outside its range adds nothing; IBL: a white diffuse sphere under a uniform sky matches the expected irradiance; cascades: split distances, texel-snapped matrices stable under small camera moves, a shadow in the far cascade; spot / point shadow occludes; shadowed-light cap honoured; LOD level selection with hysteresis.
- Budget: on the dev GPU at the snapshot default size, High preset, the `lights` scene (32 point / spot lights, 4 shadowed) costs at most ~2 ms more than without the lights (GPU timings). Report the numbers.
- `just test` green on DX12 and Vulkan; AGENTS.md (rendering notes, groups, snapshot flags) and ROADMAP (priority 3 ticked) updated.
