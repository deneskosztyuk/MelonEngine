# Asset packaging and ship builds (ROADMAP priority 9)

## Intent

Turn a MelonEngine project into a game someone else can run: Unity's Build Settings + Player Settings, Godot's Export (`.pck`), Unreal's Package Project (cook + `.pak`). Today the host is always the editor, the project root is a compile-time path (`PROJECT_ROOT` in melon_host), and scenes load only through the editor crate.

## Goals

1. **Scenes are runtime.** Move scene serialization and loading (registry-driven RON, `uid`, root order, versions and old-version conversion) out of `melon_editor` into `melon_core`, so a game without the editor can load scenes and game code can request a scene change (a core request queue, e.g. `SceneRequests::load("assets/scenes/level2.scene")`, applied between frames). The editor keeps using it (save, load, Play snapshot, undo history), with no behaviour change. Physics bodies come from the physics sync systems after spawning, as they already do.
2. **Player mode.** The host runs without the editor: no egui editor, the game Playing from the first frame, the startup scene loaded, window title = game name, window mode / resolution / vsync from settings, Esc not reserved by the engine. A cargo feature (or a second binary) builds the player without `melon_editor` linked at all. The project root is found at runtime (next to the executable for a packaged game, the source tree for dev builds).
3. **Project ▸ Player settings** (Edit ▸ Project Settings ▸ Player, same pending-until-Apply pattern as Quality / Audio / Physics, saved in `config/player.ron`): game name, startup scene, default window mode (windowed / borderless fullscreen) and size, version string.
4. **Asset pack.** Packaging writes all assets under `assets/` (minus exclude patterns from the player settings; editor-only data such as thumbnails and `.melon/` never) into one `game.pak` file with an index (path, GUID, offset, size); `.meta` GUIDs resolve from the index. `AssetServer` reads from the pack when one is present, else from disk. No hot reload in packaged games. Config defaults (`config/*.ron`) ship too.
5. **Build command.** Editor File ▸ Build… (output folder, Build / Build and Run, a log in the Console) and `just package <out>` from the command line. Both build the release player and write `<out>/<GameName>/` with the executable, `game.pak` and everything the executable needs at runtime. **The packaged game must run on a machine without the Windows SDK or Rust installed:** today DX12 loads `dxcompiler.dll` / `dxil.dll` from the Windows SDK, so either ship those files (check their redistribution terms) or use a shader-compiler setting that needs no extra DLL; document the choice.
6. **Smoke run.** The player accepts `--frames N` (render N frames, then exit 0) so a test can launch a freshly packaged game and see it start, load its startup scene and exit cleanly.

## Decisions

- Pack format: a simple custom archive (header, index, uncompressed blobs, 8-byte aligned) read with buffered seeks or a memory map without `unsafe` (std file reads are fine); compression is not needed now. Document the layout in AGENTS.md.
- All assets ship (Godot's default), filtered by exclude globs; no "scenes in build" list for now.
- Dependency direction stays `core <- render, physics, audio <- editor <- host`; the player build links core, render, physics, audio and the game, not the editor.

## Constraints

AGENTS.md (scene format rules and versions, registry, `.meta` files, game plugin: release builds call `core.add_plugin(&GamePlugin)`, never replace `core.world`, settings windows). Keep every existing scene test passing after the move (move the tests with the code).

## Done when

- Tests: scene round trip and all old-version loads from their new home in core; a scene change requested by a system applies next frame and despawns the old scene's entities; pack round trip (write a temp project, read every asset back byte-identical, GUID lookups, excluded files absent); `AssetServer` prefers the pack; player settings round trip; packaging a temp project produces the expected folder; an end-to-end test (may be `#[ignore]` if it needs a full release build, but run it once and report) packages a small project, runs the player with `--frames 30`, and checks exit code 0 and the log line for the loaded startup scene.
- `just test` green; AGENTS.md (player mode, pack, build commands, scene module location) and ROADMAP (priority 9 ticked) updated; the report gives the user the exact commands to build and run a packaged game.
