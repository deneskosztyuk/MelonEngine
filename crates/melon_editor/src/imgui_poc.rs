use std::path::PathBuf;
use std::time::Instant;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use imgui::{Condition, ConfigFlags, FontConfig, FontSource, Image, MouseCursor, TextureId};
use imgui_wgpu::{RawTextureConfig, Renderer, RendererConfig, Texture};
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use melon_core::{Name, SimulationState, Transform};
use winit::event::{Event, WindowEvent};
use winit::window::{Window, WindowId};

pub struct ImguiEditorOverlay {
    context: imgui::Context,
    platform: WinitPlatform,
    renderer: Renderer,
    selected: Option<Entity>,
    viewport_texture: Option<TextureId>,
    requested_viewport_size: winit::dpi::PhysicalSize<u32>,
    last_frame: Instant,
    last_cursor: Option<MouseCursor>,
}

impl ImguiEditorOverlay {
    pub fn new(
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let mut context = imgui::Context::create();
        context
            .io_mut()
            .config_flags
            .insert(ConfigFlags::DOCKING_ENABLE);

        let mut platform = WinitPlatform::new(&mut context);
        platform.attach_window(context.io_mut(), window, HiDpiMode::Default);

        let state_dir = PathBuf::from(".melon");
        if let Err(error) = std::fs::create_dir_all(&state_dir) {
            tracing::warn!(?error, "failed to create editor state directory");
            context.set_ini_filename(None);
        } else {
            context.set_ini_filename(Some(state_dir.join("imgui.ini")));
        }

        let hidpi_factor = window.scale_factor();
        context.io_mut().font_global_scale = (1.0 / hidpi_factor) as f32;
        context.fonts().add_font(&[FontSource::DefaultFontData {
            config: Some(FontConfig {
                size_pixels: (14.0 * hidpi_factor) as f32,
                ..Default::default()
            }),
        }]);

        let renderer = Renderer::new(
            &mut context,
            device,
            queue,
            RendererConfig {
                texture_format: surface_format.add_srgb_suffix(),
                ..Default::default()
            },
        );

        Self {
            context,
            platform,
            renderer,
            selected: None,
            viewport_texture: None,
            requested_viewport_size: winit::dpi::PhysicalSize::new(1, 1),
            last_frame: Instant::now(),
            last_cursor: None,
        }
    }

    pub fn set_viewport_target(
        &mut self,
        device: &wgpu::Device,
        target: &melon_render::ViewportTarget,
    ) {
        let size = target.size();
        let texture = Texture::from_raw_parts(
            device,
            &self.renderer,
            target.color_texture(),
            target.color_view(),
            None,
            Some(&RawTextureConfig {
                label: Some("Dear ImGui scene viewport"),
                sampler_desc: wgpu::SamplerDescriptor {
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                },
            }),
            wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
        );

        if let Some(texture_id) = self.viewport_texture {
            self.renderer.textures.replace(texture_id, texture);
        } else {
            self.viewport_texture = Some(self.renderer.textures.insert(texture));
        }
    }

    pub fn requested_viewport_size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.requested_viewport_size
    }

    pub fn on_window_event(
        &mut self,
        window: &Window,
        window_id: WindowId,
        event: WindowEvent,
    ) -> (WindowEvent, bool) {
        let event = Event::<()>::WindowEvent { window_id, event };
        self.platform
            .handle_event(self.context.io_mut(), window, &event);

        let io = self.context.io();
        let consumed = match &event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::KeyboardInput { .. }
                | WindowEvent::ModifiersChanged(_)
                | WindowEvent::Ime(_) => io.want_capture_keyboard,
                WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::Touch(_) => io.want_capture_mouse,
                _ => false,
            },
            _ => false,
        };

        let Event::WindowEvent { event, .. } = event else {
            unreachable!();
        };
        (event, consumed)
    }

    pub fn render(
        &mut self,
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        world: &mut World,
    ) -> Result<(), imgui_wgpu::RendererError> {
        let now = Instant::now();
        self.context
            .io_mut()
            .update_delta_time(now - self.last_frame);
        self.last_frame = now;

        if let Err(error) = self.platform.prepare_frame(self.context.io_mut(), window) {
            tracing::warn!(?error, "failed to prepare Dear ImGui frame");
        }

        let ui = self.context.frame();
        build_editor_shell(
            ui,
            world,
            &mut self.selected,
            self.viewport_texture,
            &mut self.requested_viewport_size,
        );

        if self.last_cursor != ui.mouse_cursor() {
            self.last_cursor = ui.mouse_cursor();
            self.platform.prepare_render(ui, window);
        }

        self.renderer
            .render(self.context.render(), queue, device, pass)
    }
}

