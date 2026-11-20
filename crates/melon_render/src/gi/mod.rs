//! Diffuse global illumination: a thin front over interchangeable backends.
//!
//! `Environment::gi_mode` picks the backend each frame ([`frame_mode`]:
//! `gi_intensity` 0 is `Off`). The front owns the mode dispatch and the GI
//! part of the mesh pipeline's group 3 layout. Each backend owns its
//! resources and passes behind the same small surface: `layout_entries` /
//! `entries` (its group 3 bindings), `create_view` (per-view resources, if
//! it has any), `update` (its per-frame uniforms) and `render` (its passes,
//! before the 3D pass). Every backend's bindings stay in the one group 3
//! layout whatever the mode; the mesh pipeline has one variant per mode
//! (`GI_MODE` override, see [`shader_mode`]) that reads only its own
//! (`indirect_diffuse` in mesh.wgsl). See AGENTS.md.

pub mod ddgi;
pub mod ssgi;

use glam::{Mat4, Vec3};
use melon_core::{Environment, GiMode};

use crate::Mesh;

/// Renderer resources the GI backends are built from (`SceneRenderer::new`).
pub(crate) struct GiInputs<'a> {
    pub frame_layout: &'a wgpu::BindGroupLayout,
    pub light: &'a wgpu::Buffer,
    pub shadow_view: &'a wgpu::TextureView,
    pub shadow_sampler: &'a wgpu::Sampler,
    pub model_layout: &'a wgpu::BindGroupLayout,
    pub material_layout: &'a wgpu::BindGroupLayout,
    pub lighting_layout: &'a wgpu::BindGroupLayout,
}

/// One view's GI resources (in `ViewportTarget`, rebuilt with it).
pub(crate) struct GiView {
    ssgi: ssgi::SsgiView,
}

/// What the GI passes of one frame draw with.
pub(crate) struct GiPasses<'a> {
    /// Group 0: this frame's camera and light.
    pub frame: &'a wgpu::BindGroup,
    /// Group 1: the instance records of this frame.
    pub instances: &'a wgpu::BindGroup,
    /// Group 3 for the DDGI voxel passes (no voxel volume bound).
    pub voxel_lighting: &'a wgpu::BindGroup,
    /// The instances inside the DDGI volume ([`ddgi::in_volume`]).
    pub voxel_draws: &'a [(&'a Mesh, &'a wgpu::BindGroup, std::ops::Range<u32>)],
    pub view: &'a GiView,
}

/// The mode a frame with `environment` runs: `gi_intensity` 0 is `Off`.
pub fn frame_mode(environment: &Environment) -> GiMode {
    if environment.gi_intensity > 0.0 {
        environment.gi_mode
    } else {
        GiMode::Off
    }
}

/// mesh.wgsl `GI_MODE` override value of `mode` (`GI_OFF`, `GI_SSGI`,
/// `GI_DDGI`).
pub(crate) fn shader_mode(mode: GiMode) -> f64 {
    match mode {
        GiMode::Off => 0.0,
        GiMode::Ssgi => 1.0,
        GiMode::Ddgi => 2.0,
    }
}

/// The GI backends and the mode of the last frame.
pub(crate) struct Gi {
    ddgi: ddgi::Ddgi,
    ssgi: ssgi::Ssgi,
    /// Mode of the last `update`: the mesh draws use its pipeline variant.
    pub(crate) mode: GiMode,
    gi_only: bool,
}

impl Gi {
    pub(crate) fn new(device: &wgpu::Device, inputs: &GiInputs) -> Self {
        Self {
            ddgi: ddgi::Ddgi::new(device, inputs),
            ssgi: ssgi::Ssgi::new(device, inputs.frame_layout),
            mode: GiMode::Off,
            gi_only: false,
        }
    }

    /// Group 3 layout entries of every backend (binding 0, the AO texture,
    /// is not GI's).
    pub(crate) fn layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
        let mut entries = ddgi::layout_entries().to_vec();
        entries.extend(ssgi::layout_entries());
        entries
    }

    /// Group 3 entries of every backend, matching `layout_entries`, for
    /// `view` (`None`: stand-ins, per-view GI reads as off). `voxels`
    /// false: the DDGI voxel passes' group, without the volume they write.
    pub(crate) fn entries<'a>(
        &'a self,
        view: Option<&'a GiView>,
        voxels: bool,
    ) -> Vec<wgpu::BindGroupEntry<'a>> {
        let mut entries = self.ddgi.entries(voxels).to_vec();
        entries.extend(self.ssgi.entries(view.map(|v| &v.ssgi)));
        entries
    }

    /// The per-view resources of a `size` view.
    pub(crate) fn create_view(
        &self,
        device: &wgpu::Device,
        size: winit::dpi::PhysicalSize<u32>,
        inputs: &ssgi::SsgiViewInputs,
    ) -> GiView {
        GiView {
            ssgi: self.ssgi.create_view(device, size, inputs),
        }
    }

    /// Debug: the mesh shader outputs only the indirect diffuse term.
    pub(crate) fn set_gi_only(&mut self, gi_only: bool) {
        self.gi_only = gi_only;
    }

    /// Writes every backend's uniforms for a frame of `view` (camera
    /// `view_proj`, `eye`) with `environment` and returns its mode. A
    /// backend that does not run this frame restarts from scratch the next
    /// frame it does.
    pub(crate) fn update(
        &mut self,
        queue: &wgpu::Queue,
        environment: &Environment,
        view: &GiView,
        view_proj: Mat4,
        eye: Vec3,
    ) -> GiMode {
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
        self.ssgi.update(
            queue,
            &view.ssgi,
            intensity(GiMode::Ssgi),
            self.gi_only,
            view_proj,
            eye,
        );
        mode
    }

    /// Records the passes of the last `update`'s mode. Call after the
    /// shadow pass, `update_instances` and the depth/normal prepass (SSGI
    /// reads it), before the 3D pass.
    pub(crate) fn render(&self, encoder: &mut wgpu::CommandEncoder, passes: &GiPasses) {
        match self.mode {
            GiMode::Off => {}
            GiMode::Ssgi => self.ssgi.render(encoder, passes.frame, &passes.view.ssgi),
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
        assert_eq!(frame_mode(&env(GiMode::Ssgi, 1.0)), GiMode::Ssgi);
        assert_eq!(frame_mode(&env(GiMode::Ssgi, 0.0)), GiMode::Off);
        assert_eq!(frame_mode(&env(GiMode::Ddgi, 0.0)), GiMode::Off);
        assert_eq!(frame_mode(&env(GiMode::Off, 1.0)), GiMode::Off);
    }
}
