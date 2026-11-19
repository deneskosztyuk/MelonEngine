// Composite the resolved overlay layer (premultiplied alpha) onto the
// viewport's display image: one fullscreen triangle, one texel per pixel.
// Blending (One, OneMinusSrcAlpha) is set on the pipeline.

@group(0) @binding(0) var overlay_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let corner = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    return textureLoad(overlay_tex, vec2i(pos.xy), 0);
}