fn build_editor_shell(
    ui: &imgui::Ui,
    world: &mut World,
    selected: &mut Option<Entity>,
    viewport_texture: Option<TextureId>,
    requested_viewport_size: &mut winit::dpi::PhysicalSize<u32>,
) {
    ui.dockspace_over_main_viewport();

    ui.main_menu_bar(|| {
        ui.menu("File", || {
            ui.menu_item("New Scene");
            ui.menu_item("Open Scene...");
            ui.separator();
            ui.menu_item("Save");
            ui.menu_item("Save As...");
        });
        ui.menu("Edit", || {
            ui.menu_item("Undo");
            ui.menu_item("Redo");
        });
        ui.menu("GameObject", || {
            ui.menu_item("Create Empty");
            ui.menu_item("Create Cube");
        });
        ui.menu("Window", || {
            ui.menu_item("Reset Layout");
        });
    });

    let entities: Vec<(Entity, String)> = world
        .query::<(Entity, &Name)>()
        .iter(world)
        .map(|(entity, name)| (entity, name.0.clone()))
        .collect();

    ui.window("Hierarchy")
        .position([0.0, 24.0], Condition::FirstUseEver)
        .size([260.0, 520.0], Condition::FirstUseEver)
        .build(|| {
            ui.text_disabled("Scene");
            ui.separator();
            for (entity, name) in &entities {
                let label = format!("{name}##{}", entity.index());
                if ui
                    .selectable_config(&label)
                    .selected(*selected == Some(*entity))
                    .build()
                {
                    *selected = Some(*entity);
                }
            }
        });

    ui.window("Scene")
        .position([270.0, 24.0], Condition::FirstUseEver)
        .size([740.0, 520.0], Condition::FirstUseEver)
        .build(|| {
            let available = ui.content_region_avail();
            let framebuffer_scale = ui.io().display_framebuffer_scale;
            *requested_viewport_size = winit::dpi::PhysicalSize::new(
                (available[0] * framebuffer_scale[0]).round().max(1.0) as u32,
                (available[1] * framebuffer_scale[1]).round().max(1.0) as u32,
            );
            if let Some(texture_id) = viewport_texture {
                Image::new(texture_id, available).build(ui);
            } else {
                ui.text_disabled("Scene viewport unavailable");
            }
        });

    ui.window("Inspector")
        .position([1020.0, 24.0], Condition::FirstUseEver)
        .size([300.0, 520.0], Condition::FirstUseEver)
        .build(
            || match selected.and_then(|entity| world.get_entity(entity).ok()) {
                Some(entity_ref) => {
                    if let Some(name) = entity_ref.get::<Name>() {
                        ui.text(&name.0);
                        ui.separator();
                    }
                    if let Some(transform) = entity_ref.get::<Transform>() {
                        ui.text("Transform");
                        ui.text(format!(
                            "Position  {:.2}  {:.2}  {:.2}",
                            transform.translation.x,
                            transform.translation.y,
                            transform.translation.z
                        ));
                        let (x, y, z) =
                            transform.rotation.to_euler(melon_core::glam::EulerRot::XYZ);
                        ui.text(format!(
                            "Rotation  {:.1}  {:.1}  {:.1}",
                            x.to_degrees(),
                            y.to_degrees(),
                            z.to_degrees()
                        ));
                        ui.text(format!(
                            "Scale     {:.2}  {:.2}  {:.2}",
                            transform.scale.x, transform.scale.y, transform.scale.z
                        ));
                    }
                }
                None => ui.text_disabled("No entity selected"),
            },
        );

    ui.window("Content Browser")
        .position([0.0, 554.0], Condition::FirstUseEver)
        .size([660.0, 230.0], Condition::FirstUseEver)
        .build(|| {
            ui.text_disabled("Assets");
            ui.separator();
            ui.text("models/");
            ui.same_line();
            ui.text("shaders/");
        });

    ui.window("Console")
        .position([670.0, 554.0], Condition::FirstUseEver)
        .size([650.0, 230.0], Condition::FirstUseEver)
        .build(|| {
            let state = world
                .get_resource::<SimulationState>()
                .copied()
                .unwrap_or_default();
            ui.text(format!("Simulation: {state:?}"));
            ui.text(format!("Entities: {}", entities.len()));
            ui.text_disabled("Tracing capture will be added after the editor domain split");
        });
}
