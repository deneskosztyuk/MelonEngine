# Assets

Place game assets here.

- `models/` — glTF models (`.glb` or `.gltf`). Loaded via `melon_render::Renderer::load_gltf`.
  - `cube.glb` — unit cube with 24 vertices, 36 indices, and a 4x4 pink base color texture. Used as the default spinning entity in the demo scene.
- `shaders/` — WGSL shader source files, included via `wgpu::include_wgsl!`.
  - `mesh.wgsl` — vertex + fragment shader for textured mesh rendering with directional lighting, 3x3 PCF sun shadows and exponential distance fog. Writes linear HDR colour (`Rgba16Float`).
  - `shadow.wgsl` — depth-only pass that renders every mesh from the sun into the 2048x2048 shadow map.
  - `sky.wgsl` — procedural sky: fullscreen triangle at the far plane behind the meshes, horizon-to-zenith gradient, darker ground below the horizon, HDR sun disc.
  - `tonemap.wgsl` — fullscreen pass that maps the HDR scene colour to the sRGB display target.
  - `line.wgsl` — gizmo/debug lines, drawn after tonemapping so their colours stay exact.