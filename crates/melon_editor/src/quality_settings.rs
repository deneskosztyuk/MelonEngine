//! Project Settings > Quality: presets, per-setting dropdowns with their GPU
//! cost, and named profiles. Edits stay pending until Apply, which writes the
//! `RenderQuality` resource and `config/quality.ron`.

use std::path::{Path, PathBuf};

use bevy_ecs::world::World;
use egui::{Align, Layout, RichText, vec2};
use melon_core::quality::{self, GiMode, PresentPreference};
use melon_core::{QualityPreset, QualitySetting, RenderQuality, RenderTimings};

use crate::{StatusKind, theme};

type Status = Option<(StatusKind, String)>;

/// Where the active quality settings of a project are saved.
pub fn quality_path(project_root: &Path) -> PathBuf {
    project_root.join("config").join("quality.ron")
}

/// The folder of named quality profiles.
pub fn profiles_dir(project_root: &Path) -> PathBuf {
    project_root.join("config").join("quality_profiles")
}

const GROUPS: [(&str, &[QualitySetting]); 3] = [
    (
        "Lighting",
        &[
            QualitySetting::Gi,
            QualitySetting::Ao,
            QualitySetting::ShadowMap,
        ],
    ),
    ("Image", &[QualitySetting::Msaa, QualitySetting::Bloom]),
    ("Display", &[QualitySetting::Present]),
];

#[derive(Default)]
pub struct QualitySettings {
    /// Edited values not applied yet; `None` = showing the applied ones.
    pending: Option<RenderQuality>,
    /// `config/quality.ron` failed to load: the first save backs it up first.
    broken_on_load: bool,
    /// Profile picked in the dropdown.
    profile: Option<String>,
    /// Profile names on disk; `None` = read the folder again.
    profiles: Option<Vec<String>>,
    /// The Save as… name prompt is open with this text.
    save_as: Option<String>,
    /// The Delete confirmation for `profile` is showing.
    confirm_delete: bool,
}

impl QualitySettings {
    pub fn set_broken_on_load(&mut self, broken: bool) {
        self.broken_on_load = broken;
    }

    /// The values the panel shows: pending edits, else the applied ones.
    fn shown(&self, applied: RenderQuality) -> RenderQuality {
        self.pending.unwrap_or(applied)
    }

    /// A dropdown change or preset click; pending until `apply`.
    fn edit(&mut self, applied: RenderQuality, edited: RenderQuality) {
        self.pending = (edited != applied).then_some(edited);
    }

    fn revert(&mut self) {
        self.pending = None;
    }

    /// Write the pending values to the resource and `config/quality.ron`.
    fn apply(&mut self, world: &mut World, project_root: &Path) -> Status {
        let quality = self.pending.take()?;
        world.insert_resource(quality);
        let path = quality_path(project_root);
        match quality.save(&path, self.broken_on_load) {
            Ok(()) => {
                self.broken_on_load = false;
                Some((StatusKind::Success, "Quality settings applied".into()))
            }
            Err(error) => {
                tracing::error!("saving {} failed: {error:#}", path.display());
                Some((
                    StatusKind::Error,
                    format!("Saving quality settings failed: {error:#}"),
                ))
            }
        }
    }

    /// Fill the pending values from a profile (not applied).
    fn load_profile(&mut self, applied: RenderQuality, project_root: &Path, name: &str) -> Status {
        match quality::load_profile(&profiles_dir(project_root), name) {
            Ok(loaded) => {
                self.edit(applied, loaded);
                None
            }
            Err(error) => Some((StatusKind::Error, format!("{error:#}"))),
        }
    }

    fn save_profile_as(&mut self, project_root: &Path, name: &str, shown: RenderQuality) -> Status {
        if let Err(message) = quality::validate_profile_name(name) {
            return Some((StatusKind::Warning, message));
        }
        self.profiles = None;
        match quality::save_profile(&profiles_dir(project_root), name, &shown) {
            Ok(()) => {
                self.save_as = None;
                self.profile = Some(name.to_owned());
                Some((
                    StatusKind::Success,
                    format!("Saved quality profile {name:?}"),
                ))
            }
            Err(error) => Some((
                StatusKind::Error,
                format!("Saving profile {name:?} failed: {error:#}"),
            )),
        }
    }

