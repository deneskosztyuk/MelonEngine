// Tonemap pass: maps the HDR scene colour into display range. Draws one
// fullscreen triangle and reads one texel per pixel (textureLoad, no sampler).
// The output target is an sRGB view, so the hardware applies the sRGB encode.

@group(0) @binding(0) var hdr_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    // Vertices (-1,-1), (3,-1), (-1,3) cover the whole screen.
    let corner = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

// Linear HDR radiance in, linear display value in 0..1 out.
// ACES filmic fit (Narkowicz 2015), applied per channel.
fn tonemap(hdr: vec3f) -> vec3f {
    let x = max(hdr, vec3f(0.0));
    return (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14);
}

@fragment
fn fs_main(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let hdr = textureLoad(hdr_tex, vec2i(pos.xy), 0).rgb;
    return vec4f(clamp(tonemap(hdr), vec3f(0.0), vec3f(1.0)), 1.0);
}
