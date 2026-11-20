//! Render quality: what the GPU spends time on (GI, AO, shadow map size,
//! MSAA, bloom, present mode), apart from the look in `Environment`.
//! Saved as `config/quality.ron`; named profiles as
//! `config/quality_profiles/<name>.ron`.

use std::path::{Path, PathBuf};

use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};

pub use crate::ron_file::LoadOutcome;

/// Diffuse global illumination technique.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GiMode {
    /// Hemisphere ambient only.
    Off,
    /// Screen-space GI at half resolution: cheaper, but only what is on
    /// screen bounces light.
    Ssgi,
    /// DDGI probes over a voxel scene: world-space, costlier.
    #[default]
    Ddgi,
}

/// Resolution of the SSGI trace and denoise passes, relative to the view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SsgiResolution {
    /// One texel per 2x2 pixels, 5x5 denoise.
    #[default]
    Half,
    /// One texel per 4x4 pixels, 3x3 denoise: about a quarter of the cost,
    /// softer contact bounce and thin objects drop out.
    Quarter,
}

impl SsgiResolution {
    /// View pixels per SSGI texel along each axis.
    pub fn scale(self) -> u32 {
        match self {
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }
}

/// How frames are presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresentPreference {
    /// FIFO: wait for vblank.
    Vsync,
    /// Mailbox when offered, with the host frame cap at the refresh rate.
    MailboxCapped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityPreset {
    Low,
    Medium,
    High,
    Max,
}

impl QualityPreset {
    pub const ALL: [QualityPreset; 4] = [Self::Low, Self::Medium, Self::High, Self::Max];
}

/// Render quality settings. `Default` is the High preset. `extract` hands
/// them to the renderer each frame: GI, AO and bloom switch on the next
/// frame; shadow map size and MSAA recreate GPU targets and pipelines before
/// it; `present` is applied by the host (`Renderer::set_present`).
/// A field missing from a file takes its `Default` value and unknown fields
/// are ignored, so adding a setting keeps old files and profiles loading.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderQuality {
    pub gi_mode: GiMode,
    /// SSGI pass resolution (used only with `GiMode::Ssgi`).
    pub ssgi_resolution: SsgiResolution,
    /// Screen-space ambient occlusion on or off.
    pub ao: bool,
    /// Sun shadow map edge in texels: 1024, 2048 or 4096.
    pub shadow_map_size: u32,
    /// MSAA samples of the 3D pass: 1 (off), 2 or 4.
    pub msaa: u32,
    pub bloom: bool,
    pub present: PresentPreference,
    /// Most point / spot lights shaded per frame (the most important
    /// visible ones; the rest are dropped).
    pub max_lights: u32,
    /// Sun shadow cascades, 1 to 4.
    pub cascades: u32,
    /// Farthest distance in metres the sun casts shadows at.
    pub shadow_distance: u32,
    /// Most point / spot lights with shadows (nearest first); 0 = none.
    pub shadowed_lights: u32,
    /// Edge in texels of the point / spot shadow atlas.
    pub local_shadow_size: u32,
}

pub const SHADOW_MAP_SIZES: [u32; 3] = [1024, 2048, 4096];
pub const MSAA_SAMPLES: [u32; 3] = [1, 2, 4];
pub const MAX_LIGHTS_OPTIONS: [u32; 4] = [64, 128, 256, 512];
pub const CASCADE_OPTIONS: [u32; 4] = [1, 2, 3, 4];
pub const SHADOW_DISTANCE_OPTIONS: [u32; 4] = [40, 60, 100, 200];
pub const SHADOWED_LIGHTS_OPTIONS: [u32; 5] = [0, 2, 4, 8, 16];
pub const LOCAL_SHADOW_SIZES: [u32; 3] = [1024, 2048, 4096];

impl Default for RenderQuality {
    fn default() -> Self {
        Self::preset(QualityPreset::High)
    }
}

/// One quality setting, for UIs that list them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualitySetting {
    Gi,
    SsgiResolution,
    Ao,
    ShadowMap,
    Msaa,
    Bloom,
    Present,
    MaxLights,
    Cascades,
    ShadowDistance,
    ShadowedLights,
    LocalShadowSize,
}

