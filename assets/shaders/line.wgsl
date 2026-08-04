struct CameraUniform {
    view_proj: mat4x4f,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;

struct VsIn {
    @location(0) position: vec3f,
    @location(1) color: vec3f,
};

struct VsOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) color: vec3f,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * vec4f(in.position, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    return vec4f(in.color, 1.0);
}