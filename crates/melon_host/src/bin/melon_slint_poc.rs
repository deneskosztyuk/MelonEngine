use std::time::{Duration, Instant};

use melon_core::glam::{Mat4, Quat, Vec3};
use melon_core::{Camera, Transform};
use melon_editor::slint_poc::{SceneEntityRow, create_editor_window};
use melon_render::{Mesh, SceneRenderer, ViewportTarget};
use slint::{ComponentHandle, ModelRc, TimerMode, VecModel};
use tracing_subscriber::EnvFilter;

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.035,
    g: 0.045,
    b: 0.06,
    a: 1.0,
};

struct SlintScene {
    renderer: SceneRenderer,
    target: ViewportTarget,
    cube: Mesh,
    camera: Camera,
    started: Instant,
}

impl SlintScene {
    fn new(device: wgpu::Device, queue: wgpu::Queue, width: u32, height: u32) -> Self {
        let renderer = SceneRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb);
        let target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(width, height));
        let cube = renderer.cube_mesh();
        let mut camera = Camera {
            eye: Vec3::new(5.5, 4.0, 7.0),
            center: Vec3::new(0.0, 0.4, 0.0),
            ..Default::default()
        };
        camera.set_aspect(width as f32 / height.max(1) as f32);

        Self {
            renderer,
            target,
            cube,
            camera,
            started: Instant::now(),
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> bool {
        let size = winit::dpi::PhysicalSize::new(width.max(1), height.max(1));
        if self.target.size() == size {
            return false;
        }

        self.target = self.renderer.create_viewport_target(size);
        self.camera
            .set_aspect(size.width as f32 / size.height as f32);
        true
    }

    fn render(&mut self) {
        let elapsed = self.started.elapsed().as_secs_f32();
        let matrices = [
            Transform {
                translation: Vec3::new(0.0, -0.1, 0.0),
                rotation: Quat::IDENTITY,
                scale: Vec3::new(6.0, 0.2, 6.0),
            }
            .to_mat4(),
            Mat4::from_scale_rotation_translation(
                Vec3::ONE,
                Quat::from_rotation_y(elapsed * 0.55),
                Vec3::new(0.0, 0.55, 0.0),
            ),
        ];

        self.renderer.update_camera(self.camera.view_proj());
        self.renderer
            .update_light(Vec3::new(-0.5, 1.0, -0.5).normalize(), 0.22, Vec3::ONE);
        self.renderer.update_model_uniforms(&matrices);

        let color_view = self.target.color_view();
        let mut encoder =
            self.renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Slint scene viewport encoder"),
                });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Slint scene viewport pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.target.hdr_target().view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: self.target.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let draw_calls = [(0, &self.cube), (1, &self.cube)];
            self.renderer.draw_meshes(&mut pass, &draw_calls);
        }
        self.renderer
            .tonemap_pass(&mut encoder, self.target.hdr_target(), color_view.as_ref());

        self.renderer.queue().submit([encoder.finish()]);
    }
}

fn viewport_size(app: &melon_editor::slint_poc::EditorWindow) -> (u32, u32) {
    let scale_factor = app.window().scale_factor();
    (
        (app.get_viewport_width() * scale_factor).round().max(1.0) as u32,
        (app.get_viewport_height() * scale_factor)
            .round()
            .max(1.0) as u32,
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    tracing::info!("MelonEngine Slint editor proof of concept starting");

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into())
        .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::default())
        .select()?;

    let app = create_editor_window()?;

    // Maximize before the event loop starts so the window opens fullscreen.
    // Doing this in RenderingSetup is too late on some winit versions — the
    // window is already shown at its default size and maximizes visibly.
    app.window().set_maximized(true);

    app.set_scene_entities(ModelRc::new(VecModel::from(vec![
        SceneEntityRow {
            id: 0,
            name: "Ground".into(),
        },
        SceneEntityRow {
            id: 1,
            name: "Editor Cube".into(),
        },
    ])));
    app.set_selected_entity_id(1);
    app.set_selected_name("Editor Cube".into());
    app.set_selected_position("0.00   0.55   0.00".into());
    app.set_selected_rotation("0.00   0.00   0.00".into());
    app.set_selected_scale("1.00   1.00   1.00".into());

    let selection_app = app.as_weak();
    app.on_select_entity(move |entity_id| {
        let Some(app) = selection_app.upgrade() else {
            return;
        };
        match entity_id {
            0 => {
                app.set_selected_name("Ground".into());
                app.set_selected_position("0.00  -0.10   0.00".into());
                app.set_selected_scale("6.00   0.20   6.00".into());
            }
            _ => {
                app.set_selected_name("Editor Cube".into());
                app.set_selected_position("0.00   0.55   0.00".into());
                app.set_selected_scale("1.00   1.00   1.00".into());
            }
        }
    });

    let render_app = app.as_weak();
    let mut scene: Option<SlintScene> = None;
    app.window()
        .set_rendering_notifier(move |state, graphics_api| {
            let Some(app) = render_app.upgrade() else {
                return;
            };

            match state {
                slint::RenderingState::RenderingSetup => {
                    let slint::GraphicsAPI::WGPU29 { device, queue, .. } = graphics_api else {
                        panic!("Slint did not provide the requested WGPU 29 renderer");
                    };
                    let (width, height) = viewport_size(&app);
                    let new_scene = SlintScene::new(device.clone(), queue.clone(), width, height);
                    let image = slint::Image::try_from(new_scene.target.color_texture_handle())
                        .expect("failed to import MelonEngine viewport texture into Slint");
                    app.set_scene_texture(image);
                    scene = Some(new_scene);
                }
                slint::RenderingState::BeforeRendering => {
                    let Some(scene) = scene.as_mut() else {
                        return;
                    };
                    let (width, height) = viewport_size(&app);
                    if scene.resize(width, height) {
                        let image = slint::Image::try_from(scene.target.color_texture_handle())
                            .expect("failed to replace resized Slint viewport texture");
                        app.set_scene_texture(image);
                    }
                    scene.render();
                }
                slint::RenderingState::RenderingTeardown => {
                    scene = None;
                }
                _ => {}
            }
        })?;

    let redraw_timer = slint::Timer::default();
    let redraw_app = app.as_weak();
    redraw_timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        if let Some(app) = redraw_app.upgrade() {
            app.window().request_redraw();
        }
    });

    app.run()?;
    Ok(())
}