impl QualitySetting {
    pub const ALL: [QualitySetting; 12] = [
        Self::Gi,
        Self::SsgiResolution,
        Self::Ao,
        Self::ShadowMap,
        Self::Msaa,
        Self::Bloom,
        Self::Present,
        Self::MaxLights,
        Self::Cascades,
        Self::ShadowDistance,
        Self::ShadowedLights,
        Self::LocalShadowSize,
    ];

    /// True when a change recreates GPU targets and pipelines (shadow map,
    /// MSAA); false when it switches on the next frame.
    pub fn needs_rebuild(self) -> bool {
        matches!(
            self,
            Self::ShadowMap | Self::Msaa | Self::Cascades | Self::LocalShadowSize
        )
    }
}

impl RenderQuality {
    pub fn preset(preset: QualityPreset) -> Self {
        use PresentPreference::*;
        use SsgiResolution::*;
        let (gi_mode, ssgi_resolution, ao, shadow_map_size, msaa, bloom, present) = match preset {
            QualityPreset::Low => (GiMode::Off, Quarter, false, 1024, 1, false, Vsync),
            QualityPreset::Medium => (GiMode::Ssgi, Quarter, true, 2048, 2, true, Vsync),
            QualityPreset::High => (GiMode::Ddgi, Half, true, 2048, 4, true, MailboxCapped),
            QualityPreset::Max => (GiMode::Ddgi, Half, true, 4096, 4, true, MailboxCapped),
        };
        // (max lights, cascades, shadow distance, shadowed lights, atlas size)
        let (max_lights, cascades, shadow_distance, shadowed_lights, local_shadow_size) =
            match preset {
                QualityPreset::Low => (64, 1, 40, 0, 1024),
                QualityPreset::Medium => (128, 2, 60, 4, 2048),
                QualityPreset::High => (256, 3, 100, 8, 2048),
                QualityPreset::Max => (512, 4, 200, 16, 4096),
            };
        Self {
            gi_mode,
            ssgi_resolution,
            ao,
            shadow_map_size,
            msaa,
            bloom,
            present,
            max_lights,
            cascades,
            shadow_distance,
            shadowed_lights,
            local_shadow_size,
        }
    }

    /// The preset these values equal, or `None` (custom).
    pub fn matching_preset(&self) -> Option<QualityPreset> {
        QualityPreset::ALL
            .into_iter()
            .find(|&p| Self::preset(p) == *self)
    }

    /// Missing, unreadable or out-of-range files give the defaults; the file
    /// is left untouched.
    pub fn load(path: &Path) -> (Self, LoadOutcome) {
        let (quality, outcome) = crate::ron_file::load::<Self>(path);
        if outcome == LoadOutcome::Loaded
            && !(SHADOW_MAP_SIZES.contains(&quality.shadow_map_size)
                && MSAA_SAMPLES.contains(&quality.msaa))
        {
            let msg = format!(
                "shadow_map_size {} or msaa {} out of range",
                quality.shadow_map_size, quality.msaa
            );
            return (Self::default(), LoadOutcome::Broken(msg));
        }
        (quality, outcome)
    }

    /// Atomic write; with `backup_broken` the old file is kept as `<path>.broken`.
    pub fn save(&self, path: &Path, backup_broken: bool) -> anyhow::Result<()> {
        crate::ron_file::save(self, path, backup_broken)
    }
}

/// Per-pass GPU time in milliseconds, averaged over the last second, and the
/// frame budget from the monitor refresh. `None` = not measured (no
/// timestamp support, or the pass did not run). Filled by
/// `Renderer::timings`; the host inserts it each frame.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderTimings {
    pub gi: Option<f32>,
    pub ao: Option<f32>,
    pub shadows: Option<f32>,
    /// The whole 3D pass (meshes at the MSAA sample count plus the resolve):
    /// timestamps measure passes, not the MSAA share alone.
    pub aa: Option<f32>,
    pub bloom: Option<f32>,
    pub total: Option<f32>,
    pub budget_ms: Option<f32>,
    /// Sample count the 3D pass runs at: `RenderQuality::msaa`, or the
    /// nearest lower count the adapter supports.
    pub msaa: Option<u32>,
}

