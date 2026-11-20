use std::sync::Arc;

use bevy_ecs::entity::Entity;
use bevy_ecs::reflect::{AppTypeRegistry, ReflectComponent};
use bevy_ecs::world::World;
use egui::ViewportId;
use egui_wgpu::ScreenDescriptor;
use melon_core::bevy_reflect::{TypePath, TypeRegistry, std_traits::ReflectDefault};
use melon_core::glam;
use melon_core::hierarchy::{
    depth_first, set_parent_keep_world, set_world_transform, world_matrix,
};
use melon_core::{
    Aabb, AssetServer, Camera, GameInputGate, Handle, InputFocus, Model, Name, SimulationState,
    Transform,
};
use melon_render::LineVertex;
use tracing::instrument;
use winit::event::WindowEvent;
use winit::window::Window;

pub mod asset_db;
pub mod asset_ops;
pub mod asset_watch;
pub mod audio_settings;
pub mod build_window;
pub mod console;
pub mod content;
mod content_view;
mod drawer;
pub mod gizmo;
pub mod history;
pub mod inspect;
pub mod layout;
mod material_assign;
mod material_card;
pub mod physics_settings;
pub mod play;
pub mod player_settings;
pub mod project_settings;
pub mod quality_settings;
pub mod scene;
pub mod theme;
pub mod thumbnails;
mod toolbar_icon;
use toolbar_icon::ToolbarIcon;
pub mod viewport;

use gizmo::{
    AXIS_X, AXIS_Y, AXIS_Z, GIZMO_SIZE_PX, GizmoHandle, GizmoPose, PICK_TOLERANCE_PX,
    compute_drag_plane, drag_move_axis, drag_move_center, drag_move_plane, drag_rotate, drag_scale,
    gizmo_draws, pick_handle, project_onto_axis, ray_plane_intersect, rotate_angle,
    screen_drag_angle, uniform_scale_factor, wrap_angle_delta,
};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum GizmoMode {
    #[default]
    Move,
    Rotate,
    Scale,
}

#[derive(Debug, Clone, Copy)]
struct GizmoDrag {
    handle: GizmoHandle,
    mode: GizmoMode,
    /// World pose at the press.
    start: Transform,
    /// Plane the pointer's hits are measured on (through the gizmo centre).
    plane_normal: glam::Vec3,
    press_hit: glam::Vec3,
    press_pointer: glam::Vec2,
    /// Rotate: angle at the press (`None` = ring edge-on, screen drag).
    press_angle: Option<f32>,
    last_angle: Option<f32>,
    angle_sum: f32,
    ring_tangent_px: glam::Vec2,
}

/// Pending scene operation for the host to execute.
/// Mesh count and bounds read from a glTF file for the Asset card.
type ModelInfo = (usize, Option<(glam::Vec3, glam::Vec3)>);

/// The answer to "Open <scene>?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenChoice {
    Save,
    DontSave,
    Cancel,
}

/// What the delete dialog shows.
struct DeleteConfirm {
    path: std::path::PathBuf,
    /// Project-relative scene files (and "the open scene") that use it.
    used_by: Vec<String>,
}

pub enum PendingOperation {
    Save(std::path::PathBuf),
    Load(std::path::PathBuf),
    /// File > Exit: the host saves the layout and closes the window.
    Exit,
}

enum DialogChannel {
    Save(std::sync::mpsc::Receiver<Option<std::path::PathBuf>>),
    Load(std::sync::mpsc::Receiver<Option<std::path::PathBuf>>),
}

/// Shared artwork for native windows and the editor chrome.
pub fn engine_icon() -> image::RgbaImage {
    image::load_from_memory(include_bytes!("../../../assets/branding/melon-engine.png"))
        .expect("embedded MelonEngine PNG")
        .into_rgba8()
}

/// Editor holds the UI state (selection, gizmo mode, etc.) and builds the egui UI.
pub struct Editor {
    selected: Option<Entity>,
    gizmo_mode: GizmoMode,
    gizmo_drag: Option<GizmoDrag>,
    hovered: Option<GizmoHandle>,
    /// The gizmo's shapes this frame (drawn by the overlay pass).
    overlay_draws: Vec<melon_render::OverlayDraw>,
    /// Icons of the point and spot lights (drawn with the gizmo).
    light_icons: Vec<melon_render::OverlayDraw>,
    pending_dialog: Option<DialogChannel>,
    pending_operation: Option<PendingOperation>,
    /// A Content Browser delete waiting for Delete / Cancel.
    delete_confirm: Option<DeleteConfirm>,
    /// A scene opened from the Content Browser, waiting for Save / Don't
    /// Save / Cancel.
    confirm_open: Option<std::path::PathBuf>,
    /// The Content Browser tab was drawn last frame (it takes OS file drops).
    content_shown: bool,
    /// The Content Browser tab (not the drawer) was drawn last frame;
    /// during the dock, whether it was drawn this frame.
    content_tab_shown: bool,
    drawer: drawer::DrawerState,
    /// Follows changes under `assets/` made outside the editor; `None` if
    /// it could not start (Refresh still works).
    watcher: Option<asset_watch::AssetWatcher>,
    /// When `keep_watching` next checks the watcher; `None` until a
    /// project is set.
    watch_check: Option<std::time::Instant>,
    /// A new file is waiting for its `.meta` (`asset_watch::META_GRACE`):
    /// reconcile again at this time.
    meta_recheck: Option<std::time::Instant>,
    /// glTF mesh counts for the Inspector's Asset card, by file and its
    /// modified time (re-read only when the file changes). Holds only the
    /// browser selection.
    mesh_counts: std::collections::HashMap<
        std::path::PathBuf,
        (Option<std::time::SystemTime>, Result<ModelInfo, String>),
    >,
    /// "Show" asked for the Content Browser tab; applied once the dock is
    /// back in `self.dock` (panels draw while it is taken out).
    focus_content_tab: bool,
    status_message: Option<(String, StatusKind, std::time::Instant)>,
    /// egui texture showing the scene (set by `EditorOverlay`).
    viewport_texture: Option<egui::TextureId>,
    /// Pixel size the viewport panel wants its render target to be.
    viewport_size: winit::dpi::PhysicalSize<u32>,
    /// Scene-view camera driven by the viewport (Unity controls).
    camera: viewport::EditorCamera,
    /// Focus the Scene viewport saw last frame: Game -> Editor adopts the game's `Camera`.
    last_viewport_focus: InputFocus,
    /// `RegistryGeneration` seen last frame: on a change, kept scene
    /// components whose type is now registered are inserted.
    registry_generation: u64,
    /// The Scene viewport was drawn this frame (reset at the start of `build_ui`).
    viewport_shown: bool,
    /// Grid and selection box, drawn depth-tested in the 3D pass.
    world_line_vertices: Vec<LineVertex>,
    /// Smoothed frame time in milliseconds, for the FPS readout.
    frame_ms: f32,
    /// Engine log shown in the Console panel.
    console: console::ConsoleLog,
    console_filter: console::ConsoleFilter,
    /// Dockable panel arrangement (saved between runs).
    dock: egui_dock::DockState<layout::EditorTab>,
    content: content::ContentBrowser,
    content_settings_file: std::path::PathBuf,
    /// Where the dock layout is saved (under the project root).
    layout_file: std::path::PathBuf,
    /// Project folder: asset paths are relative to it.
    project_root: std::path::PathBuf,
    hierarchy_query: String,
    /// Add Component popup's search text.
    component_search: String,
    /// Hierarchy rows whose children are hidden.
    collapsed: std::collections::HashSet<Entity>,
    /// Scene file last saved or loaded; File > Save writes here.
    current_scene: Option<std::path::PathBuf>,
    /// Right mouse held over the viewport (fly mode): W/E/R are fly keys then.
    flying: bool,
    show_about: bool,
    project_settings: project_settings::ProjectSettings,
    build_window: build_window::BuildWindow,
    /// A rebinding capture was waiting when this frame began: its keys are not shortcuts.
    keys_captured: bool,
    logo: Option<egui::TextureHandle>,
    /// Undo / redo steps (edit mode only).
    history: history::History,
    /// Hierarchy row being renamed inline.
    renaming: Option<Renaming>,
}

/// An inline rename in the Hierarchy.
struct Renaming {
    entity: Entity,
    text: String,
    /// First frame: take keyboard focus and select the text.
    fresh: bool,
    /// Its row was drawn this frame.
    drawn: bool,
    /// Frames it has gone undrawn after the first; a hidden row cancels it.
    undrawn: u8,
}

impl Renaming {
    fn new(entity: Entity, text: String) -> Self {
        Self {
            entity,
            text,
            fresh: true,
            drawn: false,
            undrawn: 0,
        }
    }
}

