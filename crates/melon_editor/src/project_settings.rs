//! Edit > Project Settings: a window with project-wide settings, saved under
//! the project root: the input mapping (`config/input.ron`) and render
//! quality (`config/quality.ron`, see `quality_settings`) and audio buses
//! (`config/audio.ron`, see `audio_settings`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bevy_ecs::world::World;
use melon_core::{Action, ActionKind, AxisBinding, InputButton, InputMap, KeyCode, RawPresses};

use crate::audio_settings::AudioSettingsPanel;
use crate::physics_settings::PhysicsSettingsPanel;
use crate::quality_settings::QualitySettings;
use crate::{StatusKind, theme};

/// Where the input mapping of a project is saved.
pub fn mapping_path(project_root: &Path) -> PathBuf {
    project_root.join("config").join("input.ron")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    InputMapping,
    Quality,
    Audio,
    Physics,
}

/// The binding the next raw press goes to (index into `InputMap::actions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    Button(usize),
    /// First half of an axis pair.
    AxisPositive(usize),
    /// Second half: `positive` was captured already.
    AxisNegative(usize, InputButton),
}

#[derive(Default)]
pub struct ProjectSettings {
    pub open: bool,
    section: Section,
    capture: Option<CaptureTarget>,
    /// The mapping file failed to load: the first save backs it up first.
    broken_on_load: bool,
    /// Action name being typed (index, text); applied when the field loses focus.
    name_edit: Option<(usize, String)>,
    quality: QualitySettings,
    audio: AudioSettingsPanel,
    physics: PhysicsSettingsPanel,
}

impl ProjectSettings {
    pub fn set_broken_on_load(&mut self, broken: bool) {
        self.broken_on_load = broken;
    }

    /// `config/quality.ron` failed to load: the first Apply backs it up.
    pub fn set_quality_broken(&mut self, broken: bool) {
        self.quality.set_broken_on_load(broken);
    }

    /// `config/physics.ron` failed to load: the first Apply backs it up.
    pub fn set_physics_broken(&mut self, broken: bool) {
        self.physics.set_broken_on_load(broken);
    }

    /// `config/audio.ron` failed to load: the first Apply backs it up.
    pub fn set_audio_broken(&mut self, broken: bool) {
        self.audio.set_broken_on_load(broken);
    }

