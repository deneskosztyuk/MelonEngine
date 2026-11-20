use bevy_ecs::resource::Resource;
use glam::camera::rh::proj::directx;
use glam::camera::rh::view;
use glam::{Mat4, Vec2, Vec3, Vec4};

#[derive(Resource, Debug, Clone, Copy)]
pub struct Camera {
    pub eye: Vec3,
    pub center: Vec3,
    pub up: Vec3,
    pub fov_y_radians: f32,
    pub aspect: f32,
    pub near: f32,
    pub far: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for Camera {
    fn default() -> Self {
        let eye = Vec3::new(0.0, 0.0, 4.0);
        let (yaw, pitch) = (-std::f32::consts::FRAC_PI_2, 0.0f32);
        let center = look_target(eye, yaw, pitch);
        Self {
            eye,
            center,
            up: Vec3::Y,
            fov_y_radians: std::f32::consts::FRAC_PI_4,
            aspect: 16.0 / 9.0,
            near: 0.1,
            far: 100.0,
            yaw,
            pitch,
        }
    }
}

pub fn look_target(eye: Vec3, yaw: f32, pitch: f32) -> Vec3 {
    let dir = Vec3::new(
        yaw.cos() * pitch.cos(),
        pitch.sin(),
        yaw.sin() * pitch.cos(),
    )
    .normalize();
    eye + dir
}

impl Camera {
    pub fn view_proj(&self) -> Mat4 {
        let view = view::look_at_mat4(self.eye, self.center, self.up);
        let proj = directx::perspective(self.fov_y_radians, self.aspect, self.near, self.far);
        proj * view
    }

    pub fn set_aspect(&mut self, aspect: f32) {
        self.aspect = aspect;
    }

    pub fn forward(&self) -> Vec3 {
        (self.center - self.eye).normalize_or_zero()
    }

    pub fn right(&self) -> Vec3 {
        self.forward().cross(self.up).normalize_or_zero()
    }

    /// Project a world-space position to screen-space pixels.
    /// `screen_size` is in logical points (matches egui's coordinate space).
    /// Returns `None` if the point is behind the camera (clip.w <= 0).
    pub fn world_to_screen(&self, world_pos: Vec3, screen_size: Vec2) -> Option<Vec2> {
        let clip = self.view_proj() * Vec4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = Vec3::new(clip.x, clip.y, clip.z) / clip.w;
        let screen = Vec2::new(
            (ndc.x + 1.0) * 0.5 * screen_size.x,
            (1.0 - ndc.y) * 0.5 * screen_size.y,
        );
        Some(screen)
    }

