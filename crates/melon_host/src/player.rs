//! The player: the host without the editor (`--no-default-features`). It runs
//! a game the way Play does, from the first frame: the startup scene loaded,
//! `Playing`, input to the game (Esc is the game's), window title and size
//! from `PlayerSettings`.
//!
//! The project root is the folder of the executable when `game.pak` lies
//! beside it (a packaged game), else the source tree (dev runs of the player).
//! `--frames N` renders N frames and exits 0, for smoke tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use melon_core::pack::Pack;
use melon_core::package::PACK_FILE;
use melon_core::{
    AssetServer, Camera, Core, GameInputGate, Input, InputButton, InputFocus, InputMap, Light,
    PhysicsSettings, PlayerSettings, RenderQuality, SceneRequests, SimulationState, Time,
    WindowMode,
};
use melon_physics::add_physics_systems;
use tracing_subscriber::EnvFilter;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseScrollDelta, WindowEvent};
use winit::keyboard::PhysicalKey;
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Fullscreen, Window, WindowId},
};

use crate::{
    PROJECT_ROOT, apply_cursor, clear_raw_presses, map_key, map_mouse, pacing, record_button,
    release_game_input,
};

/// Folder asset paths and `config/` are relative to, and its pack if packaged.
fn project_root() -> (PathBuf, Option<PathBuf>) {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && dir.join(PACK_FILE).is_file()
    {
        return (dir.to_path_buf(), Some(dir.join(PACK_FILE)));
    }
    (PathBuf::from(PROJECT_ROOT), None)
}

struct Player {
    window: Option<Arc<Window>>,
    renderer: Option<melon_render::Renderer>,
    /// The scene renders here, then it is copied onto the swapchain.
    target: Option<melon_render::ViewportTarget>,
    core: Core,
    audio: melon_audio::AudioEngine,
    settings: PlayerSettings,
    last_frame: Option<Instant>,
    /// Last cursor applied to the window (`None` = not yet, or dropped on focus loss).
    cursor_applied: Option<melon_core::Cursor>,
    pacer: Option<pacing::FrameDeadline>,
    frame_period: std::time::Duration,
    /// Frames drawn so far, and the `--frames N` limit.
    drawn: u32,
    limit: Option<u32>,
    /// Startup failed: the process exits with code 1.
    failed: bool,
}

impl Player {
    fn new(limit: Option<u32>) -> Result<Self, String> {
        let (root, pack_path) = project_root();
        let mut core = Core::new();
        core.world
            .insert_resource(melon_core::LogInterval { frames_per_log: 0 });
        core.world.insert_resource(Camera::default());
        core.world.insert_resource(Light::default());
        core.world.insert_resource(Input::default());

        let mut assets = AssetServer::new(root.clone());
        match &pack_path {
            Some(path) => {
                let pack = Arc::new(Pack::open(path).map_err(|e| format!("{e:#}"))?);
                melon_core::pack::install(&root, pack.clone());
                assets.set_pack(pack);
                tracing::info!("running from {}", path.display());
            }
            None => {
                assets.scan_metas();
                tracing::info!(
                    "no {PACK_FILE} beside the executable; using {}",
                    root.display()
                );
            }
        }
        core.world.insert_resource(assets);

        let config = root.join("config");
        let (map, outcome) = InputMap::load(&config.join("input.ron"));
        outcome.log(&config.join("input.ron"), "input mapping");
        core.world.insert_resource(map);
        let (quality, outcome) = RenderQuality::load(&config.join("quality.ron"));
        outcome.log(&config.join("quality.ron"), "quality settings");
        core.world.insert_resource(quality);
        let (audio, outcome) = melon_core::AudioSettings::load(&config.join("audio.ron"));
        outcome.log(&config.join("audio.ron"), "audio settings");
        core.world.insert_resource(audio);
        let (physics, outcome) = PhysicsSettings::load(&config.join("physics.ron"));
        outcome.log(&config.join("physics.ron"), "physics settings");
        core.world.insert_resource(physics);
        let (settings, outcome) = PlayerSettings::load(&config.join("player.ron"));
        outcome.log(&config.join("player.ron"), "player settings");
        core.world.insert_resource(settings.clone());

        core.add_plugin(&melon_game::GamePlugin);
        add_physics_systems(&mut core);

        if settings.startup_scene.is_empty() {
            tracing::warn!("no startup scene set (Project Settings > Player); the world is empty");
        } else {
            // Applied first thing in the first frame.
            core.world
                .resource_mut::<SceneRequests>()
                .load(&settings.startup_scene);
        }
        core.world.insert_resource(SimulationState::Playing);
        core.world.insert_resource(InputFocus::Game);
        core.world.insert_resource(GameInputGate(true));

        Ok(Self {
            window: None,
            renderer: None,
            target: None,
            core,
            audio: melon_audio::AudioEngine::new(),
            settings,
            last_frame: None,
            cursor_applied: None,
            pacer: None,
            frame_period: pacing::frame_period(None),
            drawn: 0,
            limit,
            failed: false,
        })
    }