/// Profile names become file names: letters, digits, spaces, `-` and `_`,
/// no leading or trailing space, at most 64 characters, no reserved
/// Windows device names. Returns why a name is refused.
pub fn validate_profile_name(name: &str) -> Result<(), String> {
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let upper = name.to_uppercase();
    let numbered = (upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && upper.as_bytes()[3].is_ascii_digit();
    if name.trim().is_empty() {
        Err("Profile name is empty".into())
    } else if name.chars().count() > 64 {
        Err("Profile name is longer than 64 characters".into())
    } else if name.trim() != name {
        Err("Profile name starts or ends with a space".into())
    } else if !name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
    {
        Err("Profile names allow letters, digits, spaces, - and _".into())
    } else if RESERVED.contains(&upper.as_str()) || numbered {
        Err(format!("{name:?} is a reserved file name"))
    } else {
        Ok(())
    }
}

fn profile_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    validate_profile_name(name)?;
    Ok(dir.join(format!("{name}.ron")))
}

/// Profile names in `dir`, sorted; a missing folder has none.
pub fn list_profiles(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_stem()?.to_str()?;
            (path.extension()? == "ron" && validate_profile_name(name).is_ok())
                .then(|| name.to_owned())
        })
        .collect();
    names.sort();
    names
}

/// Load the profile `name` from `dir`; a missing or broken file is an error.
pub fn load_profile(dir: &Path, name: &str) -> anyhow::Result<RenderQuality> {
    let path = profile_path(dir, name).map_err(anyhow::Error::msg)?;
    match RenderQuality::load(&path) {
        (quality, LoadOutcome::Loaded) => Ok(quality),
        (_, LoadOutcome::Missing) => anyhow::bail!("profile {name:?} not found"),
        (_, LoadOutcome::Broken(msg)) => anyhow::bail!("profile {name:?} is broken: {msg}"),
    }
}

/// Save (or overwrite) the profile `name` in `dir`.
pub fn save_profile(dir: &Path, name: &str, quality: &RenderQuality) -> anyhow::Result<()> {
    let path = profile_path(dir, name).map_err(anyhow::Error::msg)?;
    quality.save(&path, false)
}

