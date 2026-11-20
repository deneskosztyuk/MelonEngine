//! Project Settings > Physics: gravity and the 32 collision layer names.
//! Edits stay pending until Apply, which writes the `PhysicsSettings`
//! resource (gravity reaches the physics world at once) and
//! `config/physics.ron`.

use std::path::{Path, PathBuf};

use bevy_ecs::world::World;
use egui::{Align, Layout};
use melon_core::PhysicsSettings;

use crate::{StatusKind, theme};

type Status = Option<(StatusKind, String)>;

/// Where the physics settings of a project are saved.
pub fn physics_path(project_root: &Path) -> PathBuf {
    project_root.join("config").join("physics.ron")
}

#[derive(Default)]
pub struct PhysicsSettingsPanel {
    /// Edited values not applied yet; `None` = showing the applied ones.
    pending: Option<PhysicsSettings>,
    /// `config/physics.ron` failed to load: the first save backs it up first.
    broken_on_load: bool,
}

impl PhysicsSettingsPanel {
    pub fn set_broken_on_load(&mut self, broken: bool) {
        self.broken_on_load = broken;
    }

    /// Write the pending values to the resource and `config/physics.ron`;
    /// an invalid set (non-finite gravity, duplicate layer names) is refused.
    fn apply(&mut self, world: &mut World, project_root: &Path) -> Status {
        let settings = self.pending.as_ref()?;
        if let Err(message) = settings.validate() {
            return Some((StatusKind::Warning, format!("Physics settings: {message}")));
        }
        let settings = self.pending.take()?;
        let path = physics_path(project_root);
        let saved = settings.save(&path, self.broken_on_load);
        world.insert_resource(settings);
        match saved {
            Ok(()) => {
                self.broken_on_load = false;
                Some((StatusKind::Success, "Physics settings applied".into()))
            }
            Err(error) => {
                tracing::error!("saving {} failed: {error:#}", path.display());
                Some((
                    StatusKind::Error,
                    format!("Saving physics settings failed: {error:#}"),
                ))
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, world: &mut World, project_root: &Path) -> Status {
        let applied = world
            .get_resource::<PhysicsSettings>()
            .cloned()
            .unwrap_or_default();
        let mut shown = self.pending.clone().unwrap_or_else(|| applied.clone());
        ui.horizontal(|ui| {
            ui.label("Gravity (m/s\u{b2})");
            for g in &mut shown.gravity {
                ui.add(egui::DragValue::new(g).speed(0.05));
            }
        });
        ui.add_space(theme::sizes::SPACING);
        ui.label("Collision layers");
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("physics_layers")
                .num_columns(2)
                .spacing([theme::sizes::SPACING * 2.0, theme::sizes::SPACING])
                .show(ui, |ui| {
                    for (i, name) in shown.layer_names.iter_mut().enumerate() {
                        ui.weak(format!("{i}"));
                        ui.add(
                            egui::TextEdit::singleline(name)
                                .hint_text(format!("Layer {i}"))
                                .desired_width(theme::sizes::SETTINGS_NAME),
                        );
                        ui.end_row();
                    }
                });
        });
        ui.weak("A collider's Layer and Collides with fields show these names; the saved value is the raw bit mask.");
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
            }
        });
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_physics_settings_panel_{name}"));
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
        let mut panel = PhysicsSettingsPanel::default();
        let mut edited = PhysicsSettings {
            gravity: [0.0, -3.7, 0.0],
            ..Default::default()
        };
        edited.layer_names[3] = "Enemies".into();
        panel.pending = Some(edited.clone());
        let status = panel.apply(&mut world, &dir);
        assert!(
            matches!(status, Some((StatusKind::Success, _))),
            "{status:?}"
        );
        assert_eq!(*world.resource::<PhysicsSettings>(), edited);
        assert_eq!(PhysicsSettings::load(&physics_path(&dir)).0, edited);
        assert!(panel.pending.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_refuses_duplicate_names_and_bad_gravity() {
        let dir = temp_dir("refuse");
        let mut world = world();
        let mut panel = PhysicsSettingsPanel::default();
        let mut duplicate = PhysicsSettings::default();
        duplicate.layer_names[1] = "Default".into();
        let mut nan = PhysicsSettings::default();
        nan.gravity[0] = f32::NAN;
        for edited in [duplicate, nan] {
            panel.pending = Some(edited);
            let status = panel.apply(&mut world, &dir);
            assert!(matches!(status, Some((StatusKind::Warning, _))));
            assert_eq!(
                *world.resource::<PhysicsSettings>(),
                PhysicsSettings::default()
            );
            assert!(!physics_path(&dir).exists());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn panel_draws_a_row_per_layer_without_changing_anything() {
        let dir = temp_dir("draw");
        let mut world = world();
        let mut panel = PhysicsSettingsPanel::default();
        let ctx = egui::Context::default();
        let mut status = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            status = panel.ui(ui, &mut world, &dir);
        });
        assert!(status.is_none());
        assert!(panel.pending.is_none(), "drawing alone is not an edit");
    }
}
