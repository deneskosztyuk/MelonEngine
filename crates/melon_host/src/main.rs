use std::sync::Arc;
use std::time::Instant;

use melon_core::bevy_ecs::schedule::IntoScheduleConfigs;
use melon_core::bevy_ecs::system::Res;
use melon_core::bevy_ecs::world::World;
use melon_core::glam::{Quat, Vec2, Vec3};
use melon_core::{
    Aabb, BodyInfo, BodyKind, Camera, FrameCounter, Input, InputButton, KeyCode, Light, Name,
    SimulationState, Time, Transform, Update,
};
use melon_editor::Editor;
use melon_editor::EditorOverlay;
use melon_physics::rapier3d::dynamics::RigidBodyType;
use melon_physics::{
    ColliderComponent, PhysicsWorld, RigidBodyComponent as RigidBody, add_physics_systems,
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
    use melon_core::{Bob, Spin, Time, Transform, With};
    hot_functions_from_file!("crates/melon_game/src/lib.rs");
}

#[cfg(not(feature = "hot-reload"))]
mod hot_game {
    pub use melon_game::*;
}

fn map_key(code: &WinitKeyCode) -> Option<KeyCode> {
    Some(match code {
        WinitKeyCode::KeyW => KeyCode::W,
        WinitKeyCode::KeyA => KeyCode::A,
        WinitKeyCode::KeyS => KeyCode::S,
        WinitKeyCode::KeyD => KeyCode::D,
        WinitKeyCode::Space => KeyCode::Space,
        WinitKeyCode::ShiftLeft => KeyCode::ShiftLeft,
        WinitKeyCode::ShiftRight => KeyCode::ShiftRight,
        _ => return None,
    })
}

struct App {
    /// Log buffer shown in the editor's Console panel.
    console: melon_editor::console::ConsoleLog,
    window: Option<Arc<Window>>,
    renderer: Option<melon_render::Renderer>,
    editor: Option<EditorOverlay>,
    editor_ui: Option<Editor>,
    /// Offscreen image the scene renders into; the editor shows it in its viewport panel.
    viewport_target: Option<melon_render::ViewportTarget>,
    core: melon_core::Core,
    last_cursor_pos: Option<Vec2>,
    last_frame: Option<Instant>,
}