/// What the Hierarchy row menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    Rename,
    Duplicate,
    Delete,
    CreateChild,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            selected: None,
            gizmo_mode: GizmoMode::Move,
            gizmo_drag: None,
            hovered: None,
            overlay_draws: Vec::new(),
            light_icons: Vec::new(),
            pending_dialog: None,
            pending_operation: None,
            delete_confirm: None,
            confirm_open: None,
            content_shown: false,
            content_tab_shown: false,
            drawer: drawer::DrawerState::default(),
            watcher: None,
            watch_check: None,
            meta_recheck: None,
            mesh_counts: std::collections::HashMap::new(),
            focus_content_tab: false,
            status_message: None,
            viewport_texture: None,
            viewport_size: winit::dpi::PhysicalSize::new(1, 1),
            camera: viewport::EditorCamera::default(),
            last_viewport_focus: InputFocus::Editor,
            registry_generation: 0,
            viewport_shown: false,
            world_line_vertices: Vec::new(),
            frame_ms: 16.7,
            console: console::ConsoleLog::default(),
            console_filter: console::ConsoleFilter::default(),
            dock: layout::default_layout(),
            content: content::ContentBrowser::new("assets".into()),
            content_settings_file: content::settings_path(),
            layout_file: layout::layout_path(),
            project_root: ".".into(),
            hierarchy_query: String::new(),
            component_search: String::new(),
            collapsed: std::collections::HashSet::new(),
            current_scene: None,
            flying: false,
            show_about: false,
            project_settings: project_settings::ProjectSettings::default(),
            build_window: build_window::BuildWindow::default(),
            keys_captured: false,
            logo: None,
            history: history::History::default(),
            renaming: None,
        }
    }
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the editor at a project: the Content Browser shows
    /// `<root>/assets` and the dock layout is loaded from and saved to
    /// `<root>/.melon/editor_layout.ron`, whatever the working directory.
    pub fn set_project_root(&mut self, root: &std::path::Path) {
        self.content = content::ContentBrowser::new(root.join("assets"));
        self.content_settings_file = root.join(content::settings_path());
        self.content.load_settings(&self.content_settings_file);
        self.layout_file = root.join(layout::layout_path());
        self.project_root = root.to_path_buf();
        let now = std::time::Instant::now();
        self.watcher = None;
        self.watch_check = Some(now);
        self.keep_watching(now);
        if self.watcher.is_none() {
            tracing::warn!("assets/ is not watched yet (use Refresh); retrying");
        }
        self.dock = layout::load_layout(&self.layout_file);
    }

    /// Save the panel arrangement; the host calls this when the window closes.
    pub fn save_layout(&self) {
        if let Err(error) = layout::save_layout(&self.dock, &self.layout_file) {
            tracing::warn!(?error, "could not save the editor layout");
        }
        if let Err(error) = self.content.save_settings(&self.content_settings_file) {
            tracing::warn!(?error, "could not save the content browser settings");
        }
    }

    /// The host calls this after a successful save or load.
    pub fn set_current_scene(&mut self, path: std::path::PathBuf) {
        self.current_scene = Some(path);
    }

    /// Mark the selected entity for deletion and clear the selection.
    pub fn delete_selected(&mut self, world: &mut World) {
        if let Some(entity) = self.selected {
            self.history.delete(world, entity, self.selected);
        }
        self.clear_selection();
    }

    /// Spawn a cube and record it as a step.
    fn create_cube_step(&mut self, world: &mut World) {
        self.create_cube_step_in(world, None);
    }

    /// `create_cube_step` under `parent`.
    fn create_cube_step_in(&mut self, world: &mut World, parent: Option<Entity>) {
        let cube = create_cube_in(world, parent);
        let label = format!("Create {}", history::name_of(world, cube));
        self.history
            .record_created(world, cube, label, self.selected);
        self.selected = Some(cube);
    }

    /// Assign the `.mat` file `path` (project-relative) to part `part` of
    /// `entity`'s model, or to every part for `None`, as one undo step.
    pub(crate) fn assign_material_step(
        &mut self,
        world: &mut World,
        entity: Entity,
        path: &str,
        part: Option<usize>,
    ) {
        let Some(model) = world.get::<Handle<Model>>(entity).copied() else {
            return;
        };
        let parts = world
            .get_resource::<AssetServer>()
            .map_or(0, |server| server.part_bounds(model).len());
        let slot = material_assign::slot_for(&self.project_root, path);
        let before = self.history.begin(world, &[entity]);
        let mut materials = world
            .get::<melon_core::MeshMaterials>(entity)
            .cloned()
            .unwrap_or_default();
        material_assign::assign(&mut materials.parts, part, parts, slot);
        world.entity_mut(entity).insert(materials);
        self.history.commit(
            world,
            before,
            &[],
            "Assign Material".to_string(),
            self.selected,
            self.selected,
        );
    }

    /// Set every part's material of `entity` from `parts` (project-relative
    /// `.mat` paths, one per part) as one undo step.
    fn assign_materials_step(&mut self, world: &mut World, entity: Entity, parts: &[String]) {
        let slots: Vec<_> = parts
            .iter()
            .map(|p| material_assign::slot_for(&self.project_root, p))
            .collect();
        let before = self.history.begin(world, &[entity]);
        world
            .entity_mut(entity)
            .insert(melon_core::MeshMaterials { parts: slots });
        self.history.commit(
            world,
            before,
            &[],
            "Extract Materials".to_string(),
            self.selected,
            self.selected,
        );
    }

    /// The project-relative `.mat` path of a Content Browser drag, if it is one.
    fn dragged_material(&self, drag: &content::AssetDrag) -> Option<String> {
        let rel = melon_core::assets::project_relative(&self.project_root, &drag.path)?;
        content::has_extension(&rel, &["mat"]).then_some(rel)
    }

    /// Spawn a light, select it and record it as a step.
    fn create_light_step(&mut self, world: &mut World, kind: LightKind) {
        let light = create_light(world, kind);
        let label = format!("Create {}", history::name_of(world, light));
        self.history
            .record_created(world, light, label, self.selected);
        self.selected = Some(light);
    }

    /// Spawn a model (see `spawn_model`), select it and record it as a step.
    fn spawn_model_step(
        &mut self,
        world: &mut World,
        path: &str,
        at: glam::Vec3,
        parent: Option<Entity>,
    ) {
        let root = spawn_model(world, &self.project_root, path, at, parent);
        let label = format!("Create {}", history::name_of(world, root));
        self.history
            .record_created(world, root, label, self.selected);
        self.selected = Some(root);
    }

    /// Show what an undo / redo / jump selected.
    fn adopt_history_selection(&mut self, selection: Option<Option<Entity>>) {
        if let Some(selection) = selection {
            self.clear_selection();
            self.selected = selection;
            self.renaming = None;
        }
    }

    fn undo(&mut self, world: &mut World) {
        let selection = self.history.undo(world);
        self.adopt_history_selection(selection);
    }

    fn redo(&mut self, world: &mut World) {
        let selection = self.history.redo(world);
        self.adopt_history_selection(selection);
    }

    /// Copy the selected entity (Ctrl+D); the copy becomes the selection.
    fn duplicate_selected(&mut self, world: &mut World) {
        let Some(entity) = self.selected else { return };
        if let Some(copy) = self.history.duplicate(world, entity, self.selected) {
            self.clear_selection();
            self.selected = Some(copy);
        }
    }

    /// Record this frame's edits to the selected entity (Inspector, gizmo).
    fn track_history(&mut self, world: &mut World, ui: &egui::Ui) {
        let ctx = ui.ctx();
        let active =
            self.gizmo_drag.is_some() || ctx.dragged_id().is_some() || ctx.text_edit_focused();
        let hint = self.gizmo_drag.map(|d| match d.mode {
            GizmoMode::Move => "Move",
            GizmoMode::Rotate => "Rotate",
            GizmoMode::Scale => "Scale",
        });
        self.history.track(world, self.selected, active, hint);
    }

    /// A rename whose row is not on screen (filtered out, panel closed) is
    /// dropped, so it cannot pop up later.
    fn cancel_hidden_rename(&mut self) {
        if let Some(r) = &mut self.renaming {
            if std::mem::take(&mut r.drawn) {
                r.undrawn = 0;
            } else if r.undrawn >= 1 {
                self.renaming = None;
            } else {
                r.undrawn += 1;
            }
        }
    }

    /// Open the inline rename on the selected Hierarchy row (F2).
    fn start_rename(&mut self, world: &World) {
        if let Some(entity) = self.selected {
            self.renaming = Some(Renaming::new(entity, history::name_of(world, entity)));
        }
    }

    fn open_dialog(&mut self, save: bool) {
        if self.pending_dialog.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        // Scenes live in assets/scenes so the Content Browser shows them.
        let scenes = self.project_root.join("assets/scenes");
        if save && let Err(error) = std::fs::create_dir_all(&scenes) {
            tracing::warn!("could not create {}: {error}", scenes.display());
        }
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new().add_filter("RON", &["ron"]);
            let path = if save {
                dialog
                    .set_directory(&scenes)
                    .set_file_name("scene.ron")
                    .save_file()
            } else {
                dialog.pick_file()
            };
            let _ = tx.send(path);
        });
        self.pending_dialog = Some(if save {
            DialogChannel::Save(rx)
        } else {
            DialogChannel::Load(rx)
        });
    }

    /// Turn a finished file dialog into a pending Save/Load.
    fn poll_dialog(&mut self, world: &World) {
        let Some(dialog) = self.pending_dialog.take() else {
            return;
        };
        let result = match &dialog {
            DialogChannel::Save(rx) => rx.try_recv(),
            DialogChannel::Load(rx) => rx.try_recv(),
        };
        match result {
            Ok(Some(mut path)) => {
                if path.extension().is_none() {
                    path = path.with_extension("ron");
                }
                let is_save = matches!(dialog, DialogChannel::Save(_));
                self.pending_operation = dialog_operation(is_save, path, sim_state(world));
                if self.pending_operation.is_none() {
                    self.set_status_warning("Stop the game before saving or loading".into());
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => self.pending_dialog = Some(dialog),
            // Cancelled, or the dialog thread died.
            Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// File > Save: write to the current scene file, or ask for one.
    fn save(&mut self, world: &World) {
        if sim_state(world) != SimulationState::Editing {
            self.set_status_warning("Stop the game before saving".into());
            return;
        }
        match self.current_scene.clone() {
            Some(path) => self.pending_operation = Some(PendingOperation::Save(path)),
            None => self.open_dialog(true),
        }
    }

    fn handle_shortcuts(&mut self, world: &mut World, ui: &egui::Ui) {
        let typing = ui.ctx().text_edit_focused();
        let dragging = self.gizmo_drag.is_some() || ui.ctx().dragged_id().is_some();
        let pressed: Vec<Shortcut> = ui.input(|i| {
            [
                egui::Key::S,
                egui::Key::Delete,
                egui::Key::W,
                egui::Key::E,
                egui::Key::R,
                egui::Key::Escape,
                egui::Key::Space,
                egui::Key::Z,
                egui::Key::Y,
                egui::Key::D,
                egui::Key::F2,
            ]
            .into_iter()
            .filter(|key| fresh_press(i, *key))
            .filter_map(|key| {
                shortcut_for(
                    key,
                    i.modifiers.command,
                    i.modifiers.shift,
                    typing,
                    self.flying,
                )
            })
            .collect()
        });
        let game_focus = input_focus(world) == InputFocus::Game;
        for shortcut in pressed {
            // With game focus the keys belong to the game; only Esc takes them back.
            if game_focus && shortcut != Shortcut::ReleaseGame {
                continue;
            }
            match shortcut {
                Shortcut::ReleaseGame => world.insert_resource(InputFocus::Editor),
                Shortcut::Save => self.save(world),
                // With the Content Browser focused, Delete is its file delete.
                Shortcut::Delete if self.content.has_focus() => {}
                Shortcut::Delete => self.delete_selected(world),
                Shortcut::Move => self.gizmo_mode = GizmoMode::Move,
                Shortcut::Rotate => self.gizmo_mode = GizmoMode::Rotate,
                Shortcut::Scale => self.gizmo_mode = GizmoMode::Scale,
                Shortcut::ToggleDrawer => self.toggle_drawer(),
                // Not mid-gesture: the step is still being recorded.
                Shortcut::Undo | Shortcut::Redo if dragging => {}
                Shortcut::Undo => self.undo(world),
                Shortcut::Redo => self.redo(world),
                // With the Content Browser focused, Ctrl+D and F2 are its own.
                Shortcut::Duplicate | Shortcut::Rename if self.content.has_focus() => {}
                Shortcut::Duplicate => self.duplicate_selected(world),
                Shortcut::Rename => self.start_rename(world),
            }
        }
    }

    fn logo(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.logo
            .get_or_insert_with(|| {
                let pixels = engine_icon();
                let size = [pixels.width() as usize, pixels.height() as usize];
                ctx.load_texture(
                    "MelonEngine",
                    egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
                    egui::TextureOptions::LINEAR,
                )
            })
            .id()
    }

    fn menu_bar(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let editing = sim_state(world) == SimulationState::Editing;
        let dialog_open = self.pending_dialog.is_some();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add_enabled(editing && !dialog_open, egui::Button::new("Open…"))
                    .clicked()
                {
                    self.open_dialog(false);
                }
                if ui
                    .add_enabled(
                        editing && !dialog_open,
                        egui::Button::new("Save").shortcut_text("Ctrl+S"),
                    )
                    .clicked()
                {
                    self.save(world);
                }
                if ui
                    .add_enabled(editing && !dialog_open, egui::Button::new("Save As…"))
                    .clicked()
                {
                    self.open_dialog(true);
                }
                if ui.button("Build…").clicked() {
                    self.build_window.open = true;
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    self.pending_operation = Some(PendingOperation::Exit);
                }
            });
            ui.menu_button("Edit", |ui| {
                let step = |verb: &str, label: Option<&str>| match label {
                    Some(label) => format!("{verb} {label}"),
                    None => verb.to_string(),
                };
                let undo_text = step("Undo", self.history.undo_label());
                if ui
                    .add_enabled(
                        editing && self.history.can_undo(),
                        egui::Button::new(undo_text).shortcut_text("Ctrl+Z"),
                    )
                    .clicked()
                {
                    self.undo(world);
                }
                let redo_text = step("Redo", self.history.redo_label());
                if ui
                    .add_enabled(
                        editing && self.history.can_redo(),
                        egui::Button::new(redo_text).shortcut_text("Ctrl+Y"),
                    )
                    .clicked()
                {
                    self.redo(world);
                }
                ui.separator();
                let has_selection = editing && self.selected.is_some();
                if ui
                    .add_enabled(
                        has_selection,
                        egui::Button::new("Duplicate").shortcut_text("Ctrl+D"),
                    )
                    .clicked()
                {
                    self.duplicate_selected(world);
                }
                if ui
                    .add_enabled(
                        has_selection,
                        egui::Button::new("Rename").shortcut_text("F2"),
                    )
                    .clicked()
                {
                    self.start_rename(world);
                }
                if ui
                    .add_enabled(
                        self.selected.is_some(),
                        egui::Button::new("Delete").shortcut_text("Del"),
                    )
                    .clicked()
                {
                    self.delete_selected(world);
                }
                ui.separator();
                if ui.button("Project Settings…").clicked() {
                    self.project_settings.open = true;
                }
            });
            ui.menu_button("GameObject", |ui| {
                if ui.button("Create Cube").clicked() {
                    self.create_cube_step(world);
                }
                ui.menu_button("Light", |ui| {
                    if ui.button("Point Light").clicked() {
                        self.create_light_step(world, LightKind::Point);
                        ui.close();
                    }
                    if ui.button("Spot Light").clicked() {
                        self.create_light_step(world, LightKind::Spot);
                        ui.close();
                    }
                });
            });
            ui.menu_button("Window", |ui| {
                for tab in layout::EditorTab::ALL {
                    if ui.button(tab.title()).clicked() {
                        layout::reopen(&mut self.dock, tab);
                    }
                }
                ui.separator();
                if ui.button("Reset Layout").clicked() {
                    self.dock = layout::default_layout();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About MelonEngine").clicked() {
                    self.show_about = true;
                }
            });
        });
    }

    fn toolbar(&mut self, world: &mut World, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let spacing = ui.spacing().item_spacing.x;
            let controls_width =
                7.0 * theme::sizes::TOOL_BUTTON.x + 7.0 * spacing + theme::sizes::SPACING;
            ui.add_space(((ui.available_width() - controls_width) * 0.5).max(0.0));
            let tools_enabled = input_focus(world) == InputFocus::Editor;
            for (mode, icon) in [
                (GizmoMode::Move, ToolbarIcon::Move),
                (GizmoMode::Rotate, ToolbarIcon::Rotate),
                (GizmoMode::Scale, ToolbarIcon::Scale),
            ] {
                if icon
                    .button(ui, tools_enabled, Some(self.gizmo_mode == mode))
                    .clicked()
                {
                    self.gizmo_mode = mode;
                }
            }
            ui.add(
                egui::Separator::default()
                    .vertical()
                    .spacing(theme::sizes::SPACING),
            );
            self.play_controls(world, ui);
            if ui.input(|i| i.modifiers.shift) {
                ui.colored_label(theme::colors::INFO, "Snap");
            }
        });
    }

    fn status_bar(&mut self, world: &mut World, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            if let Some((message, kind, time)) = &self.status_message {
                if time.elapsed().as_secs() < 5 {
                    let color = match kind {
                        StatusKind::Success => theme::colors::SUCCESS,
                        StatusKind::Warning => theme::colors::WARNING,
                        StatusKind::Error => theme::colors::ERROR,
                    };
                    ui.colored_label(color, message);
                } else {
                    self.status_message = None;
                }
            } else {
                ui.colored_label(theme::colors::SUCCESS, "Ready");
            }
            let button = ui
                .add(egui::Button::selectable(self.drawer.open, "Content Drawer"))
                .on_hover_text("Ctrl+Space");
            self.drawer.button = Some(button.rect);
            if button.clicked() {
                self.toggle_drawer();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let tool = match self.gizmo_mode {
                    GizmoMode::Move => "Move",
                    GizmoMode::Rotate => "Rotate",
                    GizmoMode::Scale => "Scale",
                };
                ui.weak(tool);
                ui.separator();
                ui.weak(format!("{:.0} FPS", 1000.0 / self.frame_ms.max(0.001)));
                ui.separator();
                let count = world.query::<&Name>().iter(world).count();
                ui.weak(format!(
                    "{count} {}",
                    if count == 1 { "entity" } else { "entities" }
                ));
                if let Some(path) = &self.current_scene {
                    ui.separator();
                    ui.weak(
                        path.file_name()
                            .map_or(String::new(), |n| n.to_string_lossy().into_owned()),
                    );
                }
            });
        });
    }

    fn about_window(&mut self, ui: &egui::Ui) {
        let logo = self.logo(ui.ctx());
        egui::Window::new("About MelonEngine")
            .open(&mut self.show_about)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.add(
                    egui::Image::new((logo, egui::Vec2::splat(theme::sizes::ABOUT_LOGO)))
                        .alt_text("MelonEngine"),
                );
                ui.heading(format!("MelonEngine {}", melon_core::version()));
                ui.label("A Rust 3D game engine.");
                ui.weak("bevy_ecs · wgpu · rapier3d · egui");
            });
    }

    fn build_window_ui(&mut self, world: &mut World, ui: &egui::Ui) {
        match self.build_window.ui(ui.ctx(), world, &self.project_root) {
            Some((StatusKind::Success, message)) => self.set_status(message),
            Some((StatusKind::Warning, message)) => self.set_status_warning(message),
            Some((StatusKind::Error, message)) => self.set_status_error(message),
            None => {}
        }
    }

    fn project_settings_window(&mut self, world: &mut World, ui: &egui::Ui) {
        match self
            .project_settings
            .ui(ui.ctx(), world, &self.project_root)
        {
            Some((StatusKind::Success, message)) => self.set_status(message),
            Some((StatusKind::Warning, message)) => self.set_status_warning(message),
            Some((StatusKind::Error, message)) => self.set_status_error(message),
            None => {}
        }
    }

    /// The host loaded a broken `config/input.ron`: the first mapping save backs it up.
    pub fn set_input_map_broken(&mut self, broken: bool) {
        self.project_settings.set_broken_on_load(broken);
    }

    /// The host loaded a broken `config/quality.ron`: the first Apply backs it up.
    pub fn set_quality_broken(&mut self, broken: bool) {
        self.project_settings.set_quality_broken(broken);
    }

    /// The host loaded a broken `config/physics.ron`: the first Apply backs it up.
    pub fn set_physics_broken(&mut self, broken: bool) {
        self.project_settings.set_physics_broken(broken);
    }
    /// The host loaded a broken `config/player.ron`: the first Apply backs it up.
    pub fn set_player_broken(&mut self, broken: bool) {
        self.project_settings.set_player_broken(broken);
    }

    /// The host loaded a broken `config/audio.ron`: the first Apply backs it up.
    pub fn set_audio_broken(&mut self, broken: bool) {
        self.project_settings.set_audio_broken(broken);
    }

    /// Custom field editors of a component type (see `inspect::FieldWidget`).
    fn field_widgets(&self, world: &World, type_path: &str) -> inspect::FieldWidgets {
        use inspect::{FieldWidget, WidgetKind};
        let mut widgets = inspect::FieldWidgets::new();
        if type_path == melon_core::AudioSource::type_path() {
            let buses = world
                .get_resource::<melon_core::AudioSettings>()
                .map(|s| s.buses.iter().map(|b| b.name.clone()).collect())
                .unwrap_or_default();
            widgets.insert(
                "bus",
                FieldWidget {
                    label: None,
                    kind: WidgetKind::Choice(buses),
                },
            );
            widgets.insert(
                "clip",
                FieldWidget {
                    label: None,
                    kind: WidgetKind::Asset {
                        root: self.project_root.clone(),
                        extensions: &melon_core::audio::AUDIO_EXTENSIONS,
                    },
                },
            );
            widgets.insert(
                "volume",
                FieldWidget {
                    label: Some("Volume (linear)"),
                    kind: WidgetKind::Slider { min: 0.0, max: 1.0 },
                },
            );
        }
        if type_path == melon_core::PointLight::type_path()
            || type_path == melon_core::SpotLight::type_path()
        {
            widgets.insert(
                "color",
                FieldWidget {
                    label: None,
                    kind: WidgetKind::Color,
                },
            );
        }
        if type_path == melon_core::SpotLight::type_path() {
            // Stored in radians, edited in degrees.
            for (field, label) in [
                ("inner_angle", "Inner angle"),
                ("outer_angle", "Outer angle"),
            ] {
                widgets.insert(
                    field,
                    FieldWidget {
                        label: Some(label),
                        kind: WidgetKind::Degrees {
                            min: 0.0,
                            max: 85.0,
                        },
                    },
                );
            }
        }
        if type_path == melon_core::MeshMaterials::type_path() {
            widgets.insert(
                "parts",
                FieldWidget {
                    label: Some("Materials"),
                    kind: WidgetKind::Materials {
                        root: self.project_root.clone(),
                    },
                },
            );
        }
        if type_path == melon_core::Collider::type_path() {
            let names = world
                .get_resource::<melon_core::PhysicsSettings>()
                .map(|s| s.layer_names.clone())
                .unwrap_or_default();
            for (field, label) in [("membership", "Layer"), ("filter", "Collides with")] {
                widgets.insert(
                    field,
                    FieldWidget {
                        label: Some(label),
                        kind: WidgetKind::Layers(names.clone()),
                    },
                );
            }
        }
        widgets
    }

    pub fn selected_entity(&self) -> Option<Entity> {
        self.selected
    }

    pub fn set_selected(&mut self, entity: Option<Entity>) {
        self.selected = entity;
        self.gizmo_drag = None;
    }

    pub fn gizmo_mode(&self) -> GizmoMode {
        self.gizmo_mode
    }

    pub fn set_gizmo_mode(&mut self, mode: GizmoMode) {
        self.gizmo_mode = mode;
        self.gizmo_drag = None;
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.gizmo_drag = None;
        self.hovered = None;
    }

    /// Grid + selection box for this frame, drawn with `draw_world_lines`.
    pub fn take_world_line_vertices(&mut self) -> Vec<LineVertex> {
        std::mem::take(&mut self.world_line_vertices)
    }

    pub fn take_overlay_draws(&mut self) -> Vec<melon_render::OverlayDraw> {
        let mut draws = std::mem::take(&mut self.light_icons);
        draws.append(&mut self.overlay_draws);
        draws
    }

    pub fn take_pending_operation(&mut self) -> Option<PendingOperation> {
        std::mem::take(&mut self.pending_operation)
    }

    /// Run the File menu's pending Save / Load / Exit, once. Returns `true`
    /// when the window should close (the layout is already saved then).
    pub fn run_pending_operation(&mut self, world: &mut World) -> bool {
        match self.take_pending_operation() {
            None => false,
            Some(PendingOperation::Exit) => {
                self.save_layout();
                true
            }
            Some(PendingOperation::Save(path)) => {
                match scene::save_scene(world, &path) {
                    Ok(()) => {
                        self.set_status(format!("Saved to {}", path.display()));
                        self.set_current_scene(path);
                    }
                    Err(e) => self.set_status_error(format!("Save failed: {e}")),
                }
                false
            }
            Some(PendingOperation::Load(path)) => {
                if !world.contains_resource::<melon_physics::PhysicsWorld>() {
                    world.insert_resource(melon_physics::PhysicsWorld::new());
                }
                let result = world.resource_scope(
                    |world, mut physics: bevy_ecs::world::Mut<melon_physics::PhysicsWorld>| {
                        scene::load_scene(world, &mut physics, &path)
                    },
                );
                match result {
                    Ok(()) => {
                        self.clear_selection();
                        self.history.clear();
                        self.renaming = None;
                        self.set_status(format!("Loaded from {}", path.display()));
                        self.set_current_scene(path);
                    }
                    Err(e) => self.set_status_error(format!("Load failed: {e}")),
                }
                false
            }
        }
    }

    /// Once outside changes under `assets/` settle: reconcile `.meta` files
    /// and refresh the browser. A file that only just changed waits
    /// `META_GRACE` for its own `.meta` before getting a new one.
    fn follow_asset_changes(&mut self, world: &mut World, ui: &egui::Ui, now: std::time::Instant) {
        let restarted = self.keep_watching(now);
        let Some(watcher) = &mut self.watcher else {
            if self.watch_check.is_some() {
                ui.ctx().request_repaint_after(asset_watch::RETRY);
            }
            return;
        };
        // Changes made while nothing watched are caught up at once.
        let settled = watcher.poll(now) || restarted;
        if watcher.pending() {
            // Settle even if the user does not move the mouse.
            ui.ctx().request_repaint_after(asset_watch::QUIET);
        }
        if let Some(at) = self.meta_recheck {
            ui.ctx()
                .request_repaint_after(at.saturating_duration_since(now));
        }
        let recheck = self.meta_recheck.is_some_and(|at| now >= at);
        if !settled && !recheck {
            return;
        }
        if !world.contains_resource::<AssetServer>() {
            world.insert_resource(AssetServer::new(self.project_root.clone()));
        }
        let waiting =
            asset_db::reconcile_except(&mut world.resource_mut::<AssetServer>(), |file| {
                watcher.touched.recent(file, now)
            });
        self.meta_recheck = waiting.then(|| now + asset_watch::META_GRACE);
        self.content.refresh();
    }

    /// Every `RETRY`: drop the watcher if `assets/` is gone (notify stops
    /// silently then), start one if it is there. True when one started.
    // shortcut: a delete and recreate within RETRY is not noticed; compare the folder's identity if that bites.
    fn keep_watching(&mut self, now: std::time::Instant) -> bool {
        if self.watch_check.is_none_or(|check| now < check) {
            return false;
        }
        self.watch_check = Some(now + asset_watch::RETRY);
        let assets = self.project_root.join("assets");
        if !assets.is_dir() {
            if self.watcher.take().is_some() {
                tracing::warn!("assets/ is gone; live updates resume when it is back");
            }
            return false;
        }
        if self.watcher.is_some() {
            return false;
        }
        match asset_watch::AssetWatcher::start(&assets) {
            Ok(watcher) => {
                tracing::info!("watching {} for outside changes", assets.display());
                self.watcher = Some(watcher);
                true
            }
            Err(error) => {
                tracing::debug!("assets/ watcher did not start: {error}");
                false
            }
        }
    }

    /// The next model thumbnail to render (at most one per frame; the host
    /// calls this after `end_frame`).
    pub fn thumbnail_request(&mut self, world: &mut World) -> Option<thumbnails::ThumbnailRequest> {
        let mut server = world.get_resource_mut::<AssetServer>()?;
        self.content.thumbnails.request(&mut server)
    }

    /// The host rendered (`Some`) or could not render (`None`) `request`.
    pub fn thumbnail_done(
        &mut self,
        request: &thumbnails::ThumbnailRequest,
        id: Option<egui::TextureId>,
    ) {
        self.content.thumbnails.finish(request, id);
    }

    /// Run the file operations the Content Browser asked for this frame.
    pub fn apply_content_requests(&mut self, world: &mut World) {
        for request in self.content.take_requests() {
            if let Err(error) = self.run_content_request(world, request) {
                tracing::warn!("{error:#}");
                self.set_status_error(format!("{error:#}"));
            }
        }
    }

    fn run_content_request(
        &mut self,
        world: &mut World,
        request: content::BrowserRequest,
    ) -> anyhow::Result<()> {
        use content::BrowserRequest as Request;
        if !world.contains_resource::<AssetServer>() {
            world.insert_resource(AssetServer::new(self.project_root.clone()));
        }
        let done = match request {
            Request::NewFolder(parent) => {
                let dir =
                    asset_ops::create_folder(&mut world.resource_mut::<AssetServer>(), &parent)?;
                // Show the folder it went into (no search), so the rename field is on screen.
                self.content.open(parent);
                self.content.set_search(String::new());
                self.content.refresh();
                self.content.start_rename(dir.clone());
                Some(dir)
            }
            Request::NewMaterial(parent) => {
                let file =
                    asset_ops::create_material(&mut world.resource_mut::<AssetServer>(), &parent)?;
                self.content.open(parent);
                self.content.set_search(String::new());
                self.content.refresh();
                self.content.start_rename(file.clone());
                Some(file)
            }
            Request::ExtractMaterials(model) => {
                let parts =
                    asset_ops::extract_materials(&mut world.resource_mut::<AssetServer>(), &model)?;
                let rel = melon_core::assets::project_relative(&self.project_root, &model);
                // The selected entity gets them if it draws this model.
                if let Some(entity) = self.selected
                    && let Some(handle) = world.get::<Handle<Model>>(entity).copied()
                    && rel.as_deref() == Some(world.resource::<AssetServer>().path(handle))
                {
                    self.assign_materials_step(world, entity, &parts);
                }
                self.content.refresh();
                tracing::info!(
                    "extracted {} materials from {}",
                    parts.len(),
                    model.display()
                );
                None
            }
            Request::Rename(path, name) => {
                let new =
                    asset_ops::rename(&mut world.resource_mut::<AssetServer>(), &path, &name)?;
                self.follow_current_scene(&path, &new);
                Some(new)
            }
            Request::Duplicate(path) => Some(asset_ops::duplicate(
                &mut world.resource_mut::<AssetServer>(),
                &path,
            )?),
            Request::Move(path, folder) => {
                let new =
                    asset_ops::move_to(&mut world.resource_mut::<AssetServer>(), &path, &folder)?;
                self.follow_current_scene(&path, &new);
                Some(new)
            }
            Request::Import(files, folder) => {
                let results =
                    asset_ops::import(&mut world.resource_mut::<AssetServer>(), &files, &folder);
                let mut last = None;
                for result in results {
                    match result {
                        Ok(path) => last = Some(path),
                        Err(error) => {
                            tracing::warn!("import: {error:#}");
                            self.set_status_error(format!("Import failed: {error:#}"));
                        }
                    }
                }
                last
            }
            Request::AskDelete(path) => {
                let needles = asset_ops::reference_needles(world.resource::<AssetServer>(), &path);
                let mut used_by: Vec<String> =
                    asset_ops::scene_references(&self.project_root, &needles)
                        .iter()
                        .map(|f| {
                            melon_core::assets::project_relative(&self.project_root, f)
                                .unwrap_or_else(|| f.display().to_string())
                        })
                        .collect();
                let open =
                    world.resource_scope(|world, server: bevy_ecs::world::Mut<AssetServer>| {
                        asset_ops::open_scene_uses(world, &server, &path)
                    });
                if open {
                    used_by.push("the open scene".to_string());
                }
                self.delete_confirm = Some(DeleteConfirm { path, used_by });
                None
            }
            Request::Reveal(path) => {
                asset_ops::reveal(&path);
                None
            }
            Request::PreviewAudio(path) => {
                if let Some(clip) = melon_core::assets::project_relative(&self.project_root, &path)
                {
                    world
                        .resource_mut::<melon_core::AudioRequests>()
                        .preview_play(&clip);
                }
                None
            }
            Request::StopAudioPreview => {
                world
                    .resource_mut::<melon_core::AudioRequests>()
                    .preview_stop();
                None
            }
            Request::Open(path) => {
                if path.is_dir() {
                    self.content.open(path);
                } else if content::is_scene_path(&self.project_root, &path) {
                    // Asking "Save?" while playing would save the play state.
                    if sim_state(world) != SimulationState::Editing {
                        anyhow::bail!("Stop playing to open a scene");
                    }
                    self.confirm_open = Some(path);
                } else {
                    asset_ops::open_external(&path);
                }
                None
            }
        };
        if let Some(path) = done {
            self.content.refresh();
            self.content.select(Some(path));
        }
        Ok(())
    }

    /// The open scene's file (or a folder holding it) moved: Ctrl+S must
    /// write to the new place.
    fn follow_current_scene(&mut self, old: &std::path::Path, new: &std::path::Path) {
        if let Some(current) = &self.current_scene
            && let Ok(rest) = current.strip_prefix(old)
        {
            self.current_scene = Some(if rest.as_os_str().is_empty() {
                new.to_path_buf()
            } else {
                new.join(rest)
            });
        }
    }

    /// Save is offered only when the open scene has a file to save to.
    pub fn open_scene_can_save(&self) -> bool {
        self.current_scene.is_some()
    }

    /// Act on the answer to "Open <scene>?": Save writes the current scene
    /// first; both Save and Don't Save then load (only while editing).
    pub fn resolve_open(&mut self, world: &mut World, choice: OpenChoice) {
        let Some(path) = self.confirm_open.take() else {
            return;
        };
        match choice {
            OpenChoice::Cancel => return,
            OpenChoice::Save => {
                if sim_state(world) != SimulationState::Editing {
                    self.set_status_error("Stop playing to open a scene".into());
                    return;
                }
                let Some(current) = self.current_scene.clone() else {
                    self.set_status_error("Use File \u{25b8} Save As first".into());
                    return;
                };
                if let Err(e) = scene::save_scene(world, &current) {
                    self.set_status_error(format!("Save failed: {e}"));
                    return;
                }
            }
            OpenChoice::DontSave => {}
        }
        match dialog_operation(false, path, sim_state(world)) {
            Some(operation) => self.pending_operation = Some(operation),
            None => self.set_status_error("Stop playing to open a scene".into()),
        }
    }

    /// Save / Don't Save / Cancel before a scene opened from the browser.
    fn open_scene_dialog(&mut self, world: &mut World, ui: &egui::Ui) {
        let Some(path) = &self.confirm_open else {
            return;
        };
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let can_save = self.open_scene_can_save();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("content_open_scene")).show(ui.ctx(), |ui| {
            ui.heading(format!("Open {name}?"));
            ui.label("Unsaved changes in the current scene are lost.");
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_save, egui::Button::new("Save"))
                    .on_disabled_hover_text("Use File \u{25b8} Save As first")
                    .clicked()
                {
                    choice = Some(OpenChoice::Save);
                }
                if ui.button("Don't Save").clicked() {
                    choice = Some(OpenChoice::DontSave);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(OpenChoice::Cancel);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(OpenChoice::Cancel);
        }
        if let Some(choice) = choice {
            self.resolve_open(world, choice);
        }
    }

    /// Files dropped on the window from the OS go into the Content Browser's
    /// folder (winit gives no drop position on Windows, so the visible
    /// browser takes them).
    fn take_dropped_files(&mut self, ui: &egui::Ui) {
        let dropped: Vec<std::path::PathBuf> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        let shown = std::mem::take(&mut self.content_shown);
        // A browser in a hidden tab cannot own the keyboard.
        if !shown {
            self.content.focused = false;
        }
        if dropped.is_empty() {
            return;
        }
        if shown {
            let folder = self.content.current().to_path_buf();
            self.content
                .request(content::BrowserRequest::Import(dropped, folder));
        } else {
            self.set_status_warning("Open the Content Browser to import files".into());
        }
    }

    /// Delete / Cancel for a Content Browser delete, listing what uses it.
    fn delete_dialog(&mut self, world: &mut World, ui: &egui::Ui) {
        let Some(confirm) = &self.delete_confirm else {
            return;
        };
        let name = confirm
            .path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let used_by = confirm.used_by.clone();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("content_delete")).show(ui.ctx(), |ui| {
            ui.heading(format!("Delete {name}?"));
            ui.label("Moves it to the Recycle Bin.");
            if !used_by.is_empty() {
                ui.label(egui::RichText::new("Used by:").color(theme::colors::WARNING));
                for user in &used_by {
                    ui.label(format!("  {user}"));
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(false);
        }
        let Some(delete) = choice else {
            return;
        };
        let Some(confirm) = self.delete_confirm.take() else {
            return;
        };
        if !delete {
            return;
        }
        if !world.contains_resource::<AssetServer>() {
            world.insert_resource(AssetServer::new(self.project_root.clone()));
        }
        match asset_ops::delete(&mut world.resource_mut::<AssetServer>(), &confirm.path) {
            Ok(()) => {
                self.set_status(format!("Moved {name} to the Recycle Bin"));
                self.content.select(None);
                self.content.refresh();
            }
            Err(error) => {
                tracing::warn!("delete: {error:#}");
                self.set_status_error(format!("Delete failed: {error:#}"));
            }
        }
    }

    /// Share the engine's log buffer with the Console panel.
    pub fn set_console(&mut self, log: console::ConsoleLog) {
        self.console = log;
    }

    /// Pixel size of the viewport panel. The host resizes the
    /// `ViewportTarget` to this when it changes.
    pub fn requested_viewport_size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.viewport_size
    }

    pub fn set_status_error(&mut self, message: String) {
        self.status_message = Some((message, StatusKind::Error, std::time::Instant::now()));
    }

    pub fn set_status_warning(&mut self, message: String) {
        self.status_message = Some((message, StatusKind::Warning, std::time::Instant::now()));
    }

    /// Kind of the status message currently shown, if any.
    pub fn status_kind(&self) -> Option<StatusKind> {
        self.status_message.as_ref().map(|(_, kind, _)| *kind)
    }

    /// Smooth this frame's delta into the FPS readout. Runs every frame from
    /// `build_ui`, so the readout keeps updating with the Scene tab hidden.
    fn tick_frame_time(&mut self, dt: f32) {
        self.frame_ms += 0.1 * (dt * 1000.0 - self.frame_ms);
    }

    /// Show a success/info message in the status bar.
    pub fn set_status(&mut self, message: String) {
        self.status_message = Some((message, StatusKind::Success, std::time::Instant::now()));
    }

    /// Build the editor UI, reading from and writing to the ECS world.
    pub fn build_ui(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let dt = world
            .get_resource::<melon_core::Time>()
            .map_or(1.0 / 60.0, |t| t.delta_seconds);
        self.tick_frame_time(dt);
        self.poll_dialog(world);
        let generation = world
            .get_resource::<melon_core::RegistryGeneration>()
            .map_or(0, |g| g.0);
        if generation != self.registry_generation {
            self.registry_generation = generation;
            let inserted = scene::resolve_kept_components(world);
            if inserted > 0 {
                tracing::info!("{inserted} kept scene components are registered now");
            }
        }
        // A rebinding capture swallows this frame's keys (Esc cancels it, not ReleaseGame).
        // Read before the capture consumes the press later in the frame.
        self.keys_captured = self.project_settings.capturing();
        if !self.keys_captured {
            self.handle_shortcuts(world, ui);
        }
        self.viewport_shown = false;

        egui::Panel::top("menu_bar")
            .exact_size(theme::sizes::MENU_HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(theme::colors::CHROME)
                    .inner_margin(egui::vec2(6.0, 2.0)),
            )
            .show(ui, |ui| self.menu_bar(world, ui));
        egui::Panel::top("toolbar")
            .exact_size(theme::sizes::TOOLBAR_HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(theme::colors::CHROME_RAISED)
                    .inner_margin(egui::vec2(8.0, 6.0)),
            )
            .show(ui, |ui| self.toolbar(world, ui));
        egui::Panel::bottom("status_bar")
            .exact_size(theme::sizes::STATUS_HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(theme::colors::CHROME)
                    .inner_margin(egui::vec2(8.0, 2.0)),
            )
            .show(ui, |ui| self.status_bar(world, ui));
        self.about_window(ui);
        self.project_settings_window(world, ui);
        self.build_window_ui(world, ui);
        self.follow_asset_changes(world, ui, std::time::Instant::now());
        self.delete_dialog(world, ui);
        self.open_scene_dialog(world, ui);
        self.take_dropped_files(ui);
        self.content_tab_shown = false;

        // Dockable panels fill the rest of the window.
        self.overlay_draws.clear();
        let mut dock = std::mem::replace(&mut self.dock, egui_dock::DockState::new(Vec::new()));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::colors::CHROME))
            .show(ui, |ui| {
                let style = egui_dock::Style::from_egui(ui.style());
                // Per-tab close buttons only (Window menu reopens panels); no
                // group-wide close-all or collapse buttons.
                egui_dock::DockArea::new(&mut dock)
                    .style(style)
                    .show_leaf_close_all_buttons(false)
                    .show_leaf_collapse_buttons(false)
                    .show_inside(
                        ui,
                        &mut EditorTabs {
                            editor: self,
                            world,
                        },
                    );
            });
        self.dock = dock;
        self.cancel_hidden_rename();
        self.track_history(world, ui);
        self.drawer_ui(world, ui);
        if std::mem::take(&mut self.focus_content_tab) {
            layout::reopen(&mut self.dock, layout::EditorTab::ContentBrowser);
        }

        // Written after every focus change of this frame; the host applies it next frame.
        let gate = game_input_gate(
            input_focus(world),
            sim_state(world),
            ui.ctx().text_edit_focused(),
            self.viewport_shown,
        );
        world.insert_resource(GameInputGate(gate));
    }

    /// The scene viewport: image, camera navigation, gizmo, click select,
    /// grid/selection lines and overlays.
    fn viewport_panel(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let ctx = ui.ctx().clone();
        self.viewport_size = viewport::viewport_pixel_size(
            glam::Vec2::new(rect.width(), rect.height()),
            ctx.pixels_per_point(),
        );
        let Some(texture) = self.viewport_texture else {
            return;
        };
        self.viewport_shown = true;
        let response = ui.put(
            rect,
            egui::Image::new((texture, rect.size())).sense(egui::Sense::click_and_drag()),
        );
        let vp = viewport_input(&ctx, &response, rect);
        // A model dragged from the Content Browser lands on the ground under the cursor.
        if let Some(drag) = response.dnd_release_payload::<content::AssetDrag>()
            && let (Some(pointer), Some(camera)) =
                (vp.pointer, world.get_resource::<Camera>().copied())
        {
            if let Some(path) = drag.model.clone()
                && content::is_model_path(&path)
            {
                let at = viewport::drop_point(&camera, pointer, vp.size);
                self.spawn_model_step(world, &path, at, None);
                self.gizmo_drag = None;
            } else if let Some(path) = self.dragged_material(&drag) {
                // A material goes on the part of the entity under the cursor.
                let (origin, dir) = camera.screen_to_world_ray(pointer, vp.size);
                if let Some((entity, part)) = viewport::pick_part(world, origin, dir) {
                    self.assign_material_step(world, entity, &path, part);
                }
            }
        }
        self.flying = vp.right_down;
        let focus = focus_after_click(sim_state(world), input_focus(world), vp.left_clicked);
        world.insert_resource(focus);
        // Esc / Pause / Step / Stop: keep the view where the game left it (Unreal eject).
        if self.last_viewport_focus == InputFocus::Game
            && focus == InputFocus::Editor
            && let Some(camera) = world.get_resource::<Camera>()
        {
            self.camera.adopt(camera);
        }
        self.last_viewport_focus = focus;
        // With game focus the game owns keyboard, mouse and `Camera`: no editor
        // navigation, gizmo or picking (the click that took focus selects nothing).
        if focus == InputFocus::Game {
            self.hovered = None;
            self.gizmo_drag = None;
        } else {
            let dt = world
                .get_resource::<melon_core::Time>()
                .map_or(1.0 / 60.0, |t| t.delta_seconds);

            self.camera
                .apply(&viewport::camera_input(&vp, self.gizmo_drag.is_some()), dt);
            if vp.f_pressed
                && let Some((center, radius)) =
                    self.selected.and_then(|e| viewport::frame_target(world, e))
            {
                self.camera.frame(center, radius);
            }
            if let Some(mut camera) = world.get_resource_mut::<Camera>() {
                self.camera.write(&mut camera);
            }

            self.build_gizmo(world, ui, &vp);

            let on_gizmo = self.hovered.is_some() || self.gizmo_drag.is_some();
            if viewport::left_action(vp.alt, on_gizmo, vp.left_clicked)
                == viewport::LeftAction::Select
                && let (Some(pointer), Some(camera)) =
                    (vp.pointer, world.get_resource::<Camera>().copied())
            {
                let (origin, dir) = camera.screen_to_world_ray(pointer, vp.size);
                self.selected = viewport::pick(world, origin, dir);
                self.gizmo_drag = None;
            }
        }

        self.world_line_vertices = viewport::grid_lines();
        self.light_icons = viewport::light_icons(world);
        if let Some(e) = self.selected
            && world.get::<Transform>(e).is_some()
        {
            self.world_line_vertices
                .extend(viewport::selection_box_lines(
                    world_matrix(world, e),
                    viewport::model_bounds(world, e),
                ));
            self.world_line_vertices
                .extend(viewport::collider_lines(world, e));
            self.world_line_vertices
                .extend(viewport::light_lines(world, e));
        }

        let painter = ui.painter_at(rect);
        painter.text(
            rect.right_top() + egui::vec2(-10.0, 8.0),
            egui::Align2::RIGHT_TOP,
            format!(
                "{:.0} FPS  {:.1} ms",
                1000.0 / self.frame_ms.max(0.001),
                self.frame_ms
            ),
            egui::FontId::monospace(13.0),
            egui::Color32::from_gray(210),
        );
        if let Some(camera) = world.get_resource::<Camera>() {
            draw_orientation(&painter, rect.right_top() + egui::vec2(-50.0, 75.0), camera);
        }
    }

    /// Play / Pause / Stop buttons, state label and F5 / F6 / Shift+F5 shortcuts.
    fn play_controls(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let state = world
            .get_resource::<SimulationState>()
            .copied()
            .unwrap_or_default();
        let (mut want_play, mut want_pause, mut want_stop) = (false, false, false);
        // Shortcuts are ignored while typing in a text field or capturing a binding.
        if !ui.ctx().text_edit_focused() && !self.keys_captured {
            ui.input(|i| {
                if fresh_press(i, egui::Key::F5) {
                    if i.modifiers.shift {
                        want_stop = true;
                    } else {
                        want_play = true;
                    }
                }
                want_pause |= fresh_press(i, egui::Key::F6);
            });
        }
        let can_play = state != SimulationState::Playing;
        let can_pause = state == SimulationState::Playing;
        let can_stop = state != SimulationState::Editing;
        want_play |= ToolbarIcon::Play.button(ui, can_play, None).clicked();
        want_pause |= ToolbarIcon::Pause.button(ui, can_pause, None).clicked();
        let step = ToolbarIcon::Step
            .button(ui, state == SimulationState::Paused, None)
            .clicked();
        want_stop |= ToolbarIcon::Stop.button(ui, can_stop, None).clicked();
        if step {
            play::step(world);
        }

        if want_play && can_play {
            match play::play(world) {
                // Play from edit mode rebuilds the scene, so entity ids change.
                Ok(()) if state == SimulationState::Editing => self.clear_selection(),
                Ok(()) => {}
                Err(e) => self.set_status_error(format!("Play failed: {e}")),
            }
        } else if want_pause && can_pause {
            play::pause(world);
        } else if want_stop && can_stop {
            match play::stop(world) {
                Ok(()) => self.clear_selection(),
                Err(e) => self.set_status_error(format!("Stop failed: {e}")),
            }
        }

        let (color, label) = match state {
            SimulationState::Editing => (theme::colors::TEXT_MUTED, "EDITING"),
            SimulationState::Playing => (theme::colors::SUCCESS, "PLAYING"),
            SimulationState::Paused => (theme::colors::WARNING, "PAUSED"),
        };
        ui.colored_label(color, label);
    }

    fn build_hierarchy(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let mut add = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.hierarchy_query)
                    .hint_text("Search")
                    .desired_width(ui.available_width() - 60.0),
            );
            add = ui.button("+ Add").clicked();
        });

        if add {
            self.create_cube_step(world);
        }

        ui.separator();

        // Entity tree. Rows are drag sources (reparent) and drop targets for
        // entities and models; the space below the rows makes roots.
        let mut row_action: Option<(RowAction, Entity)> = None;
        let mut reparent: Option<(Entity, Option<Entity>)> = None;
        let mut model_drop: Option<(String, Option<Entity>)> = None;
        let mut material_drop: Option<(String, Entity)> = None;
        // Inline rename finished this frame: the new text, or `None` for Esc.
        let mut rename_done: Option<(Entity, Option<String>)> = None;
        let searching = !self.hierarchy_query.trim().is_empty();
        let rows: Vec<(Entity, usize, String, bool)> = depth_first(world)
            .into_iter()
            .map(|(e, depth)| {
                let name = world
                    .get::<Name>(e)
                    .map(|n| n.0.clone())
                    .unwrap_or_default();
                let has_children = world
                    .get::<melon_core::Children>(e)
                    .is_some_and(|c| !c.is_empty());
                (e, depth, name, has_children)
            })
            .collect();

        // Read what is being dragged without taking it: egui's
        // `dnd_release_payload::<T>` takes the payload even when it isn't a `T`.
        let carried_entity = egui::DragAndDrop::payload::<Entity>(ui.ctx()).map(|e| *e);
        let carried_model = egui::DragAndDrop::payload::<content::AssetDrag>(ui.ctx())
            .and_then(|drag| drag.model.clone());
        let carried_material = egui::DragAndDrop::payload::<content::AssetDrag>(ui.ctx())
            .and_then(|drag| self.dragged_material(&drag));
        let carrying =
            carried_entity.is_some() || carried_model.is_some() || carried_material.is_some();
        let released = ui.ctx().input(|i| i.pointer.any_released());
        // Row the pointer was released over, if any (a row wins over the zone).
        let mut released_on_row: Option<Entity> = None;

        let zone = egui::Frame::new().show(ui, |ui| {
            // Rows below a collapsed row are hidden.
            let mut hide_below: Option<usize> = None;
            for (entity, depth, name, has_children) in rows {
                if searching {
                    if !layout::hierarchy_matches(&name, &self.hierarchy_query) {
                        continue;
                    }
                } else {
                    if hide_below.is_some_and(|d| depth > d) {
                        continue;
                    }
                    hide_below = self.collapsed.contains(&entity).then_some(depth);
                }
                let is_selected = self.selected == Some(entity);
                // One full-width hit area per row: click selects, the arrow
                // toggles children, dragging carries the entity.
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), theme::sizes::ROW_HEIGHT),
                    egui::Sense::hover(),
                );
                let sense = if searching {
                    egui::Sense::click()
                } else {
                    egui::Sense::click_and_drag()
                };
                let response = ui.interact(rect, egui::Id::new(("hierarchy_row", entity)), sense);
                let indent = if searching {
                    0.0
                } else {
                    depth as f32 * theme::sizes::SPACING * 2.0
                };
                let arrow_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + indent, rect.top()),
                    egui::vec2(theme::sizes::ROW_HEIGHT, rect.height()),
                );
                let on_arrow = !searching
                    && has_children
                    && response
                        .interact_pointer_pos()
                        .is_some_and(|p| arrow_rect.contains(p));
                if response.clicked() {
                    if on_arrow {
                        if !self.collapsed.remove(&entity) {
                            self.collapsed.insert(entity);
                        }
                    } else {
                        self.selected = if is_selected { None } else { Some(entity) };
                        self.gizmo_drag = None;
                    }
                }
                if response.secondary_clicked() {
                    self.selected = Some(entity);
                }
                response.context_menu(|ui| {
                    for (label, action) in [
                        ("Rename", RowAction::Rename),
                        ("Duplicate", RowAction::Duplicate),
                        ("Delete", RowAction::Delete),
                        ("Create Child", RowAction::CreateChild),
                    ] {
                        if ui.button(label).clicked() {
                            row_action = Some((action, entity));
                            ui.close();
                        }
                    }
                });
                if !searching && response.double_clicked() && !on_arrow {
                    self.selected = Some(entity);
                    self.renaming = Some(Renaming::new(entity, name.clone()));
                }
                if !searching && response.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), entity);
                }

                let painter = ui.painter().clone();
                let fill = if is_selected {
                    theme::colors::ACCENT_SOFT
                } else if response.hovered() {
                    theme::colors::SURFACE_RAISED
                } else {
                    egui::Color32::TRANSPARENT
                };
                painter.rect_filled(rect, theme::sizes::RADIUS, fill);
                if !searching && has_children {
                    let c = arrow_rect.center();
                    let r = theme::sizes::ROW_HEIGHT * 0.15;
                    let points = if self.collapsed.contains(&entity) {
                        vec![
                            c + egui::vec2(-r, -r),
                            c + egui::vec2(r, 0.0),
                            c + egui::vec2(-r, r),
                        ]
                    } else {
                        vec![
                            c + egui::vec2(-r, -r * 0.6),
                            c + egui::vec2(r, -r * 0.6),
                            c + egui::vec2(0.0, r),
                        ]
                    };
                    painter.add(egui::Shape::convex_polygon(
                        points,
                        theme::colors::TEXT_MUTED,
                        egui::Stroke::NONE,
                    ));
                }
                let text_x = if searching {
                    rect.left() + theme::sizes::SPACING
                } else {
                    arrow_rect.right()
                };
                let renaming_here = self.renaming.as_ref().is_some_and(|r| r.entity == entity);
                if renaming_here && let Some(r) = self.renaming.as_mut() {
                    r.drawn = true;
                    let edit_rect = egui::Rect::from_min_max(
                        egui::pos2(text_x, rect.top()),
                        rect.right_bottom(),
                    );
                    let id = egui::Id::new("hierarchy_rename");
                    let edit = ui.put(
                        edit_rect,
                        egui::TextEdit::singleline(&mut r.text)
                            .id(id)
                            .desired_width(edit_rect.width()),
                    );
                    if std::mem::take(&mut r.fresh) {
                        edit.request_focus();
                        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                            let end = egui::text::CCursor::new(r.text.chars().count());
                            state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(0),
                                    end,
                                )));
                            state.store(ui.ctx(), id);
                        }
                    } else {
                        // Esc cancels; Enter or clicking away commits.
                        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if escape {
                            rename_done = Some((entity, None));
                        } else if edit.lost_focus() {
                            rename_done = Some((entity, Some(r.text.clone())));
                        }
                    }
                } else {
                    painter.text(
                        egui::pos2(text_x, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &name,
                        egui::TextStyle::Body.resolve(ui.style()),
                        theme::colors::TEXT,
                    );
                }

                if !searching && carrying && response.contains_pointer() {
                    painter.rect_stroke(
                        rect,
                        theme::sizes::RADIUS,
                        egui::Stroke::new(1.0, theme::colors::ACCENT),
                        egui::StrokeKind::Inside,
                    );
                    if released {
                        released_on_row = Some(entity);
                    }
                }
            }
            // Fill the rest of the panel so empty space below the rows is a target.
            ui.allocate_space(ui.available_size());
        });
        if released && (released_on_row.is_some() || zone.response.contains_pointer()) {
            if let Some(dragged) = carried_entity {
                reparent = Some((dragged, released_on_row));
            }
            if let Some(path) = carried_model {
                model_drop = Some((path, released_on_row));
            }
            if let (Some(path), Some(row)) = (carried_material, released_on_row) {
                material_drop = Some((path, row));
            }
        }

        // A material dropped on a row goes on every part of that entity.
        if let Some((path, entity)) = material_drop {
            self.assign_material_step(world, entity, &path, None);
        }

        if let Some((entity, new_name)) = rename_done {
            self.renaming = None;
            if let Some(text) = new_name
                && let Err(message) = self.history.rename(world, entity, &text, self.selected)
            {
                self.set_status_warning(message);
            }
        }

        if let Some((dragged, target)) = reparent
            && target != Some(dragged)
            && let Err(message) = self.history.reparent(world, dragged, target, self.selected)
        {
            self.set_status_warning(message);
        }
        if let Some((path, parent)) = model_drop
            && content::is_model_path(&path)
        {
            self.spawn_model_step(world, &path, glam::Vec3::ZERO, parent);
        }

        if let Some((action, entity)) = row_action {
            self.row_action(world, action, entity);
        }
    }

    /// A Hierarchy row menu entry, recorded in the history like its shortcut.
    fn row_action(&mut self, world: &mut World, action: RowAction, entity: Entity) {
        match action {
            RowAction::Rename => {
                self.selected = Some(entity);
                self.start_rename(world);
            }
            RowAction::Duplicate => {
                self.selected = Some(entity);
                self.duplicate_selected(world);
            }
            RowAction::Delete => {
                self.history.delete(world, entity, self.selected);
                if self.selected == Some(entity) {
                    self.clear_selection();
                }
            }
            RowAction::CreateChild => self.create_cube_step_in(world, Some(entity)),
        }
    }

    /// The History panel: every step, the applied ones solid; a click goes there.
    fn build_history(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let cursor = self.history.cursor();
        let mut jump = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if ui.selectable_label(cursor == 0, "Initial state").clicked() {
                    jump = Some(0);
                }
                for (i, label) in self.history.labels().enumerate() {
                    let text = if i < cursor {
                        egui::RichText::new(label)
                    } else {
                        egui::RichText::new(label).weak()
                    };
                    if ui.selectable_label(i + 1 == cursor, text).clicked() {
                        jump = Some(i + 1);
                    }
                }
            });
        if let Some(target) = jump {
            let selection = self.history.jump_to(world, target);
            self.adopt_history_selection(selection);
        }
    }

    fn build_inspector(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let asset = self.content.selected().and_then(|path| {
            world
                .get_resource::<AssetServer>()
                .and_then(|server| content::asset_details(server, path))
                .map(|details| (path.to_path_buf(), details))
        });
        // The card shows one file; keep nothing else.
        self.mesh_counts
            .retain(|cached, _| asset.as_ref().is_some_and(|(path, _)| path == cached));
        if let Some((path, details)) = &asset {
            self.asset_card(ui, path, details);
            if details.kind == content::AssetKind::Material {
                material_card::show(ui, path, world.get_resource::<AssetServer>());
            }
        }
        let Some(entity) = self.selected else {
            if asset.is_none() {
                let label = ui.weak("Nothing selected");
                ui.interact(
                    label.rect,
                    egui::Id::new("inspector_nothing"),
                    egui::Sense::hover(),
                );
            }
            return;
        };
        // Read before borrowing the entity mutably (the server is a world resource).
        let current_model = world
            .get::<Handle<Model>>(entity)
            .zip(world.get_resource::<AssetServer>())
            .map(|(h, server)| server.path(*h).to_string());
        let mut new_model: Option<String> = None;
        let mut ping: Option<std::path::PathBuf> = None;
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            self.clear_selection();
            return;
        };

        if let Some(name) = entity_mut.get::<Name>() {
            ui.label(egui::RichText::new(&name.0).strong().size(15.0));
            ui.add_space(2.0);
        }

        let mut card_fills = theme::colors::INSPECTOR_CARDS;
        let mut transform_edited = false;
        if let Some(mut transform) = entity_mut.get_mut::<Transform>() {
            // Edit a copy and write back only on change, so change detection
            // does not fire every frame just because the inspector is open.
            let mut t = *transform;
            inspector_card(ui, &mut card_fills, "Transform", |ui| {
                if let Some(current) = &current_model {
                    ui.label("Mesh");
                    // The model's file, for "show" (none for `builtin:` or a missing file).
                    let file = self
                        .project_root
                        .join(melon_core::assets::split_mesh_fragment(current).0);
                    let file = (!current.starts_with(melon_core::assets::BUILTIN_PREFIX)
                        && file.is_file())
                    .then_some(file);
                    let outside = file.as_ref().is_some_and(|f| !self.content.inside_root(f));
                    ui.horizontal(|ui| {
                        let show = ui
                            .add_enabled(file.is_some() && !outside, egui::Button::new("Show"))
                            .on_hover_text("Show the model in the Content Browser")
                            .on_disabled_hover_text(if outside {
                                "The file is outside assets/, which the Content Browser shows"
                            } else {
                                "No file to show"
                            });
                        if show.clicked() {
                            ping = file.clone();
                        }
                        let combo = egui::ComboBox::from_id_salt("inspector_mesh")
                            .selected_text(model_label(current))
                            .width(ui.available_width())
                            .show_ui(ui, |ui| {
                                // Listed only while the dropdown is open.
                                for path in content::model_list(&self.project_root) {
                                    if ui
                                        .selectable_label(*current == path, model_label(&path))
                                        .on_hover_text(&path)
                                        .clicked()
                                    {
                                        new_model = Some(path);
                                    }
                                }
                            });
                        // A model dragged from the Content Browser replaces it.
                        let drop = ui.interact(
                            combo.response.rect,
                            egui::Id::new("inspector_mesh_drop"),
                            egui::Sense::hover(),
                        );
                        if let Some(drag) = drop.dnd_release_payload::<content::AssetDrag>()
                            && let Some(model) = &drag.model
                            && content::is_model_path(model)
                        {
                            new_model = Some(model.clone());
                        }
                    });
                    ui.end_row();
                }

                ui.label("Position");
                transform_edited |=
                    vec3_field(ui, &mut t.translation, glam::Vec3::ZERO, 0.05, None);
                ui.end_row();

                ui.label("Rotation");
                let (x, y, z) = t.rotation.to_euler(glam::EulerRot::XYZ);
                let mut degrees = glam::Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
                if vec3_field(ui, &mut degrees, glam::Vec3::ZERO, 0.5, None) {
                    t.rotation = glam::Quat::from_euler(
                        glam::EulerRot::XYZ,
                        degrees.x.to_radians(),
                        degrees.y.to_radians(),
                        degrees.z.to_radians(),
                    );
                    transform_edited = true;
                }
                ui.end_row();

                ui.label("Scale");
                // Zero scale is rejected by scene validation (and Play).
                transform_edited |=
                    vec3_field(ui, &mut t.scale, glam::Vec3::ONE, 0.01, Some(0.001));
                ui.end_row();
            });
            if transform_edited {
                *transform = t;
            }
        }

        // Every other registered component, by display name. The registry is
        // an Arc cloned out of the world, so the world can change meanwhile.
        let mut components_changed = false;
        if let Some(registry) = world.get_resource::<AppTypeRegistry>().cloned() {
            let infos = melon_core::registered_components(world);
            let registry = registry.read();
            for info in infos {
                if info.type_path == Transform::type_path() {
                    continue;
                }
                let Some(reflect) = registry
                    .get_with_type_path(info.type_path)
                    .and_then(|r| r.data::<ReflectComponent>())
                else {
                    continue;
                };
                let Some(value) = reflect.reflect(world.entity(entity)) else {
                    continue;
                };
                // Edit a copy and apply only on change (quiet change detection).
                let mut copy = value
                    .reflect_clone()
                    .map(|v| v.into_partial_reflect())
                    .unwrap_or_else(|_| value.to_dynamic());
                let id = egui::Id::new(("inspector_component", info.type_path));
                let widgets = self.field_widgets(world, info.type_path);
                let (changed, remove) =
                    component_card(ui, &mut card_fills, &info.display_name, id, |ui| {
                        inspect::reflect_ui_with(ui, id, copy.as_mut(), &registry, &widgets)
                    });
                if remove {
                    reflect.remove(&mut world.entity_mut(entity));
                } else if changed {
                    reflect.apply(world.entity_mut(entity), copy.as_ref());
                }
                components_changed |= changed || remove;
            }
            if self
                .add_component_ui(ui, world, entity, &registry)
                .is_some()
            {
                components_changed = true;
            }
        }
        if components_changed {
            // Rapier follows the edit now, not at the next frame.
            melon_physics::sync_components_system(world);
        }

        if transform_edited {
            self.after_transform_edit(world, entity);
        }
        if let Some(path) = new_model {
            let handle: Handle<Model> = world.resource_mut::<AssetServer>().load(&path);
            world.entity_mut(entity).insert(handle);
        }
        if let Some(file) = ping
            && self.content.ping(&file)
        {
            self.focus_content_tab = true;
        }
    }

    /// "Add Component" button and its searchable popup; inserts the chosen
    /// component's default value. Returns the type path it added.
    fn add_component_ui(
        &mut self,
        ui: &mut egui::Ui,
        world: &mut World,
        entity: Entity,
        registry: &TypeRegistry,
    ) -> Option<&'static str> {
        ui.add_space(theme::sizes::SPACING);
        let button = ui.add_sized(
            [ui.available_width(), 0.0],
            egui::Button::new("Add Component"),
        );
        // Fixed id over the button (widget ids are positional).
        ui.interact(
            button.rect,
            egui::Id::new("inspector_add_component"),
            egui::Sense::hover(),
        );
        let popup_id = egui::Id::new("inspector_add_popup");
        let mut chosen = None;
        egui::Popup::from_toggle_button_response(&button)
            .id(popup_id)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .width(button.rect.width())
            .show(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.component_search)
                        .id(egui::Id::new("inspector_add_search"))
                        .hint_text("Search")
                        .desired_width(f32::INFINITY),
                );
                let choices = add_component_choices(world, entity, &self.component_search);
                if choices.is_empty() {
                    ui.weak("No components to add");
                }
                for c in choices {
                    let row = egui::Id::new(("inspector_add", c.type_path));
                    if content_view::id_label(ui, row, &c.display_name, false).clicked() {
                        chosen = Some(c.type_path);
                    }
                }
            });
        let path = chosen?;
        egui::Popup::close_id(ui.ctx(), popup_id);
        self.component_search.clear();
        let registration = registry.get_with_type_path(path)?;
        let component = registration.data::<ReflectComponent>()?;
        let default = registration.data::<ReflectDefault>()?.default();
        component.insert(
            &mut world.entity_mut(entity),
            default.as_partial_reflect(),
            registry,
        );
        if path == <melon_core::Collider as melon_core::bevy_reflect::TypePath>::type_path() {
            fit_collider_to_model(world, entity);
        }
        Some(path)
    }

    /// After an inspector edit of `Transform`: push it into rapier and snap
    /// `PreviousTransform`, like the gizmo does, so paused edits survive resume.
    fn after_transform_edit(&self, world: &mut World, entity: Entity) {
        if world.get::<Transform>(entity).is_none() {
            return;
        }
        // The Inspector edits the local transform; rapier wants the world pose.
        let t = Transform::from_mat4(world_matrix(world, entity));
        self.write_to_physics_body(world, entity, Some(t.translation), Some(t.rotation));
        snap_previous_transform(world, entity);
    }

    fn build_gizmo(&mut self, world: &mut World, ui: &mut egui::Ui, vp: &viewport::ViewportInput) {
        self.overlay_draws.clear();
        let Some(entity) = self.selected else {
            return;
        };
        if world.get_entity(entity).is_err() {
            self.clear_selection();
            return;
        }
        if world.get::<Transform>(entity).is_none() {
            return;
        }
        let Some(camera) = world.get_resource::<Camera>().copied() else {
            return;
        };
        // The gizmo works on the world pose (parents included).
        let transform = Transform::from_mat4(world_matrix(world, entity));
        let screen = vp.size;
        let clip = camera.view_proj() * transform.translation.extend(1.0);
        if clip.w <= 0.0 || !transform.translation.is_finite() || !transform.rotation.is_finite() {
            // Behind the camera, or a degenerate (zero-scale) pose: no gizmo.
            self.gizmo_drag = None;
            self.hovered = None;
            return;
        }
        let world_per_pixel = 2.0 * (camera.fov_y_radians * 0.5).tan() * clip.w / screen.y;
        let pose = GizmoPose {
            center: transform.translation,
            rotation: transform.rotation,
            size: GIZMO_SIZE_PX * world_per_pixel,
            view: (transform.translation - camera.eye).normalize_or_zero(),
        };

        let pointer = vp.pointer;
        let primary_down = ui.ctx().input(|i| i.pointer.primary_down());
        let primary_pressed = ui.ctx().input(|i| i.pointer.primary_pressed());
        let snap = ui
            .ctx()
            .input(|i| i.key_down(egui::Key::ShiftLeft) || i.key_down(egui::Key::ShiftRight));
        // Hover or start a drag only over the viewport, never while orbiting.
        let wants_pointer = !vp.hovered || vp.alt || vp.typing;
        let ray = |p: glam::Vec2| camera.screen_to_world_ray(p, screen);

        if let Some(mut drag) = self.gizmo_drag {
            if !primary_down {
                self.gizmo_drag = None;
            } else if let Some(p) = pointer {
                let (origin, dir) = ray(p);
                if let Some(next) = self.drag_gizmo(&mut drag, &pose, origin, dir, p, snap) {
                    set_world_transform(world, entity, next);
                    snap_previous_transform(world, entity);
                    let now = Transform::from_mat4(world_matrix(world, entity));
                    self.write_to_physics_body(
                        world,
                        entity,
                        Some(now.translation),
                        Some(now.rotation),
                    );
                }
                self.gizmo_drag = Some(drag);
            }
        }

        if self.gizmo_drag.is_none() {
            self.hovered = None;
            if !wants_pointer && let Some(p) = pointer {
                let (origin, dir) = ray(p);
                let tolerance = PICK_TOLERANCE_PX * world_per_pixel;
                self.hovered = pick_handle(self.gizmo_mode, &pose, origin, dir, tolerance);
                if primary_pressed && let Some(handle) = self.hovered {
                    self.gizmo_drag = Self::start_gizmo_drag(
                        self.gizmo_mode,
                        handle,
                        transform,
                        &pose,
                        &camera,
                        origin,
                        dir,
                        p,
                        screen,
                    );
                }
            }
        }

        let highlight = self.gizmo_drag.map(|d| d.handle).or(self.hovered);
        self.overlay_draws = gizmo_draws(self.gizmo_mode, &pose, highlight);

        if self.gizmo_drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if self.hovered.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    /// Set up a drag of `handle`: the plane its pointer hits are measured on
    /// and the values at the press.
    #[allow(clippy::too_many_arguments)]
    fn start_gizmo_drag(
        mode: GizmoMode,
        handle: GizmoHandle,
        start: Transform,
        pose: &GizmoPose,
        camera: &Camera,
        origin: glam::Vec3,
        dir: glam::Vec3,
        pointer: glam::Vec2,
        screen: glam::Vec2,
    ) -> Option<GizmoDrag> {
        let center = pose.center;
        let (plane_normal, rotate_axis) = match (mode, handle) {
            (GizmoMode::Rotate, GizmoHandle::Axis(i)) => (pose.axis(i), pose.axis(i)),
            (_, GizmoHandle::Axis(i)) => (
                compute_drag_plane(center, pose.axis(i), camera.forward()).1,
                pose.axis(i),
            ),
            (_, GizmoHandle::Plane(i)) => (pose.axis(i), pose.axis(i)),
            (_, GizmoHandle::Center) => (camera.forward(), camera.forward()),
        };
        let press_hit = ray_plane_intersect(origin, dir, center, plane_normal).unwrap_or(center);
        let press_angle = (mode == GizmoMode::Rotate)
            .then(|| rotate_angle(center, rotate_axis, origin, dir))
            .flatten();
        // Edge-on rings: drag along the ring's on-screen line (perpendicular
        // to the axis as seen on screen).
        let ring_tangent_px = match (
            camera.world_to_screen(center, screen),
            camera.world_to_screen(center + rotate_axis * pose.size, screen),
        ) {
            (Some(a), Some(b)) => glam::Vec2::new(-(b - a).y, (b - a).x),
            _ => glam::Vec2::X,
        };
        Some(GizmoDrag {
            handle,
            mode,
            start,
            plane_normal,
            press_hit,
            press_pointer: pointer,
            press_angle,
            last_angle: press_angle,
            angle_sum: 0.0,
            ring_tangent_px,
        })
    }

    /// The new world pose for this pointer position, or `None` to leave it.
    fn drag_gizmo(
        &self,
        drag: &mut GizmoDrag,
        pose: &GizmoPose,
        origin: glam::Vec3,
        dir: glam::Vec3,
        pointer: glam::Vec2,
        snap: bool,
    ) -> Option<Transform> {
        let start = drag.start;
        let center = start.translation;
        let hit = || ray_plane_intersect(origin, dir, center, drag.plane_normal);
        let axes_of = |i: usize| ((i + 1) % 3, (i + 2) % 3);
        let start_axis = |i: usize| start.rotation * [AXIS_X, AXIS_Y, AXIS_Z][i];
        Some(match (drag.mode, drag.handle) {
            (GizmoMode::Move, GizmoHandle::Axis(i)) => {
                drag_move_axis(start, start_axis(i), drag.press_hit, hit()?, snap)
            }
            (GizmoMode::Move, GizmoHandle::Plane(i)) => {
                let (a, b) = axes_of(i);
                drag_move_plane(
                    start,
                    drag.press_hit,
                    hit()?,
                    [start_axis(a), start_axis(b)],
                    snap,
                )
            }
            (GizmoMode::Move, GizmoHandle::Center) => {
                drag_move_center(start, drag.press_hit, hit()?, snap)
            }
            (GizmoMode::Rotate, GizmoHandle::Axis(i)) => {
                let axis = start_axis(i);
                match drag.press_angle {
                    Some(_) => {
                        // Accumulate small steps so turns past 180° keep going.
                        if let (Some(last), Some(now)) =
                            (drag.last_angle, rotate_angle(center, axis, origin, dir))
                        {
                            drag.angle_sum += wrap_angle_delta(now - last);
                            drag.last_angle = Some(now);
                        }
                    }
                    None => {
                        drag.angle_sum =
                            screen_drag_angle(pointer - drag.press_pointer, drag.ring_tangent_px);
                    }
                }
                drag_rotate(start, axis, drag.angle_sum, snap)
            }
            (GizmoMode::Rotate, _) => return None,
            (GizmoMode::Scale, GizmoHandle::Axis(i)) => {
                let axis = start_axis(i);
                let press = project_onto_axis(drag.press_hit - center, axis);
                if press.abs() < 1e-6 {
                    return None;
                }
                drag_scale(
                    start,
                    &[i],
                    project_onto_axis(hit()? - center, axis) / press,
                    snap,
                )
            }
            (GizmoMode::Scale, GizmoHandle::Plane(i)) => {
                let (a, b) = axes_of(i);
                let diagonal = (start_axis(a) + start_axis(b)).normalize();
                let press = (drag.press_hit - center).dot(diagonal);
                if press.abs() < 1e-6 {
                    return None;
                }
                drag_scale(
                    start,
                    &[a, b],
                    (hit()? - center).dot(diagonal) / press,
                    snap,
                )
            }
            (GizmoMode::Scale, GizmoHandle::Center) => {
                let factor = uniform_scale_factor(pointer - drag.press_pointer);
                drag_scale(start, &[0, 1, 2], factor, snap)
            }
        })
        .filter(|_| pose.size.is_finite())
    }

    fn write_to_physics_body(
        &self,
        world: &mut World,
        entity: Entity,
        translation: Option<glam::Vec3>,
        rotation: Option<glam::Quat>,
    ) {
        // Issue M7: clone handle before borrowing PhysicsWorld mutably
        let handle = world
            .get::<melon_physics::RigidBodyComponent>(entity)
            .map(|c| c.body_handle);
        if handle.is_none() {
            return;
        }
        let handle = handle.unwrap();
        // Write in every state, so paused edits are not undone on resume.

        if let Some(mut physics) = world.get_resource_mut::<melon_physics::PhysicsWorld>()
            && let Some(body) = physics.bodies.get_mut(handle)
        {
            if let Some(t) = translation {
                body.set_translation(t, true);
            }
            if let Some(r) = rotation {
                body.set_rotation(r, true);
            }
        }
    }
}

