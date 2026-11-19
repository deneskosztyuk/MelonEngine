use std::sync::Arc;
use std::time::Instant;

use melon_core::bevy_ecs::entity::Entity;
use melon_core::bevy_ecs::schedule::IntoScheduleConfigs;
use melon_core::bevy_ecs::system::Res;
use melon_core::bevy_ecs::world::World;
use melon_core::glam::{Mat4, Quat, Vec2, Vec3};
use melon_core::input::fly_camera_system;
use melon_core::{
    Aabb, BodyInfo, BodyKind, Camera, FrameCounter, Input, KeyCode, Light, MeshRef, Name,
    SimulationState, Time, Transform,
};
use melon_editor::Editor;
#[cfg(not(feature = "imgui-poc"))]
use melon_editor::EditorOverlay;
#[cfg(feature = "imgui-poc")]
use melon_editor::imgui_poc::ImguiEditorOverlay;
use melon_physics::rapier3d::dynamics::RigidBodyType;
use melon_physics::{
    ColliderComponent, PhysicsWorld, RigidBodyComponent as RigidBody, cleanup_deleted_system,
    step_system, sync_from_physics_system, sync_to_physics_system,
};
use tracing_subscriber::EnvFilter;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{KeyCode as WinitKeyCode, PhysicalKey};
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

#[cfg(feature = "hot-reload")]
#[hot_lib_reloader::hot_module(
    dylib = "melon_game",
    lib_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug")
)]
mod hot_game {
    use melon_core::bevy_ecs::system::{Query, Res};
    use melon_core::{Bob, FrameCounter, Spin, Transform, With};
    hot_functions_from_file!("crates/melon_game/src/lib.rs");
}

#[cfg(not(feature = "hot-reload"))]
mod hot_game {
    pub use melon_game::*;
}

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.06,
    g: 0.07,
    b: 0.09,
    a: 1.0,
};

fn map_key(code: &WinitKeyCode) -> Option<KeyCode> {
    Some(match code {
        WinitKeyCode::KeyW => KeyCode::W,
        WinitKeyCode::KeyA => KeyCode::A,
        WinitKeyCode::KeyS => KeyCode::S,
        WinitKeyCode::KeyD => KeyCode::D,
        WinitKeyCode::Space => KeyCode::Space,
        WinitKeyCode::ShiftLeft | WinitKeyCode::ShiftRight => KeyCode::LeftShift,
        _ => return None,
    })
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<melon_render::Renderer>,
    #[cfg(not(feature = "imgui-poc"))]
    editor: Option<EditorOverlay>,
    editor_ui: Option<Editor>,
    #[cfg(feature = "imgui-poc")]
    imgui_poc: Option<ImguiEditorOverlay>,
    #[cfg(feature = "imgui-poc")]
    viewport_target: Option<melon_render::ViewportTarget>,
    core: melon_core::Core,
    cube_mesh: Option<melon_render::Mesh>,
    loaded_model: Option<melon_render::LoadedModel>,
    last_cursor_pos: Option<Vec2>,
    last_frame: Option<Instant>,
    // Pre-allocated render loop buffers (reused each frame to avoid heap allocation)
    entity_matrices: Vec<Mat4>,
}

fn spawn_default_scene(world: &mut World, physics: &mut PhysicsWorld) {
    // Ground: flat cube scaled to 5x0.1x5, top surface at y=0
    let half_extents = Vec3::new(2.5, 0.05, 2.5);
    let translation = Vec3::new(0.0, -0.05, 0.0);
    let (body_h, collider_h) = physics.spawn_fixed_cuboid(translation, half_extents);
    world.spawn((
        Name("Ground".to_string()),
        Transform {
            translation,
            rotation: Quat::IDENTITY,
            scale: Vec3::new(5.0, 0.1, 5.0),
        },
        MeshRef::Cube,
        Aabb { half_extents },
        RigidBody {
            body_handle: body_h,
            body_type: RigidBodyType::Fixed,
        },
        ColliderComponent {
            collider_handle: collider_h,
        },
        BodyInfo {
            body_type: BodyKind::Fixed,
            half_extents,
        },
    ));
}