    /// A rebinding capture owns this frame's presses: editor shortcuts stay off.
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// Draw the window (when open). Returns a status message for the status bar.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        world: &mut World,
        project_root: &Path,
    ) -> Option<(StatusKind, String)> {
        if !self.open {
            self.capture = None;
            return None;
        }
        let current = world.get_resource::<InputMap>()?.clone();
        let mut map = current.clone();
        let press = world
            .get_resource::<RawPresses>()
            .and_then(|raw| raw.0.first().copied());
        if let (Some(target), Some(press)) = (self.capture, press) {
            self.capture = capture(&mut map, target, press);
        }

        let mut open = true;
        let mut panel_status = None;
        egui::Window::new("Project Settings")
            .open(&mut open)
            .collapsible(false)
            .default_size(theme::sizes::SETTINGS_WINDOW)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(theme::sizes::SETTINGS_SECTIONS);
                        ui.selectable_value(
                            &mut self.section,
                            Section::InputMapping,
                            "Input Mapping",
                        );
                        ui.selectable_value(&mut self.section, Section::Quality, "Quality");
                        ui.selectable_value(&mut self.section, Section::Audio, "Audio");
                        ui.selectable_value(&mut self.section, Section::Physics, "Physics");
                    });
                    ui.separator();
                    ui.vertical(|ui| match self.section {
                        Section::InputMapping => self.input_mapping(ui, &mut map),
                        Section::Quality => {
                            panel_status = self.quality.ui(ui, world, project_root);
                        }
                        Section::Audio => {
                            panel_status = self.audio.ui(ui, world, project_root);
                        }
                        Section::Physics => {
                            panel_status = self.physics.ui(ui, world, project_root);
                        }
                    });
                });
            });
        if !open {
            self.open = false;
            self.capture = None;
        }
        if map == current {
            return panel_status;
        }
        let status = self.commit(world, project_root, map);
        if status.is_some() {
            // A refused edit may have added or removed the action the capture points at.
            self.capture = None;
        }
        status
    }

    fn input_mapping(&mut self, ui: &mut egui::Ui, map: &mut InputMap) {
        let mut remove_action = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, action) in map.actions.iter_mut().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    self.name_field(ui, i, action);
                    egui::ComboBox::from_id_salt(("action_kind", i))
                        .selected_text(kind_label(action.kind))
                        .show_ui(ui, |ui| {
                            for kind in [ActionKind::Button, ActionKind::Axis] {
                                ui.selectable_value(&mut action.kind, kind, kind_label(kind));
                            }
                        });
                    self.bindings(ui, i, action);
                    if ui.button("Remove").clicked() {
                        remove_action = Some(i);
                    }
                });
                ui.separator();
            }
            if ui.button("+ Action").clicked() {
                map.actions.push(Action {
                    name: new_action_name(map),
                    kind: ActionKind::Button,
                    buttons: Vec::new(),
                    axes: Vec::new(),
                });
            }
        });
        if let Some(i) = remove_action {
            map.actions.remove(i);
            self.capture = None;
            self.name_edit = None;
        }
    }

    /// The name is typed into a buffer and applied when the field loses focus,
    /// so half-typed names are not validated and saved per keystroke.
    fn name_field(&mut self, ui: &mut egui::Ui, i: usize, action: &mut Action) {
        let mut text = match &self.name_edit {
            Some((j, text)) if *j == i => text.clone(),
            _ => action.name.clone(),
        };
        let response = ui
            .add(egui::TextEdit::singleline(&mut text).desired_width(theme::sizes::SETTINGS_NAME));
        if response.changed() {
            self.name_edit = Some((i, text));
        }
        if response.lost_focus()
            && let Some((_, text)) = self.name_edit.take_if(|(j, _)| *j == i)
        {
            action.name = text;
        }
    }

    fn bindings(&mut self, ui: &mut egui::Ui, i: usize, action: &mut Action) {
        let target = self.capture;
        match action.kind {
            ActionKind::Button => {
                let mut remove = None;
                for (j, &button) in action.buttons.iter().enumerate() {
                    if chip(ui, &button_label(button)) {
                        remove = Some(j);
                    }
                }
                if let Some(j) = remove {
                    action.buttons.remove(j);
                }
                if target == Some(CaptureTarget::Button(i)) {
                    ui.colored_label(theme::colors::WARNING, "Press a key… Esc cancels");
                } else if ui.button("+ binding").clicked() {
                    self.capture = Some(CaptureTarget::Button(i));
                }
            }
            ActionKind::Axis => {
                let mut remove = None;
                for (j, pair) in action.axes.iter().enumerate() {
                    let label = format!(
                        "+{} / −{}",
                        button_label(pair.positive),
                        button_label(pair.negative)
                    );
                    if chip(ui, &label) {
                        remove = Some(j);
                    }
                }
                if let Some(j) = remove {
                    action.axes.remove(j);
                }
                match target {
                    Some(CaptureTarget::AxisPositive(j)) if j == i => {
                        ui.colored_label(theme::colors::WARNING, "Press positive… Esc cancels");
                    }
                    Some(CaptureTarget::AxisNegative(j, _)) if j == i => {
                        ui.colored_label(theme::colors::WARNING, "Press negative… Esc cancels");
                    }
                    _ => {
                        if ui.button("+ pair").clicked() {
                            self.capture = Some(CaptureTarget::AxisPositive(i));
                        }
                    }
                }
            }
        }
    }

    /// Apply an edited mapping: refuse empty or duplicate names, otherwise
    /// update the resource and save the file.
    fn commit(
        &mut self,
        world: &mut World,
        project_root: &Path,
        map: InputMap,
    ) -> Option<(StatusKind, String)> {
        let mut seen = HashSet::new();
        for action in &map.actions {
            if action.name.trim().is_empty() {
                return Some((StatusKind::Warning, "Action names cannot be empty".into()));
            }
            if !seen.insert(action.name.as_str()) {
                return Some((
                    StatusKind::Warning,
                    format!("An action named {:?} already exists", action.name),
                ));
            }
        }
        let path = mapping_path(project_root);
        let result = map.save(&path, self.broken_on_load);
        world.insert_resource(map);
        match result {
            Ok(()) => {
                self.broken_on_load = false;
                None
            }
            Err(error) => {
                tracing::error!("saving {} failed: {error:#}", path.display());
                Some((
                    StatusKind::Error,
                    format!("Saving input mapping failed: {error:#}"),
                ))
            }
        }
    }
}

