use std::sync::Arc;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use egui::ViewportId;
use egui_wgpu::ScreenDescriptor;
use melon_core::glam;
use melon_core::hierarchy::{
    depth_first, mark_for_delete, set_parent_keep_world, set_world_transform, world_matrix,
};
use melon_core::{
    Aabb, AssetServer, BodyInfo, Camera, GameInputGate, Handle, InputFocus, Model, Name,
    SimulationState, Spin, Transform,
};
use melon_render::LineVertex;
use tracing::instrument;
use winit::event::WindowEvent;
use winit::window::Window;

pub mod asset_db;
pub mod asset_ops;
pub mod console;
pub mod content;
mod content_view;
pub mod gizmo;
pub mod layout;
pub mod play;
pub mod project_settings;
pub mod scene;
pub mod theme;
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
    pending_dialog: Option<DialogChannel>,
    pending_operation: Option<PendingOperation>,
    status_message: Option<(String, StatusKind, std::time::Instant)>,
    /// egui texture showing the scene (set by `EditorOverlay`).
    viewport_texture: Option<egui::TextureId>,
    /// Pixel size the viewport panel wants its render target to be.
    viewport_size: winit::dpi::PhysicalSize<u32>,
    /// Scene-view camera driven by the viewport (Unity controls).
    camera: viewport::EditorCamera,
    /// Focus the Scene viewport saw last frame: Game -> Editor adopts the game's `Camera`.
    last_viewport_focus: InputFocus,
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
    /// Hierarchy rows whose children are hidden.
    collapsed: std::collections::HashSet<Entity>,
    /// Scene file last saved or loaded; File > Save writes here.
    current_scene: Option<std::path::PathBuf>,
    /// Right mouse held over the viewport (fly mode): W/E/R are fly keys then.
    flying: bool,
    show_about: bool,
    project_settings: project_settings::ProjectSettings,
    /// A rebinding capture was waiting when this frame began: its keys are not shortcuts.
    keys_captured: bool,
    logo: Option<egui::TextureHandle>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            selected: None,
            gizmo_mode: GizmoMode::Move,
            gizmo_drag: None,
            hovered: None,
            overlay_draws: Vec::new(),
            pending_dialog: None,
            pending_operation: None,
            status_message: None,
            viewport_texture: None,
            viewport_size: winit::dpi::PhysicalSize::new(1, 1),
            camera: viewport::EditorCamera::default(),
            last_viewport_focus: InputFocus::Editor,
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
            collapsed: std::collections::HashSet::new(),
            current_scene: None,
            flying: false,
            show_about: false,
            project_settings: project_settings::ProjectSettings::default(),
            keys_captured: false,
            logo: None,
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
            mark_for_delete(world, entity);
        }
        self.clear_selection();
    }

    fn open_dialog(&mut self, save: bool) {
        if self.pending_dialog.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new().add_filter("RON", &["ron"]);
            let path = if save {
                dialog.set_file_name("scene.ron").save_file()
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
        let pressed: Vec<Shortcut> = ui.input(|i| {
            [
                egui::Key::S,
                egui::Key::Delete,
                egui::Key::W,
                egui::Key::E,
                egui::Key::R,
                egui::Key::Escape,
            ]
            .into_iter()
            .filter(|key| fresh_press(i, *key))
            .filter_map(|key| shortcut_for(key, i.modifiers.command, typing, self.flying))
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
                Shortcut::Delete => self.delete_selected(world),
                Shortcut::Move => self.gizmo_mode = GizmoMode::Move,
                Shortcut::Rotate => self.gizmo_mode = GizmoMode::Rotate,
                Shortcut::Scale => self.gizmo_mode = GizmoMode::Scale,
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
                ui.separator();
                if ui.button("Exit").clicked() {
                    self.pending_operation = Some(PendingOperation::Exit);
                }
            });
            ui.menu_button("Edit", |ui| {
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
                    self.selected = Some(create_cube(world));
                }
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
        std::mem::take(&mut self.overlay_draws)
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
                        self.set_status(format!("Loaded from {}", path.display()));
                        self.set_current_scene(path);
                    }
                    Err(e) => self.set_status_error(format!("Load failed: {e}")),
                }
                false
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
        if let Some(path) = response.dnd_release_payload::<String>()
            && content::is_model_path(&path)
            && let (Some(pointer), Some(camera)) =
                (vp.pointer, world.get_resource::<Camera>().copied())
        {
            let at = viewport::drop_point(&camera, pointer, vp.size);
            match spawn_model(world, &self.project_root, &path, at, None) {
                Ok(e) => self.selected = Some(e),
                Err(message) => self.set_status_warning(message),
            }
            self.gizmo_drag = None;
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
        if let Some(e) = self.selected
            && world.get::<Transform>(e).is_some()
        {
            self.world_line_vertices
                .extend(viewport::selection_box_lines(
                    world_matrix(world, e),
                    viewport::model_bounds(world, e),
                ));
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
            self.selected = Some(create_cube(world));
        }

        ui.separator();

        // Entity tree. Rows are drag sources (reparent) and drop targets for
        // entities and models; the space below the rows makes roots.
        let mut to_delete: Option<Entity> = None;
        let mut reparent: Option<(Entity, Option<Entity>)> = None;
        let mut model_drop: Option<(String, Option<Entity>)> = None;
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
        let carried_model = egui::DragAndDrop::payload::<String>(ui.ctx()).map(|p| (*p).clone());
        let carrying = carried_entity.is_some() || carried_model.is_some();
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
                    to_delete = Some(entity);
                }
                if !searching && response.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), entity);
                }

                let painter = ui.painter();
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
                painter.text(
                    egui::pos2(text_x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &name,
                    egui::TextStyle::Body.resolve(ui.style()),
                    theme::colors::TEXT,
                );

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
        }

        if let Some((dragged, target)) = reparent
            && target != Some(dragged)
            && let Err(message) = hierarchy_drop(world, dragged, target)
        {
            self.set_status_warning(message);
        }
        if let Some((path, parent)) = model_drop
            && content::is_model_path(&path)
        {
            match spawn_model(world, &self.project_root, &path, glam::Vec3::ZERO, parent) {
                Ok(e) => self.selected = Some(e),
                Err(message) => self.set_status_warning(message),
            }
        }

        if let Some(entity) = to_delete {
            mark_for_delete(world, entity);
            if self.selected == Some(entity) {
                self.clear_selection();
            }
        }
    }

    fn build_inspector(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let Some(entity) = self.selected else {
            ui.weak("Nothing selected");
            return;
        };
        // Read before borrowing the entity mutably (the server is a world resource).
        let current_model = world
            .get::<Handle<Model>>(entity)
            .zip(world.get_resource::<AssetServer>())
            .map(|(h, server)| server.path(*h).to_string());
        let mut new_model: Option<String> = None;
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
                    egui::ComboBox::from_id_salt("inspector_mesh")
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

        if let Some(body_info) = entity_mut.get::<BodyInfo>() {
            let body_info = *body_info;
            inspector_card(ui, &mut card_fills, "Rigidbody", |ui| {
                ui.label("Type");
                inspector_value(ui, format!("{:?}", body_info.body_type));
                ui.end_row();
                let h = body_info.half_extents;
                ui.label("Half extents");
                inspector_value(ui, format!("{:.2}  {:.2}  {:.2}", h.x, h.y, h.z));
                ui.end_row();
            });
        }

        let spins = entity_mut.get::<Spin>().is_some();
        let bobs = entity_mut.get::<melon_core::Bob>().is_some();
        if spins || bobs {
            inspector_card(ui, &mut card_fills, "Behaviours", |ui| {
                if spins {
                    ui.label("Spin");
                    inspector_value(ui, "Rotates about Y while playing");
                    ui.end_row();
                }
                if bobs {
                    ui.label("Bob");
                    inspector_value(ui, "Moves up and down while playing");
                    ui.end_row();
                }
            });
        }

        if transform_edited {
            self.after_transform_edit(world, entity);
        }
        if let Some(path) = new_model {
            let handle: Handle<Model> = world.resource_mut::<AssetServer>().load(&path);
            world.entity_mut(entity).insert(handle);
        }
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

/// Spawn a unit cube at the origin, named "Cube N".
pub fn create_cube(world: &mut World) -> Entity {
    let mut counter = world.get_resource_or_insert_with(melon_core::NameCounter::default);
    let name = format!("Cube {}", counter.0);
    counter.0 += 1;
    let cube: Handle<Model> = world
        .get_resource_or_insert_with(|| AssetServer::new(".".into()))
        .load("builtin:cube");
    world
        .spawn((
            Name(name),
            melon_core::Transform::default(),
            cube,
            Aabb {
                half_extents: glam::Vec3::new(0.5, 0.5, 0.5),
            },
        ))
        .id()
}

fn inspector_card(
    ui: &mut egui::Ui,
    fills: &mut [egui::Color32; 2],
    title: &str,
    rows: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::new()
        .fill(fills[0])
        .stroke(egui::Stroke::new(1.0, theme::colors::BORDER))
        .corner_radius(theme::sizes::RADIUS)
        .inner_margin(theme::sizes::SPACING)
        .show(ui, |ui| {
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
/// `Err` (nothing spawned) if the meshes would pass `MAX_ENTITIES`.
pub fn spawn_model(
    world: &mut World,
    project_root: &std::path::Path,
    path: &str,
    at: glam::Vec3,
    parent: Option<Entity>,
) -> Result<Entity, String> {
    let name = std::path::Path::new(path)
        .file_stem()
        .map_or_else(|| "Model".to_string(), |s| s.to_string_lossy().into_owned());
    let nodes = (!path.starts_with(melon_core::assets::BUILTIN_PREFIX))
        .then(|| melon_render::gltf_nodes(&project_root.join(path)).ok())
        .flatten();
    let meshes = nodes
        .as_ref()
        .map_or(1, |n| n.iter().filter(|n| n.mesh.is_some()).count());
    let used = world.query::<&Handle<Model>>().iter(world).count();
    if used + meshes > melon_render::MAX_ENTITIES {
        return Err(format!(
            "{name} has {meshes} meshes: the scene would pass the {} model limit ({used} used)",
            melon_render::MAX_ENTITIES
        ));
    }

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
        return Ok(root);
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
    Ok(root)
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
pub fn shortcut_for(key: egui::Key, ctrl: bool, typing: bool, flying: bool) -> Option<Shortcut> {
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
            EditorTab::ContentBrowser => self.editor.content.ui(ui),
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

    #[test]
    fn hierarchy_drop_moves_fixed_body_to_world_pose() {
        let mut world = World::new();
        let mut physics = melon_physics::PhysicsWorld::new();
        let (body, _) = physics.spawn_fixed_cuboid(
            glam::Vec3::new(2.0, 0.0, 0.0),
            glam::Quat::IDENTITY,
            glam::Vec3::splat(0.5),
        );
        world.insert_resource(physics);
        let target = world
            .spawn(Transform {
                translation: glam::Vec3::new(5.0, 0.0, 0.0),
                rotation: glam::Quat::from_rotation_y(1.0),
                ..Default::default()
            })
            .id();
        let dragged = world
            .spawn((
                Transform::from_translation(glam::Vec3::new(2.0, 0.0, 0.0)),
                melon_physics::RigidBodyComponent {
                    body_handle: body,
                    body_type: melon_physics::RigidBodyType::Fixed,
                },
            ))
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
            "assets/models/missing.glb".to_string(),
            Some(a),
        );
        let children = world.get::<melon_core::Children>(a).map_or(0, |c| c.len());
        assert_eq!(children, 1);
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
        let mut world = World::new();
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
        let mut physics = melon_physics::PhysicsWorld::new();
        let (body, _) = physics.spawn_dynamic_cuboid(
            glam::Vec3::ZERO,
            glam::Quat::IDENTITY,
            glam::Vec3::splat(0.5),
        );
        world.insert_resource(physics);
        world.insert_resource(SimulationState::Paused);
        let e = world
            .spawn((
                Transform::default(),
                melon_physics::RigidBodyComponent {
                    body_handle: body,
                    body_type: melon_physics::RigidBodyType::Dynamic,
                },
            ))
            .id();
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
        let mut physics = melon_physics::PhysicsWorld::new();
        let (body, _) = physics.spawn_dynamic_cuboid(
            glam::Vec3::ZERO,
            glam::Quat::IDENTITY,
            glam::Vec3::splat(0.5),
        );
        world.insert_resource(physics);
        world.insert_resource(SimulationState::Paused);
        let e = world
            .spawn((
                Transform::from_translation(glam::Vec3::new(4.0, 0.0, 0.0)),
                melon_core::PreviousTransform(Transform::default()),
                melon_physics::RigidBodyComponent {
                    body_handle: body,
                    body_type: melon_physics::RigidBodyType::Dynamic,
                },
            ))
            .id();
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
            shortcut_for(egui::Key::S, true, false, false),
            Some(Shortcut::Save)
        );
        assert_eq!(shortcut_for(egui::Key::S, false, false, false), None);
    }

    #[test]
    fn shortcuts_blocked_while_typing() {
        for (key, ctrl) in [
            (egui::Key::W, false),
            (egui::Key::Delete, false),
            (egui::Key::S, true),
        ] {
            assert_eq!(shortcut_for(key, ctrl, true, false), None, "{key:?}");
        }
    }

    #[test]
    fn tool_keys_ignored_while_flying() {
        assert_eq!(shortcut_for(egui::Key::W, false, false, true), None);
        assert_eq!(
            shortcut_for(egui::Key::W, false, false, false),
            Some(Shortcut::Move)
        );
        assert_eq!(
            shortcut_for(egui::Key::E, false, false, false),
            Some(Shortcut::Rotate)
        );
        assert_eq!(
            shortcut_for(egui::Key::R, false, false, false),
            Some(Shortcut::Scale)
        );
        assert_eq!(
            shortcut_for(egui::Key::Delete, false, false, false),
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
        let root = spawn_model(&mut world, &root_dir, "assets/models/two.gltf", at, None).unwrap();
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
        let mut world = World::new();
        spawn_model(
            &mut world,
            &root_dir,
            "assets/models/flat.gltf",
            glam::Vec3::ZERO,
            None,
        )
        .unwrap();
        let data = scene::scene_to_data(&mut world);
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
        )
        .unwrap();
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
        )
        .unwrap();
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
    fn drop_past_entity_cap_is_refused() {
        let root_dir = two_node_project("cap");
        let mut world = World::new();
        let cube: Handle<Model> = world
            .get_resource_or_insert_with(|| AssetServer::new(".".into()))
            .load("builtin:cube");
        for _ in 0..melon_render::MAX_ENTITIES - 1 {
            world.spawn((Transform::default(), cube));
        }
        let before = world.query::<Entity>().iter(&world).count();
        let result = spawn_model(
            &mut world,
            &root_dir,
            "assets/models/two.gltf",
            glam::Vec3::ZERO,
            None,
        );
        assert!(result.is_err());
        assert_eq!(world.query::<Entity>().iter(&world).count(), before);
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
        )
        .unwrap();
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
    fn create_cube_spawns_named_cube() {
        let mut world = World::new();
        let e = create_cube(&mut world);
        assert_eq!(world.get::<Name>(e).unwrap().0, "Cube 0");
        assert!(world.get::<Handle<Model>>(e).is_some());
        let e2 = create_cube(&mut world);
        assert_eq!(world.get::<Name>(e2).unwrap().0, "Cube 1");
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
            shortcut_for(egui::Key::Escape, false, false, false),
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
}
