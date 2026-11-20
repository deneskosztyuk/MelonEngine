// Screen-space diffuse GI passes (gi/ssgi.rs), fullscreen triangles at half
// resolution after the depth/normal prepass. gi_common.wgsl is prepended.
// fs_trace: per texel (the sample of full pixel 2i), SSGI_RAYS
// cosine-weighted rays (rotated per pixel and frame) march the prepass eye
// distances; a hit reads last frame's HDR colour where the hit point was
// last frame, a miss (or a hit off last frame's screen) the sky light. The
// mean blends into the reprojected history, kept where last frame's eye
// distance and normal match. fs_denoise: 5x5 depth- and normal-aware blur
// into the output that gi_ssgi.wgsl upsamples, plus its geometry.

struct CameraUniform {
    view_proj: mat4x4f,
    inv_view_proj: mat4x4f,
    eye: vec3f,
};

struct LightUniform {
    direction: vec3f,
    ambient: f32,
    color: vec3f,
    _pad: f32,
    sky_color: vec3f,
    _pad1: f32,
    ground_color: vec3f,
    _pad2: f32,
    light_view_proj: mat4x4f,
};

// Mirrors gi/ssgi.rs `SsgiUniform` (and gi_ssgi.wgsl).
struct SsgiUniform {
    prev_view_proj: mat4x4f,
    prev_eye: vec3f,
    frame: u32,
    history_valid: u32,
    intensity: f32,
    gi_only: u32,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(0) @binding(1) var<uniform> light: LightUniform;
// fs_trace: full-resolution prepass depth and normal (xyz) + eye distance
// (w, 0 where nothing was drawn), last frame's resolved HDR colour, and
// last frame's history and geometry (half resolution).
@group(1) @binding(0) var depth_tex: texture_depth_2d;
@group(1) @binding(1) var normal_tex: texture_2d<f32>;
@group(1) @binding(2) var<uniform> ssgi: SsgiUniform;
@group(1) @binding(3) var prev_hdr: texture_2d<f32>;
@group(1) @binding(4) var history_prev: texture_2d<f32>;
@group(1) @binding(5) var geometry_prev: texture_2d<f32>;
// fs_denoise: this frame's history and geometry.
@group(1) @binding(6) var history_tex: texture_2d<f32>;
@group(1) @binding(7) var geometry_tex: texture_2d<f32>;

const PI: f32 = 3.14159265;
const SSGI_RAYS: u32 = 6u;
const SSGI_STEPS: u32 = 12u;
// Metres a ray marches; steps grow quadratically (finer near the surface).
const MAX_DISTANCE: f32 = 4.0;
// How far (metres) behind the depth buffer a ray sample still hits: surfaces
// are assumed this thick.
const THICKNESS: f32 = 0.5;
// The history blends at least 1 / MAX_HISTORY of each new frame in.
const MAX_HISTORY: f32 = 32.0;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    // Vertices (-1,-1), (3,-1), (-1,3) cover the whole screen.
    let corner = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

// Screen uv (0..1, y down) of world point `p` under `m`; negative when
// behind the camera or off screen (gi/ssgi.rs `reproject`).
fn screen_uv(m: mat4x4f, p: vec3f) -> vec2f {
    let clip = m * vec4f(p, 1.0);
    let uv = clip.xy / clip.w * vec2f(0.5, -0.5) + 0.5;
    if (clip.w <= 0.0 || any(uv < vec2f(0.0)) || any(uv >= vec2f(1.0))) {
        return vec2f(-1.0);
    }
    return uv;
}

// Interleaved gradient noise in [0, 1) (Jimenez 2014).
fn noise(px: vec2f) -> f32 {
    return fract(52.9829189 * fract(dot(px, vec2f(0.06711056, 0.00583715))));
}

// Radiance arriving at `o` along unit `d`: last frame's colour of the first
// surface the ray passes behind (within THICKNESS), else the sky light.
// `jitter` in [0, 1) shifts the steps.
fn trace(o: vec3f, d: vec3f, jitter: f32) -> vec3f {
    if (ssgi.history_valid == 0u) {
        // No usable last frame (new view, cut, SSGI just on).
        return sky_light(d);
    }
    let size = vec2f(textureDimensions(normal_tex));
    for (var i = 0u; i < SSGI_STEPS; i++) {
        let f = (f32(i) + jitter + 0.5) / f32(SSGI_STEPS);
        let s = o + d * (MAX_DISTANCE * f * f);
        let uv = screen_uv(camera.view_proj, s);
        if (uv.x < 0.0) {
            break;
        }
        let scene = textureLoad(normal_tex, vec2i(uv * size), 0).w;
        let behind = distance(s, camera.eye) - scene;
        // A small margin keeps depth quantisation from hitting the surface
        // the ray starts on.
        if (scene > 0.0 && behind > 0.01 * scene && behind < THICKNESS) {
            let prev = screen_uv(ssgi.prev_view_proj, s);
            if (prev.x < 0.0) {
                break;
            }
            return textureLoad(prev_hdr, vec2i(prev * size), 0).rgb;
        }
    }
    return sky_light(d);
}

struct TraceOut {
    // Accumulated indirect diffuse (rgb), frames accumulated (a).
    @location(0) history: vec4f,
    // World normal (xyz), eye distance (w; 0 = no surface).
    @location(1) geometry: vec4f,
};

@fragment
fn fs_trace(@builtin(position) pos: vec4f) -> TraceOut {
    let texel = vec2i(pos.xy);
    let full = vec2i(textureDimensions(normal_tex));
    let px = min(texel * 2, full - 1);
    let g = textureLoad(normal_tex, px, 0);
    var out: TraceOut;
    out.history = vec4f(0.0);
    out.geometry = vec4f(0.0);
    if (g.w <= 0.0) {
        return out;
    }
    let size = vec2f(full);
    let ndc = vec2f((f32(px.x) + 0.5) / size.x * 2.0 - 1.0, 1.0 - (f32(px.y) + 0.5) / size.y * 2.0);
    let clip = camera.inv_view_proj * vec4f(ndc, textureLoad(depth_tex, px, 0), 1.0);
    let p = clip.xyz / clip.w;
    let n = normalize(g.xyz);
    let dist = g.w;

    // Cosine-weighted rays: stratified radius, golden-angle spin, both
    // rotated by noise that changes every frame.
    let helper = select(vec3f(0.0, 1.0, 0.0), vec3f(1.0, 0.0, 0.0), abs(n.y) > 0.9);
    let t = normalize(cross(helper, n));
    let b = cross(n, t);
    let shift = 5.588238 * f32(ssgi.frame % 64u);
    let r1 = noise(vec2f(texel) + shift);
    let r2 = noise(vec2f(texel.yx) + 2.0 * shift + 17.0);
    // Off the surface, more with distance (depth precision).
    let o = p + n * (0.02 + 0.002 * dist);
    var sum = vec3f(0.0);
    for (var k = 0u; k < SSGI_RAYS; k++) {
        let u = (f32(k) + r1) / f32(SSGI_RAYS);
        let phi = 2.0 * PI * fract(f32(k) * 0.618034 + r2);
        let d = (cos(phi) * t + sin(phi) * b) * sqrt(u) + n * sqrt(1.0 - u);
        sum += trace(o, d, fract(r1 + f32(k) * 0.618034));
    }
    let sample = sum / f32(SSGI_RAYS);

    // History: where this point was last frame, if that texel saw the same
    // surface (eye distance within 5%, normal within about 25 degrees).
    var history = vec3f(0.0);
    var count = 0.0;
    let prev = screen_uv(ssgi.prev_view_proj, p);
    if (ssgi.history_valid != 0u && prev.x >= 0.0) {
        let last = vec2i(textureDimensions(history_prev)) - 1;
        let q = min(vec2i(floor(prev * size * 0.5 + 0.25)), last);
        let pg = textureLoad(geometry_prev, q, 0);
        let expected = distance(p, ssgi.prev_eye);
        if (pg.w > 0.0 && abs(pg.w - expected) < 0.05 * expected && dot(pg.xyz, n) > 0.9) {
            let h = textureLoad(history_prev, q, 0);
            history = h.rgb;
            count = h.a;
        }
    }
    let frames = min(count + 1.0, MAX_HISTORY);
    // Without a last frame the sample is sky only: count it as nothing, so
    // the next frame replaces it.
    out.history = vec4f(mix(history, sample, 1.0 / frames), select(0.0, frames, ssgi.history_valid != 0u));
    out.geometry = vec4f(n, dist);
    return out;
}

struct DenoiseOut {
    // Indirect diffuse (rgb).
    @location(0) output: vec4f,
    // This texel's normal (xyz) and eye distance (w; 0 = no surface).
    @location(1) geometry: vec4f,
};

@fragment
fn fs_denoise(@builtin(position) pos: vec4f) -> DenoiseOut {
    let c = vec2i(pos.xy);
    let last = vec2i(textureDimensions(history_tex)) - 1;
    let g0 = textureLoad(geometry_tex, c, 0);
    var out: DenoiseOut;
    out.output = vec4f(0.0);
    out.geometry = g0;
    if (g0.w <= 0.0) {
        return out;
    }
    var sum = vec3f(0.0);
    var weight = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let q = clamp(c + vec2i(x, y), vec2i(0), last);
            let g = textureLoad(geometry_tex, q, 0);
            // Other surfaces (silhouettes, creases) are skipped.
            let same = g.w > 0.0 && abs(g.w - g0.w) < 0.05 * g0.w;
            let w = select(0.0, pow(max(dot(g.xyz, g0.xyz), 0.0), 8.0), same);
            sum += w * textureLoad(history_tex, q, 0).rgb;
            weight += w;
        }
    }
    out.output = vec4f(sum / max(weight, 1e-4), 1.0);
    return out;
}
