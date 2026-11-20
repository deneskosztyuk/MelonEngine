// Screen-space ambient occlusion, two fullscreen passes over the prepass.
// fs_ssao: 16 hemisphere samples around each pixel's surface point. The
// hemisphere is built around the world normal, which is view space up to a
// rotation, so positions come from depth via inv_view_proj (no view matrix).
// A sample counts when the visible surface on its view ray lies in front of
// it, above the pixel's tangent plane and within 2 radii (range check: no
// halos from distant occluders). A 4x4 tiled rotation pattern varies the
// samples per pixel. fs_blur: 4x4 depth-aware box blur, which averages the
// pattern away, then applies the strength. Output 1 = unoccluded.

struct CameraUniform {
    view_proj: mat4x4f,
    inv_view_proj: mat4x4f,
    eye: vec3f,
};

struct SsaoUniform {
    // Offsets in the unit +Z hemisphere (xyz; w unused).
    kernel: array<vec4f, 16>,
    radius: f32,
    strength: f32,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var depth_tex: texture_depth_2d;
// xyz world normal, w distance to the eye; 0 where no mesh was drawn.
@group(1) @binding(1) var normal_tex: texture_2d<f32>;
@group(1) @binding(2) var<uniform> ssao: SsaoUniform;
// fs_blur only: fs_ssao's output.
@group(1) @binding(3) var raw_ao_tex: texture_2d<f32>;

const PI: f32 = 3.14159265;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    // Vertices (-1,-1), (3,-1), (-1,3) cover the whole screen.
    let corner = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn world_at(ndc: vec2f, depth: f32) -> vec3f {
    let p = camera.inv_view_proj * vec4f(ndc, depth, 1.0);
    return p.xyz / p.w;
}

// Rotation angle of the samples at pixel `px`: a 4x4 Bayer pattern, so
// every 4x4 block (the blur window) holds all 16 angles.
fn rotation(px: vec2u) -> f32 {
    var bayer = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0,
                               3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
    return bayer[(px.y % 4u) * 4u + px.x % 4u] / 16.0 * 2.0 * PI;
}

@fragment
fn fs_ssao(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let px = vec2i(pos.xy);
    let depth = textureLoad(depth_tex, px, 0);
    if (depth >= 1.0) {
        return vec4f(1.0);
    }
    let size = vec2f(textureDimensions(depth_tex));
    let p = world_at(vec2f(pos.x / size.x * 2.0 - 1.0, 1.0 - pos.y / size.y * 2.0), depth);
    let n = normalize(textureLoad(normal_tex, px, 0).xyz);
    let helper = select(vec3f(0.0, 1.0, 0.0), vec3f(1.0, 0.0, 0.0), abs(n.y) > 0.9);
    let t0 = normalize(cross(helper, n));
    let b0 = cross(n, t0);
    let angle = rotation(vec2u(px));
    let t = cos(angle) * t0 + sin(angle) * b0;
    let b = cross(n, t);
    // Depth precision and the pixel grid: ignore surfaces this close.
    let bias = 0.05 * ssao.radius;
    var occlusion = 0.0;
    for (var i = 0; i < 16; i++) {
        let k = ssao.kernel[i].xyz;
        let s = p + (t * k.x + b * k.y + n * k.z) * ssao.radius;
        let clip = camera.view_proj * vec4f(s, 1.0);
        let ndc = clip.xy / clip.w;
        let uv = ndc * vec2f(0.5, -0.5) + 0.5;
        if (clip.w <= 0.0 || any(uv < vec2f(0.0)) || any(uv >= vec2f(1.0))) {
            continue;
        }
        // The visible surface on the sample's view ray.
        let q = world_at(ndc, textureLoad(depth_tex, vec2i(uv * size), 0));
        let in_front = distance(q, camera.eye) < distance(s, camera.eye) - bias;
        let above = dot(q - p, n) > bias;
        let range = 1.0 - smoothstep(ssao.radius, 2.0 * ssao.radius, distance(q, p));
        occlusion += select(0.0, range, in_front && above);
    }
    return vec4f(1.0 - occlusion / 16.0);
}

@fragment
fn fs_blur(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let px = vec2i(pos.xy);
    let last = vec2i(textureDimensions(raw_ao_tex)) - 1;
    let dist = textureLoad(normal_tex, px, 0).w;
    var sum = 0.0;
    var weight = 0.0;
    for (var y = -2; y < 2; y++) {
        for (var x = -2; x < 2; x++) {
            let q = clamp(px + vec2i(x, y), vec2i(0), last);
            // Neighbours on another surface (silhouette, sky) are skipped.
            let w = select(0.0, 1.0, abs(textureLoad(normal_tex, q, 0).w - dist) <= 0.05 * dist);
            sum += w * textureLoad(raw_ao_tex, q, 0).r;
            weight += w;
        }
    }
    let ao = select(1.0, sum / weight, weight > 0.0);
    return vec4f(1.0 - ssao.strength * (1.0 - ao));
}
