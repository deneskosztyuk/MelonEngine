//! `.env` files: the sun, ambient, sky, fog, AO, GI and bloom settings of a
//! scene as one asset, referenced by `WorldEnvironment`. No GPU code.

use std::path::Path;

use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::assets::{AssetGuid, AssetServer, Handle};
use crate::reflect::Vec3Reflect;
use crate::{Environment, Light, Texture};

/// File extension of environment assets.
pub const ENVIRONMENT_EXTENSION: &str = "env";

/// An asset by GUID (wins when it resolves) or project-relative path; both
/// empty = unset.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct AssetRef {
    pub guid: String,
    pub path: String,
}

impl AssetRef {
    /// Names `path`, with its GUID when the server knows it.
    pub fn for_path(server: &AssetServer, path: &str) -> Self {
        Self {
            guid: server
                .guid_of(path)
                .map(|g| g.to_string())
                .unwrap_or_default(),
            path: path.to_string(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.guid.is_empty() && self.path.is_empty()
    }

    pub fn resolve<T: Send + Sync + 'static>(&self, server: &mut AssetServer) -> Option<Handle<T>> {
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

/// How the editor draws a number field: a drag value with this range, speed,
/// unit suffix and decimals. Set with `#[reflect(@EditHint { .. })]`.
#[derive(Reflect, Clone, Copy, Debug)]
pub struct EditHint {
    pub min: f32,
    pub max: f32,
    pub speed: f32,
    pub suffix: &'static str,
    pub decimals: usize,
}

/// Sun direction (degrees) and colour.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct SunSection {
    /// Height above the horizon (90 = straight overhead).
    #[reflect(@EditHint { min: -90.0, max: 90.0, speed: 0.5, suffix: "°", decimals: 1 })]
    pub elevation: f32,
    /// Compass direction of the sun around +Y (0 = toward +Z, 90 = toward +X).
    #[reflect(@EditHint { min: -180.0, max: 180.0, speed: 0.5, suffix: "°", decimals: 1 })]
    pub azimuth: f32,
    /// Colour and strength (linear RGB).
    #[reflect(remote = Vec3Reflect)]
    pub color: Vec3,
}

/// Ambient light.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct AmbientSection {
    /// Share of light that is ambient (0..1); the sun gets the rest.
    #[reflect(@EditHint { min: 0.0, max: 1.0, speed: 0.005, suffix: "", decimals: 3 })]
    pub share: f32,
    /// From above (linear RGB).
    #[reflect(remote = Vec3Reflect)]
    pub sky: Vec3,
    /// From below (linear RGB).
    #[reflect(remote = Vec3Reflect)]
    pub ground: Vec3,
}

/// The drawn sky.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct SkySection {
    /// Straight up (linear HDR).
    #[reflect(remote = Vec3Reflect)]
    pub zenith: Vec3,
    /// At the horizon (linear HDR), also the fog colour.
    #[reflect(remote = Vec3Reflect)]
    pub horizon: Vec3,
    /// HDR equirect replacing the procedural sky; empty = procedural.
    pub texture: AssetRef,
    #[reflect(@EditHint { min: 0.0, max: 20.0, speed: 0.01, suffix: "", decimals: 2 })]
    pub texture_intensity: f32,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct FogSection {
    /// Exponential density per metre; 0 = no fog.
    #[reflect(@EditHint { min: 0.0, max: 0.2, speed: 0.0001, suffix: " /m", decimals: 4 })]
    pub density: f32,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct AoSection {
    #[reflect(@EditHint { min: 0.0, max: 2.0, speed: 0.01, suffix: "", decimals: 2 })]
    pub strength: f32,
    /// Metres.
    #[reflect(@EditHint { min: 0.0, max: 5.0, speed: 0.01, suffix: " m", decimals: 2 })]
    pub radius: f32,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct GiSection {
    #[reflect(@EditHint { min: 0.0, max: 4.0, speed: 0.01, suffix: "", decimals: 3 })]
    pub intensity: f32,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct BloomSection {
    #[reflect(@EditHint { min: 0.0, max: 1.0, speed: 0.005, suffix: "", decimals: 3 })]
    pub intensity: f32,
}

/// Everything a scene's look needs, as a file. Missing fields take today's
/// defaults, so a hand-written file can set only what it needs.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[reflect(Default)]
#[serde(default)]
pub struct EnvironmentAsset {
    pub sun: SunSection,
    pub ambient: AmbientSection,
    pub sky: SkySection,
    pub fog: FogSection,
    pub ao: AoSection,
    pub gi: GiSection,
    pub bloom: BloomSection,
}

impl Default for SunSection {
    fn default() -> Self {
        let light = Light::default();
        let to_sun = light.to_light().normalize_or(Vec3::Y);
        Self {
            elevation: to_sun.y.clamp(-1.0, 1.0).asin().to_degrees(),
            azimuth: to_sun.x.atan2(to_sun.z).to_degrees(),
            color: light.color,
        }
    }
}

impl Default for AmbientSection {
    fn default() -> Self {
        let light = Light::default();
        Self {
            share: light.ambient,
            sky: light.sky_color,
            ground: light.ground_color,
        }
    }
}

impl Default for SkySection {
    fn default() -> Self {
        let env = Environment::default();
        Self {
            zenith: env.zenith_color,
            horizon: env.horizon_color,
            texture: AssetRef::default(),
            texture_intensity: env.sky_intensity,
        }
    }
}

impl Default for FogSection {
    fn default() -> Self {
        Self {
            density: Environment::default().fog_density,
        }
    }
}

impl Default for AoSection {
    fn default() -> Self {
        let env = Environment::default();
        Self {
            strength: env.ao_strength,
            radius: env.ao_radius,
        }
    }
}

impl Default for GiSection {
    fn default() -> Self {
        Self {
            intensity: Environment::default().gi_intensity,
        }
    }
}

impl Default for BloomSection {
    fn default() -> Self {
        Self {
            intensity: Environment::default().bloom_intensity,
        }
    }
}

impl EnvironmentAsset {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(ron::from_str(text)?)
    }

    pub fn to_ron(&self) -> String {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .expect("environment serialises")
    }

    pub fn read(path: &Path) -> anyhow::Result<Self> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// Atomic write.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::ron_file::save(self, path, false)
    }

    /// The `Light` this describes.
    pub fn light(&self) -> Light {
        let (el, az) = (
            self.sun.elevation.to_radians(),
            self.sun.azimuth.to_radians(),
        );
        let to_sun = Vec3::new(az.sin() * el.cos(), el.sin(), az.cos() * el.cos());
        Light {
            direction: -to_sun,
            ambient: self.ambient.share.clamp(0.0, 1.0),
            color: self.sun.color,
            sky_color: self.ambient.sky,
            ground_color: self.ambient.ground,
        }
    }

    /// The `Environment` this describes; `sky_texture` is the resolved
    /// `sky.texture` (handles are not stored in files).
    pub fn environment(&self, sky_texture: Option<Handle<Texture>>) -> Environment {
        Environment {
            zenith_color: self.sky.zenith,
            horizon_color: self.sky.horizon,
            fog_density: self.fog.density.max(0.0),
            ao_strength: self.ao.strength,
            ao_radius: self.ao.radius,
            bloom_intensity: self.bloom.intensity.max(0.0),
            gi_intensity: self.gi.intensity,
            sky_texture,
            sky_intensity: self.sky.texture_intensity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetMeta;

    #[test]
    fn default_reproduces_todays_light_and_environment() {
        let a = EnvironmentAsset::default();
        let (l, want) = (a.light(), Light::default());
        assert!((l.direction - want.direction).length() < 1e-4);
        assert_eq!(
            (l.ambient, l.color, l.sky_color, l.ground_color),
            (want.ambient, want.color, want.sky_color, want.ground_color)
        );
        assert_eq!(a.environment(None), Environment::default());
    }

    #[test]
    fn file_round_trips() {
        let a = EnvironmentAsset {
            sun: SunSection {
                elevation: 12.0,
                azimuth: -40.0,
                color: Vec3::new(1.0, 0.5, 0.25),
            },
            ambient: AmbientSection {
                share: 0.4,
                sky: Vec3::new(0.1, 0.2, 0.3),
                ground: Vec3::new(0.3, 0.2, 0.1),
            },
            sky: SkySection {
                zenith: Vec3::new(0.2, 0.2, 0.9),
                horizon: Vec3::new(0.9, 0.5, 0.2),
                texture: AssetRef {
                    guid: "11111111-2222-3333-4444-555555555555".into(),
                    path: "assets/sky.hdr".into(),
                },
                texture_intensity: 1.5,
            },
            fog: FogSection { density: 0.02 },
            ao: AoSection {
                strength: 0.6,
                radius: 0.8,
            },
            gi: GiSection { intensity: 0.7 },
            bloom: BloomSection { intensity: 0.2 },
        };
        let dir = std::env::temp_dir().join(format!("melon_env_{}", std::process::id()));
        let file = dir.join("a.env");
        a.save(&file).unwrap();
        assert_eq!(EnvironmentAsset::read(&file).unwrap(), a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_file_takes_defaults() {
        let a = EnvironmentAsset::parse("(fog: (density: 0.02))").unwrap();
        assert_eq!(a.fog.density, 0.02);
        assert_eq!(a.sun, SunSection::default());
        assert!(EnvironmentAsset::parse("(fog: ").is_err());
    }

    #[test]
    fn sections_map_onto_the_resources() {
        let mut a = EnvironmentAsset::default();
        a.sun.elevation = 90.0;
        a.ao.strength = 0.3;
        a.gi.intensity = 0.5;
        a.bloom.intensity = 0.12;
        a.sky.texture_intensity = 2.0;
        assert!((a.light().direction - Vec3::NEG_Y).length() < 1e-4);
        let mut server = AssetServer::new(".".into());
        let h = server.load::<Texture>("assets/sky.hdr");
        let env = a.environment(Some(h));
        assert_eq!(
            (
                env.ao_strength,
                env.gi_intensity,
                env.bloom_intensity,
                env.sky_intensity
            ),
            (0.3, 0.5, 0.12, 2.0)
        );
        assert_eq!(env.sky_texture, Some(h));
    }

    #[test]
    fn asset_ref_prefers_guid_and_falls_back_to_path() {
        let root = std::env::temp_dir().join(format!("melon_assetref_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/t.hdr"), b"x").unwrap();
        let guid = AssetGuid::new();
        std::fs::write(
            root.join("assets/t.hdr.meta"),
            AssetMeta {
                version: crate::META_VERSION,
                guid,
            }
            .to_ron(),
        )
        .unwrap();
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        let by_guid = AssetRef {
            guid: guid.to_string(),
            path: "assets/old.hdr".into(),
        }
        .resolve::<Texture>(&mut server)
        .unwrap();
        assert_eq!(server.path(by_guid), "assets/t.hdr");
        let by_path = AssetRef {
            guid: AssetGuid::new().to_string(),
            path: "assets/other.hdr".into(),
        }
        .resolve::<Texture>(&mut server)
        .unwrap();
        assert_eq!(server.path(by_path), "assets/other.hdr");
        assert!(
            AssetRef::default()
                .resolve::<Texture>(&mut server)
                .is_none()
        );
        assert_eq!(
            AssetRef::for_path(&server, "assets/t.hdr").guid,
            guid.to_string()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
