#[cfg(feature = "hot-reload")]
mod game_loader;
mod pacing;

use std::sync::Arc;
use std::time::Instant;

use melon_core::bevy_ecs::world::{Mut, World};
use melon_core::glam::{Quat, Vec2, Vec3};
use melon_core::{
    Aabb, BodyInfo, BodyKind, Camera, FrameCounter, GameInputGate, Input, InputButton, KeyCode,
    Light, Name, RawPresses, RenderQuality, RenderTimings, Time, Transform,
};
use melon_editor::Editor;
use melon_editor::EditorOverlay;
use melon_physics::rapier3d::dynamics::RigidBodyType;
use melon_physics::{
    ColliderComponent, PhysicsWorld, RigidBodyComponent as RigidBody, add_physics_systems,
};
use tracing_subscriber::EnvFilter;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton as WinitMouseButton, MouseScrollDelta,
    WindowEvent,
};
use winit::keyboard::{KeyCode as WinitKeyCode, PhysicalKey};
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents, EventLoop},
    window::{CursorGrabMode, Window, WindowId},
};

fn map_key(code: &WinitKeyCode) -> Option<KeyCode> {
    use WinitKeyCode as W;
    Some(match code {
        W::KeyA => KeyCode::A,
        W::KeyB => KeyCode::B,
        W::KeyC => KeyCode::C,
        W::KeyD => KeyCode::D,
        W::KeyE => KeyCode::E,
        W::KeyF => KeyCode::F,
        W::KeyG => KeyCode::G,
        W::KeyH => KeyCode::H,
        W::KeyI => KeyCode::I,
        W::KeyJ => KeyCode::J,
        W::KeyK => KeyCode::K,
        W::KeyL => KeyCode::L,
        W::KeyM => KeyCode::M,
        W::KeyN => KeyCode::N,
        W::KeyO => KeyCode::O,
        W::KeyP => KeyCode::P,
        W::KeyQ => KeyCode::Q,
        W::KeyR => KeyCode::R,
        W::KeyS => KeyCode::S,
        W::KeyT => KeyCode::T,
        W::KeyU => KeyCode::U,
        W::KeyV => KeyCode::V,
        W::KeyW => KeyCode::W,
        W::KeyX => KeyCode::X,
        W::KeyY => KeyCode::Y,
        W::KeyZ => KeyCode::Z,
        W::Digit0 => KeyCode::Digit0,
        W::Digit1 => KeyCode::Digit1,
        W::Digit2 => KeyCode::Digit2,
        W::Digit3 => KeyCode::Digit3,
        W::Digit4 => KeyCode::Digit4,
        W::Digit5 => KeyCode::Digit5,
        W::Digit6 => KeyCode::Digit6,
        W::Digit7 => KeyCode::Digit7,
        W::Digit8 => KeyCode::Digit8,
        W::Digit9 => KeyCode::Digit9,
        W::F1 => KeyCode::F1,
        W::F2 => KeyCode::F2,
        W::F3 => KeyCode::F3,
        W::F4 => KeyCode::F4,
        W::F5 => KeyCode::F5,
        W::F6 => KeyCode::F6,
        W::F7 => KeyCode::F7,
        W::F8 => KeyCode::F8,
        W::F9 => KeyCode::F9,
        W::F10 => KeyCode::F10,
        W::F11 => KeyCode::F11,
        W::F12 => KeyCode::F12,
        W::ArrowUp => KeyCode::ArrowUp,
        W::ArrowDown => KeyCode::ArrowDown,
        W::ArrowLeft => KeyCode::ArrowLeft,
        W::ArrowRight => KeyCode::ArrowRight,
        W::Space => KeyCode::Space,
        W::Enter => KeyCode::Enter,
        W::Escape => KeyCode::Escape,
        W::Tab => KeyCode::Tab,
        W::Backspace => KeyCode::Backspace,
        W::Delete => KeyCode::Delete,
        W::Insert => KeyCode::Insert,
        W::Home => KeyCode::Home,
        W::End => KeyCode::End,
        W::PageUp => KeyCode::PageUp,
        W::PageDown => KeyCode::PageDown,
        W::ShiftLeft => KeyCode::ShiftLeft,
        W::ShiftRight => KeyCode::ShiftRight,
        W::ControlLeft => KeyCode::ControlLeft,
        W::ControlRight => KeyCode::ControlRight,
        W::AltLeft => KeyCode::AltLeft,
        W::AltRight => KeyCode::AltRight,
        W::Minus => KeyCode::Minus,
        W::Equal => KeyCode::Equal,
        W::BracketLeft => KeyCode::BracketLeft,
        W::BracketRight => KeyCode::BracketRight,
        W::Backslash => KeyCode::Backslash,
        W::Semicolon => KeyCode::Semicolon,
        W::Quote => KeyCode::Quote,
        W::Comma => KeyCode::Comma,
        W::Period => KeyCode::Period,
        W::Slash => KeyCode::Slash,
        W::Backquote => KeyCode::Backquote,
        W::Numpad0 => KeyCode::Numpad0,
        W::Numpad1 => KeyCode::Numpad1,
        W::Numpad2 => KeyCode::Numpad2,
        W::Numpad3 => KeyCode::Numpad3,
        W::Numpad4 => KeyCode::Numpad4,
        W::Numpad5 => KeyCode::Numpad5,
        W::Numpad6 => KeyCode::Numpad6,
        W::Numpad7 => KeyCode::Numpad7,
        W::Numpad8 => KeyCode::Numpad8,
        W::Numpad9 => KeyCode::Numpad9,
        _ => return None,
    })
}