/// EditorOverlay handles the egui-wgpu rendering plumbing
pub struct EditorOverlay {
    pub ctx: egui::Context,
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    paint_jobs: Vec<egui::ClippedPrimitive>,
    screen_descriptor: ScreenDescriptor,
    viewport_texture: Option<egui::TextureId>,
}

impl EditorOverlay {
    #[instrument(skip(window, device), level = "info")]
    pub fn new(
        window: &Arc<Window>,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let state =
            egui_winit::State::new(ctx.clone(), ViewportId::ROOT, &**window, None, None, None);
        let renderer = egui_wgpu::Renderer::new(
            device,
            surface_format.add_srgb_suffix(),
            egui_wgpu::RendererOptions {
                msaa_samples: 0,
                ..Default::default()
            },
        );
        let size = window.inner_size();
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [size.width, size.height],
            pixels_per_point: window.scale_factor() as f32,
        };
        Self {
            ctx,
            state,
            renderer,
            paint_jobs: Vec::new(),
            screen_descriptor,
            viewport_texture: None,
        }
    }

    /// Give egui a thumbnail view: updates `existing`, or registers a new
    /// texture. Returns its id.
    pub fn register_thumbnail(
        &mut self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        existing: Option<egui::TextureId>,
    ) -> egui::TextureId {
        match existing {
            Some(id) => {
                self.renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    view,
                    wgpu::FilterMode::Linear,
                    id,
                );
                id
            }
            None => self
                .renderer
                .register_native_texture(device, view, wgpu::FilterMode::Linear),
        }
    }

    /// Show `target`'s colour texture in the viewport panel. Call again after
    /// the target is recreated (resize).
    pub fn set_viewport_target(
        &mut self,
        device: &wgpu::Device,
        target: &melon_render::ViewportTarget,
    ) {
        let view = target.ui_view();
        match self.viewport_texture {
            Some(id) => self.renderer.update_egui_texture_from_wgpu_texture(
                device,
                view,
                wgpu::FilterMode::Linear,
                id,
            ),
            None => {
                self.viewport_texture = Some(self.renderer.register_native_texture(
                    device,
                    view,
                    wgpu::FilterMode::Linear,
                ))
            }
        }
    }

    /// Forward a winit event to egui. Returns `true` if egui consumed the event
    /// (e.g. mouse click on an egui panel). The engine should skip processing
    /// consumed events.
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        response.consumed
    }

    /// Run egui, update textures and buffers. Call this once per frame before
    /// the egui render pass. The `editor` is used to build the UI.
    pub fn update(
        &mut self,
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        world: &mut World,
        editor: &mut Editor,
    ) {
        editor.viewport_texture = self.viewport_texture;
        let raw_input = self.state.take_egui_input(window);
        let full_output = self.ctx.run_ui(raw_input, |ui| {
            editor.build_ui(world, ui);
        });

        self.state
            .handle_platform_output(window, full_output.platform_output);

        let pixels_per_point = window.scale_factor() as f32;
        self.paint_jobs = self.ctx.tessellate(full_output.shapes, pixels_per_point);
        self.screen_descriptor = ScreenDescriptor {
            size_in_pixels: [window.inner_size().width, window.inner_size().height],
            pixels_per_point,
        };

        for (id, image_delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }

        self.renderer.update_buffers(
            device,
            queue,
            encoder,
            &self.paint_jobs,
            &self.screen_descriptor,
        );
    }

    /// Render egui into an existing render pass. The pass must have `'static`
    /// lifetime (use `pass.forget_lifetime()`).
    pub fn render(&self, pass: &mut wgpu::RenderPass<'static>) {
        self.renderer
            .render(pass, &self.paint_jobs, &self.screen_descriptor);
    }
}