pub fn delete_profile(dir: &Path, name: &str) -> anyhow::Result<()> {
    let path = profile_path(dir, name).map_err(anyhow::Error::msg)?;
    std::fs::remove_file(&path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_quality_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn q(
        gi_mode: GiMode,
        ssgi_resolution: SsgiResolution,
        ao: bool,
        shadow_map_size: u32,
        msaa: u32,
        bloom: bool,
        present: PresentPreference,
    ) -> RenderQuality {
        RenderQuality {
            gi_mode,
            ssgi_resolution,
            ao,
            shadow_map_size,
            msaa,
            bloom,
            present,
            ..RenderQuality::preset(match gi_mode {
                GiMode::Off => QualityPreset::Low,
                GiMode::Ssgi => QualityPreset::Medium,
                GiMode::Ddgi if shadow_map_size == 4096 => QualityPreset::Max,
                GiMode::Ddgi => QualityPreset::High,
            })
        }
    }

    #[test]
    fn preset_values_are_as_specced() {
        use PresentPreference::*;
        use SsgiResolution::*;
        let p = RenderQuality::preset;
        assert_eq!(
            p(QualityPreset::Low),
            q(GiMode::Off, Quarter, false, 1024, 1, false, Vsync)
        );
        assert_eq!(
            p(QualityPreset::Medium),
            q(GiMode::Ssgi, Quarter, true, 2048, 2, true, Vsync)
        );
        assert_eq!(
            p(QualityPreset::High),
            q(GiMode::Ddgi, Half, true, 2048, 4, true, MailboxCapped)
        );
        assert_eq!(
            p(QualityPreset::Max),
            q(GiMode::Ddgi, Half, true, 4096, 4, true, MailboxCapped)
        );
    }

    #[test]
    fn every_preset_value_is_one_of_the_panel_options() {
        for p in QualityPreset::ALL {
            let q = RenderQuality::preset(p);
            assert!(MAX_LIGHTS_OPTIONS.contains(&q.max_lights), "{p:?}");
            assert!(CASCADE_OPTIONS.contains(&q.cascades), "{p:?}");
            assert!(
                SHADOW_DISTANCE_OPTIONS.contains(&q.shadow_distance),
                "{p:?}"
            );
            assert!(
                SHADOWED_LIGHTS_OPTIONS.contains(&q.shadowed_lights),
                "{p:?}"
            );
            assert!(LOCAL_SHADOW_SIZES.contains(&q.local_shadow_size), "{p:?}");
        }
        assert!(QualitySetting::Cascades.needs_rebuild());
        assert!(QualitySetting::LocalShadowSize.needs_rebuild());
        assert!(!QualitySetting::MaxLights.needs_rebuild());
        assert_eq!(QualitySetting::ALL.len(), 12);
    }

    #[test]
    fn high_is_default() {
        assert_eq!(
            RenderQuality::default(),
            RenderQuality::preset(QualityPreset::High)
        );
        assert_eq!(GiMode::default(), GiMode::Ddgi);
        assert_eq!(SsgiResolution::default(), SsgiResolution::Half);
        assert_eq!(
            [
                SsgiResolution::Half.scale(),
                SsgiResolution::Quarter.scale()
            ],
            [2, 4]
        );
    }

    #[test]
    fn matching_preset_finds_each_and_none_after_one_change() {
        for preset in QualityPreset::ALL {
            let base = RenderQuality::preset(preset);
            assert_eq!(base.matching_preset(), Some(preset));
            let edits: [fn(&mut RenderQuality); 7] = [
                |q| {
                    q.gi_mode = match q.gi_mode {
                        GiMode::Off => GiMode::Ssgi,
                        _ => GiMode::Off,
                    }
                },
                |q| {
                    q.ssgi_resolution = match q.ssgi_resolution {
                        SsgiResolution::Half => SsgiResolution::Quarter,
                        SsgiResolution::Quarter => SsgiResolution::Half,
                    }
                },
                |q| q.ao = !q.ao,
                |q| {
                    q.shadow_map_size = if q.shadow_map_size == 1024 {
                        2048
                    } else {
                        1024
                    }
                },
                |q| q.msaa = if q.msaa == 1 { 2 } else { 1 },
                |q| q.bloom = !q.bloom,
                |q| {
                    q.present = match q.present {
                        PresentPreference::Vsync => PresentPreference::MailboxCapped,
                        PresentPreference::MailboxCapped => PresentPreference::Vsync,
                    }
                },
            ];
            for (i, edit) in edits.iter().enumerate() {
                let mut changed = base;
                edit(&mut changed);
                assert_eq!(changed.matching_preset(), None, "{preset:?} edit {i}");
            }
        }
    }

    #[test]
    fn live_and_rebuild_settings() {
        use QualitySetting::*;
        for s in [Gi, SsgiResolution, Ao, Bloom, Present] {
            assert!(!s.needs_rebuild(), "{s:?}");
        }
        for s in [ShadowMap, Msaa] {
            assert!(s.needs_rebuild(), "{s:?}");
        }
    }

    #[test]
    fn round_trip() {
        let dir = temp_dir("round_trip");
        let path = dir.join("config/quality.ron");
        let low = RenderQuality::preset(QualityPreset::Low);
        low.save(&path, false).unwrap();
        assert_eq!(RenderQuality::load(&path), (low, LoadOutcome::Loaded));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_gives_default() {
        let dir = temp_dir("missing");
        let (quality, outcome) = RenderQuality::load(&dir.join("quality.ron"));
        assert_eq!(outcome, LoadOutcome::Missing);
        assert_eq!(quality, RenderQuality::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_or_out_of_range_file_gives_default() {
        let dir = temp_dir("corrupt");
        let path = dir.join("quality.ron");
        let mut odd = RenderQuality::preset(QualityPreset::Low);
        odd.msaa = 3;
        let odd = ron::to_string(&odd).unwrap();
        for text in ["not ron {{", odd.as_str()] {
            std::fs::write(&path, text).unwrap();
            let (quality, outcome) = RenderQuality::load(&path);
            assert!(matches!(outcome, LoadOutcome::Broken(_)), "{outcome:?}");
            assert_eq!(quality, RenderQuality::default());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_field_takes_default_and_unknown_field_is_ignored() {
        let dir = temp_dir("fields");
        let path = dir.join("quality.ron");
        // Low without `bloom` (and `ssgi_resolution`): the rest stays, they
        // take the High defaults.
        std::fs::write(
            &path,
            "(gi_mode: Off, ao: false, shadow_map_size: 1024, msaa: 1, present: Vsync)",
        )
        .unwrap();
        let mut expected = RenderQuality::preset(QualityPreset::Low);
        expected.bloom = RenderQuality::default().bloom;
        expected.ssgi_resolution = RenderQuality::default().ssgi_resolution;
        let default = RenderQuality::default();
        expected.max_lights = default.max_lights;
        expected.cascades = default.cascades;
        expected.shadow_distance = default.shadow_distance;
        expected.shadowed_lights = default.shadowed_lights;
        expected.local_shadow_size = default.local_shadow_size;
        assert_eq!(RenderQuality::load(&path), (expected, LoadOutcome::Loaded));

        // A file from before `ssgi_resolution`: Medium loads with the
        // default (Half), everything else as saved.
        std::fs::write(
            &path,
            "(gi_mode: Ssgi, ao: true, shadow_map_size: 2048, msaa: 2, bloom: true, present: Vsync)",
        )
        .unwrap();
        let expected = RenderQuality {
            ssgi_resolution: SsgiResolution::Half,
            ..RenderQuality {
                gi_mode: GiMode::Ssgi,
                ao: true,
                shadow_map_size: 2048,
                msaa: 2,
                bloom: true,
                present: PresentPreference::Vsync,
                ..RenderQuality::default()
            }
        };
        assert_eq!(RenderQuality::load(&path), (expected, LoadOutcome::Loaded));

        let low = RenderQuality::preset(QualityPreset::Low);
        let text = ron::to_string(&low)
            .unwrap()
            .replacen('(', "(future_knob: 3, ", 1);
        std::fs::write(&path, text).unwrap();
        assert_eq!(RenderQuality::load(&path), (low, LoadOutcome::Loaded));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_after_broken_writes_backup() {
        let dir = temp_dir("backup");
        let path = dir.join("quality.ron");
        std::fs::write(&path, "garbage").unwrap();
        RenderQuality::default().save(&path, true).unwrap();
        let backup = dir.join("quality.ron.broken");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "garbage");
        assert_eq!(RenderQuality::load(&path).1, LoadOutcome::Loaded);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn profile_names() {
        for good in ["my laptop", "Low-ish_2", "Ünïcode"] {
            assert_eq!(validate_profile_name(good), Ok(()), "{good:?}");
        }
        for bad in [
            "", " ", " lead", "trail ", "..", "a/b", "a\\b", "a.b", "c:", "CON", "nul", "Com1",
            "lpt9", "a*", "a?",
        ] {
            assert!(validate_profile_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_profile_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn profiles_list_save_load_delete() {
        let dir = temp_dir("profiles").join("quality_profiles");
        assert!(
            list_profiles(&dir).is_empty(),
            "missing folder: no profiles"
        );
        let low = RenderQuality::preset(QualityPreset::Low);
        save_profile(&dir, "zeta", &RenderQuality::default()).unwrap();
        save_profile(&dir, "my laptop", &low).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a profile").unwrap();
        assert_eq!(list_profiles(&dir), ["my laptop", "zeta"]);
        assert_eq!(load_profile(&dir, "my laptop").unwrap(), low);
        assert!(save_profile(&dir, "../evil", &low).is_err());
        assert!(load_profile(&dir, "missing").is_err());
        delete_profile(&dir, "my laptop").unwrap();
        assert_eq!(list_profiles(&dir), ["zeta"]);
        assert!(delete_profile(&dir, "..").is_err());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
