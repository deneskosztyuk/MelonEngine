//! Project Settings > Player: game name, startup scene, window mode and
//! size, version and pack excludes. Edits stay pending until Apply, which
//! writes the `PlayerSettings` resource and `config/player.ron`.

use std::path::{Path, PathBuf};

use bevy_ecs::world::World;
use egui::{Align, Layout};
use melon_core::assets::{asset_dir_files, project_relative};
use melon_core::{PlayerSettings, WindowMode};

use crate::{StatusKind, theme};

type Status = Option<(StatusKind, String)>;

/// Where the player settings of a project are saved.
pub fn player_path(project_root: &Path) -> PathBuf {
    project_root.join("config").join("player.ron")
}

/// Scene files (`assets/scenes/*.ron`) a game can start in.
fn scene_files(project_root: &Path) -> Vec<String> {
    asset_dir_files(project_root)
        .iter()
        .filter_map(|f| project_relative(project_root, f))
        .filter(|p| p.starts_with("assets/scenes/") && p.ends_with(".ron"))
        .collect()
}

#[derive(Default)]
pub struct PlayerSettingsPanel {
    /// Edited values not applied yet; `None` = showing the applied ones.
    pending: Option<PlayerSettings>,
    /// `config/player.ron` failed to load: the first save backs it up first.
    broken_on_load: bool,
    /// The exclude patterns as one text, one per line.
    exclude_text: Option<String>,
}

impl PlayerSettingsPanel {
    pub fn set_broken_on_load(&mut self, broken: bool) {
        self.broken_on_load = broken;
    }

    /// Write the pending values to the resource and `config/player.ron`;
    /// an invalid set is refused.
    fn apply(&mut self, world: &mut World, project_root: &Path) -> Status {
        let settings = self.pending.as_ref()?;
        if let Err(message) = settings.validate() {
            return Some((StatusKind::Warning, format!("Player settings: {message}")));
        }
        let settings = self.pending.take()?;
        self.exclude_text = None;
        let path = player_path(project_root);
        let saved = settings.save(&path, self.broken_on_load);
        world.insert_resource(settings);
        match saved {
            Ok(()) => {
                self.broken_on_load = false;
                Some((StatusKind::Success, "Player settings applied".into()))
            }
            Err(error) => {
                tracing::error!("saving {} failed: {error:#}", path.display());
                Some((
                    StatusKind::Error,
                    format!("Saving player settings failed: {error:#}"),
                ))
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, world: &mut World, project_root: &Path) -> Status {
        let applied = world
            .get_resource::<PlayerSettings>()
            .cloned()
            .unwrap_or_default();
        let mut shown = self.pending.clone().unwrap_or_else(|| applied.clone());
        egui::Grid::new("player_settings")
            .num_columns(2)
            .spacing([theme::sizes::SPACING * 2.0, theme::sizes::SPACING])
            .show(ui, |ui| {
                ui.label("Game name");
                ui.text_edit_singleline(&mut shown.game_name);
                ui.end_row();
                ui.label("Version");
                ui.text_edit_singleline(&mut shown.version);
                ui.end_row();
                ui.label("Startup scene");
                let label = if shown.startup_scene.is_empty() {
                    "(none)".to_string()
                } else {
                    shown.startup_scene.clone()
                };
                egui::ComboBox::from_id_salt("startup_scene")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut shown.startup_scene, String::new(), "(none)");
                        for scene in scene_files(project_root) {
                            ui.selectable_value(&mut shown.startup_scene, scene.clone(), scene);
                        }
                    });
                ui.end_row();
                ui.label("Window mode");
                egui::ComboBox::from_id_salt("window_mode")
                    .selected_text(mode_label(shown.window_mode))
                    .show_ui(ui, |ui| {
                        for mode in [WindowMode::Windowed, WindowMode::BorderlessFullscreen] {
                            ui.selectable_value(&mut shown.window_mode, mode, mode_label(mode));
                        }
                    });
                ui.end_row();
                ui.label("Window size");
                ui.horizontal(|ui| {
                    for n in &mut shown.window_size {
                        ui.add(egui::DragValue::new(n).range(160..=16384));
                    }
                });
                ui.end_row();
            });
        ui.add_space(theme::sizes::SPACING);
        ui.label("Pack excludes (one glob per line)");
        let mut text = self
            .exclude_text
            .clone()
            .unwrap_or_else(|| shown.exclude.join("\n"));
        if ui.text_edit_multiline(&mut text).changed() {
            shown.exclude = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect();
            self.exclude_text = Some(text);
        }
        ui.weak("Globs match project-relative paths: * inside a folder, ** across folders. Thumbnails, .meta files and .melon/ never ship.");
        self.pending = (shown != applied).then_some(shown);

        let mut status = None;
        ui.separator();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let dirty = self.pending.is_some();
            let apply = egui::Button::new("Apply").fill(theme::colors::ACCENT);
            if ui.add_enabled(dirty, apply).clicked() {
                status = self.apply(world, project_root);
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                self.pending = None;
                self.exclude_text = None;
            }
        });
        status
    }
}

fn mode_label(mode: WindowMode) -> &'static str {
    match mode {
        WindowMode::Windowed => "Windowed",
        WindowMode::BorderlessFullscreen => "Borderless fullscreen",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_player_settings_panel_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn world() -> World {
        melon_core::Core::new().world
    }

    #[test]
    fn apply_writes_resource_and_file() {
        let dir = temp_dir("apply");
        let mut world = world();
        let mut panel = PlayerSettingsPanel::default();
        let edited = PlayerSettings {
            game_name: "Space Melon".into(),
            startup_scene: "assets/scenes/main.ron".into(),
            window_mode: WindowMode::BorderlessFullscreen,
            ..Default::default()
        };
        panel.pending = Some(edited.clone());
        let status = panel.apply(&mut world, &dir);
        assert!(
            matches!(status, Some((StatusKind::Success, _))),
            "{status:?}"
        );
        assert_eq!(*world.resource::<PlayerSettings>(), edited);
        assert_eq!(PlayerSettings::load(&player_path(&dir)).0, edited);
        assert!(panel.pending.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_refuses_an_invalid_name() {
        let dir = temp_dir("refuse");
        let mut world = world();
        let mut panel = PlayerSettingsPanel {
            pending: Some(PlayerSettings {
                game_name: "a/b".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let status = panel.apply(&mut world, &dir);
        assert!(matches!(status, Some((StatusKind::Warning, _))));
        assert_eq!(
            *world.resource::<PlayerSettings>(),
            PlayerSettings::default()
        );
        assert!(!player_path(&dir).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn panel_draws_without_changing_anything_and_lists_scenes() {
        let dir = temp_dir("draw");
        std::fs::create_dir_all(dir.join("assets/scenes")).unwrap();
        std::fs::write(dir.join("assets/scenes/main.ron"), "()").unwrap();
        std::fs::write(dir.join("assets/scenes/main.ron.meta"), "()").unwrap();
        assert_eq!(scene_files(&dir), ["assets/scenes/main.ron"]);
        let mut world = world();
        let mut panel = PlayerSettingsPanel::default();
        let ctx = egui::Context::default();
        let mut status = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            status = panel.ui(ui, &mut world, &dir);
        });
        assert!(status.is_none());
        assert!(panel.pending.is_none(), "drawing alone is not an edit");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