    /// Unproject a screen-space point to a world-space ray.
    /// `screen_pos` and `screen_size` are in logical points.
    /// Uses DirectX depth convention: near plane z=0, far plane z=1.
    /// Returns (origin, direction) where direction is normalized.
    pub fn screen_to_world_ray(&self, screen_pos: Vec2, screen_size: Vec2) -> (Vec3, Vec3) {
        let inv_vp = self.view_proj().inverse();
        let x_ndc = 2.0 * screen_pos.x / screen_size.x - 1.0;
        let y_ndc = 1.0 - 2.0 * screen_pos.y / screen_size.y;

        let near = inv_vp * Vec4::new(x_ndc, y_ndc, 0.0, 1.0);
        let far = inv_vp * Vec4::new(x_ndc, y_ndc, 1.0, 1.0);

        let near_world = Vec3::new(near.x, near.y, near.z) / near.w;
        let far_world = Vec3::new(far.x, far.y, far.z) / far.w;

        let dir = (far_world - near_world).normalize_or_zero();
        (near_world, dir)
    }
}

#[derive(Resource, Debug, Clone, Copy)]
pub struct Light {
    /// Direction the light travels (from the light into the scene).
    pub direction: Vec3,
    /// Overall ambient intensity (0..1). The sun's diffuse is scaled by `1 - ambient`.
    pub ambient: f32,
    pub color: Vec3,
    /// Ambient colour from above (linear RGB): lights surfaces facing up.
    pub sky_color: Vec3,
    /// Ambient colour from below (linear RGB): lights surfaces facing down.
    pub ground_color: Vec3,
}

impl Light {
    /// Direction from the scene toward the light, as shaders expect it.
    pub fn to_light(&self) -> Vec3 {
        -self.direction
    }
}

/// Engine time state. `delta_seconds` is the time elapsed since the last
/// update; `elapsed_seconds` is play time (advances only while `Playing`).
/// Systems that need frame-rate-independent movement read `delta_seconds`
/// instead of assuming a fixed 1/60 step.
///
/// `fixed_delta` is the `FixedUpdate` step. `overstep` is frame time
/// accumulated but not yet consumed by a fixed step; `alpha()` turns it into
/// the 0..1 blend factor between the last two fixed steps.
#[derive(Resource, Debug, Clone, Copy)]
pub struct Time {
    pub delta_seconds: f32,
    pub elapsed_seconds: f32,
    pub fixed_delta: f32,
    pub overstep: f32,
}

impl Default for Time {
    fn default() -> Self {
        Self {
            delta_seconds: 1.0 / 60.0,
            elapsed_seconds: 0.0,
            fixed_delta: 1.0 / 60.0,
            overstep: 0.0,
        }
    }
}

impl Time {
    /// Longest frame the simulation will catch up on (at most 15 fixed steps).
    /// Prevents a long hitch from queueing so many steps that the next frame
    /// is slower still.
    pub const MAX_FRAME_DELTA: f32 = 0.25;

    /// Record this frame's wall-clock delta. Play time (`elapsed_seconds`)
    /// is advanced by `Core::update`, and only while `Playing`.
    pub fn update(&mut self, delta: f32) {
        self.delta_seconds = delta.min(Self::MAX_FRAME_DELTA);
    }

    /// Blend factor between the previous and current fixed step, in 0..1.
    pub fn alpha(&self) -> f32 {
        self.overstep / self.fixed_delta
    }
}

impl Default for Light {
    fn default() -> Self {
        Self {
            direction: Vec3::new(0.5, -1.0, 0.5).normalize(),
            ambient: 0.2,
            color: Vec3::ONE,
            // Clear day: cool blue sky, warm darker ground. Their midpoint is
            // a little brighter than the old flat white ambient.
            sky_color: Vec3::new(1.2, 1.4, 1.8),
            ground_color: Vec3::new(0.9, 0.75, 0.55),
        }
    }
}

/// Procedural sky and distance fog. Optional: the renderer uses the default
/// when no `Environment` resource exists.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct Environment {
    /// Sky straight up (linear HDR radiance).
    pub zenith_color: Vec3,
    /// Sky at the horizon (linear HDR radiance), also the fog colour, so
    /// distant geometry melts into the sky.
    pub horizon_color: Vec3,
    /// Exponential fog density per metre; 0 turns fog off.
    pub fog_density: f32,
    /// Screen-space ambient occlusion on ambient light: 1 = full, 0 = off
    /// (the SSAO passes are skipped).
    pub ao_strength: f32,
    /// SSAO sampling radius in metres.
    pub ao_radius: f32,
    /// Bloom: how much of the blurred HDR image is mixed into the scene
    /// colour before tonemapping; 0 = off (the bloom passes are skipped).
    pub bloom_intensity: f32,
    /// Dynamic diffuse GI (DDGI probes over a voxel scene): scales the
    /// probe irradiance that replaces the hemisphere ambient inside the GI
    /// volume; 0 = off (the voxel and probe passes are skipped).
    pub gi_intensity: f32,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            // Clear day in the same hues as `Light::default`'s sky ambient.
            zenith_color: Vec3::new(0.15, 0.3, 0.75),
            horizon_color: Vec3::new(0.7, 0.8, 0.95),
            fog_density: 0.01,
            ao_strength: 1.0,
            ao_radius: 0.5,
            bloom_intensity: 0.08,
            gi_intensity: 1.0,
        }
    }
}