/// Feed one raw press to the capture; returns what is still being captured.
fn capture(map: &mut InputMap, target: CaptureTarget, press: InputButton) -> Option<CaptureTarget> {
    if press == InputButton::Key(KeyCode::Escape) {
        return None;
    }
    let (CaptureTarget::Button(i)
    | CaptureTarget::AxisPositive(i)
    | CaptureTarget::AxisNegative(i, _)) = target;
    let action = map.actions.get_mut(i)?;
    match target {
        CaptureTarget::Button(_) => {
            if !action.buttons.contains(&press) {
                action.buttons.push(press);
            }
            None
        }
        CaptureTarget::AxisPositive(_) => Some(CaptureTarget::AxisNegative(i, press)),
        CaptureTarget::AxisNegative(_, positive) => {
            let pair = AxisBinding {
                positive,
                negative: press,
            };
            if !action.axes.contains(&pair) {
                action.axes.push(pair);
            }
            None
        }
    }
}

/// A binding chip; true when its × was clicked.
fn chip(ui: &mut egui::Ui, label: &str) -> bool {
    egui::Frame::new()
        .fill(theme::colors::SURFACE_RAISED)
        .corner_radius(theme::sizes::RADIUS)
        .inner_margin(egui::vec2(theme::sizes::SPACING, 1.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.small_button("×")
                    .on_hover_text("Remove binding")
                    .clicked()
            })
            .inner
        })
        .inner
}

fn kind_label(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Button => "Button",
        ActionKind::Axis => "Axis",
    }
}

fn button_label(button: InputButton) -> String {
    match button {
        InputButton::Key(key) => format!("{key:?}"),
        InputButton::Mouse(mouse) => format!("Mouse {mouse:?}"),
    }
}

