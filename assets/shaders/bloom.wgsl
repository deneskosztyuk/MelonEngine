// Bloom mip chain (the "next-gen post" method, Jimenez 2014): no threshold,
// every pixel blooms in proportion to its radiance. Fullscreen triangles
// into one mip of the view's bloom texture each:
// - fs_downsample_first: resolved HDR image -> mip 0 (half resolution),
//   13-tap filter with a Karis average (fireflies don't flicker).
// - fs_downsample: mip i -> mip i + 1, the same 13 taps, plain weights.
// - fs_upsample: 3x3 tent of mip i + 1, added (blend One + One) to mip i.
// Mip 0 ends up holding the sum of all levels; the tonemap pass divides by
// the level count and mixes it in.

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

struct VertexOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VertexOut {
    // Vertices (-1,-1), (3,-1), (-1,3) cover the whole target.
    let corner = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VertexOut;
    out.pos = vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2f(corner.x, 1.0 - corner.y);
    return out;
}

fn tap(uv: vec2f, texel: vec2f, x: f32, y: f32) -> vec3f {
    return textureSampleLevel(src_tex, src_sampler, uv + texel * vec2f(x, y), 0.0).rgb;
}

fn luma(c: vec3f) -> f32 {
    return dot(c, vec3f(0.2126, 0.7152, 0.0722));
}

// The 13 taps as five overlapping 2x2 boxes (bilinear taps): the centre
// box weighs 0.5, the four corner boxes 0.125 each. `karis` weights each
// box by 1 / (1 + luma) and renormalises.
fn downsample(uv: vec2f, karis: bool) -> vec3f {
    let texel = 1.0 / vec2f(textureDimensions(src_tex));
    let a = tap(uv, texel, -2.0, 2.0);
    let b = tap(uv, texel, 0.0, 2.0);
    let c = tap(uv, texel, 2.0, 2.0);
    let d = tap(uv, texel, -2.0, 0.0);
    let e = tap(uv, texel, 0.0, 0.0);
    let f = tap(uv, texel, 2.0, 0.0);
    let g = tap(uv, texel, -2.0, -2.0);
    let h = tap(uv, texel, 0.0, -2.0);
    let i = tap(uv, texel, 2.0, -2.0);
    let j = tap(uv, texel, -1.0, 1.0);
    let k = tap(uv, texel, 1.0, 1.0);
    let l = tap(uv, texel, -1.0, -1.0);
    let m = tap(uv, texel, 1.0, -1.0);
    let boxes = array<vec3f, 5>(
        (j + k + l + m) * 0.25,
        (a + b + d + e) * 0.25,
        (b + c + e + f) * 0.25,
        (d + e + g + h) * 0.25,
        (e + f + h + i) * 0.25,
    );
    var sum = vec3f(0.0);
    var total = 0.0;
    for (var n = 0; n < 5; n++) {
        var w = select(0.125, 0.5, n == 0);
        if karis {
            w /= 1.0 + luma(boxes[n]);
        }
        sum += boxes[n] * w;
        total += w;
    }
    return sum / total;
}

@fragment
fn fs_downsample_first(in: VertexOut) -> @location(0) vec4f {
    return vec4f(downsample(in.uv, true), 1.0);
}

@fragment
fn fs_downsample(in: VertexOut) -> @location(0) vec4f {
    return vec4f(downsample(in.uv, false), 1.0);
}

@fragment
fn fs_upsample(in: VertexOut) -> @location(0) vec4f {
    // 3x3 tent (1 2 1 / 2 4 2 / 1 2 1) / 16, one source texel apart.
    let texel = 1.0 / vec2f(textureDimensions(src_tex));
    let uv = in.uv;
    var c = tap(uv, texel, 0.0, 0.0) * 4.0;
    c += (tap(uv, texel, -1.0, 0.0) + tap(uv, texel, 1.0, 0.0) + tap(uv, texel, 0.0, -1.0)
        + tap(uv, texel, 0.0, 1.0)) * 2.0;
    c += tap(uv, texel, -1.0, -1.0) + tap(uv, texel, 1.0, -1.0) + tap(uv, texel, -1.0, 1.0)
        + tap(uv, texel, 1.0, 1.0);
    return vec4f(c / 16.0, 1.0);
}
