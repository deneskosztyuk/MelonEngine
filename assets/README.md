# Assets

Place game assets here.

- `models/` — glTF models (`.glb` or `.gltf`). Loaded via `melon_render::Renderer::load_gltf`.
  - `cube.glb` — unit cube with 24 vertices, 36 indices, and a 4x4 pink base color texture. Used as the default spinning entity in the demo scene.
- `shaders/` — WGSL shader source files, included via `wgpu::include_wgsl!`.
  - `mesh.wgsl` — vertex + fragment shader for textured mesh rendering with directional lighting.