fn map_mouse(button: WinitMouseButton) -> Option<melon_core::MouseButton> {
    use melon_core::MouseButton as M;
    Some(match button {
        WinitMouseButton::Left => M::Left,
        WinitMouseButton::Right => M::Right,
        WinitMouseButton::Middle => M::Middle,
        WinitMouseButton::Back => M::Back,
        WinitMouseButton::Forward => M::Forward,
        WinitMouseButton::Other(_) => return None,
    })
}

/// Keys the editor keeps while it runs a game (release focus, play / pause / stop).
const EDITOR_KEYS: [InputButton; 3] = [
    InputButton::Key(KeyCode::Escape),
    InputButton::Key(KeyCode::F5),
    InputButton::Key(KeyCode::F6),
];

/// Record one button event: presses always go to `raw`, and to the game's
/// `input` only through the gate. Key repeats are ignored.
fn record_button(
    input: &mut Input,
    raw: &mut RawPresses,
    button: InputButton,
    pressed: bool,
    repeat: bool,
    gate: bool,
) {
    if repeat {
        return;
    }
    if pressed {
        raw.0.push(button);
    }
    if !gate || EDITOR_KEYS.contains(&button) {
        return;
    }
    if pressed {
        input.press(button);
    } else {
        input.release(button);
    }
}

/// Apply this frame's `GameInputGate` against the last applied value (the
/// editor re-inserts it every frame, so change detection would always fire).
/// Closing the gate releases every held button. Returns true on a change.
fn apply_gate(input: &mut Input, applied: &mut bool, gate: bool) -> bool {
    if *applied == gate {
        return false;
    }
    *applied = gate;
    if !gate {
        input.release_all();
    }
    true
}

/// How an event matters to egui while the game owns the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EguiEvent {
    /// Moves, presses, wheel, touch: the hidden cursor must not drive the UI.
    Pointer,
    /// A button release: always forwarded, so a press egui already saw ends.
    ButtonRelease,
    /// Keyboard and window events: Esc / F5 / F6 must reach the editor.
    Other,
}

fn egui_event(event: &WindowEvent) -> EguiEvent {
    match event {
        WindowEvent::MouseInput {
            state: ElementState::Released,
            ..
        } => EguiEvent::ButtonRelease,
        WindowEvent::CursorMoved { .. }
        | WindowEvent::CursorEntered { .. }
        | WindowEvent::CursorLeft { .. }
        | WindowEvent::MouseInput { .. }
        | WindowEvent::MouseWheel { .. }
        | WindowEvent::Touch(_)
        | WindowEvent::TouchpadPressure { .. }
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::RotationGesture { .. }
        | WindowEvent::DoubleTapGesture { .. } => EguiEvent::Pointer,
        _ => EguiEvent::Other,
    }
}