fn sim_state(world: &World) -> SimulationState {
    world
        .get_resource::<SimulationState>()
        .copied()
        .unwrap_or_default()
}

/// Spawn a unit cube at the origin, named "Cube", then "Cube (1)", "Cube (2)"
/// (first free among its siblings).
pub fn create_cube(world: &mut World) -> Entity {
    create_cube_in(world, None)
}

/// `create_cube` as a child of `parent` (a root for `None`).
pub fn create_cube_in(world: &mut World, parent: Option<Entity>) -> Entity {
    let name = history::unused_name(world, parent, "Cube", true);
    let cube: Handle<Model> = world
        .get_resource_or_insert_with(|| AssetServer::new(".".into()))
        .load("builtin:cube");
    let id = world
        .spawn((
            Name(name),
            melon_core::Transform::default(),
            cube,
            Aabb {
                half_extents: glam::Vec3::new(0.5, 0.5, 0.5),
            },
        ))
        .id();
    if let Some(p) = parent {
        world.entity_mut(id).insert(melon_core::ChildOf(p));
    }
    id
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LightKind {
    Point,
    Spot,
}

/// Spawn a point or spot light 3 m up at the origin, named "Point Light" /
/// "Spot Light" (a spot light points straight down).
fn create_light(world: &mut World, kind: LightKind) -> Entity {
    let name = history::unused_name(
        world,
        None,
        match kind {
            LightKind::Point => "Point Light",
            LightKind::Spot => "Spot Light",
        },
        true,
    );
    let mut transform = melon_core::Transform::from_translation(glam::Vec3::new(0.0, 3.0, 0.0));
    let mut entity = world.spawn_empty();
    match kind {
        LightKind::Point => entity.insert(melon_core::PointLight::default()),
        LightKind::Spot => {
            transform.rotation = glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
            entity.insert(melon_core::SpotLight::default())
        }
    };
    entity.insert((Name(name), transform));
    entity.id()
}

/// Registered components Add Component offers for `entity`: addable, not on
/// it yet, display name containing `search` (any case).
fn add_component_choices(
    world: &World,
    entity: Entity,
    search: &str,
) -> Vec<melon_core::ComponentInfo> {
    let Some(registry) = world.get_resource::<AppTypeRegistry>() else {
        return Vec::new();
    };
    let infos = melon_core::registered_components(world);
    let registry = registry.read();
    let search = search.to_lowercase();
    infos
        .into_iter()
        .filter(|c| c.addable && c.display_name.to_lowercase().contains(&search))
        .filter(|c| {
            registry
                .get_with_type_path(c.type_path)
                .and_then(|r| r.data::<ReflectComponent>())
                .is_some_and(|r| !r.contains(world.entity(entity)))
        })
        .collect()
}

/// A new `Collider` on an entity with a measured model: its box takes the
/// model's half size (the bounds' centre is ignored), else it keeps 0.5.
fn fit_collider_to_model(world: &mut World, entity: Entity) {
    let bounds = world
        .get::<Handle<Model>>(entity)
        .zip(world.get_resource::<AssetServer>())
        .and_then(|(h, server)| server.bounds(*h));
    if let (Some((min, max)), Some(mut c)) = (bounds, world.get_mut::<melon_core::Collider>(entity))
    {
        c.shape = melon_core::ColliderShape::Box {
            half_extents: (max - min) * 0.5,
        };
    }
}

fn card_frame(fill: egui::Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, theme::colors::BORDER))
        .corner_radius(theme::sizes::RADIUS)
        .inner_margin(theme::sizes::SPACING)
}

/// A registered component's card: header with its name and a remove "×"
/// (fixed ids `id.with("header")`, `id.with("remove")`), then `body`.
/// Returns (what `body` returned, remove clicked).
fn component_card(
    ui: &mut egui::Ui,
    fills: &mut [egui::Color32; 2],
    title: &str,
    id: egui::Id,
    body: impl FnOnce(&mut egui::Ui) -> bool,
) -> (bool, bool) {
    let mut changed = false;
    let mut remove = false;
    card_frame(fills[0]).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            id.with("open"),
            true,
        )
        .show_header(ui, |ui| {
            let label = ui.label(title);
            ui.interact(label.rect, id.with("header"), egui::Sense::hover());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                remove = content_view::id_label(ui, id.with("remove"), "\u{d7}", false)
                    .on_hover_text("Remove component")
                    .clicked();
            });
        })
        .body_unindented(|ui| changed = body(ui));
    });
    fills.swap(0, 1);
    (changed, remove)
}

fn inspector_card(
    ui: &mut egui::Ui,
    fills: &mut [egui::Color32; 2],
    title: &str,
    rows: impl FnOnce(&mut egui::Ui),
) {
    card_frame(fills[0]).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        egui::CollapsingHeader::new(title)
            .id_salt(title)
            .default_open(true)
            .show_unindented(ui, |ui| {
                egui::Grid::new(("inspector_rows", title))
                    .num_columns(2)
                    .spacing([8.0, theme::sizes::SPACING])
                    .with_row_color(|row, _| Some(theme::colors::INSPECTOR_ROWS[row % 2]))
                    .show(ui, rows);
            });
    });
    fills.swap(0, 1);
}

impl Editor {
    /// The Content Browser's selected file or folder, read-only.
    fn asset_card(
        &mut self,
        ui: &mut egui::Ui,
        path: &std::path::Path,
        details: &content::AssetDetails,
    ) {
        let model = details.kind == content::AssetKind::Model;
        // Read from the file (not the GPU cache, which only knows what is
        // drawn), again only when its modified time changes.
        let info = model.then(|| {
            let stale = self
                .mesh_counts
                .get(path)
                .is_none_or(|(modified, _)| *modified != details.modified);
            if stale {
                let read = melon_render::gltf_nodes(path).and_then(|nodes| {
                    let meshes = nodes
                        .iter()
                        .filter_map(|n| n.mesh)
                        .collect::<std::collections::HashSet<_>>()
                        .len();
                    Ok((meshes, melon_render::gltf_bounds(path)?))
                });
                self.mesh_counts.insert(
                    path.to_path_buf(),
                    (details.modified, read.map_err(|e| format!("{e:#}"))),
                );
            }
            self.mesh_counts[path].1.clone()
        });
        let meshes = info.as_ref().map(|info| match info {
            Ok((count, _)) => count.to_string(),
            Err(error) => error.clone(),
        });
        let bounds = info.as_ref().map(|info| match info {
            Ok((_, Some((min, max)))) => format!(
                "({:.2}, {:.2}, {:.2}) \u{2026} ({:.2}, {:.2}, {:.2})",
                min.x, min.y, min.z, max.x, max.y, max.z
            ),
            Ok((_, None)) => "no meshes".to_string(),
            Err(_) => "unknown".to_string(),
        });
        let mut fills = theme::colors::INSPECTOR_CARDS;
        inspector_card(ui, &mut fills, "Asset", |ui| {
            let name = ui.label("Name");
            ui.interact(
                name.rect,
                egui::Id::new("inspector_asset_card"),
                egui::Sense::hover(),
            );
            inspector_value(ui, &details.name);
            ui.end_row();
            ui.label("Path");
            inspector_value(ui, &details.path);
            ui.end_row();
            ui.label("Type");
            inspector_value(ui, details.kind.label());
            ui.end_row();
            if details.kind != content::AssetKind::Folder {
                ui.label("Size");
                inspector_value(ui, content::format_size(details.size));
                ui.end_row();
            }
            ui.label("Modified");
            inspector_value(
                ui,
                details
                    .modified
                    .map_or_else(|| "unknown".to_string(), content::format_modified),
            );
            ui.end_row();
            ui.label("GUID");
            inspector_value(
                ui,
                details
                    .guid
                    .map_or_else(|| "none".to_string(), |g| g.to_string()),
            );
            ui.end_row();
            if let Some(meshes) = &meshes {
                ui.label("Meshes");
                inspector_value(ui, meshes);
                ui.end_row();
            }
            if let Some(bounds) = &bounds {
                ui.label("Bounds");
                inspector_value(ui, bounds);
                ui.end_row();
            }
        });
    }
}

