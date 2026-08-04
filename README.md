# MelonEngine

A from-scratch 3D game engine written in Rust.

## Status

v0.1 — released.

## Crates

| Crate | Role |
|---|---|
| `melon_core` | Math (glam) re-exports, ECS (bevy_ecs) world/schedule bootstrap, Time resource |
| `melon_render` | wgpu device/swapchain, pipelines, meshes, glTF loading, WGSL shaders |
| `melon_physics` | rapier3d wrapper + transform sync systems |
| `melon_editor` | egui-wgpu debug overlay (FPS, inspector, gizmos) |
| `melon_host` | Binary crate: winit event loop, wires all systems, owns hot-reloader |
| `melon_game` | Dylib crate: game logic, hot-reloadable |

## Run

Dev build with hot reload:

```sh
cargo run -p melon_host
```

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

- **WASD** — move horizontally
- **Space / Shift** — move up / down
- **Right-click + drag** — look around (yaw/pitch)

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
- **Hot reload**: hot-lib-reloader 0.8
- **Assets**: gltf 1.4, image 0.25
- **Math**: glam 0.33

## License

Dual-licensed under MIT or Apache-2.0, at your option. See `LICENSE-MIT`
and `LICENSE-APACHE`.