/// "NewAction", or "NewAction2", "NewAction3"… when taken.
fn new_action_name(map: &InputMap) -> String {
    let taken = |name: &str| map.actions.iter().any(|a| a.name == name);
    (1..)
        .map(|n| match n {
            1 => "NewAction".to_string(),
            n => format!("NewAction{n}"),
        })
        .find(|name| !taken(name))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::Transform;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("melon_settings_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn world_with(presses: Vec<InputButton>) -> World {
        let mut world = World::new();
        world.insert_resource(InputMap::default());
        world.insert_resource(RawPresses(presses));
        world
    }

    fn jump(world: &World) -> &Action {
        let map = world.resource::<InputMap>();
        map.actions.iter().find(|a| a.name == "Jump").unwrap()
    }

    fn jump_index(world: &World) -> usize {
        let map = world.resource::<InputMap>();
        map.actions.iter().position(|a| a.name == "Jump").unwrap()
    }

    fn frame(settings: &mut ProjectSettings, world: &mut World, root: &Path) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            settings.ui(ui.ctx(), world, root);
        });
    }

    #[test]
    fn capture_assigns_next_raw_press() {
        let root = temp_root("capture");
        let mut world = world_with(vec![InputButton::Key(KeyCode::K)]);
        let mut settings = ProjectSettings {
            open: true,
            capture: Some(CaptureTarget::Button(jump_index(&world))),
            ..Default::default()
        };
        frame(&mut settings, &mut world, &root);
        assert!(jump(&world).buttons.contains(&InputButton::Key(KeyCode::K)));
        assert_eq!(settings.capture, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn capture_axis_pair_takes_positive_then_negative() {
        let root = temp_root("pair");
        let mut world = world_with(vec![InputButton::Key(KeyCode::ArrowUp)]);
        let mut settings = ProjectSettings {
            open: true,
            capture: Some(CaptureTarget::AxisPositive(0)),
            ..Default::default()
        };
        frame(&mut settings, &mut world, &root);
        world.insert_resource(RawPresses(vec![InputButton::Key(KeyCode::ArrowDown)]));
        frame(&mut settings, &mut world, &root);
        let axes = &world.resource::<InputMap>().actions[0].axes;
        assert_eq!(
            axes.last(),
            Some(&AxisBinding {
                positive: InputButton::Key(KeyCode::ArrowUp),
                negative: InputButton::Key(KeyCode::ArrowDown),
            })
        );
        assert_eq!(settings.capture, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn capture_escape_cancels() {
        let root = temp_root("escape");
        let mut world = world_with(vec![InputButton::Key(KeyCode::Escape)]);
        let mut settings = ProjectSettings {
            open: true,
            capture: Some(CaptureTarget::Button(jump_index(&world))),
            ..Default::default()
        };
        frame(&mut settings, &mut world, &root);
        assert_eq!(settings.capture, None);
        assert_eq!(*world.resource::<InputMap>(), InputMap::default());
        assert!(
            !mapping_path(&root).exists(),
            "nothing changed, nothing saved"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One full editor frame with `key` pressed in egui and in `RawPresses`.
    fn editor_key_frame(editor: &mut crate::Editor, world: &mut World, key: egui::Key) {
        editor_key_frame_with(editor, world, key, egui::Modifiers::NONE);
    }

    fn editor_key_frame_with(
        editor: &mut crate::Editor,
        world: &mut World,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        let ctx = egui::Context::default();
        editor_frame(
            &ctx,
            editor,
            world,
            key_event(key, true, modifiers),
            modifiers,
        );
    }

    fn key_event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// One full editor frame on `ctx` (which remembers held keys) with `event`.
    fn editor_frame(
        ctx: &egui::Context,
        editor: &mut crate::Editor,
        world: &mut World,
        event: egui::Event,
        modifiers: egui::Modifiers,
    ) {
        let input = egui::RawInput {
            events: vec![event],
            modifiers,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| editor.build_ui(world, ui));
    }

    #[test]
    fn captured_key_does_not_trigger_shortcut() {
        let root = temp_root("shortcut");
        let mut world = world_with(vec![InputButton::Key(KeyCode::Delete)]);
        let entity = world.spawn(Transform::default()).id();
        let mut editor = crate::Editor::new();
        editor.project_root = root.clone();
        editor.set_selected(Some(entity));
        editor.project_settings.open = true;
        editor.project_settings.capture = Some(CaptureTarget::Button(jump_index(&world)));
        editor_key_frame(&mut editor, &mut world, egui::Key::Delete);
        assert!(world.get::<melon_core::PendingDelete>(entity).is_none());
        assert!(
            jump(&world)
                .buttons
                .contains(&InputButton::Key(KeyCode::Delete))
        );

        // Esc cancels the capture and keeps game focus (no ReleaseGame).
        world.insert_resource(melon_core::SimulationState::Playing);
        world.insert_resource(melon_core::InputFocus::Game);
        world.insert_resource(RawPresses(vec![InputButton::Key(KeyCode::Escape)]));
        editor.project_settings.capture = Some(CaptureTarget::Button(jump_index(&world)));
        editor_key_frame(&mut editor, &mut world, egui::Key::Escape);
        assert!(!editor.project_settings.capturing());
        assert_eq!(
            *world.resource::<melon_core::InputFocus>(),
            melon_core::InputFocus::Game
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn captured_play_keys_do_not_play_or_stop() {
        use melon_core::{InputFocus, SimulationState};
        let root = temp_root("play_keys");
        let mut world = world_with(vec![InputButton::Key(KeyCode::F5)]);
        world.insert_resource(SimulationState::Paused);
        world.insert_resource(InputFocus::Editor);
        let mut editor = crate::Editor::new();
        editor.project_root = root.clone();
        editor.project_settings.open = true;
        editor.project_settings.capture = Some(CaptureTarget::Button(jump_index(&world)));
        editor_key_frame(&mut editor, &mut world, egui::Key::F5);
        assert_eq!(
            *world.resource::<SimulationState>(),
            SimulationState::Paused
        );
        assert!(
            jump(&world)
                .buttons
                .contains(&InputButton::Key(KeyCode::F5))
        );

        // Shift+F5 while playing: the session keeps running with game focus.
        world.insert_resource(SimulationState::Playing);
        world.insert_resource(InputFocus::Game);
        editor.project_settings.capture = Some(CaptureTarget::Button(jump_index(&world)));
        editor_key_frame_with(
            &mut editor,
            &mut world,
            egui::Key::F5,
            egui::Modifiers::SHIFT,
        );
        assert_eq!(
            *world.resource::<SimulationState>(),
            SimulationState::Playing
        );
        assert_eq!(*world.resource::<InputFocus>(), InputFocus::Game);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn held_captured_key_does_not_trigger_on_repeat() {
        use melon_core::{InputFocus, PendingDelete, SimulationState};
        let root = temp_root("repeat");
        let mut world = world_with(vec![InputButton::Key(KeyCode::Delete)]);
        world.insert_resource(SimulationState::Paused);
        world.insert_resource(InputFocus::Editor);
        let entity = world.spawn(Transform::default()).id();
        let mut editor = crate::Editor::new();
        editor.project_root = root.clone();
        editor.set_selected(Some(entity));
        editor.project_settings.open = true;
        editor.project_settings.capture = Some(CaptureTarget::Button(jump_index(&world)));
        // One context across frames, so egui knows the keys stay held.
        let ctx = egui::Context::default();
        let none = egui::Modifiers::NONE;
        let press = |key| key_event(key, true, none);
        editor_frame(
            &ctx,
            &mut editor,
            &mut world,
            press(egui::Key::Delete),
            none,
        );
        assert!(!editor.project_settings.capturing());

        // Key repeat of the still-held Delete triggers nothing.
        editor_frame(
            &ctx,
            &mut editor,
            &mut world,
            press(egui::Key::Delete),
            none,
        );
        assert!(world.get::<PendingDelete>(entity).is_none());

        // Same for a held, just-captured F5: no Play.
        world.insert_resource(RawPresses(vec![InputButton::Key(KeyCode::F5)]));
        editor.project_settings.capture = Some(CaptureTarget::Button(jump_index(&world)));
        editor_frame(&ctx, &mut editor, &mut world, press(egui::Key::F5), none);
        editor_frame(&ctx, &mut editor, &mut world, press(egui::Key::F5), none);
        assert_eq!(
            *world.resource::<SimulationState>(),
            SimulationState::Paused
        );

        // After a release, a fresh press deletes again.
        let release = key_event(egui::Key::Delete, false, none);
        editor_frame(&ctx, &mut editor, &mut world, release, none);
        editor_frame(
            &ctx,
            &mut editor,
            &mut world,
            press(egui::Key::Delete),
            none,
        );
        assert!(world.get::<PendingDelete>(entity).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_to_duplicate_is_refused() {
        let root = temp_root("rename");
        let mut world = world_with(Vec::new());
        let mut settings = ProjectSettings::default();
        for bad in ["Jump", "", "  "] {
            let mut map = InputMap::default();
            map.actions[3].name = bad.into(); // Fire
            let status = settings.commit(&mut world, &root, map);
            assert_eq!(status.map(|s| s.0), Some(StatusKind::Warning), "{bad:?}");
            assert_eq!(*world.resource::<InputMap>(), InputMap::default());
        }
        assert!(!mapping_path(&root).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_saves_file() {
        let root = temp_root("save");
        let mut world = world_with(Vec::new());
        let mut settings = ProjectSettings::default();
        let mut map = InputMap::default();
        map.actions.push(Action {
            name: "Crouch".into(),
            kind: ActionKind::Button,
            buttons: vec![InputButton::Key(KeyCode::C)],
            axes: Vec::new(),
        });
        assert_eq!(settings.commit(&mut world, &root, map.clone()), None);
        assert_eq!(*world.resource::<InputMap>(), map);
        let (saved, outcome) = InputMap::load(&mapping_path(&root));
        assert_eq!(outcome, melon_core::LoadOutcome::Loaded);
        assert!(saved.actions.iter().any(|a| a.name == "Crouch"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_save_after_broken_load_backs_up() {
        let root = temp_root("broken");
        let path = mapping_path(&root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not ron (").unwrap();
        let backup = root.join("config").join("input.ron.broken");
        let mut world = world_with(Vec::new());
        let mut settings = ProjectSettings::default();
        settings.set_broken_on_load(true);
        assert_eq!(
            settings.commit(&mut world, &root, InputMap::default()),
            None
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "not ron (");
        // Only the first save backs up: a later one leaves the backup alone.
        std::fs::write(&backup, "kept").unwrap();
        settings.commit(&mut world, &root, InputMap::default());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "kept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_action_name_is_unique() {
        let mut map = InputMap::default();
        map.actions.push(Action {
            name: "NewAction".into(),
            kind: ActionKind::Button,
            buttons: Vec::new(),
            axes: Vec::new(),
        });
        assert_eq!(new_action_name(&map), "NewAction2");
    }
}