/// Fill the value column so read-only rows get the same band width as editable rows.
fn inspector_value(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        ui.add(egui::Label::new(text).wrap());
    });
}

/// One inspector row of three colour-coded X / Y / Z drag fields sharing the
/// row's width. Every field is always drawn (no short-circuit). Returns true
/// if any value changed.
fn vec3_field(
    ui: &mut egui::Ui,
    value: &mut glam::Vec3,
    default: glam::Vec3,
    speed: f64,
    min: Option<f32>,
) -> bool {
    let mut changed = false;
    let axes = [
        ("X", theme::colors::AXIS_X),
        ("Y", theme::colors::AXIS_Y),
        ("Z", theme::colors::AXIS_Z),
    ];
    ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        let spacing = ui.spacing().item_spacing.x;
        let field_width = ((ui.available_width() - 3.0 * (12.0 + 2.0 * spacing)) / 3.0).max(48.0);
        for (i, (label, color)) in axes.into_iter().enumerate() {
            ui.label(egui::RichText::new(label).color(color).strong());
            changed |=
                resettable_drag_value(ui, &mut value[i], default[i], speed, min, field_width)
                    .changed();
        }
    });
    changed
}

fn resettable_drag_value(
    ui: &mut egui::Ui,
    value: &mut f32,
    default: f32,
    speed: f64,
    min: Option<f32>,
    width: f32,
) -> egui::Response {
    let mut drag = egui::DragValue::new(value).speed(speed).max_decimals(3);
    if let Some(min) = min {
        drag = drag.range(min..=f32::MAX);
    }
    let mut response = ui
        .add_sized([width, theme::sizes::INPUT_HEIGHT], drag)
        .on_hover_text("Double-click to reset");
    if response.double_clicked() {
        // The first click opens DragValue's text editor. Discard its buffer
        // before it can commit the old text on losing focus.
        ui.data_mut(|data| data.remove_temp::<String>(response.id));
        response.surrender_focus();
        let reset = min.map_or(default, |min| default.max(min));
        if *value != reset {
            *value = reset;
            response.mark_changed();
        }
    }
    response
}

/// Severity of a status-bar message (its colour).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Success,
    Warning,
    Error,
}

/// Spawn the model at `path`: a root entity named after the file at `at`
/// (local to `parent`, if given) with the file's glTF node tree as children,
/// each node with a mesh drawing `<path>#mesh<N>`. A file that can't be read
/// spawns one entity drawing the whole file (placeholder until it loads).
pub fn spawn_model(
    world: &mut World,
    project_root: &std::path::Path,
    path: &str,
    at: glam::Vec3,
    parent: Option<Entity>,
) -> Entity {
    let name = std::path::Path::new(path)
        .file_stem()
        .map_or_else(|| "Model".to_string(), |s| s.to_string_lossy().into_owned());
    let nodes = (!path.starts_with(melon_core::assets::BUILTIN_PREFIX))
        .then(|| melon_render::gltf_nodes(&project_root.join(path)).ok())
        .flatten();
    world.get_resource_or_insert_with(|| AssetServer::new(".".into()));
    let root = world
        .spawn((Name(name), Transform::from_translation(at)))
        .id();
    if let Some(p) = parent {
        world.entity_mut(root).insert(melon_core::ChildOf(p));
    }
    let Some(nodes) = nodes else {
        let handle: Handle<Model> = world.resource_mut::<AssetServer>().load(path);
        world.entity_mut(root).insert(handle);
        return root;
    };
    let mut ids = Vec::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate() {
        let parent = node.parent.map_or(root, |p| ids[p]);
        let id = world
            .spawn((
                Name(node.name.clone().unwrap_or_else(|| format!("Node {i}"))),
                Transform {
                    translation: node.translation,
                    rotation: node.rotation,
                    // Scenes reject zero scale; a hidden (scale 0) node stays
                    // practically invisible at 1e-4.
                    scale: node.scale.map(|c| if c.abs() < 1e-4 { 1e-4 } else { c }),
                },
                melon_core::ChildOf(parent),
            ))
            .id();
        if let Some(mesh) = node.mesh {
            let handle: Handle<Model> = world
                .resource_mut::<AssetServer>()
                .load(&format!("{path}#mesh{mesh}"));
            world.entity_mut(id).insert(handle);
        }
        ids.push(id);
    }
    root
}

/// Hierarchy drag-and-drop: put `dragged` under `target` (or make it a root)
/// without moving it in the world, and move its physics body to match.
/// `Err` is a status-bar warning; nothing changes then.
pub fn hierarchy_drop(
    world: &mut World,
    dragged: Entity,
    target: Option<Entity>,
) -> Result<(), String> {
    set_parent_keep_world(world, dragged, target)?;
    snap_previous_transform(world, dragged);
    let pose = Transform::from_mat4(world_matrix(world, dragged));
    if let Some(handle) = world
        .get::<melon_physics::RigidBodyComponent>(dragged)
        .map(|c| c.body_handle)
        && let Some(mut physics) = world.get_resource_mut::<melon_physics::PhysicsWorld>()
        && let Some(body) = physics.bodies.get_mut(handle)
    {
        body.set_translation(pose.translation, true);
        body.set_rotation(pose.rotation, true);
    }
    Ok(())
}

/// Short label for a model path in the Inspector: the file name, plus the
/// mesh for `#mesh<N>` paths.
fn model_label(path: &str) -> String {
    let (file, mesh) = melon_core::assets::split_mesh_fragment(path);
    let name = file.rsplit('/').next().unwrap_or(file);
    match mesh {
        Some(i) => format!("{name} \u{203a} mesh{i}"),
        None => name.to_string(),
    }
}

/// Editor keyboard shortcuts (F5/F6/Shift+F5 live in `play_controls`, F in the viewport).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Save,
    Delete,
    Move,
    Rotate,
    Scale,
    /// Esc: input focus back to the editor (the game keeps running).
    ReleaseGame,
    /// Ctrl+Space: show or hide the Content Drawer.
    ToggleDrawer,
    /// Ctrl+Z.
    Undo,
    /// Ctrl+Y or Ctrl+Shift+Z.
    Redo,
    /// Ctrl+D.
    Duplicate,
    /// F2: rename in the Hierarchy.
    Rename,
}

/// `key` went down this frame. Key repeat does not count, so a key held since
/// a binding capture never fires its shortcut.
fn fresh_press(i: &egui::InputState, key: egui::Key) -> bool {
    i.events.iter().any(
        |e| matches!(e, egui::Event::Key { key: k, pressed: true, repeat: false, .. } if *k == key),
    )
}

/// Map a pressed key to a shortcut. Nothing fires while typing in a text
/// field; W/E/R are fly keys while the right mouse button flies the camera.
pub fn shortcut_for(
    key: egui::Key,
    ctrl: bool,
    shift: bool,
    typing: bool,
    flying: bool,
) -> Option<Shortcut> {
    if typing {
        return None;
    }
    match key {
        egui::Key::S if ctrl => Some(Shortcut::Save),
        egui::Key::Delete => Some(Shortcut::Delete),
        egui::Key::W if !flying && !ctrl => Some(Shortcut::Move),
        egui::Key::E if !flying && !ctrl => Some(Shortcut::Rotate),
        egui::Key::R if !flying && !ctrl => Some(Shortcut::Scale),
        egui::Key::Escape => Some(Shortcut::ReleaseGame),
        egui::Key::Space if ctrl => Some(Shortcut::ToggleDrawer),
        egui::Key::Z if ctrl && shift => Some(Shortcut::Redo),
        egui::Key::Z if ctrl => Some(Shortcut::Undo),
        egui::Key::Y if ctrl => Some(Shortcut::Redo),
        egui::Key::D if ctrl => Some(Shortcut::Duplicate),
        egui::Key::F2 => Some(Shortcut::Rename),
        _ => None,
    }
}

/// Focus after a frame: a left click on the Scene viewport while Playing
/// gives the game focus; Editing always has editor focus.
pub fn focus_after_click(
    state: SimulationState,
    focus: InputFocus,
    clicked_viewport: bool,
) -> InputFocus {
    match state {
        SimulationState::Editing => InputFocus::Editor,
        SimulationState::Playing if clicked_viewport => InputFocus::Game,
        _ => focus,
    }
}

/// Value of `GameInputGate`: game focus, Playing, nobody typing and the Scene
/// viewport on screen. The host also grabs the cursor while it is true.
fn game_input_gate(
    focus: InputFocus,
    state: SimulationState,
    typing: bool,
    viewport_shown: bool,
) -> bool {
    focus == InputFocus::Game && state == SimulationState::Playing && !typing && viewport_shown
}

fn input_focus(world: &World) -> InputFocus {
    world
        .get_resource::<InputFocus>()
        .copied()
        .unwrap_or_default()
}

/// Routes each dock tab to its panel.
struct EditorTabs<'a> {
    editor: &'a mut Editor,
    world: &'a mut World,
}

impl egui_dock::TabViewer for EditorTabs<'_> {
    type Tab = layout::EditorTab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        use layout::EditorTab;
        match tab {
            EditorTab::Scene => self.editor.viewport_panel(self.world, ui),
            EditorTab::Hierarchy => self.editor.build_hierarchy(self.world, ui),
            EditorTab::Inspector => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| self.editor.build_inspector(self.world, ui));
            }
            EditorTab::ContentBrowser => {
                self.editor.content_shown = true;
                self.editor.content_tab_shown = true;
                self.editor.content.blocked =
                    self.editor.delete_confirm.is_some() || self.editor.confirm_open.is_some();
                let playing = self
                    .world
                    .get_resource::<melon_core::AudioPreview>()
                    .and_then(|p| p.0.clone());
                self.editor.content.set_audio_preview(playing);
                self.editor.content.ui(ui);
                self.editor.apply_content_requests(self.world);
            }
            EditorTab::History => self.editor.build_history(self.world, ui),
            EditorTab::Console => {
                console::console_ui(ui, &self.editor.console, &mut self.editor.console_filter)
            }
        }
    }

    /// The Scene tab fills its panel edge to edge.
    fn clear_background(&self, tab: &Self::Tab) -> bool {
        *tab != layout::EditorTab::Scene
    }

    /// The Inspector stays docked: closing it by accident hides every
    /// property editor, so it has no close button.
    fn is_closeable(&self, tab: &Self::Tab) -> bool {
        *tab != layout::EditorTab::Inspector
    }

    /// Only the Hierarchy relies on the dock's scroll area; the other panels
    /// size themselves (and the viewport must not scroll).
    fn scroll_bars(&self, tab: &Self::Tab) -> [bool; 2] {
        let scroll = *tab == layout::EditorTab::Hierarchy;
        [false, scroll]
    }
}

/// Fill the library-independent viewport input from egui. Buttons count as
/// held only if the press started on the viewport image.
fn viewport_input(
    ctx: &egui::Context,
    response: &egui::Response,
    rect: egui::Rect,
) -> viewport::ViewportInput {
    use egui::{Key, PointerButton};
    // Read everything that touches the egui context *before* `ctx.input`:
    // `input` holds the context's write lock, and `Response::clicked()` takes
    // it again, which deadlocks on the frame a click lands.
    let down_on = response.is_pointer_button_down_on();
    let hovered = response.hovered();
    let clicked = response.clicked();
    let typing = ctx.text_edit_focused();
    ctx.input(|i| viewport::ViewportInput {
        size: glam::Vec2::new(rect.width(), rect.height()),
        pointer: i
            .pointer
            .latest_pos()
            .map(|p| glam::Vec2::new(p.x - rect.min.x, p.y - rect.min.y)),
        pointer_delta: glam::Vec2::new(i.pointer.delta().x, i.pointer.delta().y),
        hovered,
        left_down: down_on && i.pointer.button_down(PointerButton::Primary),
        right_down: down_on && i.pointer.button_down(PointerButton::Secondary),
        middle_down: down_on && i.pointer.button_down(PointerButton::Middle),
        left_clicked: clicked,
        // Wheel notches: positive = wheel away from the user.
        scroll: if hovered {
            i.raw
                .events
                .iter()
                .map(|event| match event {
                    egui::Event::MouseWheel { unit, delta, .. } => match unit {
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Point => delta.y / 50.0,
                        egui::MouseWheelUnit::Page => delta.y * 3.0,
                    },
                    _ => 0.0,
                })
                .sum()
        } else {
            0.0
        },
        alt: i.modifiers.alt,
        shift: i.modifiers.shift,
        keys: viewport::FlyKeys {
            w: i.key_down(Key::W),
            a: i.key_down(Key::A),
            s: i.key_down(Key::S),
            d: i.key_down(Key::D),
            q: i.key_down(Key::Q),
            e: i.key_down(Key::E),
        },
        f_pressed: hovered && !typing && i.key_pressed(Key::F),
        typing,
    })
}

/// Small X/Y/Z indicator showing the camera's orientation.
fn draw_orientation(painter: &egui::Painter, center: egui::Pos2, camera: &Camera) {
    let forward = camera.forward();
    let right = forward.cross(glam::Vec3::Y).normalize_or_zero();
    let up = right.cross(forward);
    let axes = [
        (glam::Vec3::X, egui::Color32::from_rgb(230, 60, 60), "X"),
        (glam::Vec3::Y, egui::Color32::from_rgb(90, 200, 90), "Y"),
        (glam::Vec3::Z, egui::Color32::from_rgb(70, 120, 235), "Z"),
    ];
    for (axis, color, label) in axes {
        let tip = center + egui::vec2(axis.dot(right), -axis.dot(up)) * 30.0;
        painter.line_segment([center, tip], egui::Stroke::new(2.0, color));
        painter.text(
            center + (tip - center) * 1.3,
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            color,
        );
    }
}

/// Turn a finished Save/Load file dialog into an operation. Dropped unless in
/// edit mode: the dialog runs on its own thread, so Play can start while it is open.
fn dialog_operation(
    is_save: bool,
    path: std::path::PathBuf,
    state: SimulationState,
) -> Option<PendingOperation> {
    if state != SimulationState::Editing {
        return None;
    }
    Some(if is_save {
        PendingOperation::Save(path)
    } else {
        PendingOperation::Load(path)
    })
}

