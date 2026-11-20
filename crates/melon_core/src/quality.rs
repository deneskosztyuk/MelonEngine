//! Render quality: what the GPU spends time on (GI, AO, shadow map size,
//! MSAA, bloom, present mode), apart from the look in `Environment`.
//! Saved as `config/quality.ron`; named profiles as
//! `config/quality_profiles/<name>.ron`.

use std::path::{Path, PathBuf};

use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};

pub use crate::ron_file::LoadOutcome;

/// Which global illumination technique runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GiMode {
    Off,
    /// Screen-space GI.
    Ssgi,
    /// Dynamic diffuse GI probes over a voxel scene.
    #[default]
    Ddgi,
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

/// Render quality settings. `Default` is the High preset.
/// Nothing reads this yet: the renderer wiring is still to come.
/// A field missing from a file takes its `Default` value and unknown fields
/// are ignored, so adding a setting keeps old files and profiles loading.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderQuality {
    pub gi_mode: GiMode,
    /// Screen-space ambient occlusion on or off.
    pub ao: bool,
    /// Sun shadow map edge in texels: 1024, 2048 or 4096.
    pub shadow_map_size: u32,
    /// MSAA samples of the 3D pass: 1 (off), 2 or 4.
    pub msaa: u32,
    pub bloom: bool,
    pub present: PresentPreference,
}

pub const SHADOW_MAP_SIZES: [u32; 3] = [1024, 2048, 4096];
pub const MSAA_SAMPLES: [u32; 3] = [1, 2, 4];

impl Default for RenderQuality {
    fn default() -> Self {
        Self::preset(QualityPreset::High)
    }
}

/// One quality setting, for UIs that list them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualitySetting {
    Gi,
    Ao,
    ShadowMap,
    Msaa,
    Bloom,
    Present,
}

impl QualitySetting {
    pub const ALL: [QualitySetting; 6] = [
        Self::Gi,
        Self::Ao,
        Self::ShadowMap,
        Self::Msaa,
        Self::Bloom,
        Self::Present,
    ];

    /// True when a change recreates GPU targets and pipelines (shadow map,
    /// MSAA); false when it switches on the next frame.
    pub fn needs_rebuild(self) -> bool {
        matches!(self, Self::ShadowMap | Self::Msaa)
    }
}

impl RenderQuality {
    pub fn preset(preset: QualityPreset) -> Self {
        use PresentPreference::*;
        let (gi_mode, ao, shadow_map_size, msaa, bloom, present) = match preset {
            QualityPreset::Low => (GiMode::Off, false, 1024, 1, false, Vsync),
            QualityPreset::Medium => (GiMode::Ssgi, true, 2048, 2, true, Vsync),
            QualityPreset::High => (GiMode::Ddgi, true, 2048, 4, true, MailboxCapped),
            QualityPreset::Max => (GiMode::Ddgi, true, 4096, 4, true, MailboxCapped),
        };
        Self {
            gi_mode,
            ao,
            shadow_map_size,
            msaa,
            bloom,
            present,
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
/// timestamp support, or the renderer does not fill it yet).
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
        ao: bool,
        shadow_map_size: u32,
        msaa: u32,
        bloom: bool,
        present: PresentPreference,
    ) -> RenderQuality {
        RenderQuality {
            gi_mode,
            ao,
            shadow_map_size,
            msaa,
            bloom,
            present,
        }
    }

    #[test]
    fn preset_values_are_as_specced() {
        use PresentPreference::*;
        let p = RenderQuality::preset;
        assert_eq!(
            p(QualityPreset::Low),
            q(GiMode::Off, false, 1024, 1, false, Vsync)
        );
        assert_eq!(
            p(QualityPreset::Medium),
            q(GiMode::Ssgi, true, 2048, 2, true, Vsync)
        );
        assert_eq!(
            p(QualityPreset::High),
            q(GiMode::Ddgi, true, 2048, 4, true, MailboxCapped)
        );
        assert_eq!(
            p(QualityPreset::Max),
            q(GiMode::Ddgi, true, 4096, 4, true, MailboxCapped)
        );
    }

    #[test]
    fn high_is_default() {
        assert_eq!(
            RenderQuality::default(),
            RenderQuality::preset(QualityPreset::High)
        );
        assert_eq!(GiMode::default(), GiMode::Ddgi);
    }

    #[test]
    fn matching_preset_finds_each_and_none_after_one_change() {
        for preset in QualityPreset::ALL {
            let base = RenderQuality::preset(preset);
            assert_eq!(base.matching_preset(), Some(preset));
            let edits: [fn(&mut RenderQuality); 6] = [
                |q| {
                    q.gi_mode = match q.gi_mode {
                        GiMode::Off => GiMode::Ssgi,
                        _ => GiMode::Off,
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
        for s in [Gi, Ao, Bloom, Present] {
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
        // Low without `bloom`: the rest stays, bloom takes the High default.
        std::fs::write(
            &path,
            "(gi_mode: Off, ao: false, shadow_map_size: 1024, msaa: 1, present: Vsync)",
        )
        .unwrap();
        let mut expected = RenderQuality::preset(QualityPreset::Low);
        expected.bloom = RenderQuality::default().bloom;
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
