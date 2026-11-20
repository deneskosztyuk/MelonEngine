// Shadow pass: depth-only render of every mesh from the sun. No fragment stage.

struct LightCamera {
    view_proj: mat4x4f,
};

// One record per drawn instance (group 1), indexed by instance_index.
struct Instance {
    model: mat4x4f,
    normal_matrix: mat4x4f,
};

@group(0) @binding(0) var<uniform> light_camera: LightCamera;
@group(1) @binding(0) var<storage, read> instances: array<Instance>;

@vertex
fn vs_main(
    @location(0) position: vec3f,
    @builtin(instance_index) instance: u32,
) -> @builtin(position) vec4f {
    return light_camera.view_proj * instances[instance].model * vec4f(position, 1.0);
}
