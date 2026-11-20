//! Project player settings (`config/player.ron`): what a packaged game is
//! called, which scene it starts in and how its window opens. Vsync and the
//! rest of the look come from `RenderQuality`.

use std::path::Path;

use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};

pub use crate::ron_file::LoadOutcome;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowMode {
    #[default]
    Windowed,
    BorderlessFullscreen,
}

#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerSettings {
    /// Window title, and the packaged folder and executable name.
    pub game_name: String,
    pub version: String,
    /// Project-relative scene path (`assets/scenes/main.ron`); empty = none.
    pub startup_scene: String,
    pub window_mode: WindowMode,
    /// Windowed size in pixels.
    pub window_size: [u32; 2],
    /// Globs (`*`, `**`, `?`) of project-relative paths left out of the pack.
    pub exclude: Vec<String>,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            game_name: "MelonGame".to_string(),
            version: "0.1.0".to_string(),
            startup_scene: String::new(),
            window_mode: WindowMode::Windowed,
            window_size: [1280, 720],
            exclude: vec!["assets/shaders/**".to_string(), "**/*.md".to_string()],
        }
    }
}

impl PlayerSettings {
    /// A usable folder / file name, a non-empty version and a sane window.
    pub fn validate(&self) -> Result<(), String> {
        let name = self.game_name.trim();
        if name.is_empty() {
            return Err("the game name is empty".into());
        }
        if name != self.game_name || name.ends_with('.') {
            return Err("the game name starts or ends with a space or dot".into());
        }
        if let Some(c) = name
            .chars()
            .find(|c| c.is_control() || "\\/:*?\"<>|".contains(*c))
        {
            return Err(format!("the game name contains {c:?}"));
        }
        // Windows device names (also with an extension) cannot be file names.
        let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
        let numbered = |p: &str| {
            stem.strip_prefix(p)
                .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
        };
        if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || numbered("COM")
            || numbered("LPT")
        {
            return Err(format!("{name} is a reserved Windows file name"));
        }
        if self.version.trim().is_empty() {
            return Err("the version is empty".into());
        }
        if self
            .window_size
            .iter()
            .any(|&n| !(160..=16384).contains(&n))
        {
            return Err("the window size must be 160 to 16384 pixels".into());
        }
        Ok(())
    }

    /// Missing, unreadable or invalid files give the defaults; the file is
    /// left untouched.
    pub fn load(path: &Path) -> (Self, LoadOutcome) {
        let (settings, outcome) = crate::ron_file::load::<Self>(path);
        if outcome == LoadOutcome::Loaded
            && let Err(message) = settings.validate()
        {
            return (Self::default(), LoadOutcome::Broken(message));
        }
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
        let dir = std::env::temp_dir().join(format!("melon_player_settings_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("player.ron")
    }

    #[test]
    fn defaults_are_valid() {
        assert!(PlayerSettings::default().validate().is_ok());
    }

    #[test]
    fn save_then_load_round_trips_and_a_broken_file_gives_defaults() {
        let path = temp("round_trip");
        let s = PlayerSettings {
            game_name: "Space Melon".into(),
            version: "1.2.3".into(),
            startup_scene: "assets/scenes/menu.ron".into(),
            window_mode: WindowMode::BorderlessFullscreen,
            window_size: [1920, 1080],
            exclude: vec!["assets/raw/**".into()],
        };
        s.save(&path, false).unwrap();
        let (loaded, outcome) = PlayerSettings::load(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(loaded, s);

        std::fs::write(&path, "(game_name: oops").unwrap();
        let (loaded, outcome) = PlayerSettings::load(&path);
        assert!(matches!(outcome, LoadOutcome::Broken(_)));
        assert_eq!(loaded, PlayerSettings::default());

        let missing = path.with_file_name("none.ron");
        assert_eq!(PlayerSettings::load(&missing).1, LoadOutcome::Missing);
    }

    #[test]
    fn invalid_settings_are_refused() {
        for bad in [
            PlayerSettings {
                game_name: " ".into(),
                ..Default::default()
            },
            PlayerSettings {
                game_name: "a/b".into(),
                ..Default::default()
            },
            PlayerSettings {
                game_name: "x.".into(),
                ..Default::default()
            },
            PlayerSettings {
                game_name: "con".into(),
                ..Default::default()
            },
            PlayerSettings {
                game_name: "Nul.Debug".into(),
                ..Default::default()
            },
            PlayerSettings {
                window_size: [1280, u32::MAX],
                ..Default::default()
            },
            PlayerSettings {
                version: "".into(),
                ..Default::default()
            },
            PlayerSettings {
                window_size: [0, 720],
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let path = temp("invalid");
        std::fs::write(&path, "(game_name: \"a|b\")").unwrap();
        assert!(matches!(
            PlayerSettings::load(&path).1,
            LoadOutcome::Broken(_)
        ));
    }

    #[test]
    fn unknown_fields_fall_back_to_defaults_per_field() {
        let path = temp("partial");
        std::fs::write(&path, "(game_name: \"Only Name\")").unwrap();
        let (s, outcome) = PlayerSettings::load(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(s.game_name, "Only Name");
        assert_eq!(s.window_size, [1280, 720]);
    }
}