    fn delete_profile(&mut self, project_root: &Path, name: &str) -> Status {
        self.confirm_delete = false;
        self.profiles = None;
        match quality::delete_profile(&profiles_dir(project_root), name) {
            Ok(()) => {
                self.profile = None;
                Some((
                    StatusKind::Success,
                    format!("Deleted quality profile {name:?}"),
                ))
            }
            Err(error) => Some((
                StatusKind::Error,
                format!("Deleting profile {name:?} failed: {error:#}"),
            )),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, world: &mut World, project_root: &Path) -> Status {
        let applied = world
            .get_resource::<RenderQuality>()
            .copied()
            .unwrap_or_default();
        let timings = world
            .get_resource::<RenderTimings>()
            .copied()
            .unwrap_or_default();
        let mut shown = self.shown(applied);
        let mut status = None;

        preset_strip(ui, &mut shown);
        ui.add_space(theme::sizes::SPACING);
        ui.horizontal(|ui| {
            ui.label(caption("GPU frame cost, last second"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(caption(&cost_caption(&timings)));
            });
        });
        cost_bar(ui, &shown, &timings);
        if timings.total.is_none() {
            ui.label(caption("GPU timing not available yet"));
        }
        for (group, settings) in GROUPS {
            ui.label(RichText::new(group).color(theme::colors::INFO));
            for (i, &setting) in settings.iter().enumerate() {
                setting_row(ui, i, setting, &mut shown, &timings);
            }
        }
        if shown != self.shown(applied) {
            self.edit(applied, shown);
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(caption("Profile"));
            let profiles = self
                .profiles
                .get_or_insert_with(|| quality::list_profiles(&profiles_dir(project_root)))
                .clone();
            egui::ComboBox::from_id_salt("quality_profile")
                .selected_text(self.profile.as_deref().unwrap_or("—"))
                .show_ui(ui, |ui| {
                    for name in profiles {
                        let selected = self.profile.as_deref() == Some(name.as_str());
                        if ui.selectable_label(selected, &name).clicked() {
                            self.profile = Some(name);
                            self.confirm_delete = false;
                        }
                    }
                });
            let picked = self.profile.clone();
            if ui
                .add_enabled(picked.is_some(), egui::Button::new("Load"))
                .clicked()
                && let Some(name) = &picked
            {
                status = self.load_profile(applied, project_root, name);
            }
            if ui.button("Save as…").clicked() {
                self.save_as = Some(self.profile.clone().unwrap_or_default());
            }
            if ui
                .add_enabled(picked.is_some(), egui::Button::new("Delete"))
                .clicked()
            {
                self.confirm_delete = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dirty = self.pending.is_some();
                let apply = egui::Button::new("Apply").fill(theme::colors::ACCENT);
                if ui.add_enabled(dirty, apply).clicked() {
                    status = self.apply(world, project_root);
                }
                if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                    self.revert();
                }
            });
        });
        if let Some(text) = &mut self.save_as {
            let mut save = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(text).desired_width(theme::sizes::SETTINGS_NAME));
                save = ui.button("Save").clicked();
                cancel = ui.button("Cancel").clicked();
                if let Err(message) = quality::validate_profile_name(text) {
                    ui.colored_label(theme::colors::WARNING, message);
                } else if self.profiles.as_ref().is_some_and(|p| p.contains(&*text)) {
                    ui.colored_label(theme::colors::WARNING, "Overwrites that profile");
                }
            });
            if save {
                let name = text.clone();
                status = self.save_profile_as(project_root, &name, self.shown(applied));
            } else if cancel {
                self.save_as = None;
            }
        }
        if self.confirm_delete
            && let Some(name) = self.profile.clone()
        {
            ui.horizontal(|ui| {
                ui.colored_label(
                    theme::colors::WARNING,
                    format!("Delete profile {name:?}? This cannot be undone."),
                );
                if ui.button("Delete").clicked() {
                    status = self.delete_profile(project_root, &name);
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_delete = false;
                }
            });
        }
        status
    }
}

fn caption(text: &str) -> RichText {
    RichText::new(text).small().color(theme::colors::TEXT_MUTED)
}

