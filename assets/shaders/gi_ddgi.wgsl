// DDGI shading for the mesh shader (GI_MODE == GI_DDGI): the 8 probes
// around a point, weighted by trilinear, backface, Chebyshev and voxel-march
// visibility. Prepended to mesh.wgsl after gi_common.wgsl and
// ddgi_common.wgsl; the voxel passes there share `gi_irradiance`. Group 3
// bindings 1 to 5 (gi/ddgi.rs `layout_entries`).

// DDGI probe atlases: irradiance (rgb, cosine-weighted mean radiance) and
// distance moments (r = mean, g = mean square), octahedral tiles with a
// 1-texel border; linear clamp sampler; volume and probe grid settings.
@group(3) @binding(1) var irradiance_atlas: texture_2d<f32>;
@group(3) @binding(2) var depth_atlas: texture_2d<f32>;
@group(3) @binding(3) var gi_sampler: sampler;
@group(3) @binding(4) var<uniform> gi: GiUniform;
// The voxel volume (read-only; see VOXEL_SCALE), for probe visibility. The
// voxel passes bind a dummy here and never read it.
@group(3) @binding(5) var<storage, read> voxel_grid: array<u32>;

// Atlas uv of direction `dir` in probe `cell`'s tile (`n` interior texels).
fn probe_uv(cell: vec3i, dir: vec3f, n: u32, atlas: texture_2d<f32>) -> vec2f {
    let px = vec2f(probe_tile(cell, n + 2u)) + 1.0 + (oct_encode(dir) * 0.5 + 0.5) * f32(n);
    return px / vec2f(textureDimensions(atlas));
}

// Whether the segment from `a` to probe `b` crosses no occupied voxel
// (3D DDA). The voxel of `a` is skipped: a surface's own voxel, or a
// neighbouring surface's, can stick out past the shading point by up to a
// voxel at inside corners. The march stops a voxel short of the probe:
// probes sit on voxel corners, where the last steps would tie.
fn voxel_clear(a: vec3f, b: vec3f) -> bool {
    let counts = vec3i(gi.voxel_counts);
    let ga = (a - gi.origin) / gi.voxel_size;
    let d = (b - a) / gi.voxel_size;
    var cell = vec3i(floor(ga));
    let step = vec3i(sign(d));
    // In fractions of the segment.
    let t_delta = 1.0 / max(abs(d), vec3f(1e-8));
    var t_max = abs(select(floor(ga), floor(ga) + 1.0, d > vec3f(0.0)) - ga) * t_delta;
    let t_stop = 1.0 - 1.0 / max(length(d), 1.0);
    // A probe is at most 2.5 m * sqrt(3) (14 voxels) from the point.
    for (var i = 0; i < 48; i++) {
        if (min(t_max.x, min(t_max.y, t_max.z)) >= t_stop) {
            break;
        }
        if (t_max.x < t_max.y && t_max.x < t_max.z) {
            cell.x += step.x;
            t_max.x += t_delta.x;
        } else if (t_max.y < t_max.z) {
            cell.y += step.y;
            t_max.y += t_delta.y;
        } else {
            cell.z += step.z;
            t_max.z += t_delta.z;
        }
        if (all(cell >= vec3i(0)) && all(cell < counts)
            && voxel_grid[3u * u32(cell.x + counts.x * (cell.y + counts.y * cell.z))] != 0u) {
            return false;
        }
    }
    return true;
}

// Diffuse irradiance (cosine-weighted mean radiance) at `p` with normal `n`
// from the 8 surrounding probes. Weights: trilinear, backface (probes
// behind the surface count less), Chebyshev visibility from the distance
// moments (probes that cannot see the point drop out; the moments end at
// the hit voxel's near side, up to a voxel before the real surface, so a
// point less than a voxel behind them still counts as seen), and with
// `trace_visibility` a voxel march from the point to each probe: probes
// behind geometry drop out, which stops the variance leaks of probes that
// see past a wall's edge (bright rims in inside corners). If the march
// blocks every probe, the weights without it are used. `v` points to the
// viewer: the lookup point moves toward it and along the normal (DDGI's
// self-shadow bias) so a surface does not shadow itself.
fn gi_irradiance(p: vec3f, n: vec3f, v: vec3f, trace_visibility: bool) -> vec3f {
    let biased = p + (0.2 * n + 0.8 * v) * (0.3 * 0.75 * gi.spacing);
    let grid = (biased - gi.origin) / gi.spacing - 0.5;
    let base = clamp(vec3i(floor(grid)), vec3i(0), vec3i(gi.probe_counts) - 2);
    let alpha = clamp(grid - vec3f(base), vec3f(0.0), vec3f(1.0));
    var sum = vec3f(0.0);
    var weight_sum = 0.0;
    var seen_sum = vec3f(0.0);
    var seen_weight = 0.0;
    for (var i = 0u; i < 8u; i++) {
        let offset = vec3u(i, i >> 1u, i >> 2u) & vec3u(1u);
        let cell = base + vec3i(offset);
        let probe = probe_position(cell);
        let to_probe = normalize(probe - p);
        let wrap = (dot(to_probe, n) + 1.0) * 0.5;
        var w = wrap * wrap + 0.2;
        let from_probe = biased - probe;
        let r = length(from_probe);
        let moments = textureSampleLevel(depth_atlas, gi_sampler,
            probe_uv(cell, from_probe / max(r, 1e-4), DEPTH_TEXELS, depth_atlas), 0.0).rg;
        let d = r - moments.x - gi.voxel_size;
        if (d > 0.0) {
            let variance = abs(moments.y - moments.x * moments.x);
            let chebyshev = variance / (variance + d * d);
            w *= chebyshev * chebyshev * chebyshev;
        }
        w = max(w, 1e-6);
        // Crush tiny weights further, so probes that see the point win
        // over leaky ones.
        if (w < 0.2) {
            w *= w * w / (0.2 * 0.2);
        }
        let t = select(1.0 - alpha, alpha, offset == vec3u(1u));
        w *= t.x * t.y * t.z;
        let irradiance = textureSampleLevel(irradiance_atlas, gi_sampler,
            probe_uv(cell, n, IRRADIANCE_TEXELS, irradiance_atlas), 0.0).rgb;
        sum += w * irradiance;
        weight_sum += w;
        if (trace_visibility && voxel_clear(biased, probe)) {
            seen_sum += w * irradiance;
            seen_weight += w;
        }
    }
    if (seen_weight > 0.0) {
        return seen_sum / seen_weight;
    }
    return sum / max(weight_sum, 1e-30);
}

// 1 inside the GI volume, fading to 0 one probe spacing outside it.
fn gi_volume_weight(p: vec3f) -> f32 {
    let hi = gi.origin + vec3f(gi.probe_counts) * gi.spacing;
    let outside = max(max(gi.origin - p, p - hi), vec3f(0.0));
    return clamp(1.0 - max(outside.x, max(outside.y, outside.z)) / gi.spacing, 0.0, 1.0);
}

// Diffuse light at `p` (normal `n`, `v` toward the viewer) from the probes,
// in place of the hemisphere `hemi`: blends back to it within one probe
// spacing outside the GI volume.
fn ddgi_diffuse(p: vec3f, n: vec3f, v: vec3f, hemi: vec3f) -> vec3f {
    let inside = gi_volume_weight(p);
    var diffuse_light = hemi;
    if (inside > 0.0) {
        diffuse_light = mix(hemi, gi.intensity * gi_irradiance(p, n, v, true), inside);
    }
    return diffuse_light;
}