fn forwards_to_egui(kind: EguiEvent, gate_open: bool) -> bool {
    !(gate_open && kind == EguiEvent::Pointer)
}

/// Window focus lost: Windows sends key releases but no mouse releases, so
/// release every held game button (focus stays as it is).
fn release_game_input(world: &mut World) {
    if let Some(mut input) = world.get_resource_mut::<Input>() {
        input.release_all();
    }
}

/// End of the host frame, after the editor UI read this frame's presses.
fn clear_raw_presses(world: &mut World) {
    if let Some(mut raw) = world.get_resource_mut::<RawPresses>() {
        raw.0.clear();
    }
}

/// Hide and confine the cursor for the game, or give it back to the editor.
fn grab_cursor(window: &Window, grab: bool) {
    let result = if grab {
        window
            .set_cursor_grab(CursorGrabMode::Confined)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Locked))
    } else {
        window.set_cursor_grab(CursorGrabMode::None)
    };
    if let Err(error) = result {
        tracing::warn!(?error, grab, "cursor grab failed");
    }
    window.set_cursor_visible(!grab);
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
    /// Shared target for Content Browser thumbnails (`THUMBNAIL_SIZE` square).
    thumbnail_target: Option<melon_render::ViewportTarget>,
    core: melon_core::Core,
    /// Last `GameInputGate` value applied (input routing and cursor grab).
    game_input: bool,
    /// `config/input.ron` failed to load: the editor backs it up before its first save.
    input_map_broken: bool,
    /// `config/quality.ron` failed to load: the editor backs it up before its first Apply.
    quality_broken: bool,
    /// `config/audio.ron` failed to load: the editor backs it up before its first Apply.
    audio_broken: bool,
    /// The audio device; updated once per frame, after the core update.
    audio: melon_audio::AudioEngine,
    last_frame: Option<Instant>,
    /// Frame cap, set when the present mode does not wait for vblank.
    pacer: Option<pacing::FrameDeadline>,
    /// The monitor's refresh period (frame cap and GPU budget).
    frame_period: std::time::Duration,
    /// `MELON_FRAME_STATS=1` timings.
    stats: pacing::FrameStats,
    /// Dev builds load the game plugin from its dylib and reload it live.
    #[cfg(feature = "hot-reload")]
    game: game_loader::GameLoader,
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
        let mut assets = melon_core::AssetServer::new(PROJECT_ROOT.into());
        melon_editor::asset_db::reconcile(&mut assets);
        core.world.insert_resource(assets);
        let map_path = melon_editor::project_settings::mapping_path(PROJECT_ROOT.as_ref());
        let (input_map, outcome) = melon_core::InputMap::load(&map_path);
        let input_map_broken = match outcome {
            melon_core::LoadOutcome::Loaded => false,
            melon_core::LoadOutcome::Missing => {
                tracing::info!(
                    "{} not found; using the default input mapping",
                    map_path.display()
                );
                false
            }
            melon_core::LoadOutcome::Broken(message) => {
                tracing::error!(
                    "{} is broken ({message}); using the default input mapping",
                    map_path.display()
                );
                true
            }
        };
        core.world.insert_resource(input_map);
        let quality_path = melon_editor::quality_settings::quality_path(PROJECT_ROOT.as_ref());
        let (quality, outcome) = melon_core::RenderQuality::load(&quality_path);
        let quality_broken = outcome.log(&quality_path, "quality settings");
        core.world.insert_resource(quality);
        let audio_path = melon_editor::audio_settings::audio_path(PROJECT_ROOT.as_ref());
        let (audio_settings, outcome) = melon_core::AudioSettings::load(&audio_path);
        let audio_broken = outcome.log(&audio_path, "audio settings");
        core.world.insert_resource(audio_settings);
        let mut physics = PhysicsWorld::new();
        spawn_default_scene(&mut core.world, &mut physics);
        core.world.insert_resource(physics);
        #[cfg(not(feature = "hot-reload"))]
        core.add_plugin(&melon_game::GamePlugin);
        #[cfg(feature = "hot-reload")]
        let game = game_loader::GameLoader::start(&mut core);
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
            thumbnail_target: None,
            core,
            game_input: false,
            input_map_broken,
            quality_broken,
            audio_broken,
            audio: melon_audio::AudioEngine::new(),
            last_frame: None,
            pacer: None,
            frame_period: pacing::frame_period(None),
            stats: pacing::FrameStats::from_env(),
            #[cfg(feature = "hot-reload")]
            game,
        }
    }

    /// Route one button event into `RawPresses` and, through the gate, `Input`.
    fn record(&mut self, button: InputButton, pressed: bool, repeat: bool) {
        let gate = self.game_input;
        self.core
            .world
            .resource_scope(|world, mut raw: Mut<RawPresses>| {
                if let Some(mut input) = world.get_resource_mut::<Input>() {
                    record_button(&mut input, &mut raw, button, pressed, repeat, gate);
                }
            });
    }
}