/// Low | Medium | High | Max | Custom. A click sets the preset's values;
/// Custom only lights up when nothing matches.
fn preset_strip(ui: &mut egui::Ui, shown: &mut RenderQuality) {
    let active = shown.matching_preset();
    ui.horizontal(|ui| {
        ui.label(caption("Preset"));
        ui.spacing_mut().item_spacing.x = 0.0;
        for preset in QualityPreset::ALL {
            let on = active == Some(preset);
            let mut button = egui::Button::new(format!("{preset:?}"));
            if on {
                button = button.fill(theme::colors::ACCENT);
            }
            if ui.add(button).clicked() {
                *shown = RenderQuality::preset(preset);
            }
        }
        let custom = RichText::new("Custom");
        let mut button = egui::Button::new(match active {
            None => custom.color(theme::colors::WARNING),
            Some(_) => custom,
        })
        .sense(egui::Sense::hover());
        if active.is_none() {
            button = button.fill(theme::colors::ACCENT);
        }
        ui.add(button);
    });
}

fn cost_bar(ui: &mut egui::Ui, shown: &RenderQuality, timings: &RenderTimings) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        vec2(width, theme::sizes::QUALITY_BAR_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let radius = theme::sizes::RADIUS;
    painter.rect_filled(rect, radius, theme::colors::BORDER);
    let Some(scale) = timings.budget_ms.or(timings.total).filter(|&ms| ms > 0.0) else {
        return;
    };
    let mut x = rect.left();
    for setting in QualitySetting::ALL {
        let (Some(ms), Some(color)) = (cost(timings, setting), pass_color(setting)) else {
            continue;
        };
        if !enabled(shown, setting) {
            continue;
        }
        let w = (ms / scale * width).min(rect.right() - x);
        let segment = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, rect.height()));
        painter.rect_filled(segment, 0, color);
        x += w;
    }
}

fn setting_row(
    ui: &mut egui::Ui,
    i: usize,
    setting: QualitySetting,
    shown: &mut RenderQuality,
    timings: &RenderTimings,
) {
    egui::Frame::new()
        .fill(theme::colors::QUALITY_ROWS[i % 2])
        .corner_radius(theme::sizes::RADIUS)
        .inner_margin(vec2(theme::sizes::SPACING, 2.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let dot = theme::sizes::QUALITY_DOT;
                let (rect, _) = ui.allocate_exact_size(vec2(dot, dot), egui::Sense::hover());
                if let Some(color) = pass_color(setting) {
                    ui.painter().rect_filled(rect, 2, color);
                }
                ui.label(name(setting));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut index = selected(shown, setting);
                    egui::ComboBox::from_id_salt(("quality_setting", setting_id(setting)))
                        .width(theme::sizes::QUALITY_DROPDOWN)
                        .selected_text(options(setting)[index])
                        .show_ui(ui, |ui| {
                            for (j, label) in options(setting).iter().enumerate() {
                                ui.selectable_value(&mut index, j, *label);
                            }
                        });
                    select(shown, setting, index);
                    let (tag, color) = if setting.needs_rebuild() {
                        ("rebuild", theme::colors::WARNING)
                    } else {
                        ("live", theme::colors::SUCCESS)
                    };
                    fixed_right(
                        ui,
                        theme::sizes::QUALITY_TAG,
                        RichText::new(tag).small().color(color),
                    );
                    fixed_right(
                        ui,
                        theme::sizes::QUALITY_COST,
                        RichText::new(cost_text(shown, timings, setting))
                            .small()
                            .monospace()
                            .color(theme::colors::TEXT_MUTED),
                    );
                });
            });
        });
}

/// A label right-aligned in a fixed-width column.
fn fixed_right(ui: &mut egui::Ui, width: f32, text: RichText) {
    ui.allocate_ui_with_layout(
        vec2(width, theme::sizes::INPUT_HEIGHT),
        Layout::right_to_left(Align::Center),
        |ui| ui.label(text),
    );
}

fn setting_id(setting: QualitySetting) -> usize {
    QualitySetting::ALL
        .iter()
        .position(|&s| s == setting)
        .unwrap_or_default()
}

fn name(setting: QualitySetting) -> &'static str {
    match setting {
        QualitySetting::Gi => "Global illumination",
        QualitySetting::Ao => "Ambient occlusion",
        QualitySetting::ShadowMap => "Shadow map",
        QualitySetting::Msaa => "Anti-aliasing",
        QualitySetting::Bloom => "Bloom",
        QualitySetting::Present => "Frame pacing",
    }
}

fn options(setting: QualitySetting) -> &'static [&'static str] {
    match setting {
        QualitySetting::Gi => &["Off", "SSGI", "DDGI"],
        QualitySetting::Ao | QualitySetting::Bloom => &["Off", "On"],
        QualitySetting::ShadowMap => &["1024", "2048", "4096"],
        QualitySetting::Msaa => &["Off", "MSAA 2x", "MSAA 4x"],
        QualitySetting::Present => &["Vsync", "Mailbox + cap"],
    }
}

