# Changelog

All notable changes to MelonEngine are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.1.0 — 2026-07-09

First release. A from-scratch 3D game engine in Rust.

### Milestones

- **M0 — Scaffolding**: Workspace with 6 crates, dependency pins, workspace lints.
- **M1 — Window + Device**: winit event loop, wgpu device/swapchain initialization.
- **M2 — ECS + Triangle**: bevy_ecs world/schedule bootstrap, hardcoded triangle shader.
- **M3 — Mesh + Camera + Transform**: Vertex buffer pipeline, camera uniform, Transform component, depth buffer.
- **M4 — Textures + Lighting**: Checkerboard albedo texture, directional light uniform, normal-matrix lighting.
- **M5 — Input + Camera**: Keyboard/mouse input resource, fly camera system with yaw/pitch clamping.
- **M6 — Physics**: rapier3d integration, dynamic/fixed cuboid spawning, transform sync systems.
- **M7 — egui overlay**: egui-wgpu debug overlay with entity list, transform inspector, light/camera editors.
- **M8 — Hot reload**: melon_game dylib loaded via hot-lib-reloader, rotation/bob systems swappable at runtime.
- **M9 — v0.1 polish**: Time resource, glTF asset loading, ship build target, LICENSE/docs.

### Features

- Forward single-pass wgpu 29 renderer with WGSL shaders
- bevy_ecs 0.19 standalone (no full Bevy App/Plugin system)
- rapier3d 0.34 physics with dynamic/fixed rigid bodies
- egui 0.35 debug overlay (FPS, entity inspector, light editor)
- Hot-reloadable game logic via hot-lib-reloader (dev only)
- glTF (.glb/.gltf) mesh + texture loading
- Fly camera with WASD + right-click-drag look
- Frame-rate-independent movement via Time resource