/// Project folder asset paths are relative to.
// shortcut: the source tree, fixed at compile time; ship builds need a real
// project path (CLI argument or project file).
const PROJECT_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn spawn_default_scene(world: &mut World, physics: &mut PhysicsWorld) {
    let cube: melon_core::Handle<melon_core::Model> = world
        .resource_mut::<melon_core::AssetServer>()
        .load("builtin:cube");
    // Ground: flat cube scaled to 5x0.1x5, top surface at y=0
    let half_extents = Vec3::new(2.5, 0.05, 2.5);
    let translation = Vec3::new(0.0, -0.05, 0.0);
    let (body_h, collider_h) =
        physics.spawn_fixed_cuboid(translation, Quat::IDENTITY, half_extents);
    world.spawn((
        Name("Ground".to_string()),
        Transform {
            translation,
            rotation: Quat::IDENTITY,
            scale: Vec3::new(5.0, 0.1, 5.0),
        },
        cube,
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
    fn new(console: melon_editor::console::ConsoleLog) -> Self {
        let mut core = melon_core::Core::new();
        // The Console shows engine logs; a per-second "frame N" line would drown them.
        core.world
            .insert_resource(melon_core::LogInterval { frames_per_log: 0 });
        core.world.insert_resource(Camera::default());
        core.world.insert_resource(Light::default());
        core.world.insert_resource(Input::default());

        // Asset paths are relative to the project root (the source tree in dev builds).
        core.world
            .insert_resource(melon_core::AssetServer::new(PROJECT_ROOT.into()));
        let mut physics = PhysicsWorld::new();
        spawn_default_scene(&mut core.world, &mut physics);
        core.world.insert_resource(physics);
        // Per-frame game visuals, only while playing. Physics runs in FixedUpdate.
        let is_playing = |state: Res<SimulationState>| *state == SimulationState::Playing;
        core.add_systems(
            Update,
            (hot_game::rotation_system, hot_game::bob_system).run_if(is_playing),
        );
        add_physics_systems(&mut core);
        // `--play` skips edit mode, for quick game runs.
        if std::env::args().any(|arg| arg == "--play")
            && let Err(error) = melon_editor::play::play(&mut core.world)
        {
            tracing::error!(?error, "--play failed; staying in edit mode");
        }

        Self {
            console,
            window: None,
            renderer: None,
            editor: None,
            editor_ui: None,
            viewport_target: None,
            core,
            last_cursor_pos: None,
            last_frame: None,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("MelonEngine v0.1")
            .with_maximized(true);
        #[cfg(target_os = "windows")]
        let attributes = {
            use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
            let scale = event_loop
                .primary_monitor()
                .map_or(1.0, |m| m.scale_factor());
            let icon = |points: u32| {
                let size = winit::dpi::LogicalSize::new(points, points).to_physical(scale);
                winit::window::Icon::from_resource(1, Some(size)).expect("embedded engine icon")
            };
            attributes
                .with_window_icon(Some(icon(16)))
                .with_taskbar_icon(Some(icon(32)))
        };
        #[cfg(not(target_os = "windows"))]
        let attributes = {
            let pixels = melon_editor::engine_icon();
            let (width, height) = pixels.dimensions();
            let icon = winit::window::Icon::from_rgba(pixels.into_raw(), width, height)
                .expect("embedded engine icon");
            attributes.with_window_icon(Some(icon))
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
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

        let viewport_target = renderer.create_viewport_target(renderer.size());

        let mut editor = EditorOverlay::new(&window, renderer.device(), renderer.surface_format());
        editor.set_viewport_target(renderer.device(), &viewport_target);
        let mut editor_ui = Editor::new();
        editor_ui.set_console(self.console.clone());
        // shortcut: the project root is the source tree, fixed at compile time;
        // ship builds need a real project path (CLI argument or project file).
        editor_ui.set_project_root(std::path::Path::new(PROJECT_ROOT));

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
        tracing::info!(
            "viewport: right-drag look, right+WASD/QE fly, Alt+left orbit, middle pan, scroll zoom, F frame"
        );
        tracing::info!("scene: ground plane");

        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        {
            self.editor = Some(editor);
            self.editor_ui = Some(editor_ui);
        }
        self.viewport_target = Some(viewport_target);
        window.focus_window();
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Forward events to egui first. If egui consumed the event, skip engine processing.
        if let Some(editor) = self.editor.as_mut()
            && let Some(window) = self.window.as_ref()
            && editor.on_window_event(window, &event)
        {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("close requested");
                if let Some(editor) = self.editor_ui.as_ref() {
                    editor.save_layout();
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
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
                        ElementState::Pressed => input.press(InputButton::Key(mapped)),
                        ElementState::Released => input.release(InputButton::Key(mapped)),
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Right {
                    let Some(mut input) = self.core.world.get_resource_mut::<Input>() else {
                        return;
                    };
                    let rmb = InputButton::Mouse(melon_core::MouseButton::Right);
                    if state == ElementState::Pressed {
                        input.press(rmb);
                    } else {
                        input.release(rmb);
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
                if input.pressed(InputButton::Mouse(melon_core::MouseButton::Right))
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
                let Some(window) = self.window.as_ref() else {
                    return;
                };
                let (Some(editor), Some(editor_ui)) =
                    (self.editor.as_mut(), self.editor_ui.as_mut())
                else {
                    return;
                };
                let Some(mut frame) = renderer.begin_frame() else {
                    return;
                };

                // Editor UI first: it may move the camera, edit entities or
                // queue a File operation, and this frame should show all of it.
                editor.update(
                    window,
                    renderer.device(),
                    renderer.queue(),
                    &mut frame.encoder,
                    &mut self.core.world,
                    editor_ui,
                );
                if editor_ui.run_pending_operation(&mut self.core.world) {
                    event_loop.exit();
                }
                let overlay = editor_ui.take_overlay_draws();
                let world_line_vertices = editor_ui.take_world_line_vertices();

                // Follow the viewport panel's size: recreate the target, hand it
                // to the editor UI, and match the camera aspect before drawing.
                let requested_size = editor_ui.requested_viewport_size();
                if self
                    .viewport_target
                    .as_ref()
                    .is_none_or(|t| t.size() != requested_size)
                {
                    let viewport_target = renderer.create_viewport_target(requested_size);
                    editor.set_viewport_target(renderer.device(), &viewport_target);
                    self.viewport_target = Some(viewport_target);
                    let aspect = requested_size.width as f32 / requested_size.height as f32;
                    if let Some(mut camera) = self.core.world.get_resource_mut::<Camera>() {
                        camera.set_aspect(aspect);
                    }
                }

                let frame_data = melon_render::extract(&mut self.core.world);
                if let Some(target) = self.viewport_target.as_ref() {
                    let mut server = self.core.world.resource_mut::<melon_core::AssetServer>();
                    renderer.render_scene(
                        &mut frame.encoder,
                        &frame_data,
                        &mut server,
                        target,
                        &world_line_vertices,
                        &overlay,
                    );
                }
                {
                    let mut pass = renderer.ui_pass(&mut frame);
                    editor.render(&mut pass);
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
    use tracing_subscriber::prelude::*;
    let console = melon_editor::console::ConsoleLog::default();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(EnvFilter::from_default_env()))
        .with(
            console
                .layer()
                .with_filter(melon_editor::console::capture_filter()),
        )
        .init();
    tracing_log::LogTracer::init().ok();
    tracing::info!("MelonEngine v{} starting up", melon_core::version());

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new(console);
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
        assert_eq!(map_key(&WinitKeyCode::ShiftLeft), Some(KeyCode::ShiftLeft));
        assert_eq!(
            map_key(&WinitKeyCode::ShiftRight),
            Some(KeyCode::ShiftRight)
        );
    }

    #[test]
    fn map_key_returns_none_for_unsupported() {
        assert_eq!(map_key(&WinitKeyCode::Escape), None);
        assert_eq!(map_key(&WinitKeyCode::Enter), None);
        assert_eq!(map_key(&WinitKeyCode::Tab), None);
    }
}