impl App {
    /// One editor + render frame. Called from `about_to_wait`, not from
    /// `RedrawRequested`: on Windows winit raises `RedrawRequested` from
    /// `WM_PAINT`, which Windows only generates when no input is waiting,
    /// so a fast mouse (or raw input while playing) held frames back for
    /// 100+ ms. `about_to_wait` runs once the queued events are handled.
    fn draw_frame(&mut self, event_loop: &ActiveEventLoop) {
        let frame_n = self
            .core
            .world
            .get_resource::<FrameCounter>()
            .map(|c| c.frame)
            .unwrap_or(0);
        let _frame_span = tracing::info_span!("frame", n = frame_n).entered();

        // Frame cap: sleep to the deadline here. Waiting in the event
        // loop (WaitUntil) missed deadlines by ~100 ms while input
        // events streamed in.
        if let Some(pacer) = self.pacer.as_ref() {
            let next = pacer.next();
            let slept = self.stats.clock();
            std::thread::sleep(pacing::sleep_until_deadline(
                next,
                Instant::now(),
                pacer.period(),
            ));
            self.stats.sleep_done(slept);
        }
        self.stats.frame_start();
        let now = Instant::now();
        let dt = self
            .last_frame
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(1.0 / 60.0);
        if let Some(pacer) = self.pacer.as_mut() {
            pacer.advance(now);
        }
        if let Some(mut time) = self.core.world.get_resource_mut::<Time>() {
            time.update(dt);
        }
        self.last_frame = Some(now);
        #[cfg(feature = "hot-reload")]
        self.game.poll(dt, &mut self.core);

        self.core.update();
        // Editor requests (clip preview) from the last frame play now.
        self.audio.update(&mut self.core.world);
        self.stats.lap(pacing::Stage::Update);

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let (Some(editor), Some(editor_ui)) = (self.editor.as_mut(), self.editor_ui.as_mut())
        else {
            return;
        };
        let present = self.core.world.resource::<RenderQuality>().present;
        if renderer.set_present(present) {
            self.pacer = pacing::frame_cap(renderer.present_mode(), self.frame_period);
        }
        let Some(mut frame) = renderer.begin_frame() else {
            // No editor pass this frame (e.g. minimised): drop the presses anyway.
            clear_raw_presses(&mut self.core.world);
            return;
        };
        self.stats.lap(pacing::Stage::Acquire);

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
        self.stats.lap(pacing::Stage::Editor);
        clear_raw_presses(&mut self.core.world);
        // The editor wrote the gate during its UI pass; it applies from the next events on.
        let gate = self
            .core
            .world
            .get_resource::<GameInputGate>()
            .is_some_and(|g| g.0);
        if let Some(mut input) = self.core.world.get_resource_mut::<Input>()
            && apply_gate(&mut input, &mut self.game_input, gate)
        {
            grab_cursor(window, gate);
            event_loop.listen_device_events(pacing::device_events(gate));
        }
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
        if let Some(target) = self.viewport_target.as_mut() {
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
        self.stats.lap(pacing::Stage::Render);
        renderer.end_frame(frame);
        self.stats.lap(pacing::Stage::Submit);
        let timings = renderer.timings();
        self.stats.gpu_total(timings.total);
        self.core.world.insert_resource(RenderTimings {
            budget_ms: Some(self.frame_period.as_secs_f32() * 1000.0),
            ..timings
        });

        // One Content Browser thumbnail per frame, in its own
        // submission after this frame's (shared uniforms are written
        // with `queue.write_buffer`, applied at the next submit).
        if let Some(target) = self.thumbnail_target.as_mut()
            && let Some(request) = editor_ui.thumbnail_request(&mut self.core.world)
        {
            let view = {
                let mut server = self.core.world.resource_mut::<melon_core::AssetServer>();
                renderer.render_thumbnail(&mut server, request.handle, target)
            };
            let id = view
                .map(|view| editor.register_thumbnail(renderer.device(), &view, request.existing));
            editor_ui.thumbnail_done(&request, id);
        }
        self.stats.lap(pacing::Stage::Thumbnail);
        self.stats.mark("frame end");
    }
}

impl App {
    /// Draw from inside an event callback once a frame is a period late
    /// (`pacing::frame_overdue`): a moving mouse keeps winit's queue full,
    /// so `about_to_wait` alone would hold frames back until it stops.
    fn draw_if_overdue(&mut self, event_loop: &ActiveEventLoop) {
        if pacing::frame_overdue(self.last_frame, Instant::now(), self.frame_period) {
            self.stats.mark("overdue");
            self.draw_unless_minimized(event_loop);
        }
    }

