# <img src="assets/branding/melon-engine.png" alt="" width="48" height="48"> MelonEngine

A from-scratch 3D game engine written in Rust.

## Status

v0.1 — released.

## Crates

| Crate | Role |
|---|---|
| `melon_core` | Math (glam) re-exports, ECS (bevy_ecs) world/schedule bootstrap, Time resource |
| `melon_render` | wgpu device/swapchain, pipelines, meshes, glTF loading, WGSL shaders. Frame: sun shadow pass, 3D pass into an HDR target, then a tonemap pass onto the display, then an overlay pass (solid editor shapes such as the gizmo) |
| `melon_physics` | rapier3d wrapper + transform sync systems |
| `melon_editor` | egui-wgpu debug overlay (FPS, inspector, gizmos) |
| `melon_host` | Binary crate: winit event loop, wires all systems, loads the game plugin |
| `melon_game` | Game plugin (`GamePlugin`): game logic, live-reloaded in dev builds |

## Run

Dev build with hot reload:

```sh
cargo run -p melon_host
```

Hot reload: while it runs, edit `crates/melon_game` and rebuild only the game
crate. The editor reloads the game plugin and the Console shows "Game plugin
reloaded"; the scene stays as it is.

```sh
just game      # cargo build -p melon_host --lib
```

Changing the fields of any game component **or resource** type needs a
restart. Only a size/align change of a resource registered in `build` is
detected (the plugin stops with an error); any other field change is not, so
always restart. `build` must be idempotent: only `add_systems`,
`init_resource`, `insert_resource`. Spawning entities, observers or component
hooks from `build` repeats on every reload (a second hook registration panics).

`cargo test` / `just test` while a dev-run is open rewrites
`target/debug/melon_game.dll` with other features; the running host refuses
that reload (fingerprint error). Run `just game` again.

Or via the justfile:

```sh
just run       # gate (fmt + clippy + check) then run
just fast-run  # skip gate, run immediately
```

## Ship build

Release build with hot reload disabled (statically links melon_game):

```sh
just ship       # cargo build -p melon_host --release --no-default-features
just ship-run   # cargo run -p melon_host --release --no-default-features
```

## Controls

Viewport (Unity-style, pointer over the viewport):

- **Right drag** — look around
- **Right held + W/A/S/D** — fly; **Q / E** down / up; **Shift** ×3 speed
- **Right held + scroll** — change fly speed
- **Alt + left drag** — orbit around the pivot
- **Middle drag** — pan
- **Scroll** — zoom toward the pivot
- **F** — frame the selected entity
- **Left click** — select (click empty space to deselect); drag a gizmo handle to move/rotate/scale. Move: arrows (one axis), the small squares (two axes), the grey centre ball (free). Scale: cube tips (one axis), the triangles (two axes), the centre cube (uniform). Hold **Shift** while dragging to snap

Simulation:

- **F5** — Play / resume, **F6** — Pause, **Shift+F5** — Stop (restores the scene)
- While playing, the game owns keyboard and mouse (cursor hidden). **Esc**
  hands them back to the editor (the game keeps running); **left click** on
  the Scene viewport gives them to the game again
- **Step** (toolbar, while paused) — advance one fixed step

Editor:

- **W / E / R** — Move / Rotate / Scale tool
- **Delete** — delete the selected entity
- **Ctrl+S** — save the scene (File ▸ Save As… the first time)
- **Models:** drag a `.glb` / `.gltf` from the Content Browser into the Scene
  (lands on the ground under the cursor) or the Hierarchy (at the origin, or
  under the row you drop it on). It unpacks into an entity per glTF node. Or
  pick a whole file in the Inspector's **Mesh** field. Edit and re-export a model and
  the editor reloads it within about a second.
- **Hierarchy:** drag an entity onto another to make it a child (it keeps its
  place in the world), or onto the empty space below to make it a root.
  Deleting an entity deletes its children.
- **Edit ▸ Project Settings…** — Input Mapping: game actions (Button or
  Axis) and their keys / mouse buttons. **+ binding** (or **+ pair** for an
  axis: positive, then negative) waits for the next key; **Esc** cancels.
  Changes apply live and save to `config/input.ron`; a broken file falls
  back to the defaults and is kept as `config/input.ron.broken` on the first save.
- Panels (Hierarchy, Scene, Inspector, Content Browser, Console) dock and tab
  by dragging their tabs; the arrangement is saved to `.melon/editor_layout.ron`.
  **Window** reopens closed panels; **Window ▸ Reset Layout** restores the default.

The editor opens in edit mode. Pass `--play` to start playing right away:

```sh
cargo run -p melon_host -- --play
```

## Assets

glTF models go in `assets/models/`, WGSL shaders in `assets/shaders/`.
The engine loads `assets/models/cube.glb` at startup as the central spinning
entity. See `assets/README.md` for details.

## Stack

- **ECS**: bevy_ecs 0.19
- **Render**: wgpu 29 (Vulkan/Metal/D3D12)
- **Window**: winit 0.30
- **Physics**: rapier3d 0.34
- **Editor UI**: egui 0.35 + egui-wgpu + egui-winit
- **Game reload**: libloading 0.8
- **Assets**: gltf 1.4, image 0.25
- **Math**: glam 0.33

## License

Dual-licensed under MIT or Apache-2.0, at your option. See `LICENSE-MIT`
and `LICENSE-APACHE`.