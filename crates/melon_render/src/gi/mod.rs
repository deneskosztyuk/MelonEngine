//! Diffuse global illumination: a thin front over interchangeable backends.
//!
//! `Environment::gi_mode` picks the backend each frame ([`frame_mode`]:
//! `gi_intensity` 0 is `Off`). The front owns the mode dispatch and the GI
//! part of the mesh pipeline's group 3 layout. Each backend owns its
//! resources and passes behind the same small surface: `layout_entries` /
//! `entries` (its group 3 bindings), `update` (its per-frame uniforms) and
//! `render` (its passes, before the 3D pass). Every backend's bindings stay
//! in the one group 3 layout whatever the mode; the mesh pipeline has one
//! variant per mode (`GI_MODE` override, see [`shader_mode`]) that reads
//! only its own (`indirect_diffuse` in mesh.wgsl). See AGENTS.md.

pub mod ddgi;

use melon_core::{Environment, GiMode};

use crate::Mesh;

/// Renderer resources the GI backends are built from (`SceneRenderer::new`).
pub(crate) struct GiInputs<'a> {
    pub light: &'a wgpu::Buffer,
    pub shadow_view: &'a wgpu::TextureView,
    pub shadow_sampler: &'a wgpu::Sampler,
    pub model_layout: &'a wgpu::BindGroupLayout,
    pub material_layout: &'a wgpu::BindGroupLayout,
    pub lighting_layout: &'a wgpu::BindGroupLayout,
}

/// What the GI passes of one frame draw with.
pub(crate) struct GiPasses<'a> {
    /// Group 1: the instance records of this frame.
    pub instances: &'a wgpu::BindGroup,
    /// Group 3 for the DDGI voxel passes (no voxel volume bound).
    pub voxel_lighting: &'a wgpu::BindGroup,
    /// The instances inside the DDGI volume ([`ddgi::in_volume`]).
    pub voxel_draws: &'a [(&'a Mesh, &'a wgpu::BindGroup, std::ops::Range<u32>)],
}

/// The mode a frame with `environment` runs: `gi_intensity` 0 is `Off`.
pub fn frame_mode(environment: &Environment) -> GiMode {
    if environment.gi_intensity > 0.0 {
        environment.gi_mode
    } else {
        GiMode::Off
    }
}

/// mesh.wgsl `GI_MODE` override value of `mode` (`GI_OFF`, `GI_DDGI`).
pub(crate) fn shader_mode(mode: GiMode) -> f64 {
    match mode {
        GiMode::Off => 0.0,
        GiMode::Ddgi => 2.0,
    }
}

/// The GI backends and the mode of the last frame.
pub(crate) struct Gi {
    ddgi: ddgi::Ddgi,
    /// Mode of the last `update`: the mesh draws use its pipeline variant.
    pub(crate) mode: GiMode,
    gi_only: bool,
}

impl Gi {
    pub(crate) fn new(device: &wgpu::Device, inputs: &GiInputs) -> Self {
        Self {
            ddgi: ddgi::Ddgi::new(device, inputs),
            mode: GiMode::Off,
            gi_only: false,
        }
    }

    /// Group 3 layout entries of every backend (binding 0, the AO texture,
    /// is not GI's).
    pub(crate) fn layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
        ddgi::layout_entries().to_vec()
    }

    /// Group 3 entries of every backend, matching `layout_entries`.
    /// `voxels` false: the DDGI voxel passes' group, without the volume
    /// they write.
    pub(crate) fn entries(&self, voxels: bool) -> Vec<wgpu::BindGroupEntry<'_>> {
        self.ddgi.entries(voxels).to_vec()
    }

    /// Debug: the mesh shader outputs only the indirect diffuse term.
    pub(crate) fn set_gi_only(&mut self, gi_only: bool) {
        self.gi_only = gi_only;
    }

    /// Writes every backend's uniforms for a frame with `environment` and
    /// returns its mode. A backend that does not run this frame restarts
    /// from scratch the next frame it does.
    pub(crate) fn update(&mut self, queue: &wgpu::Queue, environment: &Environment) -> GiMode {
        let mode = frame_mode(environment);
        self.mode = mode;
        let intensity = |m| {
            if mode == m {
                environment.gi_intensity
            } else {
                0.0
            }
        };
        self.ddgi
            .update(queue, intensity(GiMode::Ddgi), self.gi_only);
        mode
    }

    /// Records the passes of the last `update`'s mode. Call after the
    /// shadow pass and `update_instances`, before the 3D pass.
    pub(crate) fn render(&self, encoder: &mut wgpu::CommandEncoder, passes: &GiPasses) {
        match self.mode {
            GiMode::Off => {}
            GiMode::Ddgi => self.ddgi.render(
                encoder,
                passes.instances,
                passes.voxel_lighting,
                passes.voxel_draws,
            ),
        }
    }
}

impl crate::SceneRenderer {
    /// Debug: the mesh shader outputs only the indirect diffuse term.
    pub fn set_gi_only(&mut self, gi_only: bool) {
        self.gi.set_gi_only(gi_only);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_intensity_is_off() {
        let env = |gi_mode, gi_intensity| Environment {
            gi_mode,
            gi_intensity,
            ..Default::default()
        };
        assert_eq!(frame_mode(&Environment::default()), GiMode::Ddgi);
        assert_eq!(frame_mode(&env(GiMode::Ddgi, 0.0)), GiMode::Off);
        assert_eq!(frame_mode(&env(GiMode::Off, 1.0)), GiMode::Off);
    }
}
