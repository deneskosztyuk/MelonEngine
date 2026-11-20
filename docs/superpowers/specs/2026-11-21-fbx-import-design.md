# FBX import with ufbx

## Intent

Load `.fbx` models everywhere a `.glb` / `.gltf` works, the way Unity, Godot (ufbx since 4.3) and Unreal import FBX. The user's art pipeline (Tripo3D Smart Mesh, Blender) produces FBX: low-poly (500-2,000 triangles), clean topology, UVs, one or a few materials, 512 px textures usually kept as separate files (the sample `Sich/animals/horse/brown horse 3d model_v2.fbx` has no texture linked inside, no rig, no animation).

## Goals

1. **Load FBX models** (binary and ASCII, FBX 6.1 and later, as ufbx supports) through `AssetServer::load::<Model>("assets/models/x.fbx")`, from disk and from `game.pak`, with hot reload, into the same GPU model the glTF path builds (parts per material, bounds, tangents generated when missing, node hierarchy with `#meshN` addressing for unpacked models).
2. **Units and axes:** convert to the engine's convention (metres, right-handed, Y up) at load (ufbx `target_unit_meters` / `target_axes`), so an FBX in centimetres or Z-up matches a glTF of the same object. Mirrored transforms keep correct winding and tangent sign.
3. **Materials and textures:** map FBX materials to the engine's material system (base colour + texture, metallic / roughness, normal map, emissive; use ufbx's PBR mapping, falling back to the classic diffuse / specular fields). Textures: embedded content first, then the file path relative to the FBX, then a file with the same name next to the FBX; read through `AssetServer` (pack-aware). A missing texture logs one warning and binds the fallback.
4. **Editor parity:** `.fbx` is a Model in the Content Browser (thumbnail, drag into the scene, unpack into a hierarchy, Extract Materials writes `.mat` files and copies textures next to it, Inspector model field accepts it).
5. **Packaging:** `.fbx` and its textures ship in `game.pak` and load from it.

## Non-goals (now)

- **Skinning and animation playback.** The animation area (wave 3) owns the skeleton / clip data model; its spec gains the requirement "skins and clips import from glTF and FBX alike". This area only keeps the ufbx scene data needed later reachable (no throwaway animation types).
- Writing FBX, cameras / lights inside FBX files, NURBS, blend shapes.

## Decisions

- Crate: `ufbx = "0.11"` in `[workspace.dependencies]` (MIT / Unlicense, official bindings; builds the C library with the MSVC toolchain already installed). Use only the safe API (`load_memory` for both disk and pack bytes); the workspace's `unsafe_code = deny` stays untouched.
- Runtime load-time conversion, like glTF (no import / cook step, no converted copies on disk).
- One loader front: dispatch on the file extension inside `melon_render`'s model loading, sharing the mesh / material / bounds building with the glTF path instead of duplicating it.
- Test fixtures: small hand-written ASCII FBX files under the test code or `assets/models/review/` (cube with a material and an external texture, a Z-up centimetre file, a mirrored node, a multi-material mesh). Do not commit the user's art; the horse is a manual check only.

## Constraints

AGENTS.md (assets, `.meta` files, pack reads, ModelCache hot reload, materials, tangents, thumbnails, GPU test rules). Builds on the Rendering area's material system (`.mat` materials, `MeshMaterials`, Extract Materials), already on `main`.

## Done when

- Tests: ASCII fixtures load with the right vertex / triangle counts, bounds in metres and Y up; a centimetre Z-up file matches its metre Y-up twin; a mirrored node keeps outward normals; multi-material parts; embedded vs relative vs same-folder texture resolution and the missing-texture fallback; load from a pack; hot reload; a corrupt file keeps the last good model and logs an error (no panic); a GPU check that an FBX cube renders like the glTF cube.
- Manual check for the user (documented): drag `brown horse 3d model_v2.fbx` (with its texture next to it) into `assets/models`, see its thumbnail, drop it in the scene, check size and orientation against a glTF model.
- `just test` green; AGENTS.md (assets: FBX support, units / axes, texture lookup) and ROADMAP updated.