const GI_MODES: [GiMode; 3] = [GiMode::Off, GiMode::Ssgi, GiMode::Ddgi];
const PRESENTS: [PresentPreference; 2] =
    [PresentPreference::Vsync, PresentPreference::MailboxCapped];

/// Index of the current value in `options(setting)`.
fn selected(q: &RenderQuality, setting: QualitySetting) -> usize {
    let pos = |found: Option<usize>| found.unwrap_or_default();
    match setting {
        QualitySetting::Gi => pos(GI_MODES.iter().position(|&m| m == q.gi_mode)),
        QualitySetting::Ao => q.ao as usize,
        QualitySetting::ShadowMap => pos(quality::SHADOW_MAP_SIZES
            .iter()
            .position(|&s| s == q.shadow_map_size)),
        QualitySetting::Msaa => pos(quality::MSAA_SAMPLES.iter().position(|&s| s == q.msaa)),
        QualitySetting::Bloom => q.bloom as usize,
        QualitySetting::Present => pos(PRESENTS.iter().position(|&p| p == q.present)),
    }
}

fn select(q: &mut RenderQuality, setting: QualitySetting, index: usize) {
    match setting {
        QualitySetting::Gi => q.gi_mode = GI_MODES[index],
        QualitySetting::Ao => q.ao = index == 1,
        QualitySetting::ShadowMap => q.shadow_map_size = quality::SHADOW_MAP_SIZES[index],
        QualitySetting::Msaa => q.msaa = quality::MSAA_SAMPLES[index],
        QualitySetting::Bloom => q.bloom = index == 1,
        QualitySetting::Present => q.present = PRESENTS[index],
    }
}

/// False when the setting turns its pass off (shown as "off", no bar segment).
fn enabled(q: &RenderQuality, setting: QualitySetting) -> bool {
    match setting {
        QualitySetting::Gi => q.gi_mode != GiMode::Off,
        QualitySetting::Ao => q.ao,
        QualitySetting::Bloom => q.bloom,
        _ => true,
    }
}

fn cost(t: &RenderTimings, setting: QualitySetting) -> Option<f32> {
    match setting {
        QualitySetting::Gi => t.gi,
        QualitySetting::Ao => t.ao,
        QualitySetting::ShadowMap => t.shadows,
        QualitySetting::Msaa => t.aa,
        QualitySetting::Bloom => t.bloom,
        QualitySetting::Present => None,
    }
}

fn pass_color(setting: QualitySetting) -> Option<egui::Color32> {
    use theme::colors::*;
    match setting {
        QualitySetting::Gi => Some(PASS_GI),
        QualitySetting::Ao => Some(PASS_AO),
        QualitySetting::ShadowMap => Some(PASS_SHADOWS),
        QualitySetting::Msaa => Some(PASS_AA),
        QualitySetting::Bloom => Some(PASS_BLOOM),
        QualitySetting::Present => None,
    }
}

/// "0.9 ms", "off", "—" (not measured), or nothing for Frame pacing.
fn cost_text(q: &RenderQuality, t: &RenderTimings, setting: QualitySetting) -> String {
    if setting == QualitySetting::Present {
        String::new()
    } else if !enabled(q, setting) {
        "off".into()
    } else {
        cost(t, setting).map_or("—".into(), |ms| format!("{ms:.1} ms"))
    }
}

