// SSGI shading for the mesh shader (GI_MODE == GI_SSGI): the view's
// denoised half- or quarter-resolution indirect diffuse (ssgi.wgsl), upsampled with
// geometry-aware bilinear weights. Prepended to mesh.wgsl. Group 3 bindings
// 6 to 8 (gi/ssgi.rs `layout_entries`).

// Mirrors gi/ssgi.rs `SsgiUniform` (and ssgi.wgsl).
struct SsgiUniform {
    prev_view_proj: mat4x4f,
    prev_eye: vec3f,
    frame: u32,
    history_valid: u32,
    intensity: f32,
    // 1 = the mesh shader outputs indirect diffuse only (debug).
    gi_only: u32,
    // View pixels per texel (0 in the stand-in).
    scale: u32,
};

// Indirect diffuse (rgb, cosine-weighted mean radiance) and the geometry
// of each texel: world normal (xyz), eye distance (w, 0 = no surface; the
// 1x1 zero stand-in without SSGI).
@group(3) @binding(6) var ssgi_tex: texture_2d<f32>;
@group(3) @binding(7) var<uniform> ssgi: SsgiUniform;
@group(3) @binding(8) var ssgi_geometry: texture_2d<f32>;

// Diffuse light at `p` (geometric normal `ng`, framebuffer position
// `frag`) in place of the hemisphere `hemi`: the 4 nearest
// reduced-resolution texels, bilinear, each only if it saw this surface (eye
// distance within 10%, normal within about 25 degrees). None does (a
// silhouette or crease the reduced-resolution pass missed): the hemisphere.
fn ssgi_diffuse(p: vec3f, ng: vec3f, frag: vec2f, hemi: vec3f) -> vec3f {
    let last = vec2i(textureDimensions(ssgi_tex)) - 1;
    // Texel i holds full pixel scale * i, centred at scale * i + 0.5.
    let h = (frag - 0.5) / f32(max(ssgi.scale, 1u));
    let base = vec2i(floor(h));
    let f = h - floor(h);
    let dist = distance(p, camera.eye);
    var sum = vec3f(0.0);
    var weight = 0.0;
    for (var i = 0; i < 4; i++) {
        let o = vec2i(i & 1, i >> 1u);
        let q = clamp(base + o, vec2i(0), last);
        let g = textureLoad(ssgi_geometry, q, 0);
        let bilinear = select(1.0 - f, f, o == vec2i(1));
        let seen = g.w > 0.0 && abs(g.w - dist) < 0.1 * dist && dot(g.xyz, ng) > 0.9;
        let w = select(0.0, bilinear.x * bilinear.y + 1e-4, seen);
        sum += w * textureLoad(ssgi_tex, q, 0).rgb;
        weight += w;
    }
    if (weight <= 0.0) {
        return hemi;
    }
    return ssgi.intensity * sum / weight;
}
