use std::sync::Arc;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use egui::ViewportId;
use egui_wgpu::ScreenDescriptor;
use melon_core::glam;
use melon_core::{Aabb, BodyInfo, Camera, MeshRef, Name, SimulationState, Spin, Transform};
use melon_render::LineVertex;
use tracing::instrument;
use winit::event::WindowEvent;
use winit::window::Window;

pub mod gizmo;
pub mod scene;

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
    status_message: Option<(String, std::time::Instant)>,
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
        }
    }
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
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

    pub fn take_gizmo_vertices(&mut self) -> Vec<LineVertex> {
        std::mem::take(&mut self.gizmo_vertices)
    }

    pub fn take_pending_operation(&mut self) -> Option<PendingOperation> {
        std::mem::take(&mut self.pending_operation)
    }

    pub fn set_status(&mut self, message: String) {
        self.status_message = Some((message, std::time::Instant::now()));
    }

    /// Build the editor UI, reading from and writing to the ECS world.
    pub fn build_ui(&mut self, world: &mut World, ui: &mut egui::Ui) {
        // Top bar
        egui::Panel::top("debug_bar").min_size(24.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("MelonEngine v{}", melon_core::version()));
                let frame = world
                    .get_resource::<melon_core::FrameCounter>()
                    .map(|fc| fc.frame)
                    .unwrap_or(0);
                ui.label(format!("Frame {}", frame));
                ui.separator();
                if let Some(state) = world.get_resource::<SimulationState>() {
                    if *state == SimulationState::Paused {
                        ui.colored_label(egui::Color32::YELLOW, "⏸ PAUSED");
                    } else {
                        ui.colored_label(egui::Color32::GREEN, "▶ PLAYING");
                    }
                }
                ui.separator();
                ui.label("Gizmo:");
                ui.radio_value(&mut self.gizmo_mode, GizmoMode::Move, "Move");
                ui.radio_value(&mut self.gizmo_mode, GizmoMode::Rotate, "Rotate");
                ui.radio_value(&mut self.gizmo_mode, GizmoMode::Scale, "Scale");
                ui.separator();
                let snap = ui.input(|i| {
                    i.key_down(egui::Key::ShiftLeft) || i.key_down(egui::Key::ShiftRight)
                });
                if snap {
                    ui.colored_label(egui::Color32::CYAN, " SNAP");
                }
                ui.separator();
                let dialog_open = self.pending_dialog.is_some();
                if ui
                    .add_enabled(!dialog_open, egui::Button::new("Save"))
                    .clicked()
                {
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let path = rfd::FileDialog::new()
                            .add_filter("RON", &["ron"])
                            .set_file_name("scene.ron")
                            .save_file();
                        let _ = tx.send(path);
                    });
                    self.pending_dialog = Some(DialogChannel::Save(rx));
                }
                if ui
                    .add_enabled(!dialog_open, egui::Button::new("Load"))
                    .clicked()
                {
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let path = rfd::FileDialog::new()
                            .add_filter("RON", &["ron"])
                            .pick_file();
                        let _ = tx.send(path);
                    });
                    self.pending_dialog = Some(DialogChannel::Load(rx));
                }
                // Check for dialog results
                if let Some(dialog) = self.pending_dialog.take() {
                    let result = match &dialog {
                        DialogChannel::Save(rx) => rx.try_recv(),
                        DialogChannel::Load(rx) => rx.try_recv(),
                    };
                    match result {
                        Ok(Some(mut path)) => {
                            // Append .ron extension if missing
                            if path.extension().is_none() {
                                path = path.with_extension("ron");
                            }
                            match dialog {
                                DialogChannel::Save(_) => {
                                    self.pending_operation = Some(PendingOperation::Save(path));
                                }
                                DialogChannel::Load(_) => {
                                    self.pending_operation = Some(PendingOperation::Load(path));
                                }
                            }
                        }
                        Ok(None) => {
                            // User cancelled
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            // Dialog still open — put it back
                            self.pending_dialog = Some(dialog);
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            // Thread died
                        }
                    }
                }
                // Show status message for 3 seconds
                if let Some((msg, time)) = &self.status_message {
                    if time.elapsed().as_secs() < 3 {
                        ui.colored_label(egui::Color32::LIGHT_BLUE, msg);
                    } else {
                        self.status_message = None;
                    }
                }
            });
        });

        // Left panel: Scene hierarchy + inspector
        egui::Panel::left("debug_panel")
            .default_size(280.0)
            .resizable(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.build_hierarchy(world, ui);
                    ui.separator();
                    self.build_inspector(world, ui);
                });
            });

        // Central area: gizmo interaction + rendering.
        // Uses a transparent frame so the 3D scene shows through.
        self.gizmo_vertices.clear();
        let central_frame = egui::Frame::new()
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE);
        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| {
                self.build_gizmo(world, ui);
            });
    }

    fn build_hierarchy(&mut self, world: &mut World, ui: &mut egui::Ui) {
        ui.heading("Scene");

        // Add Entity button
        if ui.button("+ Add Entity").clicked() {
            let mut counter = world.get_resource_or_insert_with(melon_core::NameCounter::default);
            let name = format!("Entity {}", counter.0);
            counter.0 += 1;
            world.spawn((
                Name(name),
                melon_core::Transform::default(),
                MeshRef::Cube,
                Aabb {
                    half_extents: glam::Vec3::new(0.5, 0.5, 0.5),
                },
            ));
        }

        ui.separator();

        // Entity list
        let mut to_delete: Option<Entity> = None;
        let entities: Vec<(Entity, Name)> = world
            .query::<(Entity, &Name)>()
            .iter(world)
            .map(|(e, n)| (e, n.clone()))
            .collect();

        for (entity, name) in entities {
            let is_selected = self.selected == Some(entity);
            let label = format!("{} ({})", name.0, entity.index());
            let response = ui.selectable_label(is_selected, label);
            if response.clicked() {
                self.selected = if is_selected { None } else { Some(entity) };
                self.gizmo_drag = None;
            }
            if response.secondary_clicked() {
                to_delete = Some(entity);
            }
        }

        if let Some(entity) = to_delete {
            world.entity_mut(entity).insert(melon_core::PendingDelete);
            if self.selected == Some(entity) {
                self.clear_selection();
            }
        }
    }

    fn build_inspector(&mut self, world: &mut World, ui: &mut egui::Ui) {
        if let Some(entity) = self.selected {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                ui.heading("Inspector");

                // Name
                if let Some(name) = entity_mut.get::<Name>() {
                    ui.label(format!("Name: {}", name.0));
                    ui.separator();
                }

                // Transform
                if let Some(mut transform) = entity_mut.get_mut::<Transform>() {
                    ui.heading("Transform");
                    let mut pos = transform.translation.to_array();
                    let pos_changed = ui
                        .add(egui::Slider::new(&mut pos[0], -10.0..=10.0).text("X"))
                        .changed()
                        || ui
                            .add(egui::Slider::new(&mut pos[1], -10.0..=10.0).text("Y"))
                            .changed()
                        || ui
                            .add(egui::Slider::new(&mut pos[2], -10.0..=10.0).text("Z"))
                            .changed();
                    if pos_changed {
                        transform.translation = glam::Vec3::from_array(pos);
                    }

                    let euler = transform.rotation.to_euler(glam::EulerRot::XYZ);
                    let mut rot_deg = [
                        euler.0.to_degrees(),
                        euler.1.to_degrees(),
                        euler.2.to_degrees(),
                    ];
                    ui.label("Rotation (degrees)");
                    let rot_changed = ui
                        .add(egui::Slider::new(&mut rot_deg[0], -180.0..=180.0).text("X"))
                        .changed()
                        || ui
                            .add(egui::Slider::new(&mut rot_deg[1], -180.0..=180.0).text("Y"))
                            .changed()
                        || ui
                            .add(egui::Slider::new(&mut rot_deg[2], -180.0..=180.0).text("Z"))
                            .changed();
                    if rot_changed {
                        transform.rotation = glam::Quat::from_euler(
                            glam::EulerRot::XYZ,
                            rot_deg[0].to_radians(),
                            rot_deg[1].to_radians(),
                            rot_deg[2].to_radians(),
                        );
                    }

                    let mut s = transform.scale.to_array();
                    ui.label("Scale");
                    let scale_changed = ui
                        .add(egui::Slider::new(&mut s[0], 0.1..=5.0).text("X"))
                        .changed()
                        || ui
                            .add(egui::Slider::new(&mut s[1], 0.1..=5.0).text("Y"))
                            .changed()
                        || ui
                            .add(egui::Slider::new(&mut s[2], 0.1..=5.0).text("Z"))
                            .changed();
                    if scale_changed {
                        transform.scale = glam::Vec3::from_array(s);
                    }
                    ui.separator();
                }

                // Physics info
                if let Some(body_info) = entity_mut.get::<BodyInfo>() {
                    ui.heading("Physics");
                    ui.label(format!("Body: {:?}", body_info.body_type));
                    if let Some(aabb) = entity_mut.get::<Aabb>() {
                        ui.label(format!("Half-extents: {:?}", aabb.half_extents));
                    }
                    ui.separator();
                }

                // Markers
                if entity_mut.get::<Spin>().is_some() {
                    ui.label("⟳ Spins (Spin marker)");
                }
                if entity_mut.get::<melon_core::Bob>().is_some() {
                    ui.label("↕ Bobs (Bob marker)");
                }
            } else {
                self.clear_selection();
            }
        }
    }

    fn build_gizmo(&mut self, world: &mut World, ui: &mut egui::Ui) {
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

        // Guard M5: skip if no Transform
        let transform = match world.get::<Transform>(entity) {
            Some(t) => *t,
            None => return,
        };

        let camera = match world.get_resource::<Camera>() {
            Some(c) => *c,
            None => return,
        };

        let screen_size = ui.max_rect().size();
        let screen_size_vec2 = glam::Vec2::new(screen_size.x, screen_size.y);

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

        let pointer_pos = ui.ctx().input(|i| i.pointer.latest_pos());
        let primary_down = ui.ctx().input(|i| i.pointer.primary_down());
        let primary_pressed = ui.ctx().input(|i| i.pointer.primary_pressed());
        let primary_released = ui.ctx().input(|i| i.pointer.primary_released());
        let snap = ui
            .ctx()
            .input(|i| i.key_down(egui::Key::ShiftLeft) || i.key_down(egui::Key::ShiftRight));
        let wants_pointer = ui.ctx().egui_wants_pointer_input();

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
                            if let Some(mut t) = world.get_mut::<Transform>(entity) {
                                t.translation = new_translation;
                            }
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
                            if let Some(mut t) = world.get_mut::<Transform>(entity) {
                                t.rotation = new_rotation;
                            }
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
                                if let Some(mut t) = world.get_mut::<Transform>(entity) {
                                    t.scale = new_scale;
                                }
                            }
                        }
                    }
                }
            }

            if primary_released {
                self.gizmo_drag = None;
            }
        }

        // Picking (only when not dragging and egui doesn't want pointer)
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

        let sim_state = world
            .get_resource::<SimulationState>()
            .copied()
            .unwrap_or(SimulationState::Playing);
        if sim_state != SimulationState::Playing {
            return;
        }

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
}

impl EditorOverlay {
    #[instrument(skip(window, device), level = "info")]
    pub fn new(
        window: &Arc<Window>,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let ctx = egui::Context::default();
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