    fn record(&mut self, button: InputButton, pressed: bool, repeat: bool) {
        self.core.world.resource_scope(
            |world, mut raw: melon_core::bevy_ecs::world::Mut<melon_core::RawPresses>| {
                if let Some(mut input) = world.get_resource_mut::<Input>() {
                    record_button(&mut input, &mut raw, button, pressed, repeat, true, &[]);
                }
            },
        );
    }

    fn draw_frame(&mut self, event_loop: &ActiveEventLoop) {
        if self.limit.is_some_and(|n| self.drawn >= n) {
            return; // a redraw queued before exit was requested
        }
        if let Some(pacer) = self.pacer.as_ref() {
            let slept = pacing::sleep_until_deadline(pacer.next(), Instant::now(), pacer.period());
            std::thread::sleep(slept);
        }
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
        self.core.update();
        self.audio.update(&mut self.core.world);
        clear_raw_presses(&mut self.core.world);
        if self.core.world.resource_mut::<melon_core::AppExit>().take() {
            tracing::info!("the game asked to quit");
            event_loop.exit();
            return;
        }
        if let Some(window) = self.window.as_ref() {
            let wanted = *self.core.world.resource::<melon_core::Cursor>();
            if let Some(cursor) = wanted.to_apply(&mut self.cursor_applied, false) {
                apply_cursor(window, cursor);
            }
        }

        let (Some(renderer), Some(target)) = (self.renderer.as_mut(), self.target.as_mut()) else {
            return;
        };
        let present = self.core.world.resource::<RenderQuality>().present;
        if renderer.set_present(present) {
            self.pacer = pacing::frame_cap(renderer.present_mode(), self.frame_period);
        }
        let Some(mut frame) = renderer.begin_frame() else {
            return;
        };
        let frame_data = melon_render::extract(&mut self.core.world);
        {
            let mut server = self.core.world.resource_mut::<AssetServer>();
            renderer.render_scene(
                &mut frame.encoder,
                &frame_data,
                &mut server,
                target,
                &[],
                &[],
            );
        }
        renderer.present_target(&mut frame, target);
        renderer.end_frame(frame);

        self.drawn += 1;
        if self.limit.is_some_and(|n| self.drawn >= n) {
            tracing::info!("rendered {} frames; exiting", self.drawn);
            event_loop.exit();
        }
    }

    /// Draw from inside an event callback once a frame is a period late: a
    /// mouse-look flood keeps winit's queue full, so `about_to_wait` alone would
    /// hold frames back (the editor does the same).
    fn draw_if_overdue(&mut self, event_loop: &ActiveEventLoop) {
        if pacing::frame_overdue(self.last_frame, Instant::now(), self.frame_period) {
            self.draw_unless_minimized(event_loop);
        }
    }

    /// Minimized: no frames (and no game updates).
    fn draw_unless_minimized(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .window
            .as_ref()
            .is_some_and(|w| w.is_minimized() != Some(true))
        {
            self.draw_frame(event_loop);
        }
    }

