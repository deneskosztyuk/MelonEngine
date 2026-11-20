// Procedural sky: one fullscreen triangle at the far plane, drawn after the
// meshes with a LessEqual depth test, so it fills only pixels no geometry
// covered. Horizon-to-zenith gradient above the horizon, a darker ground tone
// below it, and a small soft sun disc. Output is linear HDR.

struct CameraUniform {
    view_proj: mat4x4f,
    inv_view_proj: mat4x4f,
    eye: vec3f,
};

struct LightUniform {
    // Direction TOWARD the light (world space, normalized on CPU).
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

struct EnvironmentUniform {
    zenith_color: vec3f,
    fog_density: f32,
    horizon_color: vec3f,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(0) @binding(1) var<uniform> light: LightUniform;
@group(0) @binding(4) var<uniform> env: EnvironmentUniform;

// Sun disc radiance relative to the light colour: bright enough for bloom.
const SUN_RADIANCE: f32 = 40.0;
// Cosines of the disc's outer and inner angular radius (about 1.5 and 1 degrees).
const SUN_COS_OUTER: f32 = 0.99966;
const SUN_COS_INNER: f32 = 0.99985;

struct VsOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) ndc: vec2f,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // Vertices (-1,-1), (3,-1), (-1,3) cover the whole screen, at depth 1.
    let ndc = vec2f(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    var out: VsOut;
    out.clip_pos = vec4f(ndc, 1.0, 1.0);
    out.ndc = ndc;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    // View ray through this pixel: near plane to mid depth (the far plane may
    // sit at infinity for some projections).
    let near = camera.inv_view_proj * vec4f(in.ndc, 0.0, 1.0);
    let mid = camera.inv_view_proj * vec4f(in.ndc, 0.5, 1.0);
    let dir = normalize(mid.xyz / mid.w - near.xyz / near.w);

    let above = mix(env.horizon_color, env.zenith_color, sqrt(max(dir.y, 0.0)));
    let ground = 0.5 * env.horizon_color * light.ground_color;
    let below = mix(env.horizon_color, ground, smoothstep(0.0, 0.2, -dir.y));
    var color = select(below, above, dir.y >= 0.0);

    let sun = smoothstep(SUN_COS_OUTER, SUN_COS_INNER, dot(dir, light.direction));
    color += sun * SUN_RADIANCE * light.color;
    return vec4f(color, 1.0);
}
