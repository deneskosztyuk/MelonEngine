//! Project physics settings (`config/physics.ron`): gravity and up to 32
//! named collision layers. The `Collider` `membership` / `filter` masks stay
//! raw bits; the names only label them in the editor.

use std::path::Path;

use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};

pub use crate::ron_file::LoadOutcome;

/// Number of collision layers (one bit each in a `u32` mask).
pub const LAYER_COUNT: usize = 32;

#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PhysicsSettings {
    /// m/s^2, applied to the physics world.
    pub gravity: [f32; 3],
    /// Name of each layer (index = bit); empty = unnamed.
    pub layer_names: Vec<String>,
}

impl Default for PhysicsSettings {
    fn default() -> Self {
        let mut layer_names = vec![String::new(); LAYER_COUNT];
        layer_names[0] = "Default".to_string();
        Self {
            gravity: [0.0, -9.81, 0.0],
            layer_names,
        }
    }
}

impl PhysicsSettings {
    /// The name of layer `i`, or `Layer i` when it has none.
    pub fn layer_label(&self, i: usize) -> String {
        match self.layer_names.get(i) {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ => format!("Layer {i}"),
        }
    }

    /// Finite gravity, at most 32 layers, no two layers with the same name.
    pub fn validate(&self) -> Result<(), String> {
        if !self.gravity.iter().all(|g| g.is_finite()) {
            return Err("gravity is not finite".into());
        }
        if self.layer_names.len() > LAYER_COUNT {
            return Err(format!("more than {LAYER_COUNT} layer names"));
        }
        for (i, name) in self.layer_names.iter().enumerate() {
            let name = name.trim();
            if !name.is_empty()
                && self.layer_names[..i]
                    .iter()
                    .any(|other| other.trim() == name)
            {
                return Err(format!("layer name {name} appears twice"));
            }
        }
        Ok(())
    }

    /// Missing, unreadable or invalid files give the defaults; the file is
    /// left untouched. Short name lists are padded to 32.
    pub fn load(path: &Path) -> (Self, LoadOutcome) {
        let (mut settings, outcome) = crate::ron_file::load::<Self>(path);
        if outcome == LoadOutcome::Loaded
            && let Err(message) = settings.validate()
        {
            return (Self::default(), LoadOutcome::Broken(message));
        }
        settings.layer_names.resize(LAYER_COUNT, String::new());
        (settings, outcome)
    }

    /// Atomic write; with `backup_broken` the old file is kept as `<path>.broken`.
    pub fn save(&self, path: &Path, backup_broken: bool) -> anyhow::Result<()> {
        crate::ron_file::save(self, path, backup_broken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_physics_settings_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("physics.ron")
    }

    #[test]
    fn defaults_are_earth_gravity_and_one_named_layer() {
        let s = PhysicsSettings::default();
        assert_eq!(s.gravity, [0.0, -9.81, 0.0]);
        assert_eq!(s.layer_names.len(), LAYER_COUNT);
        assert_eq!(s.layer_label(0), "Default");
        assert_eq!(s.layer_label(5), "Layer 5");
        assert!(s.validate().is_ok());
    }

    #[test]
    fn invalid_settings_are_refused() {
        let mut s = PhysicsSettings::default();
        s.gravity[1] = f32::NAN;
        assert!(s.validate().is_err());
        let mut s = PhysicsSettings::default();
        s.layer_names[1] = "Default".into();
        assert!(s.validate().unwrap_err().contains("twice"));
        let mut s = PhysicsSettings::default();
        s.layer_names.push("extra".into());
        assert!(s.validate().is_err());
    }

    #[test]
    fn save_then_load_round_trips_and_a_broken_file_gives_defaults() {
        let path = temp("round_trip");
        let mut s = PhysicsSettings {
            gravity: [0.0, -3.7, 0.0],
            ..Default::default()
        };
        s.layer_names[3] = "Enemies".into();
        s.save(&path, false).unwrap();
        let (loaded, outcome) = PhysicsSettings::load(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(loaded, s);
        std::fs::write(&path, "(gravity: oops").unwrap();
        let (loaded, outcome) = PhysicsSettings::load(&path);
        assert!(matches!(outcome, LoadOutcome::Broken(_)));
        assert_eq!(loaded, PhysicsSettings::default());
    }

    #[test]
    fn a_short_name_list_is_padded_to_32() {
        let path = temp("short");
        std::fs::write(&path, "(layer_names: [\"A\", \"B\"])").unwrap();
        let (loaded, _) = PhysicsSettings::load(&path);
        assert_eq!(loaded.layer_names.len(), LAYER_COUNT);
        assert_eq!(loaded.layer_label(1), "B");
        assert_eq!(loaded.gravity, [0.0, -9.81, 0.0]);
    }
}