#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SimulationState {
    /// Editor edit mode: nothing simulates, play time is 0.
    #[default]
    Editing,
    Playing,
    /// Frozen mid-play: play time and the fixed-step remainder hold still.
    Paused,
}

#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct NameCounter(pub u32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_ao_defaults() {
        let env = Environment::default();
        assert_eq!((env.ao_strength, env.ao_radius), (1.0, 0.5));
    }

    #[test]
    fn environment_gi_default_is_on() {
        assert_eq!(Environment::default().gi_intensity, 1.0);
    }

    #[test]
    fn environment_bloom_default() {
        assert_eq!(Environment::default().bloom_intensity, 0.08);
    }

    #[test]
    fn camera_default_looks_at_negz() {
        let c = Camera::default();
        assert_eq!(c.eye, Vec3::new(0.0, 0.0, 4.0));
        assert!((c.yaw - (-std::f32::consts::FRAC_PI_2)).abs() < 1e-6);
        assert!(
            c.center.x.abs() < 1e-6,
            "center.x should be ~0, got {}",
            c.center.x
        );
        assert!(c.center.y.abs() < 1e-6);
        assert!((c.center.z - 3.0).abs() < 1e-6);
    }

    #[test]
    fn look_target_yaw_zero_is_posx() {
        let target = look_target(Vec3::ZERO, 0.0, 0.0);
        assert!((target.x - 1.0).abs() < 1e-6);
        assert!(target.y.abs() < 1e-6);
        assert!(target.z.abs() < 1e-6);
    }

    #[test]
    fn look_target_yaw_neg_half_pi_is_negz() {
        let target = look_target(Vec3::ZERO, -std::f32::consts::FRAC_PI_2, 0.0);
        assert!(target.x.abs() < 1e-6);
        assert!(target.y.abs() < 1e-6);
        assert!((target.z + 1.0).abs() < 1e-6);
    }

    #[test]
    fn look_target_pitch_half_pi_is_up() {
        let target = look_target(Vec3::ZERO, 0.0, std::f32::consts::FRAC_PI_2);
        assert!(target.x.abs() < 1e-6);
        assert!((target.y - 1.0).abs() < 1e-6);
        assert!(target.z.abs() < 1e-6);
    }

    #[test]
    fn camera_view_proj_is_finite() {
        let c = Camera::default();
        let m = c.view_proj();
        for col in &m.to_cols_array_2d() {
            for &v in col {
                assert!(v.is_finite(), "view_proj contains non-finite value: {v}");
            }
        }
    }

    #[test]
    fn camera_set_aspect_updates() {
        let mut c = Camera::default();
        c.set_aspect(2.0);
        assert!((c.aspect - 2.0).abs() < 1e-6);
    }

    #[test]
    fn camera_forward_is_normalized() {
        let c = Camera::default();
        let f = c.forward();
        assert!((f.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn camera_right_perpendicular_to_forward() {
        let c = Camera::default();
        let f = c.forward();
        let r = c.right();
        assert!(
            f.dot(r).abs() < 1e-6,
            "forward and right are not perpendicular"
        );
    }

    #[test]
    fn world_to_screen_center_is_screen_center() {
        let c = Camera::default();
        let screen = Vec2::new(800.0, 600.0);
        // Camera looks down -Z from (0,0,4), so a point at (0,0,3) is directly ahead
        let result = c.world_to_screen(Vec3::new(0.0, 0.0, 3.0), screen);
        assert!(result.is_some(), "point in front of camera should project");
        let p = result.unwrap();
        assert!(
            (p.x - 400.0).abs() < 1.0,
            "x should be ~400 (center), got {}",
            p.x
        );
        assert!(
            (p.y - 300.0).abs() < 1.0,
            "y should be ~300 (center), got {}",
            p.y
        );
    }

    #[test]
    fn world_to_screen_behind_camera_returns_none() {
        let c = Camera::default();
        let screen = Vec2::new(800.0, 600.0);
        // Point at z=5 is behind the camera (camera at z=4 looking -Z)
        let result = c.world_to_screen(Vec3::new(0.0, 0.0, 5.0), screen);
        assert!(result.is_none(), "point behind camera should return None");
    }

    #[test]
    fn screen_to_world_ray_direction_is_normalized() {
        let c = Camera::default();
        let screen = Vec2::new(800.0, 600.0);
        let (_origin, dir) = c.screen_to_world_ray(Vec2::new(400.0, 300.0), screen);
        assert!(
            (dir.length() - 1.0).abs() < 1e-5,
            "ray direction should be normalized, got length {}",
            dir.length()
        );
    }

    #[test]
    fn screen_to_world_ray_center_points_forward() {
        let c = Camera::default();
        let screen = Vec2::new(800.0, 600.0);
        let (_origin, dir) = c.screen_to_world_ray(Vec2::new(400.0, 300.0), screen);
        let forward = c.forward();
        assert!(
            dir.dot(forward) > 0.99,
            "center ray should point along camera forward, dot={}",
            dir.dot(forward)
        );
    }

    #[test]
    fn screen_to_world_ray_origin_is_near_camera() {
        let c = Camera::default();
        let screen = Vec2::new(800.0, 600.0);
        let (origin, _dir) = c.screen_to_world_ray(Vec2::new(400.0, 300.0), screen);
        let dist = (origin - c.eye).length();
        assert!(
            dist < 1.0,
            "ray origin should be near camera eye, dist={dist}"
        );
    }

    #[test]
    fn light_default_direction_normalized() {
        let l = Light::default();
        assert!((l.direction.length() - 1.0).abs() < 1e-6);
        assert!((l.ambient - 0.2).abs() < 1e-6);
        assert_eq!(l.color, Vec3::ONE);
    }

    #[test]
    fn light_default_hemisphere_is_a_clear_day() {
        let l = Light::default();
        let lum = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        for c in [l.sky_color, l.ground_color] {
            assert!(c.is_finite() && c.min_element() >= 0.0, "{c:?}");
        }
        assert!(lum(l.sky_color) > lum(l.ground_color));
        // A side-facing surface gets the midpoint; at least the old flat white.
        assert!(lum((l.sky_color + l.ground_color) * 0.5) >= 1.0);
    }

    #[test]
    fn environment_default_is_a_clear_day() {
        let e = Environment::default();
        for c in [e.zenith_color, e.horizon_color] {
            assert!(c.is_finite() && c.min_element() >= 0.0, "{c:?}");
        }
        assert!(e.fog_density.is_finite() && e.fog_density >= 0.0);
        // Zenith: darker and bluer than the horizon.
        assert!(e.zenith_color.length() < e.horizon_color.length());
        let blue = |c: Vec3| c.z / c.x;
        assert!(blue(e.zenith_color) > blue(e.horizon_color));
    }

    #[test]
    fn light_default_to_light_points_up() {
        assert!(Light::default().to_light().y > 0.0);
    }

    #[test]
    fn time_update_clamps_delta() {
        let mut t = Time::default();
        t.update(1.0);
        assert_eq!(t.delta_seconds, Time::MAX_FRAME_DELTA);
        assert_eq!(t.delta_seconds, 0.25);
    }

    #[test]
    fn time_default_fixed_delta() {
        let t = Time::default();
        assert!((t.fixed_delta - 1.0 / 60.0).abs() < 1e-9);
        assert_eq!(t.overstep, 0.0);
    }

    #[test]
    fn time_default_is_one_over_60() {
        let t = Time::default();
        assert!((t.delta_seconds - 1.0 / 60.0).abs() < 1e-6);
        assert_eq!(t.elapsed_seconds, 0.0);
    }

    #[test]
    fn time_update_sets_delta_only() {
        let mut t = Time::default();
        t.update(0.1);
        t.update(0.2);
        assert!((t.delta_seconds - 0.2).abs() < 1e-6);
        assert_eq!(t.elapsed_seconds, 0.0, "Core::update owns play time");
    }

    #[test]
    fn simulation_state_default_is_editing() {
        assert_eq!(SimulationState::default(), SimulationState::Editing);
    }
}
