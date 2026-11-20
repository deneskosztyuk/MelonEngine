//! Shadows of point and spot lights: every shadowed light renders its
//! casters into tiles of one depth atlas, a spot light into one tile (a
//! perspective view along its cone), a point light into six (one per cube
//! face). The mesh shader picks the face by the dominant axis of the vector
//! from the light and compares against that tile. Pure CPU math here; the
//! renderer draws the tiles in one pass with a viewport per tile.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

use crate::lights::LocalLight;

/// Point light cube faces (+X, -X, +Y, -Y, +Z, -Z), slightly wider than 90
/// degrees so PCF taps near a face edge still land inside it.
const FACE_FOV: f32 = 92.0 * std::f32::consts::PI / 180.0;
/// Nearest distance a shadow view renders (metres).
const NEAR: f32 = 0.05;
/// Smallest tile edge worth drawing.
const MIN_TILE: u32 = 128;
const MAX_TILE: u32 = 1024;
/// Most views per frame: 16 shadowed lights, six faces each.
pub const MAX_VIEWS: usize = 96;

/// A shadow view as the shader reads it (96 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ShadowViewGpu {
    pub view_proj: [[f32; 4]; 4],
    /// Atlas uv offset (xy) and scale (zw) of the tile.
    pub rect: [f32; 4],
    /// x: world size of one shadow texel per metre of distance.
    pub params: [f32; 4],
}

/// The view matrices of one light's shadow tiles: one for a spot light, six
/// for a point light (face order +X, -X, +Y, -Y, +Z, -Z), and the tangent of
/// half their field of view.
pub fn views_for(light: &LocalLight) -> (Vec<Mat4>, f32) {
    use glam::camera::rh::{proj::directx, view};
    let far = light.range.max(NEAR * 2.0);
    let look = |dir: Vec3, fov: f32| {
        let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
        directx::perspective(fov, 1.0, NEAR, far)
            * view::look_at_mat4(light.position, light.position + dir, up)
    };
    match light.spot {
        Some((_, outer)) => {
            let fov = (2.0 * outer + 0.1).clamp(0.35, 2.6);
            (vec![look(light.direction, fov)], (fov * 0.5).tan())
        }
        None => (
            [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z]
                .map(|dir| look(dir, FACE_FOV))
                .to_vec(),
            (FACE_FOV * 0.5).tan(),
        ),
    }
}

/// Shadow views `light` needs: one for a spot light, six for a point light.
pub fn view_count(light: &LocalLight) -> usize {
    if light.spot.is_some() { 1 } else { 6 }
}

/// Most tiles an `atlas`-sized square holds at the smallest tile size.
pub fn view_capacity(atlas: u32) -> usize {
    (((atlas / MIN_TILE) as usize).pow(2)).min(MAX_VIEWS)
}

/// How many of the leading lights (given their view counts, most important
/// first) fit the atlas; the rest must draw without shadows.
pub fn lights_that_fit(atlas: u32, view_counts: impl IntoIterator<Item = usize>) -> usize {
    let capacity = view_capacity(atlas);
    let mut used = 0;
    let mut fits = 0;
    for count in view_counts {
        if used + count > capacity {
            break;
        }
        used += count;
        fits += 1;
    }
    fits
}

/// Tile edge and tiles per atlas side for `views` tiles in an `atlas`-sized
/// square: the largest power-of-two tile (128 to 1024) whose grid holds
/// them all.
pub fn tile_layout(atlas: u32, views: usize) -> (u32, u32) {
    let mut tile = MAX_TILE.min(atlas.max(MIN_TILE));
    while tile > MIN_TILE && ((atlas / tile) as usize).pow(2) < views {
        tile /= 2;
    }
    (tile, (atlas / tile).max(1))
}

/// Pixel rectangle (x, y) of tile `index` in an atlas of `grid` tiles per side.
pub fn tile_origin(index: usize, tile: u32, grid: u32) -> (u32, u32) {
    ((index as u32 % grid) * tile, (index as u32 / grid) * tile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(spot: Option<(f32, f32)>) -> LocalLight {
        LocalLight {
            position: Vec3::new(1.0, 3.0, -2.0),
            direction: Vec3::NEG_Y,
            color: Vec3::splat(10.0),
            range: 10.0,
            spot,
            cast_shadows: true,
        }
    }

    #[test]
    fn a_point_light_has_six_views_that_cover_every_direction() {
        let l = light(None);
        let (views, tan_half) = views_for(&l);
        assert_eq!(views.len(), 6);
        assert!(tan_half > 1.0);
        for dir in [
            Vec3::new(1.0, 0.2, 0.1),
            Vec3::new(-0.3, -1.0, 0.4),
            Vec3::new(0.1, 0.1, -1.0),
            Vec3::new(0.5, 0.5, 0.5),
        ] {
            let p = l.position + dir.normalize() * 4.0;
            let inside = views
                .iter()
                .filter(|m| {
                    let c = m.project_point3(p);
                    c.x.abs() <= 1.0 && c.y.abs() <= 1.0 && (0.0..=1.0).contains(&c.z)
                })
                .count();
            assert!(inside >= 1, "{dir} is in no face");
        }
    }

    #[test]
    fn a_spot_light_has_one_view_along_its_axis() {
        let l = light(Some((0.2, 0.5)));
        let (views, _) = views_for(&l);
        assert_eq!(views.len(), 1);
        let on_axis = views[0].project_point3(l.position + Vec3::NEG_Y * 5.0);
        assert!(on_axis.x.abs() < 1e-4 && on_axis.y.abs() < 1e-4);
        let outside = views[0].project_point3(l.position + Vec3::new(5.0, -1.0, 0.0));
        assert!(outside.x.abs() > 1.0, "{outside}");
    }

    #[test]
    fn views_that_do_not_fit_are_dropped_and_every_kept_tile_is_inside() {
        // The panel's worst case: 16 shadowed point lights (96 views) in
        // every atlas size it offers.
        for atlas in [256u32, 512, 1024, 2048, 4096] {
            let fits = lights_that_fit(atlas, [6; 16]);
            let views = fits * 6;
            assert!(views <= view_capacity(atlas), "{atlas}");
            let (tile, grid) = tile_layout(atlas, views);
            for i in 0..views {
                let (x, y) = tile_origin(i, tile, grid);
                assert!(x + tile <= atlas && y + tile <= atlas, "{atlas}: tile {i}");
            }
        }
        assert_eq!(lights_that_fit(1024, [6; 16]), 10);
        assert_eq!(lights_that_fit(1024, [1; 16]), 16);
        assert_eq!(lights_that_fit(2048, [6; 16]), 16);
        assert_eq!(lights_that_fit(256, [6]), 0);
    }

    #[test]
    fn tiles_fit_the_atlas() {
        for (atlas, views) in [(2048, 6), (2048, 48), (4096, 96), (1024, 96), (2048, 1)] {
            let (tile, grid) = tile_layout(atlas, views);
            assert!(tile.is_power_of_two() && (MIN_TILE..=MAX_TILE).contains(&tile));
            assert!(grid * tile <= atlas);
            if tile > MIN_TILE {
                assert!((grid * grid) as usize >= views, "{atlas} {views}: {tile}");
            }
        }
        assert_eq!(tile_layout(2048, 1), (1024, 2));
        assert_eq!(tile_origin(3, 256, 4), (768, 0));
        assert_eq!(tile_origin(5, 256, 4), (256, 256));
    }
}