    /// Minimized: no frames (and no game updates), as when frames waited
    /// for `WM_PAINT`, which a minimized window never gets.
    fn draw_unless_minimized(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .window
            .as_ref()
            .is_some_and(|w| w.is_minimized() != Some(true))
        {
            self.draw_frame(event_loop);
        }
    }
}

impl ApplicationHandler for App {
    fn new_events(&mut self, _: &ActiveEventLoop, _: winit::event::StartCause) {
        self.stats.mark("new_events");
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.stats.mark("about_to_wait");
        self.draw_unless_minimized(event_loop);
    }

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

        let mut renderer = match pollster::block_on(melon_render::Renderer::new(window.clone())) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("renderer init failed: {e:?}");
                event_loop.exit();
                return;
            }
        };

        let viewport_target = renderer.create_viewport_target(renderer.size());
        let thumbnail_target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(
            melon_render::THUMBNAIL_SIZE,
            melon_render::THUMBNAIL_SIZE,
        ));

        let mut editor = EditorOverlay::new(&window, renderer.device(), renderer.surface_format());
        editor.set_viewport_target(renderer.device(), &viewport_target);
        let mut editor_ui = Editor::new();
        editor_ui.set_console(self.console.clone());
        // shortcut: the project root is the source tree, fixed at compile time;
        // ship builds need a real project path (CLI argument or project file).
        editor_ui.set_project_root(std::path::Path::new(PROJECT_ROOT));
        editor_ui.set_input_map_broken(self.input_map_broken);
        editor_ui.set_quality_broken(self.quality_broken);
        editor_ui.set_audio_broken(self.audio_broken);

        let size = renderer.size();
        let aspect = size.width as f32 / size.height.max(1) as f32;
        if let Some(mut camera) = self.core.world.get_resource_mut::<Camera>() {
            camera.set_aspect(aspect);
        }

        let present = self.core.world.resource::<RenderQuality>().present;
        renderer.set_present(present);
        tracing::info!(
            "renderer ready: {}x{} {:?} {:?} {:?}",
            size.width,
            size.height,
            renderer.surface_format(),
            renderer.backend(),
            renderer.present_mode()
        );
        // ponytail: the rate is read once; moving the window to another
        // monitor keeps the first one's cap and budget.
        let refresh = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz());
        self.frame_period = pacing::frame_period(refresh);
        self.pacer = pacing::frame_cap(renderer.present_mode(), self.frame_period);
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
        self.thumbnail_target = Some(thumbnail_target);
        window.focus_window();
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.stats.window_event();
        // Event handling time; the redraw's own time is in its stages.
        let event_start = if matches!(event, WindowEvent::RedrawRequested) {
            None
        } else {
            self.stats.clock()
        };
        let name = event_start.map(|_| event_name(&event));
        // Whether egui used an event no longer matters: the raw stream records
        // everything and the gate decides what the game gets. While the game has
        // the mouse, egui gets no pointer input (the hidden cursor would click the UI).
        if let Some(editor) = self.editor.as_mut()
            && let Some(window) = self.window.as_ref()
            && forwards_to_egui(egui_event(&event), self.game_input)
        {
            editor.on_window_event(window, &event);
        }

        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("close requested");
                if let Some(editor) = self.editor_ui.as_ref() {
                    editor.save_layout();
                }
                event_loop.exit();
            }
            WindowEvent::Focused(false) => release_game_input(&mut self.core.world),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key
                    && let Some(mapped) = map_key(&code)
                {
                    let pressed = event.state == ElementState::Pressed;
                    self.record(InputButton::Key(mapped), pressed, event.repeat);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(mapped) = map_mouse(button) {
                    let pressed = state == ElementState::Pressed;
                    self.record(InputButton::Mouse(mapped), pressed, false);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    // shortcut: 50 px per notch (egui's line height); tune per platform if needed.
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                if self.game_input
                    && let Some(mut input) = self.core.world.get_resource_mut::<Input>()
                {
                    input.wheel += notches;
                }
            }
            // Only paints Windows asks for itself (resize, expose): frames are
            // drawn from `about_to_wait`, see `draw_frame`.
            WindowEvent::RedrawRequested => self.draw_frame(event_loop),
            _ => {}
        }
        self.stats.event_done(event_start);
        self.stats
            .callback(event_start, name.as_deref().unwrap_or_default());
        self.draw_if_overdue(event_loop);
    }

    /// Raw mouse motion (unaccelerated, works with a confined cursor) drives
    /// the game's mouse look.
    fn device_event(&mut self, event_loop: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        self.stats.device_event();
        let start = self.stats.clock();
        if let DeviceEvent::MouseMotion { delta: (x, y) } = event
            && self.game_input
            && let Some(mut input) = self.core.world.get_resource_mut::<Input>()
        {
            input.mouse_delta += Vec2::new(x as f32, y as f32);
        }
        self.stats.event_done(start);
        self.stats.callback(start, "device");
        self.draw_if_overdue(event_loop);
    }
}