    /// Match the render target and the camera to the window.
    fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.resize(size);
        let size = renderer.size();
        self.target = Some(renderer.create_viewport_target(size));
        if let Some(mut camera) = self.core.world.get_resource_mut::<Camera>() {
            camera.set_aspect(size.width as f32 / size.height.max(1) as f32);
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, message: &str) {
        tracing::error!("{message}");
        self.failed = true;
        event_loop.exit();
    }
}

impl ApplicationHandler for Player {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.draw_unless_minimized(event_loop);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let [width, height] = self.settings.window_size;
        let mut attributes = Window::default_attributes()
            .with_title(self.settings.game_name.clone())
            .with_inner_size(winit::dpi::PhysicalSize::new(width, height));
        if self.settings.window_mode == WindowMode::BorderlessFullscreen {
            attributes = attributes.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
            let scale = event_loop
                .primary_monitor()
                .map_or(1.0, |m| m.scale_factor());
            let icon = |points: u32| {
                let size = winit::dpi::LogicalSize::new(points, points).to_physical(scale);
                winit::window::Icon::from_resource(1, Some(size)).ok()
            };
            attributes = attributes
                .with_window_icon(icon(16))
                .with_taskbar_icon(icon(32));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                return self.fail(event_loop, &format!("cannot create the window: {error}"));
            }
        };
        // FXC ships with Windows; the default would look for dxcompiler.dll.
        let renderer = pollster::block_on(melon_render::Renderer::new_with_dx12_compiler(
            window.clone(),
            wgpu::Dx12Compiler::Fxc,
        ));
        let mut renderer = match renderer {
            Ok(renderer) => renderer,
            Err(error) => {
                return self.fail(event_loop, &format!("renderer init failed: {error:#}"));
            }
        };
        let present = self.core.world.resource::<RenderQuality>().present;
        renderer.set_present(present);
        let size = renderer.size();
        tracing::info!(
            "renderer ready: {}x{} {:?} {:?} {:?}",
            size.width,
            size.height,
            renderer.surface_format(),
            renderer.backend(),
            renderer.present_mode()
        );
        let refresh = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz());
        self.frame_period = pacing::frame_period(refresh);
        self.pacer = pacing::frame_cap(renderer.present_mode(), self.frame_period);
        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        self.resize(size);
        event_loop.listen_device_events(pacing::device_events(true));
        window.focus_window();
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("close requested");
                event_loop.exit();
            }
            WindowEvent::Focused(focused) => {
                if let Some(window) = self.window.as_ref() {
                    if focused {
                        // Re-apply the game's cursor (a lost focus dropped the grab).
                        self.cursor_applied = None;
                    } else {
                        apply_cursor(window, melon_core::Cursor::default());
                        self.cursor_applied = None;
                    }
                }
                if !focused {
                    release_game_input(&mut self.core.world);
                }
            }
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => self.resize(size),
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
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                if let Some(mut input) = self.core.world.get_resource_mut::<Input>() {
                    input.wheel += notches;
                }
            }
            WindowEvent::RedrawRequested => self.draw_frame(event_loop),
            _ => {}
        }
        self.draw_if_overdue(event_loop);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (x, y) } = event
            && let Some(mut input) = self.core.world.get_resource_mut::<Input>()
        {
            input.mouse_delta += melon_core::glam::Vec2::new(x as f32, y as f32);
        }
        self.draw_if_overdue(event_loop);
    }
}

pub fn run() {
    // Without RUST_LOG the player still logs info lines (to stdout).
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    tracing_log::LogTracer::init().ok();
    tracing::info!("MelonEngine v{} player starting", melon_core::version());

    let args: Vec<String> = std::env::args().collect();
    let limit = args
        .windows(2)
        .find(|w| w[0] == "--frames")
        .and_then(|w| w[1].parse().ok());
    let mut player = match Player::new(limit) {
        Ok(player) => player,
        Err(error) => {
            tracing::error!("cannot start: {error}");
            std::process::exit(1);
        }
    };
    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    if let Err(error) = event_loop.run_app(&mut player) {
        tracing::error!("event loop exited with error: {error:?}");
        std::process::exit(1);
    }
    if player.failed {
        std::process::exit(1);
    }
}