/// After a gizmo edit, make `PreviousTransform` match the new pose, so the
/// interpolated draw shows the drag directly instead of trailing one step behind.
fn snap_previous_transform(world: &mut World, entity: Entity) {
    let Some(current) = world.get::<Transform>(entity).copied() else {
        return;
    };
    if let Some(mut prev) = world.get_mut::<melon_core::PreviousTransform>(entity) {
        prev.0 = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::change_detection::DetectChanges;
    use melon_core::Spin;

    #[test]
    fn double_click_resets_drag_value_to_its_field_default() {
        let ctx = egui::Context::default();
        let mut value = 2.5;
        let mut pos = egui::Pos2::ZERO;
        let mut changed = false;
        for frame in 0..7 {
            let events = match frame {
                0 | 6 => vec![],
                1 => vec![egui::Event::PointerMoved(pos)],
                _ => vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: frame % 2 == 0,
                    modifiers: egui::Modifiers::NONE,
                }],
            };
            let input = egui::RawInput {
                time: Some(f64::from(frame) * 0.05),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                let response = resettable_drag_value(ui, &mut value, 1.0, 0.01, Some(0.001), 80.0);
                pos = response.rect.center();
                changed = response.changed();
            });
            assert_eq!(value, if frame < 5 { 2.5 } else { 1.0 });
            assert_eq!(changed, frame == 5);
        }
    }

    fn world_pos(world: &World, e: Entity) -> glam::Vec3 {
        world_matrix(world, e).transform_point3(glam::Vec3::ZERO)
    }

    #[test]
    fn hierarchy_drop_onto_entity_reparents_keeping_world() {
        let mut world = World::new();
        let target = world
            .spawn(Transform::from_translation(glam::Vec3::new(3.0, 0.0, 0.0)))
            .id();
        let dragged = world
            .spawn(Transform::from_translation(glam::Vec3::new(1.0, 2.0, 0.0)))
            .id();
        hierarchy_drop(&mut world, dragged, Some(target)).unwrap();
        assert_eq!(
            world.get::<melon_core::ChildOf>(dragged).unwrap().parent(),
            target
        );
        assert!((world_pos(&world, dragged) - glam::Vec3::new(1.0, 2.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn hierarchy_drop_onto_empty_space_makes_root() {
        let mut world = World::new();
        let parent = world.spawn(Transform::from_translation(glam::Vec3::X)).id();
        let child = world
            .spawn((
                Transform::from_translation(glam::Vec3::Y),
                melon_core::ChildOf(parent),
            ))
            .id();
        hierarchy_drop(&mut world, child, None).unwrap();
        assert!(world.get::<melon_core::ChildOf>(child).is_none());
        assert!((world_pos(&world, child) - glam::Vec3::new(1.0, 1.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn hierarchy_drop_onto_descendant_is_refused() {
        let mut world = World::new();
        let parent = world.spawn(Transform::default()).id();
        let child = world
            .spawn((Transform::default(), melon_core::ChildOf(parent)))
            .id();
        assert!(hierarchy_drop(&mut world, parent, Some(child)).is_err());
        assert!(world.get::<melon_core::ChildOf>(parent).is_none());
        assert_eq!(
            world.get::<melon_core::ChildOf>(child).unwrap().parent(),
            parent
        );
    }

    /// An entity with a unit box body of `kind` at `translation`, and rapier
    /// made to match. The world gets its `PhysicsWorld` if it has none.
    fn spawn_body_entity(
        world: &mut World,
        translation: glam::Vec3,
        kind: melon_core::RigidBodyKind,
    ) -> (Entity, melon_physics::rapier3d::dynamics::RigidBodyHandle) {
        if !world.contains_resource::<melon_physics::PhysicsWorld>() {
            world.insert_resource(melon_physics::PhysicsWorld::new());
        }
        let e = world
            .spawn((
                Transform::from_translation(translation),
                melon_core::RigidBody {
                    kind,
                    ..Default::default()
                },
                melon_core::Collider::default(),
            ))
            .id();
        melon_physics::sync_components_system(world);
        let handle = world
            .get::<melon_physics::RigidBodyComponent>(e)
            .unwrap()
            .body_handle;
        (e, handle)
    }

    #[test]
    fn hierarchy_drop_moves_fixed_body_to_world_pose() {
        let mut world = World::new();
        let (dragged, body) = spawn_body_entity(
            &mut world,
            glam::Vec3::new(2.0, 0.0, 0.0),
            melon_core::RigidBodyKind::Fixed,
        );
        let target = world
            .spawn(Transform {
                translation: glam::Vec3::new(5.0, 0.0, 0.0),
                rotation: glam::Quat::from_rotation_y(1.0),
                ..Default::default()
            })
            .id();
        // Move the dragged entity first so the body is stale, then drop.
        world.get_mut::<Transform>(dragged).unwrap().translation = glam::Vec3::new(4.0, 0.0, 0.0);
        hierarchy_drop(&mut world, dragged, Some(target)).unwrap();
        let physics = world.resource::<melon_physics::PhysicsWorld>();
        let at = physics.bodies.get(body).unwrap().translation();
        assert!(
            (at - glam::Vec3::new(4.0, 0.0, 0.0)).length() < 1e-4,
            "{at}"
        );
    }

    /// One Hierarchy frame with the pointer at `pos`, optionally released.
    fn hierarchy_frame(
        editor: &mut Editor,
        world: &mut World,
        ctx: &egui::Context,
        pos: egui::Pos2,
        release: bool,
    ) {
        let mut events = vec![egui::Event::PointerMoved(pos)];
        if release {
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 600.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| editor.build_hierarchy(world, ui));
    }

    /// Release `payload` over the row of `row` (or the empty area for `None`).
    fn drop_in_hierarchy<P: std::any::Any + Send + Sync>(
        editor: &mut Editor,
        world: &mut World,
        payload: P,
        row: Option<Entity>,
    ) {
        let ctx = egui::Context::default();
        let empty = egui::pos2(100.0, 550.0);
        hierarchy_frame(editor, world, &ctx, empty, false);
        let pos = match row {
            Some(e) => ctx
                .read_response(egui::Id::new(("hierarchy_row", e)))
                .expect("row was drawn")
                .rect
                .center(),
            None => empty,
        };
        egui::DragAndDrop::set_payload(&ctx, payload);
        hierarchy_frame(editor, world, &ctx, pos, true);
    }

    fn two_roots() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(AssetServer::new(".".into()));
        let a = world
            .spawn((Name("A".into()), Transform::from_translation(glam::Vec3::X)))
            .id();
        let b = world.spawn((Name("B".into()), Transform::default())).id();
        (world, a, b)
    }

    /// Rect of `e`'s Hierarchy row after one layout frame.
    fn row_rect(
        editor: &mut Editor,
        world: &mut World,
        ctx: &egui::Context,
        e: Entity,
    ) -> egui::Rect {
        hierarchy_frame(editor, world, ctx, egui::pos2(100.0, 550.0), false);
        ctx.read_response(egui::Id::new(("hierarchy_row", e)))
            .expect("row was drawn")
            .rect
    }

    /// Move to `pos`, press, release: one click, as egui sees it.
    fn click_hierarchy(
        editor: &mut Editor,
        world: &mut World,
        ctx: &egui::Context,
        pos: egui::Pos2,
    ) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for events in [
            vec![egui::Event::PointerMoved(pos)],
            vec![button(true)],
            vec![button(false)],
        ] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| editor.build_hierarchy(world, ui));
        }
    }

    #[test]
    fn hierarchy_ui_click_selects_row() {
        let (mut world, a, _) = two_roots();
        let mut editor = Editor::new();
        let ctx = egui::Context::default();
        let rect = row_rect(&mut editor, &mut world, &ctx, a);
        click_hierarchy(&mut editor, &mut world, &ctx, rect.center());
        assert_eq!(editor.selected_entity(), Some(a));
    }

    #[test]
    fn hierarchy_rows_span_full_width() {
        let (mut world, a, _) = two_roots();
        let mut editor = Editor::new();
        let ctx = egui::Context::default();
        let rect = row_rect(&mut editor, &mut world, &ctx, a);
        assert!(rect.width() > 380.0, "row is {} wide", rect.width());
    }

    #[test]
    fn hierarchy_ui_arrow_click_collapses_without_selecting() {
        let (mut world, a, b) = two_roots();
        world.entity_mut(b).insert(melon_core::ChildOf(a));
        let mut editor = Editor::new();
        let ctx = egui::Context::default();
        let rect = row_rect(&mut editor, &mut world, &ctx, a);
        let arrow = egui::pos2(
            rect.left() + theme::sizes::ROW_HEIGHT * 0.5,
            rect.center().y,
        );
        click_hierarchy(&mut editor, &mut world, &ctx, arrow);
        assert!(editor.collapsed.contains(&a));
        assert_eq!(editor.selected_entity(), None);
    }

    #[test]
    fn hierarchy_ui_entity_onto_row_reparents() {
        let (mut world, a, b) = two_roots();
        let mut editor = Editor::new();
        drop_in_hierarchy(&mut editor, &mut world, b, Some(a));
        assert_eq!(
            world.get::<melon_core::ChildOf>(b).map(|c| c.parent()),
            Some(a)
        );
    }

    #[test]
    fn hierarchy_ui_entity_onto_empty_area_makes_root() {
        let (mut world, a, b) = two_roots();
        world.entity_mut(b).insert(melon_core::ChildOf(a));
        let mut editor = Editor::new();
        drop_in_hierarchy(&mut editor, &mut world, b, None);
        assert!(world.get::<melon_core::ChildOf>(b).is_none());
    }

    #[test]
    fn hierarchy_ui_model_onto_row_spawns_child() {
        let (mut world, a, _) = two_roots();
        let mut editor = Editor::new();
        drop_in_hierarchy(
            &mut editor,
            &mut world,
            content::AssetDrag {
                path: "assets/models/missing.glb".into(),
                model: Some("assets/models/missing.glb".to_string()),
            },
            Some(a),
        );
        let children = world.get::<melon_core::Children>(a).map_or(0, |c| c.len());
        assert_eq!(children, 1);
    }

    #[test]
    fn non_model_asset_drag_spawns_nothing() {
        let (mut world, a, _) = two_roots();
        let mut editor = Editor::new();
        drop_in_hierarchy(
            &mut editor,
            &mut world,
            content::AssetDrag {
                path: "assets/README.md".into(),
                model: None,
            },
            Some(a),
        );
        let children = world.get::<melon_core::Children>(a).map_or(0, |c| c.len());
        assert_eq!(children, 0);
    }

    fn pending_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_pending_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pending_save_writes_file_and_reports() {
        let dir = pending_dir("save");
        let path = dir.join("scene.ron");
        let mut world = melon_core::Core::new().world;
        world.spawn((Name("A".into()), Transform::default()));
        let mut editor = Editor::new();
        editor.pending_operation = Some(PendingOperation::Save(path.clone()));
        assert!(!editor.run_pending_operation(&mut world));
        assert!(path.exists());
        assert_eq!(editor.status_kind(), Some(StatusKind::Success));
        std::fs::remove_file(&path).unwrap();
        assert!(!editor.run_pending_operation(&mut world));
        assert!(!path.exists(), "the operation ran twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_load_failure_keeps_world_and_physics() {
        let dir = pending_dir("load");
        let mut world = World::new();
        world.insert_resource(melon_physics::PhysicsWorld::new());
        world.spawn((Name("A".into()), Transform::default()));
        let mut editor = Editor::new();
        editor.pending_operation = Some(PendingOperation::Load(dir.join("missing.ron")));
        assert!(!editor.run_pending_operation(&mut world));
        assert_eq!(editor.status_kind(), Some(StatusKind::Error));
        assert_eq!(world.query::<&Name>().iter(&world).count(), 1);
        assert!(
            world
                .get_resource::<melon_physics::PhysicsWorld>()
                .is_some()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_exit_returns_true() {
        let dir = pending_dir("exit");
        let mut world = World::new();
        let mut editor = Editor::new();
        editor.layout_file = dir.join("layout.ron");
        editor.pending_operation = Some(PendingOperation::Exit);
        assert!(editor.run_pending_operation(&mut world));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One frame of `build_gizmo` for the selected entity (default camera).
    fn gizmo_frame(editor: &mut Editor, world: &mut World) {
        let ctx = egui::Context::default();
        let vp = viewport::ViewportInput {
            size: glam::Vec2::new(800.0, 600.0),
            ..Default::default()
        };
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_gizmo(world, ui, &vp)
        });
    }

    fn gizmo_world(scale: glam::Vec3) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(Camera::default());
        let e = world
            .spawn(Transform {
                scale,
                ..Default::default()
            })
            .id();
        (world, e)
    }

    #[test]
    fn take_overlay_draws_after_gizmo_frame() {
        let (mut world, e) = gizmo_world(glam::Vec3::ONE);
        let mut editor = Editor::new();
        editor.set_selected(Some(e));
        gizmo_frame(&mut editor, &mut world);
        let draws = editor.take_overlay_draws();
        assert!(!draws.is_empty());
        assert!(
            draws
                .iter()
                .all(|d| d.depth == melon_render::OverlayDepth::OnTop)
        );
    }

    #[test]
    fn no_gizmo_for_deleted_or_zero_scale_entity() {
        let (mut world, e) = gizmo_world(glam::Vec3::new(0.0, 1.0, 1.0));
        let mut editor = Editor::new();
        editor.set_selected(Some(e));
        gizmo_frame(&mut editor, &mut world);
        assert!(editor.take_overlay_draws().is_empty(), "zero scale");
        world.despawn(e);
        gizmo_frame(&mut editor, &mut world);
        assert!(editor.take_overlay_draws().is_empty(), "deleted");
        assert_eq!(editor.selected_entity(), None);
    }

    #[test]
    fn delete_selected_marks_subtree() {
        let mut world = World::new();
        let parent = world.spawn(Transform::default()).id();
        let child = world
            .spawn((Transform::default(), melon_core::ChildOf(parent)))
            .id();
        let mut editor = Editor::new();
        editor.set_selected(Some(parent));
        editor.delete_selected(&mut world);
        assert!(world.get::<melon_core::PendingDelete>(parent).is_some());
        assert!(world.get::<melon_core::PendingDelete>(child).is_some());
    }

    #[test]
    fn snap_previous_transform_matches_current() {
        let mut world = World::new();
        let e = world
            .spawn((
                Transform::from_translation(glam::Vec3::new(5.0, 0.0, 0.0)),
                melon_core::PreviousTransform(Transform::default()),
            ))
            .id();
        snap_previous_transform(&mut world, e);
        let prev = world.get::<melon_core::PreviousTransform>(e).unwrap();
        assert_eq!(prev.0.translation, glam::Vec3::new(5.0, 0.0, 0.0));
    }

    #[test]
    fn gizmo_physics_write_works_while_paused() {
        let mut world = World::new();
        let (e, body) = spawn_body_entity(
            &mut world,
            glam::Vec3::ZERO,
            melon_core::RigidBodyKind::Dynamic,
        );
        world.insert_resource(SimulationState::Paused);
        Editor::new().write_to_physics_body(
            &mut world,
            e,
            Some(glam::Vec3::new(4.0, 0.0, 0.0)),
            None,
        );
        let x = world
            .resource::<melon_physics::PhysicsWorld>()
            .body_translation(body)
            .unwrap()
            .x;
        assert!((x - 4.0).abs() < 1e-6, "rapier x {x}");
    }

    #[test]
    fn inspector_edit_while_paused_reaches_physics_and_prev() {
        let mut world = World::new();
        let (e, body) = spawn_body_entity(
            &mut world,
            glam::Vec3::ZERO,
            melon_core::RigidBodyKind::Dynamic,
        );
        world.insert_resource(SimulationState::Paused);
        world.entity_mut(e).insert((
            Transform::from_translation(glam::Vec3::new(4.0, 0.0, 0.0)),
            melon_core::PreviousTransform(Transform::default()),
        ));
        Editor::new().after_transform_edit(&mut world, e);
        let x = world
            .resource::<melon_physics::PhysicsWorld>()
            .body_translation(body)
            .unwrap()
            .x;
        assert!((x - 4.0).abs() < 1e-6, "rapier x {x}");
        let prev = world.get::<melon_core::PreviousTransform>(e).unwrap();
        assert_eq!(prev.0.translation.x, 4.0);
    }

    #[test]
    fn scene_dialog_result_dropped_unless_editing() {
        let path = std::path::PathBuf::from("a.ron");
        assert!(dialog_operation(true, path.clone(), SimulationState::Playing).is_none());
        assert!(dialog_operation(false, path.clone(), SimulationState::Paused).is_none());
        assert!(matches!(
            dialog_operation(true, path, SimulationState::Editing),
            Some(PendingOperation::Save(_))
        ));
    }

    #[test]
    fn status_kind_follows_setter_not_text() {
        let mut editor = Editor::default();
        editor.set_status(r"Saved to C:\failover\scene.ron".into());
        assert_eq!(editor.status_kind(), Some(StatusKind::Success));
        editor.set_status_error("Save failed: disk full".into());
        assert_eq!(editor.status_kind(), Some(StatusKind::Error));
        editor.set_status_warning("Stop the game before saving".into());
        assert_eq!(editor.status_kind(), Some(StatusKind::Warning));
    }

    #[test]
    fn frame_time_smoothing_tracks_delta() {
        let mut editor = Editor::default();
        for _ in 0..200 {
            editor.tick_frame_time(0.010);
        }
        assert!(
            (editor.frame_ms - 10.0).abs() < 0.1,
            "frame_ms {}",
            editor.frame_ms
        );
    }

    #[test]
    fn project_root_sets_layout_file_and_assets() {
        let root = std::env::temp_dir().join(format!("melon_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets/models")).unwrap();
        std::fs::create_dir_all(root.join(".melon")).unwrap();
        std::fs::write(
            root.join(".melon/content_browser.ron"),
            "(version: 1, favorites: [], tile_size: 72.0, last_folder: Some(\"models\"), tree_width: 200.0)",
        )
        .unwrap();
        let mut saved = layout::default_layout();
        let console = saved.find_tab(&layout::EditorTab::Console).unwrap();
        saved.remove_tab(console);
        layout::save_layout(&saved, &root.join(".melon/editor_layout.ron")).unwrap();

        let mut editor = Editor::default();
        editor.set_project_root(&root);
        assert!(
            editor.dock.find_tab(&layout::EditorTab::Console).is_none(),
            "layout loaded from the root"
        );
        assert_eq!(editor.content.current(), root.join("assets/models"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inspector_tab_cannot_be_closed() {
        use egui_dock::TabViewer;
        let mut editor = Editor::default();
        let mut world = World::new();
        let tabs = EditorTabs {
            editor: &mut editor,
            world: &mut world,
        };
        assert!(!tabs.is_closeable(&layout::EditorTab::Inspector));
        assert!(tabs.is_closeable(&layout::EditorTab::Console));
    }

    /// Regression: `Response::clicked()` takes the egui context lock, so it
    /// must not be called inside `ctx.input(..)` (deadlock on a viewport click).
    #[test]
    fn viewport_input_on_click_does_not_deadlock() {
        let ctx = egui::Context::default();
        let pos = egui::pos2(50.0, 50.0);
        let press = |pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let frames = [
            vec![egui::Event::PointerMoved(pos)],
            vec![press(true)],
            vec![press(false)],
        ];
        let mut clicked = false;
        for events in frames {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                let rect =
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                let vp = viewport_input(ui.ctx(), &response, rect);
                clicked |= vp.left_clicked;
            });
        }
        assert!(clicked, "the click reached the viewport input");
    }

    #[test]
    fn ctrl_s_saves() {
        assert_eq!(
            shortcut_for(egui::Key::S, true, false, false, false),
            Some(Shortcut::Save)
        );
        assert_eq!(shortcut_for(egui::Key::S, false, false, false, false), None);
    }

    #[test]
    fn shortcuts_blocked_while_typing() {
        for (key, ctrl) in [
            (egui::Key::W, false),
            (egui::Key::Delete, false),
            (egui::Key::S, true),
        ] {
            assert_eq!(shortcut_for(key, ctrl, false, true, false), None, "{key:?}");
        }
    }

    #[test]
    fn tool_keys_ignored_while_flying() {
        assert_eq!(shortcut_for(egui::Key::W, false, false, false, true), None);
        assert_eq!(
            shortcut_for(egui::Key::W, false, false, false, false),
            Some(Shortcut::Move)
        );
        assert_eq!(
            shortcut_for(egui::Key::E, false, false, false, false),
            Some(Shortcut::Rotate)
        );
        assert_eq!(
            shortcut_for(egui::Key::R, false, false, false, false),
            Some(Shortcut::Scale)
        );
        assert_eq!(
            shortcut_for(egui::Key::Delete, false, false, false, false),
            Some(Shortcut::Delete)
        );
    }

    /// One triangle used by two nodes at x = -2 and x = +3 (as in melon_render).
    const TWO_NODE_GLTF: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1]}],
        "nodes": [
            {"mesh": 0, "translation": [-2, 0, 0]},
            {"mesh": 0, "translation": [3, 0, 0]}
        ],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "buffers": [{"byteLength": 36,
            "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA"}],
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3,
            "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]}]
    }"#;

    fn two_node_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_unpack_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets/models")).unwrap();
        std::fs::write(dir.join("assets/models/two.gltf"), TWO_NODE_GLTF).unwrap();
        dir
    }

    fn model_path(world: &World, e: Entity) -> Option<String> {
        world
            .get::<Handle<Model>>(e)
            .map(|h| world.resource::<AssetServer>().path(*h).to_string())
    }

    #[test]
    fn unpacked_drop_spawns_root_and_node_children() {
        let root_dir = two_node_project("tree");
        let mut world = World::new();
        let at = glam::Vec3::new(1.0, 0.0, 2.0);
        let root = spawn_model(&mut world, &root_dir, "assets/models/two.gltf", at, None);
        assert_eq!(world.get::<Name>(root).unwrap().0, "two");
        assert_eq!(world.get::<Transform>(root).unwrap().translation, at);
        assert!(model_path(&world, root).is_none());
        let children: Vec<Entity> = world
            .get::<melon_core::Children>(root)
            .unwrap()
            .iter()
            .copied()
            .collect();
        assert_eq!(children.len(), 2);
        let expect = [("Node 0", -2.0), ("Node 1", 3.0)];
        for (&c, (name, x)) in children.iter().zip(expect) {
            assert_eq!(world.get::<Name>(c).unwrap().0, name);
            assert_eq!(
                world.get::<Transform>(c).unwrap().translation,
                glam::Vec3::new(x, 0.0, 0.0)
            );
            assert_eq!(
                model_path(&world, c).as_deref(),
                Some("assets/models/two.gltf#mesh0")
            );
        }
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn zero_scale_node_still_saves_and_loads() {
        let root_dir = two_node_project("zero_scale");
        let flat = TWO_NODE_GLTF.replace(
            r#"{"mesh": 0, "translation": [3, 0, 0]}"#,
            r#"{"mesh": 0, "translation": [3, 0, 0], "scale": [0, 1, 1]}"#,
        );
        std::fs::write(root_dir.join("assets/models/flat.gltf"), flat).unwrap();
        let mut world = melon_core::Core::new().world;
        spawn_model(
            &mut world,
            &root_dir,
            "assets/models/flat.gltf",
            glam::Vec3::ZERO,
            None,
        );
        let data = scene::scene_to_data(&mut world).unwrap();
        let mut physics = melon_physics::PhysicsWorld::new();
        scene::apply_scene(&mut world, &mut physics, &data).unwrap();
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn unreadable_file_spawns_single_whole_file_entity() {
        let root_dir = two_node_project("unreadable");
        std::fs::write(root_dir.join("assets/models/bad.glb"), "garbage").unwrap();
        let mut world = World::new();
        let e = spawn_model(
            &mut world,
            &root_dir,
            "assets/models/bad.glb",
            glam::Vec3::ZERO,
            None,
        );
        assert_eq!(
            model_path(&world, e).as_deref(),
            Some("assets/models/bad.glb")
        );
        assert!(world.get::<melon_core::Children>(e).is_none());
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn drop_onto_row_parents_new_root() {
        let root_dir = two_node_project("row");
        let mut world = World::new();
        let row = world
            .spawn((
                Name("Row".into()),
                Transform::from_translation(glam::Vec3::X),
            ))
            .id();
        let root = spawn_model(
            &mut world,
            &root_dir,
            "assets/models/two.gltf",
            glam::Vec3::ZERO,
            Some(row),
        );
        assert_eq!(
            world.get::<melon_core::ChildOf>(root).unwrap().parent(),
            row
        );
        assert_eq!(
            world.get::<Transform>(root).unwrap().translation,
            glam::Vec3::ZERO
        );
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn large_scene_still_takes_a_model() {
        // There is no model cap: a drop into a scene with 5000 models spawns.
        let root_dir = two_node_project("no_cap");
        let mut world = World::new();
        let cube: Handle<Model> = world
            .get_resource_or_insert_with(|| AssetServer::new(".".into()))
            .load("builtin:cube");
        for _ in 0..5000 {
            world.spawn((Transform::default(), cube));
        }
        let before = world.query::<&Handle<Model>>().iter(&world).count();
        spawn_model(
            &mut world,
            &root_dir,
            "assets/models/two.gltf",
            glam::Vec3::ZERO,
            None,
        );
        assert!(world.query::<&Handle<Model>>().iter(&world).count() > before);
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn mesh_label_shows_fragment() {
        assert_eq!(
            model_label("assets/models/car.glb#mesh2"),
            "car.glb › mesh2"
        );
        assert_eq!(model_label("assets/models/car.glb"), "car.glb");
    }

    #[test]
    fn spawn_model_names_from_stem() {
        let mut world = World::new();
        let e = spawn_model(
            &mut world,
            std::path::Path::new("."),
            "assets/models/sub/Barrel.glb",
            glam::Vec3::new(1.0, 0.0, 2.0),
            None,
        );
        assert_eq!(world.get::<Name>(e).unwrap().0, "Barrel");
        assert_eq!(
            world.get::<Transform>(e).unwrap().translation,
            glam::Vec3::new(1.0, 0.0, 2.0)
        );
        let h = *world.get::<Handle<Model>>(e).unwrap();
        assert_eq!(
            world.resource::<AssetServer>().path(h),
            "assets/models/sub/Barrel.glb"
        );
    }

    #[test]
    fn assigning_a_material_is_one_undoable_step_and_part_aware() {
        use melon_core::MeshMaterials;
        let (mut editor, mut world, root) = browser_project("assign_material");
        std::fs::write(root.join("assets/m.mat"), "()").unwrap();
        let model: Handle<Model> = world.resource_mut::<AssetServer>().load("builtin:cube");
        let e = world
            .spawn((Name("Cube".to_string()), Transform::default(), model))
            .id();
        world
            .resource_mut::<AssetServer>()
            .set_part_bounds(model, vec![(glam::Vec3::ZERO, glam::Vec3::ONE); 3]);
        editor.assign_material_step(&mut world, e, "assets/m.mat", None);
        let parts = world.get::<MeshMaterials>(e).unwrap().parts.clone();
        assert_eq!(parts.len(), 3, "every part of the model");
        assert!(parts.iter().all(|p| p.path == "assets/m.mat"));
        assert_eq!(editor.history.undo_label(), Some("Assign Material"));
        editor.assign_material_step(&mut world, e, "assets/other.mat", Some(1));
        let parts = world.get::<MeshMaterials>(e).unwrap().parts.clone();
        assert_eq!(parts[1].path, "assets/other.mat");
        assert_eq!(parts[0].path, "assets/m.mat");
        editor.undo(&mut world);
        assert_eq!(
            world.get::<MeshMaterials>(e).unwrap().parts[1].path,
            "assets/m.mat"
        );
        editor.undo(&mut world);
        assert!(world.get::<MeshMaterials>(e).is_none(), "undone to none");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_light_spawns_a_named_light_pointing_down_for_spots() {
        let mut world = World::new();
        let point = create_light(&mut world, LightKind::Point);
        assert_eq!(world.get::<Name>(point).unwrap().0, "Point Light");
        assert!(world.get::<melon_core::PointLight>(point).is_some());
        let spot = create_light(&mut world, LightKind::Spot);
        assert_eq!(world.get::<Name>(spot).unwrap().0, "Spot Light");
        let t = world.get::<melon_core::Transform>(spot).unwrap();
        let axis = t.rotation * glam::Vec3::NEG_Z;
        assert!((axis - glam::Vec3::NEG_Y).length() < 1e-5, "{axis}");
    }

    #[test]
    fn create_cube_spawns_named_cube() {
        let mut world = World::new();
        let e = create_cube(&mut world);
        assert_eq!(world.get::<Name>(e).unwrap().0, "Cube");
        assert!(world.get::<Handle<Model>>(e).is_some());
        let e2 = create_cube(&mut world);
        assert_eq!(world.get::<Name>(e2).unwrap().0, "Cube (1)");
    }

    #[test]
    fn delete_selected_marks_entity_and_clears_selection() {
        let mut world = World::new();
        let e = create_cube(&mut world);
        let mut editor = Editor::default();
        editor.set_selected(Some(e));
        editor.delete_selected(&mut world);
        assert!(world.get::<melon_core::PendingDelete>(e).is_some());
        assert_eq!(editor.selected_entity(), None);
    }

    #[test]
    fn snap_previous_transform_ignores_entity_without_it() {
        let mut world = World::new();
        let e = world.spawn(Transform::default()).id();
        snap_previous_transform(&mut world, e);
        assert!(world.get::<melon_core::PreviousTransform>(e).is_none());
    }

    #[test]
    fn escape_returns_focus_to_editor_while_playing() {
        assert_eq!(
            shortcut_for(egui::Key::Escape, false, false, false, false),
            Some(Shortcut::ReleaseGame)
        );
        let mut world = World::new();
        world.insert_resource(SimulationState::Playing);
        world.insert_resource(InputFocus::Game);
        let mut editor = Editor::new();
        key_frame(&mut editor, &mut world, egui::Key::Escape, Vec::new());
        assert_eq!(*world.resource::<InputFocus>(), InputFocus::Editor);
        assert_eq!(sim_state(&world), SimulationState::Playing);
    }

    #[test]
    fn click_on_viewport_while_playing_takes_game_focus() {
        assert_eq!(
            focus_after_click(SimulationState::Playing, InputFocus::Editor, true),
            InputFocus::Game
        );
        assert_eq!(
            focus_after_click(SimulationState::Playing, InputFocus::Editor, false),
            InputFocus::Editor
        );
        assert_eq!(
            focus_after_click(SimulationState::Editing, InputFocus::Editor, true),
            InputFocus::Editor
        );
        assert_eq!(
            focus_after_click(SimulationState::Paused, InputFocus::Editor, true),
            InputFocus::Editor
        );
    }

    /// One frame of shortcuts plus the Scene viewport (100 x 100 at the
    /// origin, pointer inside) with `key` pressed and `extra` events.
    fn key_frame(editor: &mut Editor, world: &mut World, key: egui::Key, extra: Vec<egui::Event>) {
        editor.viewport_texture = Some(egui::TextureId::default());
        let ctx = egui::Context::default();
        let pos = egui::pos2(50.0, 50.0);
        let mut events = vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        events.extend(extra);
        // The first frame only moves the pointer in, so the viewport is hovered on the second.
        for events in [vec![egui::Event::PointerMoved(pos)], events] {
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(100.0, 100.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                editor.handle_shortcuts(world, ui);
                editor.viewport_panel(world, ui);
            });
        }
    }

    #[test]
    fn game_focus_blocks_editor_camera_and_tool_keys() {
        let wheel = || {
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 3.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        let mut world = World::new();
        world.insert_resource(Camera::default());
        world.insert_resource(SimulationState::Playing);

        // Editor focus: W picks Move and the wheel zooms (the test can see both).
        let mut editor = Editor::new();
        editor.gizmo_mode = GizmoMode::Rotate;
        let distance = editor.camera.distance;
        key_frame(&mut editor, &mut world, egui::Key::W, wheel());
        assert_eq!(editor.gizmo_mode, GizmoMode::Move);
        assert_ne!(editor.camera.distance, distance);

        world.insert_resource(InputFocus::Game);
        let mut editor = Editor::new();
        editor.gizmo_mode = GizmoMode::Rotate;
        let before = editor.camera;
        key_frame(&mut editor, &mut world, egui::Key::W, wheel());
        assert_eq!(editor.gizmo_mode, GizmoMode::Rotate);
        let after = editor.camera;
        assert_eq!(
            (before.pivot, before.distance, before.yaw, before.pitch),
            (after.pivot, after.distance, after.yaw, after.pitch)
        );
    }

    #[test]
    fn gate_is_false_while_typing_or_editing() {
        use InputFocus::{Editor as EditorFocus, Game};
        use SimulationState::{Editing, Paused, Playing};
        assert!(game_input_gate(Game, Playing, false, true));
        assert!(!game_input_gate(Game, Playing, true, true), "typing");
        assert!(!game_input_gate(Game, Editing, false, true));
        assert!(!game_input_gate(Game, Paused, false, true));
        assert!(!game_input_gate(EditorFocus, Playing, false, true));
        assert!(
            !game_input_gate(Game, Playing, false, false),
            "Scene hidden"
        );
    }

    #[test]
    fn viewport_panel_marks_the_scene_shown() {
        let mut world = World::new();
        let mut editor = Editor::new();
        assert!(!editor.viewport_shown);
        key_frame(&mut editor, &mut world, egui::Key::W, Vec::new());
        assert!(editor.viewport_shown);
    }

    #[test]
    fn leaving_game_focus_adopts_the_game_camera() {
        let mut world = World::new();
        world.insert_resource(SimulationState::Playing);
        world.insert_resource(InputFocus::Game);
        // The game moved the camera while it had focus.
        let game_camera = Camera {
            eye: glam::Vec3::new(10.0, 2.0, -3.0),
            center: glam::Vec3::new(10.0, 2.0, -8.0),
            ..Default::default()
        };
        world.insert_resource(game_camera);
        let mut editor = Editor::new();
        // Frame 1 sees game focus; frame 2's Esc hands focus back.
        key_frame(&mut editor, &mut world, egui::Key::Escape, Vec::new());
        assert_eq!(*world.resource::<InputFocus>(), InputFocus::Editor);
        let camera = world.resource::<Camera>();
        assert!(
            camera.eye.distance(game_camera.eye) < 1e-3,
            "{}",
            camera.eye
        );
        assert!(camera.center.distance(game_camera.center) < 1e-3);
    }

    /// An editor on a temp project with `assets/models/x.glb` (and its
    /// `.meta`), and a world holding that project's `AssetServer`.
    fn browser_project(name: &str) -> (Editor, World, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("melon_browser_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets/models")).unwrap();
        std::fs::write(root.join("assets/models/x.glb"), b"x").unwrap();
        let mut server = AssetServer::new(root.clone());
        asset_db::reconcile(&mut server);
        let mut world = melon_core::Core::new().world;
        world.insert_resource(server);
        let mut editor = Editor::new();
        editor.set_project_root(&root);
        (editor, world, root)
    }

    fn editor_key(editor: &mut Editor, world: &mut World, key: egui::Key) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| editor.build_ui(world, ui));
    }

    #[test]
    fn delete_key_goes_to_the_focused_panel() {
        let (mut editor, mut world, root) = browser_project("delete_focus");
        let x = root.join("assets/models/x.glb");
        let entity = world
            .spawn((Name("E".to_string()), Transform::default()))
            .id();
        editor.set_selected(Some(entity));
        editor.content.open(root.join("assets/models"));
        editor.content.select(Some(x.clone()));
        // One frame first: the browser tab has been drawn, as in the running editor.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_ui(&mut world, ui)
        });
        editor.content.focused = true;
        editor_key(&mut editor, &mut world, egui::Key::Delete);
        assert!(world.get::<melon_core::PendingDelete>(entity).is_none());
        assert_eq!(
            editor.delete_confirm.as_ref().map(|c| c.path.clone()),
            Some(x.clone())
        );

        editor.delete_confirm = None;
        editor.content.focused = false;
        editor_key(&mut editor, &mut world, egui::Key::Delete);
        assert!(world.get::<melon_core::PendingDelete>(entity).is_some());
        assert!(x.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extract_materials_request_assigns_them_to_the_selected_entity_using_the_model() {
        use melon_core::MeshMaterials;
        let (mut editor, mut world, root) = browser_project("extract_request");
        let model_file = root.join("assets/models/thing.gltf");
        std::fs::write(&model_file, crate::asset_ops::ONE_MATERIAL_GLTF).unwrap();
        let model: Handle<Model> = world
            .resource_mut::<AssetServer>()
            .load("assets/models/thing.gltf");
        let user = world.spawn((Transform::default(), model)).id();
        let other = world.spawn((Transform::default(), model)).id();
        let cube: Handle<Model> = world.resource_mut::<AssetServer>().load("builtin:cube");
        let unrelated = world.spawn((Transform::default(), cube)).id();
        editor.set_selected(Some(user));
        editor
            .content
            .request(content::BrowserRequest::ExtractMaterials(model_file));
        editor.apply_content_requests(&mut world);
        assert!(root.join("assets/models/thing_Paint.mat").is_file());
        let parts = world.get::<MeshMaterials>(user).unwrap().parts.clone();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].path, "assets/models/thing_Paint.mat");
        assert!(!parts[0].guid.is_empty());
        assert!(
            world.get::<MeshMaterials>(other).is_none(),
            "only the selected one"
        );
        // A selection that does not use the model is left alone.
        editor.set_selected(Some(unrelated));
        editor
            .content
            .request(content::BrowserRequest::ExtractMaterials(
                root.join("assets/models/thing.gltf"),
            ));
        editor.apply_content_requests(&mut world);
        assert!(world.get::<MeshMaterials>(unrelated).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_folder_request_creates_and_starts_rename() {
        let (mut editor, mut world, root) = browser_project("new_folder");
        let assets = root.join("assets");
        editor
            .content
            .request(content::BrowserRequest::NewFolder(assets.clone()));
        editor.apply_content_requests(&mut world);
        let made = assets.join("New Folder");
        assert!(made.is_dir());
        assert_eq!(editor.content.selected(), Some(made.as_path()));
        assert_eq!(
            editor.content.renaming.as_ref().map(|r| r.path.clone()),
            Some(made)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_request_failure_goes_to_the_status_bar() {
        let (mut editor, mut world, root) = browser_project("rename_fail");
        let x = root.join("assets/models/x.glb");
        editor.content.request(content::BrowserRequest::Rename(
            x.clone(),
            "bad/name".into(),
        ));
        editor.apply_content_requests(&mut world);
        assert!(x.exists());
        let status = editor
            .status_message
            .as_ref()
            .map(|(m, ..)| m.clone())
            .unwrap_or_default();
        assert!(status.contains("cannot contain"), "{status}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_scene_path_only_ron_under_assets() {
        let root = std::path::Path::new("project");
        assert!(content::is_scene_path(
            root,
            &root.join("assets/scenes/a.RON")
        ));
        assert!(!content::is_scene_path(
            root,
            &root.join("config/input.ron")
        ));
        assert!(!content::is_scene_path(
            root,
            &root.join("assets/models/a.glb")
        ));
    }

    fn scene_file(root: &std::path::Path) -> std::path::PathBuf {
        let scene = root.join("assets/scenes/level.ron");
        std::fs::create_dir_all(scene.parent().unwrap()).unwrap();
        std::fs::write(&scene, "(version: 4, entities: [], name_counter: 0)").unwrap();
        scene
    }

    #[test]
    fn opening_a_scene_asks_first_and_dont_save_loads() {
        let (mut editor, mut world, root) = browser_project("open_scene");
        let scene = scene_file(&root);
        editor
            .content
            .request(content::BrowserRequest::Open(scene.clone()));
        editor.apply_content_requests(&mut world);
        assert_eq!(editor.confirm_open, Some(scene.clone()));
        assert!(editor.pending_operation.is_none());
        editor.resolve_open(&mut world, OpenChoice::DontSave);
        assert!(
            matches!(editor.take_pending_operation(), Some(PendingOperation::Load(p)) if p == scene)
        );
        assert!(editor.confirm_open.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opening_a_scene_while_playing_is_refused() {
        let (mut editor, mut world, root) = browser_project("open_playing");
        let scene = scene_file(&root);
        world.insert_resource(SimulationState::Playing);
        editor.set_current_scene(root.join("assets/scenes/current.ron"));
        editor.content.request(content::BrowserRequest::Open(scene));
        editor.apply_content_requests(&mut world);
        assert!(
            editor.confirm_open.is_none(),
            "no Save prompt while playing"
        );
        editor.resolve_open(&mut world, OpenChoice::Save);
        assert!(
            !root.join("assets/scenes/current.ron").exists(),
            "play state not saved"
        );
        assert!(editor.take_pending_operation().is_none());
        let status = editor
            .status_message
            .as_ref()
            .map(|(m, ..)| m.clone())
            .unwrap_or_default();
        assert!(status.contains("Stop playing"), "{status}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_choice_needs_a_current_scene() {
        let (mut editor, mut world, root) = browser_project("open_save");
        let scene = scene_file(&root);
        editor
            .content
            .request(content::BrowserRequest::Open(scene.clone()));
        editor.apply_content_requests(&mut world);
        assert!(!editor.open_scene_can_save());
        editor.resolve_open(&mut world, OpenChoice::Save);
        assert!(editor.take_pending_operation().is_none());
        // With a current scene, Save writes it and then loads.
        let current = root.join("assets/scenes/current.ron");
        editor.set_current_scene(current.clone());
        editor
            .content
            .request(content::BrowserRequest::Open(scene.clone()));
        editor.apply_content_requests(&mut world);
        assert!(editor.open_scene_can_save());
        editor.resolve_open(&mut world, OpenChoice::Save);
        assert!(current.exists());
        assert!(
            matches!(editor.take_pending_operation(), Some(PendingOperation::Load(p)) if p == scene)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dropped_files_import_into_current_folder() {
        let (mut editor, mut world, root) = browser_project("dropped");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("crate.glb"), b"x").unwrap();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_ui(&mut world, ui)
        });
        let input = egui::RawInput {
            dropped_files: vec![egui::DroppedFile {
                path: Some(outside.join("crate.glb")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| editor.build_ui(&mut world, ui));
        let imported = root.join("assets/crate.glb");
        assert!(imported.exists());
        assert!(melon_core::meta_path(&imported).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn renaming_the_open_scene_updates_current_scene() {
        let (mut editor, mut world, root) = browser_project("rename_open_scene");
        let scene = scene_file(&root);
        editor.set_current_scene(scene.clone());
        editor
            .content
            .request(content::BrowserRequest::Rename(scene, "level2.ron".into()));
        editor.apply_content_requests(&mut world);
        assert_eq!(
            editor.current_scene,
            Some(root.join("assets/scenes/level2.ron"))
        );
        let folder = root.join("assets/scenes");
        editor
            .content
            .request(content::BrowserRequest::Rename(folder, "levels".into()));
        editor.apply_content_requests(&mut world);
        assert_eq!(
            editor.current_scene,
            Some(root.join("assets/levels/level2.ron"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_folder_elsewhere_shows_it_for_renaming() {
        let (mut editor, mut world, root) = browser_project("new_folder_elsewhere");
        editor.content.set_search("x".into());
        let models = root.join("assets/models");
        editor
            .content
            .request(content::BrowserRequest::NewFolder(models.clone()));
        editor.apply_content_requests(&mut world);
        assert_eq!(editor.content.current(), models);
        assert_eq!(editor.content.search(), "");
        assert!(
            editor
                .content
                .visible_entries()
                .iter()
                .any(|e| e.path == models.join("New Folder"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hidden_browser_loses_focus() {
        let (mut editor, mut world, root) = browser_project("hidden_focus");
        let tab = editor
            .dock
            .find_tab(&layout::EditorTab::ContentBrowser)
            .unwrap();
        editor.dock.remove_tab(tab);
        editor.content.focused = true;
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                editor.build_ui(&mut world, ui)
            });
        }
        assert!(!editor.content.has_focus());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn browser_keys_wait_while_a_dialog_is_open() {
        let (mut editor, mut world, root) = browser_project("keys_dialog");
        let scene = scene_file(&root);
        editor.content.open(root.join("assets/scenes"));
        editor.content.select(Some(scene.clone()));
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_ui(&mut world, ui)
        });
        editor.content.focused = true;
        editor.delete_confirm = Some(DeleteConfirm {
            path: scene,
            used_by: Vec::new(),
        });
        editor_key(&mut editor, &mut world, egui::Key::Enter);
        assert!(editor.confirm_open.is_none());
        assert!(editor.content.take_requests().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn editor_follows_outside_move_with_meta() {
        let (mut editor, mut world, root) = browser_project("outside_move");
        let x = root.join("assets/models/x.glb");
        let guid = world
            .resource::<AssetServer>()
            .guid_of("assets/models/x.glb")
            .unwrap();
        let h: Handle<Model> = world
            .resource_mut::<AssetServer>()
            .load("assets/models/x.glb");
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_ui(&mut world, ui)
        });
        let y = root.join("assets/y.glb");
        std::fs::rename(&x, &y).unwrap();
        std::fs::rename(melon_core::meta_path(&x), melon_core::meta_path(&y)).unwrap();
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while world.resource::<AssetServer>().path(h) != "assets/y.glb" {
            assert!(
                std::time::Instant::now() < give_up,
                "move not followed within 5 s"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                editor.build_ui(&mut world, ui)
            });
        }
        assert_eq!(
            world.resource::<AssetServer>().guid_of("assets/y.glb"),
            Some(guid)
        );
        assert!(melon_core::meta_path(&y).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inspector_shows_asset_card_for_browser_selection() {
        let (mut editor, mut world, root) = browser_project("asset_card");
        editor
            .content
            .select(Some(root.join("assets/models/x.glb")));
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_inspector(&mut world, ui)
        });
        assert!(
            ctx.read_response(egui::Id::new("inspector_asset_card"))
                .is_some()
        );
        assert!(
            ctx.read_response(egui::Id::new("inspector_nothing"))
                .is_none()
        );
        editor.content.select(None);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_inspector(&mut world, ui)
        });
        assert!(
            ctx.read_response(egui::Id::new("inspector_nothing"))
                .is_some()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watcher_waits_before_giving_a_new_file_a_meta() {
        let (mut editor, mut world, root) = browser_project("meta_grace");
        let ctx = egui::Context::default();
        let new = root.join("assets/models/new.glb");
        std::fs::write(&new, b"x").unwrap();
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Until a settled reconcile ran and left the file waiting.
        while editor.meta_recheck.is_none() {
            assert!(std::time::Instant::now() < give_up, "no settle within 5 s");
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                editor.follow_asset_changes(&mut world, ui, std::time::Instant::now())
            });
        }
        assert!(
            !melon_core::meta_path(&new).exists(),
            "its .meta may still be on the way"
        );
        // None arrived: after the grace the file gets one.
        let later = std::time::Instant::now() + asset_watch::META_GRACE;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.follow_asset_changes(&mut world, ui, later)
        });
        assert!(melon_core::meta_path(&new).exists());
        assert!(editor.meta_recheck.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watcher_starts_once_assets_exists_and_restarts_after_removal() {
        let root = std::env::temp_dir().join(format!("melon_rewatch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut editor = Editor::new();
        editor.set_project_root(&root);
        assert!(editor.watcher.is_none(), "no assets/ yet");
        let t0 = std::time::Instant::now();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        assert!(!editor.keep_watching(t0), "not due before the retry");
        assert!(editor.keep_watching(t0 + asset_watch::RETRY));
        assert!(editor.watcher.is_some());
        std::fs::remove_dir_all(root.join("assets")).unwrap();
        assert!(!editor.keep_watching(t0 + asset_watch::RETRY * 2));
        assert!(editor.watcher.is_none(), "the watched root is gone");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        assert!(editor.keep_watching(t0 + asset_watch::RETRY * 3));
        // The new watcher sees changes.
        let watcher = editor.watcher.as_mut().unwrap();
        std::fs::write(root.join("assets/new.glb"), b"x").unwrap();
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !watcher.poll(std::time::Instant::now()) {
            assert!(std::time::Instant::now() < give_up, "no event within 5 s");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(editor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn asset_card_cache_keeps_only_the_selection() {
        let (mut editor, mut world, root) = browser_project("mesh_cache");
        let x = root.join("assets/models/x.glb");
        let y = root.join("assets/models/y.glb");
        std::fs::write(&y, b"y").unwrap();
        asset_db::reconcile(&mut world.resource_mut::<AssetServer>());
        let ctx = egui::Context::default();
        let mut show = |editor: &mut Editor, selection: Option<std::path::PathBuf>| {
            editor.content.select(selection);
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                editor.build_inspector(&mut world, ui)
            });
        };
        show(&mut editor, Some(x.clone()));
        assert!(editor.mesh_counts.contains_key(&x));
        show(&mut editor, Some(y.clone()));
        assert_eq!(
            editor.mesh_counts.keys().collect::<Vec<_>>(),
            vec![&y],
            "only the selected model stays cached"
        );
        show(&mut editor, None);
        assert!(editor.mesh_counts.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Release `drag` over the Inspector's Mesh field for a selected cube.
    fn drop_on_mesh_field(drag: content::AssetDrag) -> String {
        // One temp project per test: they run in parallel.
        let name = format!("mesh_drop_{}", drag.model.is_some());
        let (mut editor, mut world, root) = browser_project(&name);
        let cube: Handle<Model> = world.resource_mut::<AssetServer>().load("builtin:cube");
        let entity = world
            .spawn((Name("Cube".to_string()), Transform::default(), cube))
            .id();
        editor.set_selected(Some(entity));
        let ctx = egui::Context::default();
        let run = |editor: &mut Editor, world: &mut World, events| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| editor.build_inspector(world, ui));
        };
        run(&mut editor, &mut world, Vec::new());
        let pos = ctx
            .read_response(egui::Id::new("inspector_mesh_drop"))
            .expect("mesh field drawn")
            .rect
            .center();
        run(
            &mut editor,
            &mut world,
            vec![egui::Event::PointerMoved(pos)],
        );
        egui::DragAndDrop::set_payload(&ctx, drag);
        let release = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        run(
            &mut editor,
            &mut world,
            vec![egui::Event::PointerMoved(pos), release],
        );
        let handle = *world.get::<Handle<Model>>(entity).unwrap();
        let path = world.resource::<AssetServer>().path(handle).to_string();
        let _ = std::fs::remove_dir_all(&root);
        path
    }

    #[test]
    fn mesh_field_takes_a_model_drop() {
        let path = drop_on_mesh_field(content::AssetDrag {
            path: "assets/models/x.glb".into(),
            model: Some("assets/models/x.glb".to_string()),
        });
        assert_eq!(path, "assets/models/x.glb");
    }

    #[test]
    fn mesh_field_ignores_non_model_drop() {
        let path = drop_on_mesh_field(content::AssetDrag {
            path: "assets/x.png".into(),
            model: None,
        });
        assert_eq!(path, "builtin:cube");
    }

    fn full_frame(
        ctx: &egui::Context,
        editor: &mut Editor,
        world: &mut World,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| editor.build_ui(world, ui));
    }

    /// A selected entity drawing `model` (its file at `<root>/<model>`), the
    /// Console in front of the browser, two frames drawn. Returns the
    /// position of the Inspector's "Show" button.
    fn show_setup(
        name: &str,
        model: &str,
    ) -> (Editor, World, std::path::PathBuf, egui::Context, egui::Pos2) {
        let (mut editor, mut world, root) = browser_project(name);
        let file = root.join(model);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let handle: Handle<Model> = world.resource_mut::<AssetServer>().load(model);
        let entity = world
            .spawn((Name("X".to_string()), Transform::default(), handle))
            .id();
        editor.set_selected(Some(entity));
        if let Some(console) = editor.dock.find_tab(&layout::EditorTab::Console) {
            let _ = editor.dock.set_active_tab(console);
        }
        let ctx = egui::Context::default();
        full_frame(&ctx, &mut editor, &mut world, Vec::new());
        full_frame(&ctx, &mut editor, &mut world, Vec::new());
        let combo = ctx
            .read_response(egui::Id::new("inspector_mesh_drop"))
            .expect("mesh field drawn")
            .rect;
        // "Show" sits just left of the combo box.
        let pos = egui::pos2(combo.left() - 12.0, combo.center().y);
        (editor, world, root, ctx, pos)
    }

    fn click_at(ctx: &egui::Context, editor: &mut Editor, world: &mut World, pos: egui::Pos2) {
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        full_frame(
            ctx,
            editor,
            world,
            vec![egui::Event::PointerMoved(pos), press(true), press(false)],
        );
        full_frame(ctx, editor, world, Vec::new());
    }

    /// Whether the topmost clickable widget at `pos` was enabled last frame.
    fn clickable_enabled_at(ctx: &egui::Context, pos: egui::Pos2) -> Option<bool> {
        ctx.viewport(|v| {
            v.prev_pass
                .widgets
                .layers()
                .flat_map(|(_, widgets)| widgets)
                .filter(|w| w.rect.contains(pos) && w.sense.senses_click())
                .last()
                .map(|w| w.enabled)
        })
    }

    #[test]
    fn show_is_disabled_outside_the_browser_root() {
        let (_editor, _world, root, ctx, pos) = show_setup("show_inside", "assets/models/x.glb");
        assert_eq!(clickable_enabled_at(&ctx, pos), Some(true));
        let _ = std::fs::remove_dir_all(&root);
        let (_editor, _world, root, ctx, pos) = show_setup("show_outside", "other/z.glb");
        assert_eq!(clickable_enabled_at(&ctx, pos), Some(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn show_reopens_a_closed_content_browser() {
        let (mut editor, mut world, root, ctx, pos) =
            show_setup("show_reopen", "assets/models/x.glb");
        let tab = editor
            .dock
            .find_tab(&layout::EditorTab::ContentBrowser)
            .unwrap();
        editor.dock.remove_tab(tab);
        full_frame(&ctx, &mut editor, &mut world, Vec::new());
        click_at(&ctx, &mut editor, &mut world, pos);
        assert!(
            editor
                .dock
                .find_tab(&layout::EditorTab::ContentBrowser)
                .is_some()
        );
        assert!(editor.content_shown, "Show put the browser back");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn show_brings_content_browser_to_front() {
        let (mut editor, mut world, root, ctx, pos) =
            show_setup("show_front", "assets/models/x.glb");
        assert!(!editor.content_shown, "browser starts behind the Console");
        click_at(&ctx, &mut editor, &mut world, pos);
        assert_eq!(
            editor.content.selected(),
            Some(root.join("assets/models/x.glb").as_path())
        );
        assert!(
            editor.content_shown,
            "Show brought the browser to the front"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One 1600 x 900 frame at `time` with `modifiers` held.
    fn drawer_frame(
        ctx: &egui::Context,
        editor: &mut Editor,
        world: &mut World,
        time: f64,
        modifiers: egui::Modifiers,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            time: Some(time),
            modifiers,
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| editor.build_ui(world, ui));
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn ctrl_space(ctx: &egui::Context, editor: &mut Editor, world: &mut World, time: f64) {
        let ctrl = egui::Modifiers::COMMAND;
        drawer_frame(
            ctx,
            editor,
            world,
            time,
            ctrl,
            vec![
                key_event(egui::Key::Space, ctrl),
                egui::Event::Key {
                    key: egui::Key::Space,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: ctrl,
                },
            ],
        );
    }

    fn press_events(pos: egui::Pos2) -> Vec<egui::Event> {
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![egui::Event::PointerMoved(pos), press(true), press(false)]
    }

    /// The Console in front of the Content Browser in their shared group.
    fn hide_content_tab(editor: &mut Editor) {
        let console = editor.dock.find_tab(&layout::EditorTab::Console).unwrap();
        let _ = editor.dock.set_active_tab(console);
    }

    /// A project with the browser tab hidden and the drawer opened by
    /// Ctrl+Space and slid fully in. Returns the time of the last frame.
    fn open_drawer(name: &str) -> (Editor, World, std::path::PathBuf, egui::Context, f64) {
        let (mut editor, mut world, root) = browser_project(name);
        hide_content_tab(&mut editor);
        let ctx = egui::Context::default();
        let none = egui::Modifiers::NONE;
        drawer_frame(&ctx, &mut editor, &mut world, 0.0, none, Vec::new());
        ctrl_space(&ctx, &mut editor, &mut world, 0.1);
        let mut t = 0.1;
        for _ in 0..5 {
            t += 0.1;
            drawer_frame(&ctx, &mut editor, &mut world, t, none, Vec::new());
        }
        assert!(editor.drawer.open);
        (editor, world, root, ctx, t)
    }

    #[test]
    fn ctrl_space_toggles_the_drawer_when_the_tab_is_hidden() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_toggle");
        let rect = editor.drawer.rect.expect("drawer drawn");
        assert_eq!(rect.width(), 1600.0);
        let full = 900.0 * theme::sizes::DRAWER_HEIGHT_SHARE;
        assert!((rect.height() - full).abs() < 1.0, "{rect:?}");
        assert!((rect.bottom() - (900.0 - theme::sizes::STATUS_HEIGHT)).abs() < 1.0);
        assert!(!editor.content_tab_shown);
        ctrl_space(&ctx, &mut editor, &mut world, t + 0.1);
        assert!(!editor.drawer.open);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ctrl_space_with_the_tab_visible_flashes_it_instead() {
        let (mut editor, mut world, root) = browser_project("drawer_flash");
        let ctx = egui::Context::default();
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            0.0,
            egui::Modifiers::NONE,
            Vec::new(),
        );
        ctrl_space(&ctx, &mut editor, &mut world, 0.1);
        assert!(!editor.drawer.open);
        assert!(editor.content.flashing());
        assert!(editor.content_tab_shown);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ctrl_space_ignored_with_game_focus() {
        let (mut editor, mut world, root) = browser_project("drawer_game");
        hide_content_tab(&mut editor);
        world.insert_resource(SimulationState::Playing);
        world.insert_resource(InputFocus::Game);
        let ctx = egui::Context::default();
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            0.0,
            egui::Modifiers::NONE,
            Vec::new(),
        );
        ctrl_space(&ctx, &mut editor, &mut world, 0.1);
        assert!(!editor.drawer.open);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn esc_closes_the_drawer() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_esc");
        let none = egui::Modifiers::NONE;
        let esc = vec![key_event(egui::Key::Escape, none)];
        drawer_frame(&ctx, &mut editor, &mut world, t + 0.1, none, esc);
        assert!(!editor.drawer.open);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn esc_while_renaming_keeps_the_drawer() {
        let (mut editor, mut world, root, ctx, mut t) = open_drawer("drawer_rename");
        let none = egui::Modifiers::NONE;
        editor.content.open(root.join("assets/models"));
        editor
            .content
            .start_rename(root.join("assets/models/x.glb"));
        for _ in 0..3 {
            t += 0.1;
            drawer_frame(&ctx, &mut editor, &mut world, t, none, Vec::new());
        }
        assert!(ctx.text_edit_focused(), "rename field has focus");
        let esc = vec![key_event(egui::Key::Escape, none)];
        drawer_frame(&ctx, &mut editor, &mut world, t + 0.1, none, esc);
        assert!(
            editor.content.renaming.is_none(),
            "Esc cancelled the rename"
        );
        assert!(editor.drawer.open, "and only the rename");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn press_outside_closes_inside_keeps() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_outside");
        let none = egui::Modifiers::NONE;
        let inside = editor.drawer.rect.unwrap().center();
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            t + 0.1,
            none,
            press_events(inside),
        );
        assert!(editor.drawer.open);
        let top = egui::pos2(800.0, 5.0);
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            t + 0.2,
            none,
            press_events(top),
        );
        assert!(!editor.drawer.open);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn button_toggles_closed_not_reopened() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_button");
        let none = egui::Modifiers::NONE;
        let button = editor
            .drawer
            .button
            .expect("status bar button drawn")
            .center();
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            t + 0.1,
            none,
            press_events(button),
        );
        assert!(!editor.drawer.open);
        drawer_frame(&ctx, &mut editor, &mut world, t + 0.2, none, Vec::new());
        assert!(!editor.drawer.open);
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            t + 0.3,
            none,
            press_events(button),
        );
        assert!(editor.drawer.open, "the button opens it again");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dragging_out_of_the_drawer_hides_it() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_drag");
        let none = egui::Modifiers::NONE;
        let rect = editor.drawer.rect.unwrap();
        let drag = content::AssetDrag {
            path: root.join("assets/models/x.glb"),
            model: Some("assets/models/x.glb".into()),
        };
        egui::DragAndDrop::set_payload(&ctx, drag);
        let inside = vec![egui::Event::PointerMoved(rect.center())];
        drawer_frame(&ctx, &mut editor, &mut world, t + 0.1, none, inside);
        assert!(editor.drawer.open, "a drag inside the drawer keeps it");
        let above = vec![egui::Event::PointerMoved(egui::pos2(
            800.0,
            rect.top() - 50.0,
        ))];
        drawer_frame(&ctx, &mut editor, &mut world, t + 0.2, none, above);
        assert!(!editor.drawer.open);
        assert!(
            egui::DragAndDrop::has_payload_of_type::<content::AssetDrag>(&ctx),
            "the drop still lands on what is under the cursor"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dock_in_layout_opens_the_tab_and_closes() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_dock");
        editor.dock_drawer();
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            t + 0.1,
            egui::Modifiers::NONE,
            Vec::new(),
        );
        assert!(!editor.drawer.open);
        assert!(editor.content_tab_shown, "the tab was drawn");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn drawer_takes_dropped_files() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_drop");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("crate.glb"), b"x").unwrap();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            time: Some(t + 0.1),
            dropped_files: vec![egui::DroppedFile {
                path: Some(outside.join("crate.glb")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| editor.build_ui(&mut world, ui));
        let imported = root.join("assets/crate.glb");
        assert!(imported.exists(), "the drawer's folder took the drop");
        assert!(melon_core::meta_path(&imported).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn closing_the_drawer_drops_browser_focus() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_focus");
        let none = egui::Modifiers::NONE;
        editor.content.focused = true;
        drawer_frame(&ctx, &mut editor, &mut world, t + 0.1, none, Vec::new());
        assert!(editor.content.has_focus(), "the drawer keeps the focus");
        let esc = vec![key_event(egui::Key::Escape, none)];
        drawer_frame(&ctx, &mut editor, &mut world, t + 0.2, none, esc);
        assert!(!editor.drawer.open);
        assert!(!editor.content.has_focus());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn drawer_tiles_want_thumbnails() {
        let (mut editor, mut world, root, ctx, t) = open_drawer("drawer_thumbs");
        editor.content.open(root.join("assets/models"));
        drawer_frame(
            &ctx,
            &mut editor,
            &mut world,
            t + 0.1,
            egui::Modifiers::NONE,
            Vec::new(),
        );
        assert_eq!(
            editor.content.thumbnails.wanted(),
            ["assets/models/x.glb".to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn type_path<T: melon_core::bevy_reflect::TypePath>() -> &'static str {
        T::type_path()
    }

    fn card_id<T: melon_core::bevy_reflect::TypePath>() -> egui::Id {
        egui::Id::new(("inspector_component", type_path::<T>()))
    }

    fn add_row_id<T: melon_core::bevy_reflect::TypePath>() -> egui::Id {
        egui::Id::new(("inspector_add", type_path::<T>()))
    }

    /// The Inspector alone, headless, with `entity` selected.
    struct InspectorHarness {
        editor: Editor,
        world: World,
        entity: Entity,
        ctx: egui::Context,
        time: f64,
    }

    impl InspectorHarness {
        fn new(world: World, entity: Entity) -> Self {
            let mut editor = Editor::new();
            editor.set_selected(Some(entity));
            let mut h = Self {
                editor,
                world,
                entity,
                ctx: egui::Context::default(),
                time: 0.0,
            };
            // egui's first frame is a sizing pass: clicks need a second one.
            h.frame(Vec::new());
            h.frame(Vec::new());
            h
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 900.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (editor, world) = (&mut self.editor, &mut self.world);
            let _ = self
                .ctx
                .run_ui(input, |ui| editor.build_inspector(world, ui));
        }

        fn has(&self, id: egui::Id) -> bool {
            self.ctx.read_response(id).is_some()
        }

        /// Move, press, release, then one quiet frame.
        fn click(&mut self, id: egui::Id) {
            let pos = self
                .ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("no widget with id {id:?}"))
                .rect
                .center();
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for events in [
                vec![egui::Event::PointerMoved(pos)],
                vec![button(true)],
                vec![button(false)],
                vec![],
            ] {
                self.frame(events);
            }
        }

        fn open_add_component(&mut self) {
            self.click(egui::Id::new("inspector_add_component"));
        }
    }

    fn entity_with(bundle: impl bevy_ecs::bundle::Bundle) -> InspectorHarness {
        let mut world = melon_core::Core::new().world;
        let e = world
            .spawn((Name("E".to_string()), Transform::default(), bundle))
            .id();
        InspectorHarness::new(world, e)
    }

    /// An entity with a fixed `RigidBody` and a unit box `Collider`, and
    /// rapier made to match.
    fn fixed_body_entity() -> InspectorHarness {
        let mut world = melon_core::Core::new().world;
        world.insert_resource(melon_physics::PhysicsWorld::new());
        let e = world
            .spawn((
                Name("Body".to_string()),
                Transform::default(),
                melon_core::RigidBody {
                    kind: melon_core::RigidBodyKind::Fixed,
                    ..Default::default()
                },
                melon_core::Collider::default(),
            ))
            .id();
        melon_physics::sync_components_system(&mut world);
        InspectorHarness::new(world, e)
    }

    #[test]
    fn every_registered_component_gets_a_card() {
        let mut h = entity_with((Aabb::default(), Spin));
        assert!(h.has(card_id::<Aabb>().with("header")));
        assert!(h.has(card_id::<Spin>().with("header")));
        assert!(!h.has(card_id::<melon_core::Bob>().with("header")));
        // Drawing without edits writes nothing back.
        h.world.clear_trackers();
        h.frame(Vec::new());
        let aabb = h.world.entity(h.entity).get_ref::<Aabb>().unwrap();
        assert!(!aabb.is_changed());
    }

    #[test]
    fn name_and_transform_have_no_remove() {
        let h = entity_with(Spin);
        assert!(!h.has(card_id::<Transform>().with("remove")));
        // `Name` is not reflected: it is the header, never a card.
        let name = egui::Id::new(("inspector_component", std::any::type_name::<Name>()));
        assert!(!h.has(name.with("remove")));
        assert!(h.has(card_id::<Spin>().with("remove")));
    }

    #[test]
    fn removing_spin_removes_the_component() {
        let mut h = entity_with(Spin);
        h.click(card_id::<Spin>().with("remove"));
        assert!(h.world.get::<Spin>(h.entity).is_none());
        assert!(h.world.get::<Transform>(h.entity).is_some());
    }

    #[test]
    fn add_component_lists_only_missing_addable() {
        let mut h = entity_with(Spin);
        h.open_add_component();
        assert!(h.has(add_row_id::<melon_core::Bob>()));
        assert!(h.has(add_row_id::<melon_core::RigidBody>()));
        assert!(h.has(add_row_id::<Aabb>()));
        assert!(!h.has(add_row_id::<Spin>()));
        assert!(!h.has(add_row_id::<Transform>()));
    }

    #[test]
    fn adding_a_collider_fits_its_box_to_the_model_bounds() {
        let mut world = melon_core::Core::new().world;
        let handle: Handle<Model> = world.resource_mut::<AssetServer>().load("builtin:cube");
        world.resource_mut::<AssetServer>().set_bounds(
            handle,
            glam::Vec3::new(-1.0, -2.0, -3.0),
            glam::Vec3::new(1.0, 2.0, 3.0),
        );
        let with_model = world
            .spawn((Name("M".to_string()), Transform::default(), handle))
            .id();
        let mut h = InspectorHarness::new(world, with_model);
        h.open_add_component();
        h.click(add_row_id::<melon_core::Collider>());
        let c = h.world.get::<melon_core::Collider>(h.entity).unwrap();
        assert_eq!(
            c.shape,
            melon_core::ColliderShape::Box {
                half_extents: glam::Vec3::new(1.0, 2.0, 3.0)
            }
        );
        // Without a model the default 0.5 box stays.
        let mut h = entity_with(Spin);
        h.open_add_component();
        h.click(add_row_id::<melon_core::Collider>());
        let c = h.world.get::<melon_core::Collider>(h.entity).unwrap();
        assert_eq!(c.shape, melon_core::ColliderShape::default());
    }

    #[test]
    fn collider_layer_fields_use_the_named_layer_widget() {
        let mut world = melon_core::Core::new().world;
        world
            .resource_mut::<melon_core::PhysicsSettings>()
            .layer_names[4] = "Enemies".into();
        let widgets = Editor::new().field_widgets(
            &world,
            <melon_core::Collider as melon_core::bevy_reflect::TypePath>::type_path(),
        );
        for field in ["membership", "filter"] {
            let widget = widgets
                .get(field)
                .unwrap_or_else(|| panic!("no widget for {field}"));
            let inspect::WidgetKind::Layers(names) = &widget.kind else {
                panic!("{field} is not a layer widget");
            };
            assert_eq!(names[0], "Default");
            assert_eq!(names[4], "Enemies");
        }
        assert!(widgets["membership"].label.is_some());
    }

    #[test]
    fn add_component_inserts_the_default() {
        let mut h = entity_with(Spin);
        h.open_add_component();
        h.click(add_row_id::<melon_core::Bob>());
        assert!(h.world.get::<melon_core::Bob>(h.entity).is_some());
        assert!(h.world.get::<Spin>(h.entity).is_some());
    }

    #[test]
    fn add_component_list_has_no_duplicates_after_reregister() {
        let mut core = melon_core::Core::new();
        core.register_component::<Spin>();
        let mut world = core.world;
        let e = world
            .spawn((Name("E".to_string()), Transform::default()))
            .id();
        let spins = add_component_choices(&world, e, "")
            .into_iter()
            .filter(|c| c.type_path == type_path::<Spin>())
            .count();
        assert_eq!(spins, 1);
        let mut h = InspectorHarness::new(world, e);
        h.open_add_component();
        assert!(h.has(add_row_id::<Spin>()));
    }

    #[test]
    fn editing_the_body_kind_updates_the_rapier_body() {
        let mut h = fixed_body_entity();
        let kind = card_id::<melon_core::RigidBody>().with("kind");
        h.click(kind);
        h.click(kind.with("Dynamic"));
        let rb = h.world.get::<melon_core::RigidBody>(h.entity).unwrap();
        assert_eq!(rb.kind, melon_core::RigidBodyKind::Dynamic);
        let handle = h
            .world
            .get::<melon_physics::RigidBodyComponent>(h.entity)
            .unwrap();
        assert_eq!(handle.body_type, melon_physics::RigidBodyType::Dynamic);
        let physics = h.world.resource::<melon_physics::PhysicsWorld>();
        assert!(physics.bodies.get(handle.body_handle).unwrap().is_dynamic());
        assert_eq!(physics.bodies.len(), 1, "the body is updated in place");
    }

    #[test]
    fn editing_the_collider_shape_rebuilds_the_collider() {
        let mut h = fixed_body_entity();
        let shape = card_id::<melon_core::Collider>().with("shape");
        h.click(shape.with("variant"));
        h.click(shape.with("Sphere"));
        let handle = h
            .world
            .get::<melon_physics::ColliderComponent>(h.entity)
            .unwrap()
            .collider_handle
            .unwrap();
        let physics = h.world.resource::<melon_physics::PhysicsWorld>();
        let colliders = physics.colliders();
        let co = colliders.get(handle).unwrap();
        assert!(co.shared_shape().as_ball().is_some());
        assert_eq!(physics.colliders().len(), 1);
    }

    #[test]
    fn removing_the_rigid_body_removes_the_rapier_body() {
        let mut h = fixed_body_entity();
        h.click(card_id::<melon_core::RigidBody>().with("remove"));
        assert!(h.world.get::<melon_core::RigidBody>(h.entity).is_none());
        assert!(
            h.world
                .get::<melon_physics::RigidBodyComponent>(h.entity)
                .is_none()
        );
        let physics = h.world.resource::<melon_physics::PhysicsWorld>();
        assert_eq!(physics.bodies.len(), 0);
        // The collider stays, as a static one.
        assert_eq!(physics.colliders().len(), 1);
    }

    /// A moving, selected entity must not make egui run extra passes (each
    /// one redraws the whole editor; "new Grid" every frame halved the FPS).
    #[test]
    fn moving_selected_entity_needs_one_pass_per_frame() {
        let (mut editor, mut world, root) = browser_project("one_pass");
        let entity = world
            .spawn((
                Name("Cube".to_string()),
                Transform::default(),
                melon_core::Spin,
                melon_core::Aabb {
                    half_extents: glam::Vec3::splat(0.5),
                },
            ))
            .id();
        editor.set_selected(Some(entity));
        let ctx = egui::Context::default();
        let mut reasons = Vec::new();
        for frame in 0..8 {
            world.get_mut::<Transform>(entity).unwrap().rotation =
                glam::Quat::from_rotation_y(frame as f32 * 0.7);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| editor.build_ui(&mut world, ui));
            if frame >= 3 {
                reasons.extend(
                    output
                        .platform_output
                        .request_discard_reasons
                        .iter()
                        .map(|r| format!("{r:?}")),
                );
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(reasons.is_empty(), "extra passes: {reasons:#?}");
    }

    fn history_world() -> World {
        let mut world = melon_core::Core::new().world;
        world.insert_resource(melon_physics::PhysicsWorld::new());
        world
    }

    fn live_names(world: &mut World) -> Vec<String> {
        let mut names: Vec<String> = world
            .query_filtered::<&Name, bevy_ecs::query::Without<melon_core::PendingDelete>>()
            .iter(world)
            .map(|n| n.0.clone())
            .collect();
        names.sort();
        names
    }

    fn press(editor: &mut Editor, world: &mut World, key: egui::Key, shift: bool) {
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            shift,
            ..Default::default()
        };
        let input = egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        };
        let _ = egui::Context::default().run_ui(input, |ui| editor.handle_shortcuts(world, ui));
    }

    #[test]
    fn undo_redo_duplicate_shortcuts_map() {
        let s = |key, ctrl, shift, typing| shortcut_for(key, ctrl, shift, typing, false);
        assert_eq!(s(egui::Key::Z, true, false, false), Some(Shortcut::Undo));
        assert_eq!(s(egui::Key::Z, true, true, false), Some(Shortcut::Redo));
        assert_eq!(s(egui::Key::Y, true, false, false), Some(Shortcut::Redo));
        assert_eq!(
            s(egui::Key::D, true, false, false),
            Some(Shortcut::Duplicate)
        );
        assert_eq!(
            s(egui::Key::F2, false, false, false),
            Some(Shortcut::Rename)
        );
        assert_eq!(s(egui::Key::Z, false, false, false), None);
        for key in [egui::Key::Z, egui::Key::Y, egui::Key::D] {
            assert_eq!(
                s(key, true, false, true),
                None,
                "typing keeps egui's own undo"
            );
        }
    }

    #[test]
    fn ctrl_z_and_ctrl_y_walk_a_delete_and_restore_the_selection() {
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        let cube = editor.selected.unwrap();
        editor.delete_selected(&mut world);
        assert_eq!(editor.selected, None);
        // What the physics PostUpdate does with marked entities.
        let mut cleanup = bevy_ecs::schedule::Schedule::default();
        cleanup.add_systems(melon_physics::cleanup_deleted_system);
        cleanup.run(&mut world);
        assert!(live_names(&mut world).is_empty());

        press(&mut editor, &mut world, egui::Key::Z, false);
        assert_eq!(live_names(&mut world), ["Cube"]);
        assert!(
            editor.selected.is_some(),
            "the deleted entity is selected again"
        );
        let _ = cube;
        press(&mut editor, &mut world, egui::Key::Z, true); // redo
        assert!(live_names(&mut world).is_empty());
        assert_eq!(editor.selected, None);
        press(&mut editor, &mut world, egui::Key::Z, false);
        press(&mut editor, &mut world, egui::Key::Z, false); // undoes the create
        assert!(live_names(&mut world).is_empty());
    }

    #[test]
    fn ctrl_d_duplicates_and_selects_the_copy() {
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        let original = editor.selected.unwrap();
        press(&mut editor, &mut world, egui::Key::D, false);
        assert_ne!(editor.selected, Some(original));
        assert_eq!(live_names(&mut world), ["Cube", "Cube (1)"]);
        assert_eq!(
            editor.history.undo_label(),
            Some("Duplicate Cube"),
            "one step"
        );
    }

    #[test]
    fn create_while_playing_records_nothing() {
        let mut world = history_world();
        let mut editor = Editor::new();
        world.insert_resource(SimulationState::Playing);
        editor.create_cube_step(&mut world);
        assert!(!editor.history.can_undo());
    }

    #[test]
    fn f2_renames_inline_on_enter_and_esc_cancels() {
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        let cube = editor.selected.unwrap();
        let ctx = egui::Context::default();
        let frame = |editor: &mut Editor, world: &mut World, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| editor.build_hierarchy(world, ui));
        };
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        editor.start_rename(&world);
        frame(&mut editor, &mut world, vec![]);
        frame(&mut editor, &mut world, vec![]);
        frame(
            &mut editor,
            &mut world,
            vec![egui::Event::Text("Hero".into())],
        );
        frame(&mut editor, &mut world, vec![key(egui::Key::Enter)]);
        frame(&mut editor, &mut world, vec![]);
        assert_eq!(world.get::<Name>(cube).unwrap().0, "Hero");
        assert!(editor.renaming.is_none());
        assert_eq!(editor.history.undo_label(), Some("Rename Cube"));

        editor.start_rename(&world);
        frame(&mut editor, &mut world, vec![]);
        frame(
            &mut editor,
            &mut world,
            vec![egui::Event::Text("Nope".into())],
        );
        frame(&mut editor, &mut world, vec![key(egui::Key::Escape)]);
        frame(&mut editor, &mut world, vec![]);
        assert_eq!(world.get::<Name>(cube).unwrap().0, "Hero");
        assert!(editor.renaming.is_none());
    }

    #[test]
    fn loading_a_scene_clears_the_history() {
        let dir = pending_dir("history_clear");
        let path = dir.join("scene.ron");
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        editor.pending_operation = Some(PendingOperation::Save(path.clone()));
        editor.run_pending_operation(&mut world);
        assert!(editor.history.can_undo(), "saving keeps the history");
        editor.pending_operation = Some(PendingOperation::Load(path));
        editor.run_pending_operation(&mut world);
        assert!(!editor.history.can_undo() && !editor.history.can_redo());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_z_with_an_empty_history_changes_nothing() {
        let mut world = history_world();
        let mut editor = Editor::new();
        let e = create_cube(&mut world);
        editor.selected = Some(e);
        press(&mut editor, &mut world, egui::Key::Z, false);
        press(&mut editor, &mut world, egui::Key::Y, false);
        assert_eq!(editor.selected, Some(e));
        assert_eq!(live_names(&mut world), ["Cube"]);
    }

    #[test]
    fn undoing_a_create_leaves_no_stale_selection() {
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        press(&mut editor, &mut world, egui::Key::Z, false);
        assert_eq!(editor.selected, None, "nothing was selected before");
        press(&mut editor, &mut world, egui::Key::Z, true);
        assert!(editor.selected.is_some());
        // With a selection before the create, undo returns to it.
        let first = editor.selected.unwrap();
        editor.create_cube_step(&mut world);
        assert_ne!(editor.selected, Some(first));
        press(&mut editor, &mut world, egui::Key::Z, false);
        assert_eq!(editor.selected, Some(first));
        let mut cleanup = bevy_ecs::schedule::Schedule::default();
        cleanup.add_systems(melon_physics::cleanup_deleted_system);
        cleanup.run(&mut world);
        assert_eq!(live_names(&mut world), ["Cube"]);
    }

    /// One frame: a text field (focused from the first frame when `typing`),
    /// then the shortcuts with Ctrl+Z pressed on frame 3.
    fn ctrl_z_frames(editor: &mut Editor, world: &mut World, typing: bool) {
        let ctx = egui::Context::default();
        let mut text = String::new();
        for frame in 0..4 {
            let modifiers = if frame == 3 {
                egui::Modifiers::COMMAND
            } else {
                egui::Modifiers::NONE
            };
            let events = if frame == 3 {
                vec![egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }]
            } else {
                vec![]
            };
            let input = egui::RawInput {
                modifiers,
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                if typing {
                    let edit = ui.add(egui::TextEdit::singleline(&mut text).id(egui::Id::new("t")));
                    if frame == 0 {
                        edit.request_focus();
                    }
                }
                editor.handle_shortcuts(world, ui);
            });
        }
    }

    #[test]
    fn ctrl_z_in_a_focused_text_field_leaves_the_scene_alone() {
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        ctrl_z_frames(&mut editor, &mut world, true);
        assert_eq!(live_names(&mut world), ["Cube"], "scene not undone");
        assert!(editor.history.can_undo());
        // Control: with no text field focused the same keys undo.
        ctrl_z_frames(&mut editor, &mut world, false);
        assert!(!editor.history.can_undo());
    }

    #[test]
    fn row_menu_actions_are_recorded() {
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        let cube = editor.selected.unwrap();
        editor.row_action(&mut world, RowAction::CreateChild, cube);
        let child = editor.selected.unwrap();
        assert_eq!(
            world.get::<melon_core::ChildOf>(child).unwrap().parent(),
            cube
        );
        assert_eq!(world.get::<Name>(child).unwrap().0, "Cube");
        assert_eq!(editor.history.undo_label(), Some("Create Cube"));
        editor.row_action(&mut world, RowAction::Duplicate, child);
        assert_eq!(editor.history.undo_label(), Some("Duplicate Cube"));
        editor.row_action(&mut world, RowAction::Delete, child);
        assert_eq!(editor.history.undo_label(), Some("Delete Cube"));
        editor.row_action(&mut world, RowAction::Rename, cube);
        assert_eq!(editor.renaming.as_ref().map(|r| r.entity), Some(cube));
    }

    #[test]
    fn rename_is_cancelled_when_its_row_is_hidden() {
        let mut world = history_world();
        let mut editor = Editor::new();
        editor.create_cube_step(&mut world);
        editor.hierarchy_query = "zzz".into();
        editor.start_rename(&world);
        let ctx = egui::Context::default();
        full_frame(&ctx, &mut editor, &mut world, vec![]);
        full_frame(&ctx, &mut editor, &mut world, vec![]);
        assert!(editor.renaming.is_none());
    }
}