/// `CursorMoved { .. }` -> `CursorMoved`: the variant name, for the stall line.
fn event_name(event: &WindowEvent) -> String {
    let debug = format!("{event:?}");
    debug
        .split([' ', '(', '{'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn main() {
    use tracing_subscriber::prelude::*;
    let console = melon_editor::console::ConsoleLog::default();
    // The Console's lines also go to a file that tools (and agents) can read.
    let log_path = std::path::Path::new(PROJECT_ROOT).join(".melon/logs/editor.log");
    let log_file = melon_editor::console::file_layer(&log_path);
    let (log_file, log_file_error) = match log_file {
        Ok(layer) => (Some(layer), None),
        Err(error) => (None, Some(error)),
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(EnvFilter::from_default_env()))
        .with(
            console
                .layer()
                .with_filter(melon_editor::console::capture_filter()),
        )
        .with(log_file.with_filter(melon_editor::console::capture_filter()))
        .init();
    tracing_log::LogTracer::init().ok();
    if let Some(error) = log_file_error {
        tracing::warn!(%error, path = %log_path.display(), "log file not opened");
    }
    tracing::info!("MelonEngine v{} starting up", melon_core::version());

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    // Raw mouse motion is only for the game; the gate turns it on.
    event_loop.listen_device_events(DeviceEvents::Never);

    let mut app = App::new(console);
    if let Err(e) = event_loop.run_app(&mut app) {
        tracing::error!("event loop exited with error: {e:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use melon_core::{MouseButton as EngineMouse, RawPresses};
    use winit::event::MouseButton as WinitMouse;

    const W: InputButton = InputButton::Key(KeyCode::W);

    /// Every winit code that `map_key` must handle.
    const WINIT_KEYS: &[WinitKeyCode] = &[
        WinitKeyCode::KeyA,
        WinitKeyCode::KeyB,
        WinitKeyCode::KeyC,
        WinitKeyCode::KeyD,
        WinitKeyCode::KeyE,
        WinitKeyCode::KeyF,
        WinitKeyCode::KeyG,
        WinitKeyCode::KeyH,
        WinitKeyCode::KeyI,
        WinitKeyCode::KeyJ,
        WinitKeyCode::KeyK,
        WinitKeyCode::KeyL,
        WinitKeyCode::KeyM,
        WinitKeyCode::KeyN,
        WinitKeyCode::KeyO,
        WinitKeyCode::KeyP,
        WinitKeyCode::KeyQ,
        WinitKeyCode::KeyR,
        WinitKeyCode::KeyS,
        WinitKeyCode::KeyT,
        WinitKeyCode::KeyU,
        WinitKeyCode::KeyV,
        WinitKeyCode::KeyW,
        WinitKeyCode::KeyX,
        WinitKeyCode::KeyY,
        WinitKeyCode::KeyZ,
        WinitKeyCode::Digit0,
        WinitKeyCode::Digit1,
        WinitKeyCode::Digit2,
        WinitKeyCode::Digit3,
        WinitKeyCode::Digit4,
        WinitKeyCode::Digit5,
        WinitKeyCode::Digit6,
        WinitKeyCode::Digit7,
        WinitKeyCode::Digit8,
        WinitKeyCode::Digit9,
        WinitKeyCode::F1,
        WinitKeyCode::F2,
        WinitKeyCode::F3,
        WinitKeyCode::F4,
        WinitKeyCode::F5,
        WinitKeyCode::F6,
        WinitKeyCode::F7,
        WinitKeyCode::F8,
        WinitKeyCode::F9,
        WinitKeyCode::F10,
        WinitKeyCode::F11,
        WinitKeyCode::F12,
        WinitKeyCode::ArrowUp,
        WinitKeyCode::ArrowDown,
        WinitKeyCode::ArrowLeft,
        WinitKeyCode::ArrowRight,
        WinitKeyCode::Space,
        WinitKeyCode::Enter,
        WinitKeyCode::Escape,
        WinitKeyCode::Tab,
        WinitKeyCode::Backspace,
        WinitKeyCode::Delete,
        WinitKeyCode::Insert,
        WinitKeyCode::Home,
        WinitKeyCode::End,
        WinitKeyCode::PageUp,
        WinitKeyCode::PageDown,
        WinitKeyCode::ShiftLeft,
        WinitKeyCode::ShiftRight,
        WinitKeyCode::ControlLeft,
        WinitKeyCode::ControlRight,
        WinitKeyCode::AltLeft,
        WinitKeyCode::AltRight,
        WinitKeyCode::Minus,
        WinitKeyCode::Equal,
        WinitKeyCode::BracketLeft,
        WinitKeyCode::BracketRight,
        WinitKeyCode::Backslash,
        WinitKeyCode::Semicolon,
        WinitKeyCode::Quote,
        WinitKeyCode::Comma,
        WinitKeyCode::Period,
        WinitKeyCode::Slash,
        WinitKeyCode::Backquote,
        WinitKeyCode::Numpad0,
        WinitKeyCode::Numpad1,
        WinitKeyCode::Numpad2,
        WinitKeyCode::Numpad3,
        WinitKeyCode::Numpad4,
        WinitKeyCode::Numpad5,
        WinitKeyCode::Numpad6,
        WinitKeyCode::Numpad7,
        WinitKeyCode::Numpad8,
        WinitKeyCode::Numpad9,
    ];

    #[test]
    fn map_key_covers_every_engine_key() {
        let mapped: Vec<KeyCode> = WINIT_KEYS.iter().filter_map(map_key).collect();
        for key in KeyCode::ALL {
            assert!(mapped.contains(key), "no winit code maps to {key:?}");
        }
        assert_eq!(map_key(&WinitKeyCode::KeyW), Some(KeyCode::W));
        assert_eq!(map_key(&WinitKeyCode::Numpad7), Some(KeyCode::Numpad7));
        assert_eq!(map_key(&WinitKeyCode::CapsLock), None);
    }

    #[test]
    fn map_mouse_covers_every_button() {
        let mapped: Vec<EngineMouse> = [
            WinitMouse::Left,
            WinitMouse::Right,
            WinitMouse::Middle,
            WinitMouse::Back,
            WinitMouse::Forward,
        ]
        .into_iter()
        .filter_map(map_mouse)
        .collect();
        assert_eq!(mapped, EngineMouse::ALL);
        assert_eq!(map_mouse(WinitMouse::Other(7)), None);
    }

    #[test]
    fn repeat_events_do_not_press_again() {
        let (mut input, mut raw) = (Input::default(), RawPresses::default());
        record_button(&mut input, &mut raw, W, true, false, true);
        input.clear_frame();
        raw.0.clear();
        record_button(&mut input, &mut raw, W, true, true, true);
        assert!(input.pressed(W));
        assert!(!input.just_pressed(W));
        assert!(raw.0.is_empty(), "a repeat is not a press");
    }

    #[test]
    fn gate_off_records_raw_only() {
        let (mut input, mut raw) = (Input::default(), RawPresses::default());
        record_button(&mut input, &mut raw, W, true, false, false);
        assert!(!input.pressed(W));
        assert_eq!(raw.0, vec![W]);
        record_button(&mut input, &mut raw, W, false, false, false);
        assert_eq!(raw.0, vec![W], "releases are not presses");
    }

    #[test]
    fn editor_keys_never_reach_the_game() {
        let (mut input, mut raw) = (Input::default(), RawPresses::default());
        for key in [KeyCode::Escape, KeyCode::F5, KeyCode::F6] {
            record_button(
                &mut input,
                &mut raw,
                InputButton::Key(key),
                true,
                false,
                true,
            );
            assert!(!input.key(key), "{key:?} reached the game");
        }
        assert_eq!(raw.0.len(), 3, "the raw stream still sees them");
    }

    #[test]
    fn pointer_events_skip_egui_while_the_game_has_input() {
        use EguiEvent::{ButtonRelease, Other, Pointer};
        assert!(
            !forwards_to_egui(Pointer, true),
            "hidden cursor drives the UI"
        );
        assert!(forwards_to_egui(Pointer, false));
        // A press egui saw before the gate opened must still end.
        assert!(forwards_to_egui(ButtonRelease, true));
        assert!(
            forwards_to_egui(Other, true),
            "Esc / F5 / F6 reach the editor"
        );
    }

    #[test]
    fn raw_presses_last_until_the_host_clears_them() {
        let mut core = melon_core::Core::new();
        core.world.insert_resource(Input::default());
        core.world
            .resource_scope(|world, mut raw: Mut<RawPresses>| {
                let mut input = world.resource_mut::<Input>();
                record_button(&mut input, &mut raw, W, true, false, false);
            });
        core.update();
        assert_eq!(
            core.world.resource::<RawPresses>().0,
            vec![W],
            "editor reads it"
        );
        clear_raw_presses(&mut core.world);
        assert!(core.world.resource::<RawPresses>().0.is_empty());
    }

    #[test]
    fn gate_turning_off_releases_held_keys() {
        let (mut input, mut raw) = (Input::default(), RawPresses::default());
        let mut applied = false;
        assert!(apply_gate(&mut input, &mut applied, true));
        record_button(&mut input, &mut raw, W, true, false, applied);
        input.clear_frame();
        assert!(!apply_gate(&mut input, &mut applied, true), "same value");
        assert!(input.pressed(W));
        assert!(apply_gate(&mut input, &mut applied, false));
        assert!(!input.pressed(W));
        assert!(input.just_released(W));
    }

    #[test]
    fn focus_loss_releases_held_mouse_buttons() {
        let mut world = World::new();
        let mut input = Input::default();
        let lmb = InputButton::Mouse(EngineMouse::Left);
        input.press(lmb);
        input.press(W);
        world.insert_resource(input);
        world.insert_resource(melon_core::InputFocus::Game);
        release_game_input(&mut world);
        let input = world.resource::<Input>();
        assert!(!input.pressed(lmb) && !input.pressed(W));
        assert!(input.just_released(lmb));
        assert_eq!(
            *world.resource::<melon_core::InputFocus>(),
            melon_core::InputFocus::Game
        );
    }
}
