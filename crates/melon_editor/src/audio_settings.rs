//! Project Settings > Audio: the mixer buses (volume, mute, names). Edits
//! stay pending until Apply, which writes the `AudioSettings` resource and
//! `config/audio.ron`.

use std::path::{Path, PathBuf};

use bevy_ecs::world::World;
use egui::{Align, Layout};
use melon_core::audio::MASTER;
use melon_core::{AudioBus, AudioSettings};

use crate::{StatusKind, theme};

type Status = Option<(StatusKind, String)>;

/// Where the audio buses of a project are saved.
pub fn audio_path(project_root: &Path) -> PathBuf {
    project_root.join("config").join("audio.ron")
}

#[derive(Default)]
pub struct AudioSettingsPanel {
    /// Edited values not applied yet; `None` = showing the applied ones.
    pending: Option<AudioSettings>,
    /// `config/audio.ron` failed to load: the first save backs it up first.
    broken_on_load: bool,
}

/// A bus name nobody uses yet: `Bus 1`, `Bus 2`, ...
fn new_bus_name(settings: &AudioSettings) -> String {
    (1..)
        .map(|n| format!("Bus {n}"))
        .find(|name| settings.buses.iter().all(|b| &b.name != name))
        .expect("an unused name exists")
}

impl AudioSettingsPanel {
    pub fn set_broken_on_load(&mut self, broken: bool) {
        self.broken_on_load = broken;
    }

    /// Write the pending values to the resource and `config/audio.ron`;
    /// an invalid set (empty or duplicate names) is refused.
    fn apply(&mut self, world: &mut World, project_root: &Path) -> Status {
        let settings = self.pending.as_ref()?;
        if let Err(message) = settings.validate() {
            return Some((StatusKind::Warning, format!("Audio settings: {message}")));
        }
        let settings = self.pending.take()?;
        let path = audio_path(project_root);
        let saved = settings.save(&path, self.broken_on_load);
        world.insert_resource(settings);
        match saved {
            Ok(()) => {
                self.broken_on_load = false;
                Some((StatusKind::Success, "Audio settings applied".into()))
            }
            Err(error) => {
                tracing::error!("saving {} failed: {error:#}", path.display());
                Some((
                    StatusKind::Error,
                    format!("Saving audio settings failed: {error:#}"),
                ))
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, world: &mut World, project_root: &Path) -> Status {
        let applied = world
            .get_resource::<AudioSettings>()
            .cloned()
            .unwrap_or_default();
        let mut shown = self.pending.clone().unwrap_or_else(|| applied.clone());
        let mut remove = None;
        egui::Grid::new("audio_buses")
            .num_columns(4)
            .spacing([theme::sizes::SPACING * 2.0, theme::sizes::SPACING])
            .show(ui, |ui| {
                ui.weak("Bus");
                ui.weak("Volume (0 to 1)");
                ui.weak("Mute");
                ui.end_row();
                for (i, bus) in shown.buses.iter_mut().enumerate() {
                    if bus.name == MASTER && i == 0 {
                        ui.label(MASTER);
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(&mut bus.name)
                                .desired_width(theme::sizes::SETTINGS_NAME),
                        );
                    }
                    ui.add(egui::Slider::new(&mut bus.volume, 0.0..=1.0));
                    ui.checkbox(&mut bus.mute, "");
                    if i > 0 && ui.button("Remove").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            shown.buses.remove(i);
        }
        if ui.button("+ Bus").clicked() {
            let name = new_bus_name(&shown);
            shown.buses.push(AudioBus {
                name,
                ..AudioBus::default()
            });
        }
        ui.weak("A source plays on the bus named in its Bus field; Master scales every bus.");
        if shown != applied {
            self.pending = Some(shown);
        } else {
            self.pending = None;
        }

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
        let dir = std::env::temp_dir().join(format!("melon_audio_settings_{name}"));
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
        let mut panel = AudioSettingsPanel::default();
        let mut edited = AudioSettings::default();
        edited.buses[1].volume = 0.4;
        edited.buses[2].mute = true;
        panel.pending = Some(edited.clone());
        let status = panel.apply(&mut world, &dir);
        assert!(
            matches!(status, Some((StatusKind::Success, _))),
            "{status:?}"
        );
        assert_eq!(*world.resource::<AudioSettings>(), edited);
        assert_eq!(AudioSettings::load(&audio_path(&dir)).0, edited);
        assert!(panel.pending.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_refuses_duplicate_or_empty_names() {
        let dir = temp_dir("refuse");
        let mut world = world();
        let mut panel = AudioSettingsPanel::default();
        for name in ["Sfx", " "] {
            let mut edited = AudioSettings::default();
            edited.buses.push(AudioBus {
                name: name.into(),
                ..AudioBus::default()
            });
            panel.pending = Some(edited);
            let status = panel.apply(&mut world, &dir);
            assert!(matches!(status, Some((StatusKind::Warning, _))), "{name:?}");
            assert_eq!(*world.resource::<AudioSettings>(), AudioSettings::default());
            assert!(!audio_path(&dir).exists());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn new_bus_names_skip_used_ones() {
        let mut s = AudioSettings::default();
        assert_eq!(new_bus_name(&s), "Bus 1");
        s.buses.push(AudioBus {
            name: "Bus 1".into(),
            ..AudioBus::default()
        });
        assert_eq!(new_bus_name(&s), "Bus 2");
    }
}
