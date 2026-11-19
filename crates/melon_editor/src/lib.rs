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
    Aabb, AssetServer, BodyInfo, Camera, Handle, Model, Name, SimulationState, Spin, Transform,
};
use melon_render::LineVertex;
use tracing::instrument;
use winit::event::WindowEvent;
use winit::window::Window;

pub mod console;
pub mod content;
pub mod gizmo;
pub mod layout;
pub mod play;
pub mod scene;
pub mod theme;
pub mod viewport;

use gizmo::{
    AXIS_X, AXIS_Y, AXIS_Z, CIRCLE_SEGMENTS, DEGENERATE_CIRCLE_PX, GIZMO_SIZE_PX, PICK_THRESHOLD,
    SCALE_MAX, SCALE_MIN, SNAP_MOVE, SNAP_ROTATE_RAD, SNAP_SCALE, axis_color, circle_lines,
    compute_drag_plane, cube_handle_lines, hovered_color, plane_basis, point_to_segment_dist_2d,
    project_onto_axis, ray_plane_intersect, rotate_angle_in_plane, snap_angle, snap_value,
    wrap_angle_delta,
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
    axis: glam::Vec3,
    axis_index: usize,
    mode: GizmoMode,
    initial_translation: glam::Vec3,
    initial_rotation: glam::Quat,
    initial_scale: glam::Vec3,
    initial_hit_point: glam::Vec3,
    initial_angle: f32,
    initial_projection: f32,
    plane_point: glam::Vec3,
    plane_normal: glam::Vec3,
    plane_tangent: glam::Vec3,
    plane_bitangent: glam::Vec3,
    snap: bool,
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

/// Editor holds the UI state (selection, gizmo mode, etc.) and builds the egui UI.
pub struct Editor {
    selected: Option<Entity>,
    gizmo_mode: GizmoMode,
    gizmo_drag: Option<GizmoDrag>,
    hovered_axis: Option<usize>,
    gizmo_vertices: Vec<LineVertex>,
    pending_dialog: Option<DialogChannel>,
    pending_operation: Option<PendingOperation>,
    status_message: Option<(String, StatusKind, std::time::Instant)>,
    /// egui texture showing the scene (set by `EditorOverlay`).
    viewport_texture: Option<egui::TextureId>,
    /// Pixel size the viewport panel wants its render target to be.
    viewport_size: winit::dpi::PhysicalSize<u32>,
    /// Scene-view camera driven by the viewport (Unity controls).
    camera: viewport::EditorCamera,
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
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            selected: None,
            gizmo_mode: GizmoMode::Move,
            gizmo_drag: None,
            hovered_axis: None,
            gizmo_vertices: Vec::new(),
            pending_dialog: None,
            pending_operation: None,
            status_message: None,
            viewport_texture: None,
            viewport_size: winit::dpi::PhysicalSize::new(1, 1),
            camera: viewport::EditorCamera::default(),
            world_line_vertices: Vec::new(),
            frame_ms: 16.7,
            console: console::ConsoleLog::default(),
            console_filter: console::ConsoleFilter::default(),
            dock: layout::default_layout(),
            content: content::ContentBrowser::new("assets".into()),
            layout_file: layout::layout_path(),
            project_root: ".".into(),
            hierarchy_query: String::new(),
            collapsed: std::collections::HashSet::new(),
            current_scene: None,
            flying: false,
            show_about: false,
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
        self.layout_file = root.join(layout::layout_path());
        self.project_root = root.to_path_buf();
        self.dock = layout::load_layout(&self.layout_file);
    }

    /// Save the panel arrangement; the host calls this when the window closes.
    pub fn save_layout(&self) {
        if let Err(error) = layout::save_layout(&self.dock, &self.layout_file) {
            tracing::warn!(?error, "could not save the editor layout");
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
            ]
            .into_iter()
            .filter(|key| i.key_pressed(*key))
            .filter_map(|key| shortcut_for(key, i.modifiers.command, typing, self.flying))
            .collect()
        });
        for shortcut in pressed {
            match shortcut {
                Shortcut::Save => self.save(world),
                Shortcut::Delete => self.delete_selected(world),
                Shortcut::Move => self.gizmo_mode = GizmoMode::Move,
                Shortcut::Rotate => self.gizmo_mode = GizmoMode::Rotate,
                Shortcut::Scale => self.gizmo_mode = GizmoMode::Scale,
            }
        }
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
            for (mode, label, key) in [
                (GizmoMode::Move, "Move", "W"),
                (GizmoMode::Rotate, "Rotate", "E"),
                (GizmoMode::Scale, "Scale", "R"),
            ] {
                ui.selectable_value(&mut self.gizmo_mode, mode, label)
                    .on_hover_text(format!("{label} tool ({key})"));
            }
            let snap = ui.input(|i| i.modifiers.shift);
            if snap {
                ui.colored_label(theme::colors::INFO, "Snap");
            }
            // Centre the play controls in the remaining width.
            let controls_width = 330.0;
            ui.add_space(((ui.available_width() - controls_width) / 2.0 - 160.0).max(8.0));
            self.play_controls(world, ui);
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
        egui::Window::new("About MelonEngine")
            .open(&mut self.show_about)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.heading(format!("MelonEngine {}", melon_core::version()));
                ui.label("A Rust 3D game engine.");
                ui.weak("bevy_ecs · wgpu · rapier3d · egui");
            });
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
        self.hovered_axis = None;
    }

    /// Grid + selection box for this frame, drawn with `draw_world_lines`.
    pub fn take_world_line_vertices(&mut self) -> Vec<LineVertex> {
        std::mem::take(&mut self.world_line_vertices)
    }

    pub fn take_gizmo_vertices(&mut self) -> Vec<LineVertex> {
        std::mem::take(&mut self.gizmo_vertices)
    }

    pub fn take_pending_operation(&mut self) -> Option<PendingOperation> {
        std::mem::take(&mut self.pending_operation)
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
        self.handle_shortcuts(world, ui);

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

        // Dockable panels fill the rest of the window.
        self.gizmo_vertices.clear();
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

        let on_gizmo = self.hovered_axis.is_some() || self.gizmo_drag.is_some();
        if viewport::left_action(vp.alt, on_gizmo, vp.left_clicked) == viewport::LeftAction::Select
            && let (Some(pointer), Some(camera)) =
                (vp.pointer, world.get_resource::<Camera>().copied())
        {
            let (origin, dir) = camera.screen_to_world_ray(pointer, vp.size);
            self.selected = viewport::pick(world, origin, dir);
            self.gizmo_drag = None;
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
        // Shortcuts are ignored while typing in a text field.
        if !ui.ctx().text_edit_focused() {
            ui.input(|i| {
                if i.key_pressed(egui::Key::F5) {
                    if i.modifiers.shift {
                        want_stop = true;
                    } else {
                        want_play = true;
                    }
                }
                want_pause |= i.key_pressed(egui::Key::F6);
            });
        }
        let can_play = state != SimulationState::Playing;
        let can_pause = state == SimulationState::Playing;
        let can_stop = state != SimulationState::Editing;
        want_play |= ui
            .add_enabled(can_play, egui::Button::new("▶ Play"))
            .clicked();
        want_pause |= ui
            .add_enabled(can_pause, egui::Button::new("⏸ Pause"))
            .clicked();
        let step = ui
            .add_enabled(
                state == SimulationState::Paused,
                egui::Button::new("⏭ Step"),
            )
            .on_hover_text("Advance one fixed step")
            .clicked();
        want_stop |= ui
            .add_enabled(can_stop, egui::Button::new("⏹ Stop"))
            .clicked();
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
                let row = ui.horizontal(|ui| {
                    if !searching {
                        ui.add_space(depth as f32 * theme::sizes::SPACING * 2.0);
                        if has_children {
                            let open = !self.collapsed.contains(&entity);
                            if ui
                                .small_button(if open { "\u{25be}" } else { "\u{25b8}" })
                                .clicked()
                            {
                                if open {
                                    self.collapsed.insert(entity);
                                } else {
                                    self.collapsed.remove(&entity);
                                }
                            }
                        } else {
                            ui.add_space(theme::sizes::SPACING * 3.0);
                        }
                    }
                    if searching {
                        ui.selectable_label(is_selected, &name)
                    } else {
                        ui.dnd_drag_source(egui::Id::new(("hierarchy_row", entity)), entity, |ui| {
                            ui.selectable_label(is_selected, &name)
                        })
                        .inner
                    }
                });
                let response = row.inner;
                if response.clicked() {
                    self.selected = if is_selected { None } else { Some(entity) };
                    self.gizmo_drag = None;
                }
                if response.secondary_clicked() {
                    to_delete = Some(entity);
                }
                if !searching && carrying && row.response.contains_pointer() {
                    ui.painter().rect_stroke(
                        row.response.rect,
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

        let mut transform_edited = false;
        if let Some(mut transform) = entity_mut.get_mut::<Transform>() {
            // Edit a copy and write back only on change, so change detection
            // does not fire every frame just because the inspector is open.
            let mut t = *transform;
            egui::CollapsingHeader::new("Transform")
                .default_open(true)
                .show(ui, |ui| {
                    egui::Grid::new("inspector_transform")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            if let Some(current) = &current_model {
                                ui.label("Mesh");
                                egui::ComboBox::from_id_salt("inspector_mesh")
                                    .selected_text(model_label(current))
                                    .width(ui.available_width())
                                    .show_ui(ui, |ui| {
                                        // Listed only while the dropdown is open.
                                        for path in content::model_list(&self.project_root) {
                                            if ui
                                                .selectable_label(
                                                    *current == path,
                                                    model_label(&path),
                                                )
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
                            transform_edited |= vec3_field(ui, &mut t.translation, 0.05, None);
                            ui.end_row();

                            ui.label("Rotation");
                            let (x, y, z) = t.rotation.to_euler(glam::EulerRot::XYZ);
                            let mut degrees =
                                glam::Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
                            if vec3_field(ui, &mut degrees, 0.5, None) {
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
                            transform_edited |= vec3_field(ui, &mut t.scale, 0.01, Some(0.001));
                            ui.end_row();
                        });
                });
            if transform_edited {
                *transform = t;
            }
        }

        if let Some(body_info) = entity_mut.get::<BodyInfo>() {
            let body_info = *body_info;
            egui::CollapsingHeader::new("Rigidbody")
                .default_open(true)
                .show(ui, |ui| {
                    egui::Grid::new("inspector_rigidbody")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Type");
                            ui.label(format!("{:?}", body_info.body_type));
                            ui.end_row();
                            let h = body_info.half_extents;
                            ui.label("Half extents");
                            ui.label(format!("{:.2}  {:.2}  {:.2}", h.x, h.y, h.z));
                            ui.end_row();
                        });
                });
        }

        let spins = entity_mut.get::<Spin>().is_some();
        let bobs = entity_mut.get::<melon_core::Bob>().is_some();
        if spins || bobs {
            egui::CollapsingHeader::new("Behaviours")
                .default_open(true)
                .show(ui, |ui| {
                    if spins {
                        ui.label("Spin: rotates about Y while playing");
                    }
                    if bobs {
                        ui.label("Bob: moves up and down while playing");
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
        // Guard M4: check entity existence
        let entity = match self.selected {
            Some(e) => {
                if world.get_entity(e).is_err() {
                    self.clear_selection();
                    return;
                }
                e
            }
            None => return,
        };

        // Guard M5: skip if no Transform. The gizmo works on the world pose.
        if world.get::<Transform>(entity).is_none() {
            return;
        }
        let transform = Transform::from_mat4(world_matrix(world, entity));

        let camera = match world.get_resource::<Camera>() {
            Some(c) => *c,
            None => return,
        };

        // Viewport-local points, matching the camera's aspect.
        let screen_size_vec2 = vp.size;

        // Guard C4: project entity center, skip if behind camera
        let entity_screen = match camera.world_to_screen(transform.translation, screen_size_vec2) {
            Some(p) => p,
            None => {
                self.gizmo_drag = None;
                self.hovered_axis = None;
                return;
            }
        };

        let entity_screen_egui = egui::pos2(entity_screen.x, entity_screen.y);

        // Compute gizmo size in world space
        let clip = camera.view_proj()
            * glam::Vec4::new(
                transform.translation.x,
                transform.translation.y,
                transform.translation.z,
                1.0,
            );
        let depth = if clip.w > 0.0 { clip.w } else { 1.0 };
        let world_per_pixel = 2.0 * (camera.fov_y_radians * 0.5).tan() * depth / screen_size_vec2.y;
        let axis_length = GIZMO_SIZE_PX * world_per_pixel;

        // Local-space axes (Issue M6)
        let axes = [
            transform.rotation * AXIS_X,
            transform.rotation * AXIS_Y,
            transform.rotation * AXIS_Z,
        ];

        let camera_forward = camera.forward();

        let pointer_pos = vp.pointer;
        let primary_down = ui.ctx().input(|i| i.pointer.primary_down());
        let primary_pressed = ui.ctx().input(|i| i.pointer.primary_pressed());
        let primary_released = ui.ctx().input(|i| i.pointer.primary_released());
        let snap = ui
            .ctx()
            .input(|i| i.key_down(egui::Key::ShiftLeft) || i.key_down(egui::Key::ShiftRight));
        // Hover/start a gizmo drag only over the viewport, never while orbiting.
        let wants_pointer = !vp.hovered || vp.alt || vp.typing;

        // Handle active drag
        if let Some(drag) = self.gizmo_drag {
            if !primary_down {
                self.gizmo_drag = None;
            } else if let Some(pp) = pointer_pos {
                let pointer_screen = glam::Vec2::new(pp.x, pp.y);
                let (ray_origin, ray_dir) =
                    camera.screen_to_world_ray(pointer_screen, screen_size_vec2);

                match drag.mode {
                    GizmoMode::Move => {
                        if let Some(current_hit) = ray_plane_intersect(
                            ray_origin,
                            ray_dir,
                            drag.plane_point,
                            drag.plane_normal,
                        ) {
                            let delta = current_hit - drag.initial_hit_point;
                            let mut axis_delta = project_onto_axis(delta, drag.axis);
                            if drag.snap {
                                axis_delta = snap_value(axis_delta, SNAP_MOVE);
                            }
                            let new_translation = drag.initial_translation + drag.axis * axis_delta;
                            set_world_transform(
                                world,
                                entity,
                                Transform {
                                    translation: new_translation,
                                    ..transform
                                },
                            );
                            snap_previous_transform(world, entity);
                            self.write_to_physics_body(world, entity, Some(new_translation), None);
                        }
                    }
                    GizmoMode::Rotate => {
                        if let Some(current_hit) = ray_plane_intersect(
                            ray_origin,
                            ray_dir,
                            drag.plane_point,
                            drag.plane_normal,
                        ) {
                            let current_angle = rotate_angle_in_plane(
                                current_hit,
                                drag.initial_translation,
                                drag.plane_tangent,
                                drag.plane_bitangent,
                            );
                            let mut delta_angle =
                                wrap_angle_delta(current_angle - drag.initial_angle);
                            if drag.snap {
                                delta_angle = snap_angle(delta_angle, SNAP_ROTATE_RAD);
                            }
                            let new_rotation = glam::Quat::from_axis_angle(drag.axis, delta_angle)
                                * drag.initial_rotation;
                            set_world_transform(
                                world,
                                entity,
                                Transform {
                                    rotation: new_rotation,
                                    ..transform
                                },
                            );
                            snap_previous_transform(world, entity);
                            self.write_to_physics_body(world, entity, None, Some(new_rotation));
                        }
                    }
                    GizmoMode::Scale => {
                        if let Some(current_hit) = ray_plane_intersect(
                            ray_origin,
                            ray_dir,
                            drag.plane_point,
                            drag.plane_normal,
                        ) {
                            let current_proj = project_onto_axis(
                                current_hit - drag.initial_translation,
                                drag.axis,
                            );
                            if drag.initial_projection.abs() > 1e-6 {
                                let mut scale_factor = current_proj / drag.initial_projection;
                                scale_factor = scale_factor.clamp(SCALE_MIN, SCALE_MAX);
                                if drag.snap {
                                    scale_factor = snap_value(scale_factor, SNAP_SCALE);
                                }
                                let mut new_scale = drag.initial_scale;
                                match drag.axis_index {
                                    0 => new_scale.x = drag.initial_scale.x * scale_factor,
                                    1 => new_scale.y = drag.initial_scale.y * scale_factor,
                                    2 => new_scale.z = drag.initial_scale.z * scale_factor,
                                    _ => {}
                                }
                                set_world_transform(
                                    world,
                                    entity,
                                    Transform {
                                        scale: new_scale,
                                        ..transform
                                    },
                                );
                                snap_previous_transform(world, entity);
                            }
                        }
                    }
                }
            }

            if primary_released {
                self.gizmo_drag = None;
            }
        }

        // Picking (only when not dragging and egui doesn't want pointer).
        // Off the viewport, forget the hovered handle so the Grab cursor clears.
        if self.gizmo_drag.is_none() && wants_pointer {
            self.hovered_axis = None;
        }
        if self.gizmo_drag.is_none() && !wants_pointer {
            self.hovered_axis = None;
            if let Some(pp) = pointer_pos {
                let pointer_vec = glam::Vec2::new(pp.x, pp.y);
                let local_axes = [AXIS_X, AXIS_Y, AXIS_Z];
                for (i, &local_axis) in local_axes.iter().enumerate() {
                    let world_axis = transform.rotation * local_axis;
                    let endpoint_world = transform.translation + world_axis * axis_length;
                    if let Some(endpoint_screen) =
                        camera.world_to_screen(endpoint_world, screen_size_vec2)
                    {
                        let dist = point_to_segment_dist_2d(
                            pointer_vec,
                            entity_screen,
                            glam::Vec2::new(endpoint_screen.x, endpoint_screen.y),
                        );
                        if dist < PICK_THRESHOLD {
                            self.hovered_axis = Some(i);
                            break;
                        }
                    }
                }

                // Start drag on click
                if primary_pressed && let Some(axis_index) = self.hovered_axis {
                    let axis = axes[axis_index];
                    let (plane_point, plane_normal) =
                        compute_drag_plane(transform.translation, axis, camera_forward);
                    let (plane_tangent, plane_bitangent) = plane_basis(axis, plane_normal);

                    let pointer_screen = glam::Vec2::new(pp.x, pp.y);
                    let (ray_origin, ray_dir) =
                        camera.screen_to_world_ray(pointer_screen, screen_size_vec2);
                    let initial_hit =
                        ray_plane_intersect(ray_origin, ray_dir, plane_point, plane_normal);

                    if let Some(hit) = initial_hit {
                        let initial_angle = rotate_angle_in_plane(
                            hit,
                            transform.translation,
                            plane_tangent,
                            plane_bitangent,
                        );
                        let initial_projection =
                            project_onto_axis(hit - transform.translation, axis);

                        self.gizmo_drag = Some(GizmoDrag {
                            axis,
                            axis_index,
                            mode: self.gizmo_mode,
                            initial_translation: transform.translation,
                            initial_rotation: transform.rotation,
                            initial_scale: transform.scale,
                            initial_hit_point: hit,
                            initial_angle,
                            initial_projection,
                            plane_point,
                            plane_normal,
                            plane_tangent,
                            plane_bitangent,
                            snap,
                        });
                    }
                }
            }
        }

        // Generate geometry
        self.generate_gizmo_geometry(
            transform.translation,
            axes,
            axis_length,
            camera,
            screen_size_vec2,
        );

        // Draw cursor hint
        if self.gizmo_drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if self.hovered_axis.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        let _ = entity_screen_egui;
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

    fn generate_gizmo_geometry(
        &mut self,
        center: glam::Vec3,
        axes: [glam::Vec3; 3],
        axis_length: f32,
        camera: Camera,
        screen_size: glam::Vec2,
    ) {
        let mut verts: Vec<LineVertex> = Vec::new();

        match self.gizmo_mode {
            GizmoMode::Move => {
                for (i, &axis) in axes.iter().enumerate() {
                    let color = if self.hovered_axis == Some(i) {
                        hovered_color(i)
                    } else {
                        axis_color(i)
                    };
                    let endpoint = center + axis * axis_length;
                    verts.push(LineVertex {
                        position: center.to_array(),
                        color: color.to_array(),
                    });
                    verts.push(LineVertex {
                        position: endpoint.to_array(),
                        color: color.to_array(),
                    });

                    let handle_size = axis_length * 0.15;
                    let lines = cube_handle_lines(endpoint, handle_size, color);
                    for [a, b] in lines {
                        verts.push(LineVertex {
                            position: a.to_array(),
                            color: color.to_array(),
                        });
                        verts.push(LineVertex {
                            position: b.to_array(),
                            color: color.to_array(),
                        });
                    }
                }
            }
            GizmoMode::Rotate => {
                for (i, &axis) in axes.iter().enumerate() {
                    let radius = axis_length;
                    let color = if self.hovered_axis == Some(i) {
                        hovered_color(i)
                    } else {
                        axis_color(i)
                    };

                    // Check degenerate circle (Issue 3.5)
                    let circle_center_screen = camera.world_to_screen(center, screen_size);
                    let point_on_circle = center + axis.cross(glam::Vec3::Y).normalize() * radius;
                    let circle_point_screen = camera.world_to_screen(point_on_circle, screen_size);

                    if let (Some(cs), Some(ps)) = (circle_center_screen, circle_point_screen) {
                        let screen_radius = ((ps.x - cs.x).powi(2) + (ps.y - cs.y).powi(2)).sqrt();
                        if screen_radius < DEGENERATE_CIRCLE_PX {
                            continue;
                        }
                    }

                    let lines = circle_lines(center, radius, axis, color, CIRCLE_SEGMENTS);
                    for [a, b] in lines {
                        verts.push(LineVertex {
                            position: a.to_array(),
                            color: color.to_array(),
                        });
                        verts.push(LineVertex {
                            position: b.to_array(),
                            color: color.to_array(),
                        });
                    }
                }
            }
            GizmoMode::Scale => {
                for (i, &axis) in axes.iter().enumerate() {
                    let color = if self.hovered_axis == Some(i) {
                        hovered_color(i)
                    } else {
                        axis_color(i)
                    };
                    let endpoint = center + axis * axis_length;
                    verts.push(LineVertex {
                        position: center.to_array(),
                        color: color.to_array(),
                    });
                    verts.push(LineVertex {
                        position: endpoint.to_array(),
                        color: color.to_array(),
                    });

                    let handle_size = axis_length * 0.15;
                    let lines = cube_handle_lines(endpoint, handle_size, color);
                    for [a, b] in lines {
                        verts.push(LineVertex {
                            position: a.to_array(),
                            color: color.to_array(),
                        });
                        verts.push(LineVertex {
                            position: b.to_array(),
                            color: color.to_array(),
                        });
                    }
                }
            }
        }

        self.gizmo_vertices = verts;
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

/// One inspector row of three colour-coded X / Y / Z drag fields sharing the
/// row's width. Every field is always drawn (no short-circuit). Returns true
/// if any value changed.
fn vec3_field(ui: &mut egui::Ui, value: &mut glam::Vec3, speed: f64, min: Option<f32>) -> bool {
    let mut changed = false;
    let axes = [
        ("X", theme::colors::AXIS_X),
        ("Y", theme::colors::AXIS_Y),
        ("Z", theme::colors::AXIS_Z),
    ];
    ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        let field_width = ((ui.available_width() - 3.0 * (12.0 + 2.0 * spacing)) / 3.0).max(48.0);
        for (i, (label, color)) in axes.into_iter().enumerate() {
            ui.label(egui::RichText::new(label).color(color).strong());
            let mut drag = egui::DragValue::new(&mut value[i])
                .speed(speed)
                .max_decimals(3);
            if let Some(min) = min {
                drag = drag.range(min..=f32::MAX);
            }
            changed |= ui
                .add_sized([field_width, theme::sizes::INPUT_HEIGHT], drag)
                .changed();
        }
    });
    changed
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
        _ => None,
    }
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
        std::fs::create_dir_all(root.join("assets")).unwrap();
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
        assert_eq!(editor.content.current(), root.join("assets"));
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
}