/// "4.6 ms of 8.3 ms budget (120 Hz)"; "—" for what is not measured.
fn cost_caption(t: &RenderTimings) -> String {
    let total = t.total.map_or("—".into(), |ms| format!("{ms:.1} ms"));
    match t.budget_ms.filter(|&ms| ms > 0.0) {
        Some(budget) => format!(
            "{total} of {budget:.1} ms budget ({:.0} Hz)",
            1000.0 / budget
        ),
        None => format!("{total} of — budget"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("melon_quality_ui_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn world() -> World {
        let mut world = World::new();
        world.insert_resource(RenderQuality::default());
        world
    }

    fn applied(world: &World) -> RenderQuality {
        *world.resource::<RenderQuality>()
    }

    #[test]
    fn edit_then_revert_gives_original() {
        let world = world();
        let mut panel = QualitySettings::default();
        let mut edited = applied(&world);
        edited.bloom = false;
        panel.edit(applied(&world), edited);
        assert_eq!(panel.shown(applied(&world)), edited);
        panel.revert();
        assert_eq!(panel.shown(applied(&world)), RenderQuality::default());
        assert_eq!(applied(&world), RenderQuality::default());
    }

    #[test]
    fn edit_then_apply_updates_resource_and_file() {
        let root = temp_root("apply");
        let mut world = world();
        let mut panel = QualitySettings::default();
        let mut edited = applied(&world);
        edited.msaa = 2;
        panel.edit(applied(&world), edited);
        assert_eq!(applied(&world), RenderQuality::default(), "pending only");
        assert_eq!(
            panel.apply(&mut world, &root).map(|s| s.0),
            Some(StatusKind::Success)
        );
        assert_eq!(applied(&world), edited);
        assert_eq!(panel.pending, None);
        let (saved, _) = RenderQuality::load(&quality_path(&root));
        assert_eq!(saved, edited);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_apply_after_broken_load_backs_up() {
        let root = temp_root("broken");
        let path = quality_path(&root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not ron (").unwrap();
        let mut world = world();
        let mut panel = QualitySettings::default();
        panel.set_broken_on_load(true);
        panel.edit(applied(&world), RenderQuality::preset(QualityPreset::Low));
        panel.apply(&mut world, &root);
        let backup = root.join("config").join("quality.ron.broken");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "not ron (");
        std::fs::write(&backup, "kept").unwrap();
        panel.edit(applied(&world), RenderQuality::default());
        panel.apply(&mut world, &root);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "kept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preset_click_is_pending_until_apply() {
        let root = temp_root("preset");
        let mut world = world();
        let mut panel = QualitySettings::default();
        let low = RenderQuality::preset(QualityPreset::Low);
        panel.edit(applied(&world), low);
        assert_eq!(
            panel.shown(applied(&world)).matching_preset(),
            Some(QualityPreset::Low)
        );
        assert_eq!(applied(&world), RenderQuality::default());
        assert!(!quality_path(&root).exists());
        panel.apply(&mut world, &root);
        assert_eq!(applied(&world), low);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loading_profile_sets_pending_not_applied() {
        let root = temp_root("profile");
        let world = world();
        let low = RenderQuality::preset(QualityPreset::Low);
        quality::save_profile(&profiles_dir(&root), "laptop", &low).unwrap();
        let mut panel = QualitySettings::default();
        assert_eq!(panel.load_profile(applied(&world), &root, "laptop"), None);
        assert_eq!(panel.shown(applied(&world)), low);
        assert_eq!(applied(&world), RenderQuality::default());
        let status = panel.load_profile(applied(&world), &root, "missing");
        assert_eq!(status.map(|s| s.0), Some(StatusKind::Error));
        assert_eq!(
            panel.shown(applied(&world)),
            low,
            "a failed load keeps pending"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn options_round_trip_every_setting() {
        for preset in QualityPreset::ALL {
            let q = RenderQuality::preset(preset);
            for setting in QualitySetting::ALL {
                let mut copy = RenderQuality::preset(QualityPreset::Max);
                select(&mut copy, setting, selected(&q, setting));
                assert_eq!(selected(&copy, setting), selected(&q, setting));
                assert!(selected(&q, setting) < options(setting).len());
            }
        }
    }

    #[test]
    fn cost_texts_without_timings() {
        let t = RenderTimings::default();
        let low = RenderQuality::preset(QualityPreset::Low);
        assert_eq!(cost_text(&low, &t, QualitySetting::Gi), "off");
        assert_eq!(cost_text(&low, &t, QualitySetting::ShadowMap), "—");
        assert_eq!(cost_text(&low, &t, QualitySetting::Present), "");
        assert_eq!(cost_caption(&t), "— of — budget");
        let t = RenderTimings {
            total: Some(4.6),
            budget_ms: Some(1000.0 / 120.0),
            ..Default::default()
        };
        assert_eq!(cost_caption(&t), "4.6 ms of 8.3 ms budget (120 Hz)");
    }

    /// The panel draws without panicking, with and without pending edits.
    #[test]
    fn panel_draws() {
        let root = temp_root("draw");
        let mut world = world();
        let mut panel = QualitySettings::default();
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                panel.ui(ui, &mut world, &root);
            });
            panel.edit(applied(&world), RenderQuality::preset(QualityPreset::Low));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
