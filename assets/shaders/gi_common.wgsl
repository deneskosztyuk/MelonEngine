// Helpers shared by the GI shaders: the renderer prepends this file to
// mesh.wgsl, ddgi_update.wgsl and ssgi.wgsl (WGSL has no include). The
// including module declares `light` (its LightUniform).

// Sky light along a ray that leaves the scene (a GI miss): the hemisphere
// ambient's colours, `light.ambient * sky_color` above the horizon,
// `ground_color` below. Cosine-weighted over a normal's hemisphere this is
// exactly the GI-off ambient `ambient * mix(ground, sky, n.y * 0.5 + 0.5)`
// (an open plane tilted by t sees (1 + cos t) / 2 of the sky), so an
// unoccluded surface keeps the GI-off brightness. The drawn sky (sky.wgsl)
// is the background, several times brighter, and would wash the scene out.
fn sky_light(dir: vec3f) -> vec3f {
    return light.ambient * select(light.ground_color, light.sky_color, dir.y >= 0.0);
}

fn sign_not_zero(v: vec2f) -> vec2f {
    return select(vec2f(-1.0), vec2f(1.0), v >= vec2f(0.0));
}

// Unit direction -> octahedral square [-1, 1]^2.
fn oct_encode(n: vec3f) -> vec2f {
    let p = n.xy / (abs(n.x) + abs(n.y) + abs(n.z));
    if (n.z < 0.0) {
        return (1.0 - abs(p.yx)) * sign_not_zero(p);
    }
    return p;
}

// Octahedral square [-1, 1]^2 -> unit direction.
fn oct_decode(e: vec2f) -> vec3f {
    let z = 1.0 - abs(e.x) - abs(e.y);
    var xy = e;
    if (z < 0.0) {
        xy = (1.0 - abs(e.yx)) * sign_not_zero(e);
    }
    return normalize(vec3f(xy, z));
}