impl App {
    fn new() -> Self {
        let mut core = melon_core::Core::new();
        core.world.insert_resource(Camera::default());
        core.world.insert_resource(Light::default());
        core.world.insert_resource(Input::default());
        core.world.insert_resource(Time::default());
        core.world.insert_resource(SimulationState::default());

        let mut physics = PhysicsWorld::new();
        spawn_default_scene(&mut core.world, &mut physics);
        core.world.insert_resource(physics);
        // Always-running systems
        core.schedule.add_systems(fly_camera_system);
        core.schedule.add_systems(sync_to_physics_system);
        core.schedule.add_systems(cleanup_deleted_system);
        // Game systems that run only when playing
        let is_playing = |state: Res<SimulationState>| *state == SimulationState::Playing;
        core.schedule
            .add_systems(hot_game::rotation_system.run_if(is_playing));
        core.schedule
            .add_systems(hot_game::bob_system.run_if(is_playing));
        core.schedule.add_systems(step_system.run_if(is_playing));
        core.schedule
            .add_systems(sync_from_physics_system.run_if(is_playing));

        Self {
            window: None,
            renderer: None,
            #[cfg(not(feature = "imgui-poc"))]
            editor: None,
            editor_ui: None,
            #[cfg(feature = "imgui-poc")]
            imgui_poc: None,
            #[cfg(feature = "imgui-poc")]
            viewport_target: None,
            core,
            cube_mesh: None,
            loaded_model: None,
            last_cursor_pos: None,
            last_frame: None,
            entity_matrices: Vec::with_capacity(64),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("MelonEngine v0.1")
                        .with_maximized(true),
                )
                .expect("failed to create window"),
        );

