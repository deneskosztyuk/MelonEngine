//! Level of detail: the `Lod` component lists models of falling detail with
//! the smallest on-screen height each is used at (like Unity's LOD Group);
//! the renderer draws the level `select_level` picks, or nothing when the
//! object is smaller than `cull_height`. A level switches only once the
//! screen height is past its threshold by the `hysteresis` margin, so it
//! does not flicker back and forth on the line. No GPU code.

use bevy_ecs::component::Component;
use bevy_ecs::reflect::ReflectComponent;
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use glam::{Mat4, Vec3};

use crate::assets::{AssetGuid, AssetServer, Handle, Model};

/// One level: a model (GUID wins, else path) used while the object covers at
/// least `screen_height` of the view height (0 to 1).
#[derive(Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Default)]
pub struct LodLevel {
    pub guid: String,
    pub path: String,
    pub screen_height: f32,
}

impl LodLevel {
    pub fn resolve(&self, server: &mut AssetServer) -> Option<Handle<Model>> {
        if let Some(handle) = self
            .guid
            .parse::<AssetGuid>()
            .ok()
            .and_then(|g| server.load_guid(g, None))
        {
            return Some(handle);
        }
        (!self.path.is_empty()).then(|| server.load(&self.path))
    }
}

/// Levels from the most to the least detailed, with falling thresholds; the
/// object is culled (not drawn) below the last level's threshold. The
/// entity's own `Handle<Model>` is not drawn while it has a `Lod` with
/// levels.
#[derive(Component, Reflect, Clone, Debug, PartialEq)]
#[reflect(Component, Default)]
pub struct Lod {
    pub levels: Vec<LodLevel>,
    /// Margin (share of a threshold) a switch must be past; 0.1 = 10%.
    pub hysteresis: f32,
}

impl Default for Lod {
    fn default() -> Self {
        Self {
            levels: Vec::new(),
            hysteresis: 0.1,
        }
    }
}

/// The level an entity was drawn at last frame (`levels.len()` = culled),
/// kept by the renderer's extraction so a level holds on its threshold.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LodState(pub usize);

/// Share of the view height a sphere of `radius` at `centre` covers (its
/// diameter over the view height; any camera: perspective or orthographic).
pub fn screen_height(view_proj: Mat4, centre: Vec3, radius: f32) -> f32 {
    let inverse = view_proj.inverse();
    let up = (inverse.project_point3(Vec3::new(0.0, 1.0, 0.5))
        - inverse.project_point3(Vec3::new(0.0, 0.0, 0.5)))
    .normalize_or(Vec3::Y);
    let top = view_proj.project_point3(centre + up * radius).y;
    let bottom = view_proj.project_point3(centre - up * radius).y;
    // NDC spans 2 over the view height.
    (top - bottom).abs() / 2.0
}

/// The level to draw at screen height `height`: `thresholds` (descending,
/// the smallest height each level is used at), `hysteresis` as on `Lod`,
/// `current` the level drawn last frame (`thresholds.len()` = was culled).
/// `Some(i)` = level i, `None` = culled (below the last threshold).
pub fn select_level(
    thresholds: &[f32],
    hysteresis: f32,
    height: f32,
    current: Option<usize>,
) -> Option<usize> {
    let n = thresholds.len();
    // Without history: the first level whose threshold is met.
    let raw = thresholds.iter().position(|t| height >= *t).unwrap_or(n);
    let Some(current) = current.map(|c| c.min(n)) else {
        return (raw < n).then_some(raw);
    };
    // Hold the current level until the height is clearly past a threshold:
    // down to less detail below its own threshold less the margin, up to
    // more detail above the target's threshold plus the margin.
    let chosen = if raw == current {
        current
    } else if raw > current {
        if height < thresholds[current] * (1.0 - hysteresis) {
            raw
        } else {
            current
        }
    } else if height >= thresholds[raw] * (1.0 + hysteresis) {
        raw
    } else {
        current
    };
    (chosen < n).then_some(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: [f32; 3] = [0.5, 0.2, 0.05];

    fn pick(height: f32, current: Option<usize>) -> Option<usize> {
        select_level(&T, 0.1, height, current)
    }

    #[test]
    fn levels_follow_the_screen_height_without_history() {
        assert_eq!(pick(0.9, None), Some(0));
        assert_eq!(pick(0.5, None), Some(0));
        assert_eq!(pick(0.3, None), Some(1));
        assert_eq!(pick(0.1, None), Some(2));
        // Below the last threshold: culled.
        assert_eq!(pick(0.04, None), None);
    }

    #[test]
    fn a_level_holds_on_its_threshold_until_clearly_past_it() {
        // Level 1 (0.2 to 0.5): stepping up to level 0 needs 0.5 * 1.1.
        assert_eq!(pick(0.52, Some(1)), Some(1));
        assert_eq!(pick(0.56, Some(1)), Some(0));
        // Level 0 holds down to 0.5 * 0.9.
        assert_eq!(pick(0.46, Some(0)), Some(0));
        assert_eq!(pick(0.44, Some(0)), Some(1));
        // A height wobbling around the threshold never flickers.
        let mut level = Some(0);
        let mut switches = 0;
        for h in [0.51, 0.49, 0.5, 0.48, 0.52, 0.47, 0.5] {
            let next = pick(h, level);
            switches += usize::from(next != level);
            level = next;
        }
        assert_eq!((level, switches), (Some(0), 0));
    }

    #[test]
    fn culling_has_the_same_margin() {
        assert_eq!(pick(0.049, Some(2)), Some(2));
        assert_eq!(pick(0.044, Some(2)), None);
        // Back from culled needs 0.05 * 1.1.
        assert_eq!(pick(0.052, Some(3)), None);
        assert_eq!(pick(0.06, Some(3)), Some(2));
    }

    #[test]
    fn a_big_jump_skips_levels() {
        assert_eq!(pick(0.9, Some(2)), Some(0));
        assert_eq!(pick(0.1, Some(0)), Some(2));
    }

    #[test]
    fn no_levels_draw_nothing() {
        assert_eq!(select_level(&[], 0.1, 1.0, None), None);
    }

    #[test]
    fn screen_height_halves_when_the_distance_doubles() {
        let camera = crate::Camera {
            eye: Vec3::ZERO,
            center: Vec3::new(0.0, 0.0, -1.0),
            aspect: 1.0,
            ..Default::default()
        };
        let m = camera.view_proj();
        let near = screen_height(m, Vec3::new(0.0, 0.0, -10.0), 1.0);
        let far = screen_height(m, Vec3::new(0.0, 0.0, -20.0), 1.0);
        assert!((near / far - 2.0).abs() < 0.05, "{near} {far}");
        assert!(near > 0.0 && near < 1.0);
    }
}