        let renderer = match pollster::block_on(melon_render::Renderer::new(window.clone())) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("renderer init failed: {e:?}");
                event_loop.exit();
                return;
            }
        };

        #[cfg(not(feature = "imgui-poc"))]
        let editor = EditorOverlay::new(&window, renderer.device(), renderer.surface_format());
        #[cfg(not(feature = "imgui-poc"))]
        let editor_ui = Editor::new();

        #[cfg(feature = "imgui-poc")]
        let (imgui_poc, viewport_target) = {
            let viewport_target = renderer.create_viewport_target(renderer.size());
            let mut imgui_poc = ImguiEditorOverlay::new(
                &window,
                renderer.device(),
                renderer.queue(),
                renderer.surface_format(),
            );
            imgui_poc.set_viewport_target(renderer.device(), &viewport_target);
            (imgui_poc, viewport_target)
        };

        let size = renderer.size();
        let aspect = size.width as f32 / size.height.max(1) as f32;
        if let Some(mut camera) = self.core.world.get_resource_mut::<Camera>() {
            camera.set_aspect(aspect);
        }

        tracing::info!(
            "renderer ready: {}x{} {:?}",
            size.width,
            size.height,
            renderer.surface_format()
        );
        tracing::info!("controls: WASD move, Space/Shift up/down, right-click-drag look");
        tracing::info!("scene: ground plane");

        let cube_mesh = renderer.cube_mesh();
        let glb_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/models/cube.glb");
        let loaded_model = match renderer.load_gltf(glb_path) {
            Ok(m) => {
                tracing::info!("loaded glTF model from {glb_path}");
                Some(m)
            }
            Err(e) => {
                tracing::warn!("failed to load glTF model, falling back to procedural cube: {e:?}");
                None
            }
        };

        self.cube_mesh = Some(cube_mesh);
        self.loaded_model = loaded_model;
        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        #[cfg(not(feature = "imgui-poc"))]
        {
            self.editor = Some(editor);
            self.editor_ui = Some(editor_ui);
        }
        #[cfg(feature = "imgui-poc")]
        {
            self.imgui_poc = Some(imgui_poc);
            self.viewport_target = Some(viewport_target);
        }
        window.focus_window();
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        #[cfg(feature = "imgui-poc")]
        let event = if let Some(editor) = self.imgui_poc.as_mut()
            && let Some(window) = self.window.as_ref()
        {
            let (forwarded_event, consumed) = editor.on_window_event(window, _id, event);
            if consumed {
                return;
            }
            forwarded_event
        } else {
            event
        };

        // Forward events to egui first. If egui consumed the event, skip engine processing.
        #[cfg(not(feature = "imgui-poc"))]
        if let Some(editor) = self.editor.as_mut()
            && let Some(window) = self.window.as_ref()
            && editor.on_window_event(window, &event)
        {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("close requested");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                    let aspect = size.width as f32 / size.height.max(1) as f32;
                    if let Some(mut camera) = self.core.world.get_resource_mut::<Camera>() {
                        camera.set_aspect(aspect);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key
                    && let Some(mapped) = map_key(&code)
                {
                    let Some(mut input) = self.core.world.get_resource_mut::<Input>() else {
                        return;
                    };
                    match event.state {
                        ElementState::Pressed => {
                            input.keys_pressed.insert(mapped);
                        }
                        ElementState::Released => {
                            input.keys_pressed.remove(&mapped);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Right {
                    let Some(mut input) = self.core.world.get_resource_mut::<Input>() else {
                        return;
                    };
                    input.right_mouse_held = state == ElementState::Pressed;
                    if state == ElementState::Released {
                        self.last_cursor_pos = None;
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let new_pos = Vec2::new(position.x as f32, position.y as f32);
                let Some(mut input) = self.core.world.get_resource_mut::<Input>() else {
                    self.last_cursor_pos = Some(new_pos);
                    return;
                };
                if input.right_mouse_held
                    && let Some(last) = self.last_cursor_pos
                {
                    input.mouse_delta += new_pos - last;
                }
                self.last_cursor_pos = Some(new_pos);
            }
            WindowEvent::RedrawRequested => {
                let frame_n = self
                    .core
                    .world
                    .get_resource::<FrameCounter>()
                    .map(|c| c.frame)
                    .unwrap_or(0);
                let _frame_span = tracing::info_span!("frame", n = frame_n).entered();

                let now = Instant::now();
                if let Some(mut time) = self.core.world.get_resource_mut::<Time>() {
                    let dt = self
                        .last_frame
                        .map(|t| now.duration_since(t).as_secs_f32())
                        .unwrap_or(1.0 / 60.0);
                    time.update(dt);
                }
                self.last_frame = Some(now);

                self.core.update();

                let Some(renderer) = self.renderer.as_mut() else {
                    return;
                };
                let Some(mesh) = self.cube_mesh.as_ref() else {
                    return;
                };
                let Some(window) = self.window.as_ref() else {
                    return;
                };

                let view_proj = self
                    .core
                    .world
                    .get_resource::<Camera>()
                    .map(|c| c.view_proj())
                    .unwrap_or_default();
                renderer.update_camera(view_proj);

                let light = self
                    .core
                    .world
                    .get_resource::<Light>()
                    .copied()
                    .unwrap_or_default();
                renderer.update_light(light.to_light(), light.ambient, light.color);

                // Collect model matrices from all entities with Transform and MeshRef
                let entities: Vec<(Entity, Mat4, MeshRef)> = self
                    .core
                    .world
                    .query::<(Entity, &Transform, &MeshRef)>()
                    .iter(&self.core.world)
                    .map(|(e, t, m)| (e, t.to_mat4(), *m))
                    .collect();

                // Write all model uniforms to the shared buffer (one write_buffer call)
                self.entity_matrices.clear();
                self.entity_matrices
                    .extend(entities.iter().map(|(_, mat, _)| *mat));
                renderer.update_model_uniforms(&self.entity_matrices);

                // Build draw calls based on MeshRef: (entity_index, mesh)
                let loaded_model = self.loaded_model.as_ref();
                let mut textured_draw_calls: Vec<(
                    usize,
                    &melon_render::Mesh,
                    Option<&wgpu::BindGroup>,
                )> = Vec::with_capacity(entities.len());
                let mut cube_draw_calls: Vec<(usize, &melon_render::Mesh)> =
                    Vec::with_capacity(entities.len());

                for (i, (_, _, mesh_ref)) in entities.iter().enumerate() {
                    if *mesh_ref == MeshRef::GltfModel {
                        if let Some(model) = loaded_model
                            && let (Some(mesh), Some(albedo_bg)) =
                                (model.meshes.first(), model.albedo_bind_groups.first())
                        {
                            textured_draw_calls.push((i, mesh, albedo_bg.as_ref()));
                        } else {
                            cube_draw_calls.push((i, mesh));
                        }
                    } else {
                        cube_draw_calls.push((i, mesh));
                    }
                }

                // --- Three-pass render: egui update, 3D + gizmo, egui overlay ---

                let Some(mut frame) = renderer.begin_frame() else {
                    return;
                };

                // Editor update: run UI, upload textures/buffers.
                // This generates gizmo vertices stored on the editor.
                #[cfg(not(feature = "imgui-poc"))]
                self.editor.as_mut().unwrap().update(
                    window,
                    renderer.device(),
                    renderer.queue(),
                    &mut frame.encoder,
                    &mut self.core.world,
                    self.editor_ui.as_mut().unwrap(),
                );

                // Retrieve gizmo line vertices generated by the editor
                #[cfg(not(feature = "imgui-poc"))]
                let gizmo_vertices = self.editor_ui.as_mut().unwrap().take_gizmo_vertices();
                #[cfg(feature = "imgui-poc")]
                let gizmo_vertices: Vec<melon_render::LineVertex> = Vec::new();

                // Check for pending scene save/load operations from the editor
                #[cfg(not(feature = "imgui-poc"))]
                let pending_op = self.editor_ui.as_mut().unwrap().take_pending_operation();
                #[cfg(feature = "imgui-poc")]
                let pending_op: Option<melon_editor::PendingOperation> = None;
                if let Some(op) = pending_op {
                    match op {
                        melon_editor::PendingOperation::Save(path) => {
                            let result =
                                melon_editor::scene::save_scene(&mut self.core.world, &path);
                            match result {
                                Ok(()) => self
                                    .editor_ui
                                    .as_mut()
                                    .unwrap()
                                    .set_status(format!("Saved to {}", path.display())),
                                Err(e) => self
                                    .editor_ui
                                    .as_mut()
                                    .unwrap()
                                    .set_status(format!("Save failed: {e}")),
                            }
                        }
                        melon_editor::PendingOperation::Load(path) => {
                            let mut physics = self
                                .core
                                .world
                                .get_resource_mut::<PhysicsWorld>()
                                .map(|mut pw| {
                                    let mut new_pw = PhysicsWorld::new();
                                    std::mem::swap(&mut *pw, &mut new_pw);
                                    new_pw
                                })
                                .unwrap_or_default();
                            let result = melon_editor::scene::load_scene(
                                &mut self.core.world,
                                &mut physics,
                                &path,
                            );
                            // Put physics world back
                            if let Some(mut pw) = self.core.world.get_resource_mut::<PhysicsWorld>()
                            {
                                std::mem::swap(&mut *pw, &mut physics);
                            }
                            match result {
                                Ok(()) => {
                                    self.editor_ui.as_mut().unwrap().clear_selection();
                                    self.editor_ui
                                        .as_mut()
                                        .unwrap()
                                        .set_status(format!("Loaded from {}", path.display()));
                                }
                                Err(e) => self
                                    .editor_ui
                                    .as_mut()
                                    .unwrap()
                                    .set_status(format!("Load failed: {e}")),
                            }
                        }
                    }
                }

                // Pass 1: clear + draw 3D meshes + gizmo lines
                #[cfg(feature = "imgui-poc")]
                let viewport_color_view = self.viewport_target.as_ref().unwrap().color_view();
                #[cfg(feature = "imgui-poc")]
                let scene_color_view = viewport_color_view.as_ref();
                #[cfg(feature = "imgui-poc")]
                let scene_depth_view = self.viewport_target.as_ref().unwrap().depth_view();
                #[cfg(not(feature = "imgui-poc"))]
                let scene_color_view = &frame.view;
                #[cfg(not(feature = "imgui-poc"))]
                let scene_depth_view = renderer.depth_view();

                {
                    let mut pass = frame
                        .encoder
                        .begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("melon_render 3D pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: scene_color_view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: Some(
                                wgpu::RenderPassDepthStencilAttachment {
                                    view: scene_depth_view,
                                    depth_ops: Some(wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(1.0),
                                        store: wgpu::StoreOp::Store,
                                    }),
                                    stencil_ops: None,
                                },
                            ),
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                    renderer.draw_meshes_textured(&mut pass, &textured_draw_calls);
                    renderer.draw_meshes(&mut pass, &cube_draw_calls);
                    if !gizmo_vertices.is_empty() {
                        renderer.draw_lines(&mut pass, &gizmo_vertices, view_proj);
                    }
                }

                // Pass 2: egui overlay (load existing color, no depth)
                {
                    let pass = frame
                        .encoder
                        .begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("melon_render egui pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &frame.view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: if cfg!(feature = "imgui-poc") {
                                        wgpu::LoadOp::Clear(CLEAR_COLOR)
                                    } else {
                                        wgpu::LoadOp::Load
                                    },
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                    let mut pass = pass.forget_lifetime();
                    #[cfg(not(feature = "imgui-poc"))]
                    self.editor.as_mut().unwrap().render(&mut pass);
                    #[cfg(feature = "imgui-poc")]
                    if let Err(error) = self.imgui_poc.as_mut().unwrap().render(
                        window,
                        renderer.device(),
                        renderer.queue(),
                        &mut pass,
                        &mut self.core.world,
                    ) {
                        tracing::error!(?error, "Dear ImGui rendering failed");
                    }
                }

                #[cfg(feature = "imgui-poc")]
                {
                    let requested_size = self.imgui_poc.as_ref().unwrap().requested_viewport_size();
                    let current_size = self.viewport_target.as_ref().unwrap().size();
                    if requested_size != current_size {
                        let viewport_target = renderer.create_viewport_target(requested_size);
                        self.imgui_poc
                            .as_mut()
                            .unwrap()
                            .set_viewport_target(renderer.device(), &viewport_target);
                        self.viewport_target = Some(viewport_target);

                        let aspect = requested_size.width as f32 / requested_size.height as f32;
                        if let Some(mut camera) = self.core.world.get_resource_mut::<Camera>() {
                            camera.set_aspect(aspect);
                        }
                    }
                }

                renderer.end_frame(frame);

                window.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // ControlFlow::Poll already spins continuously — no redraw request needed.
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    tracing_log::LogTracer::init().ok();
    tracing::info!("MelonEngine v{} starting up", melon_core::version());

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new();
    if let Err(e) = event_loop.run_app(&mut app) {
        tracing::error!("event loop exited with error: {e:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_key_translates_supported_keys() {
        assert_eq!(map_key(&WinitKeyCode::KeyW), Some(KeyCode::W));
        assert_eq!(map_key(&WinitKeyCode::KeyA), Some(KeyCode::A));
        assert_eq!(map_key(&WinitKeyCode::KeyS), Some(KeyCode::S));
        assert_eq!(map_key(&WinitKeyCode::KeyD), Some(KeyCode::D));
        assert_eq!(map_key(&WinitKeyCode::Space), Some(KeyCode::Space));
        assert_eq!(map_key(&WinitKeyCode::ShiftLeft), Some(KeyCode::LeftShift));
        assert_eq!(map_key(&WinitKeyCode::ShiftRight), Some(KeyCode::LeftShift));
    }

    #[test]
    fn map_key_returns_none_for_unsupported() {
        assert_eq!(map_key(&WinitKeyCode::Escape), None);
        assert_eq!(map_key(&WinitKeyCode::Enter), None);
        assert_eq!(map_key(&WinitKeyCode::Tab), None);
    }
}
