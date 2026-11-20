// Renders the fixed review scene offscreen and saves it as a PNG.
// Run: cargo run -p melon_render --example snapshot -- <out.png> [view] [flag]
// Views: overview (default), low, sun, materials, corner, crowd (overview
// plus about 10,000 grey cubes and spheres), cornell (the review scene plus
// a red/green open-front room at (-13, 0, -13), seen through its opening).
// Prints the backend, the mean frame time and the `RenderStats`. The timed frames run
// before the capture, so the GI probes have converged.
// Debug flags (any order): --ao-only saves the SSAO term (grey, white =
// open) instead of the image; --no-ao renders with `ao_strength` 0;
// --no-shadow moves the scene and camera far outside the fixed shadow box, so
// nothing casts or receives sun shadows (the image is otherwise unchanged);
// --no-bloom renders with `bloom_intensity` 0; --bloom-only renders with
// `bloom_intensity` 1, so the tonemapped image is the bloom alone;
// --gi off|ssgi|ddgi picks `gi_mode` (default ddgi; --no-gi is --gi off);
// --gi-only saves the indirect diffuse term of that mode alone (bloom off).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use glam::{Mat4, Quat, Vec3};
use melon_core::{AssetServer, Camera, Environment, GiMode, Handle, Light, Model};
use melon_render::{FrameData, RenderStats, Renderer};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

/// The review scene: grey floor, blocks, a tall pillar and a wall corner,
/// one checkerboard `builtin:cube`, `cube.glb`, the material spheres, the
/// normal-mapped panel and the emissive cube, all inside the shadow box
/// around the origin.
fn review_scene(server: &mut AssetServer) -> Vec<(Mat4, Handle<Model>)> {
    let checker: Handle<Model> = server.load("builtin:cube");
    let cube: Handle<Model> = server.load("assets/models/review/grey_cube.glb");
    let glb: Handle<Model> = server.load("assets/models/cube.glb");
    let spheres: Handle<Model> = server.load("assets/models/review/material_spheres.glb");
    let panel: Handle<Model> = server.load("assets/models/review/normal_panel.glb");
    let glow: Handle<Model> = server.load("assets/models/review/emissive_cube.glb");
    let place = |scale: Vec3, yaw_deg: f32, pos: Vec3| {
        Mat4::from_scale_rotation_translation(
            scale,
            Quat::from_rotation_y(yaw_deg.to_radians()),
            pos,
        )
    };
    vec![
        // Floor, top at y = 0.
        (
            place(Vec3::new(20.0, 0.1, 20.0), 0.0, Vec3::new(0.0, -0.05, 0.0)),
            cube,
        ),
        // Cubes at different heights and rotations.
        (place(Vec3::ONE, 0.0, Vec3::new(0.0, 0.5, 0.0)), checker),
        (
            place(Vec3::splat(1.5), 30.0, Vec3::new(-2.5, 0.75, 3.0)),
            cube,
        ),
        (
            place(Vec3::splat(0.8), 45.0, Vec3::new(1.5, 2.0, 1.0)),
            cube,
        ),
        (
            place(Vec3::new(2.0, 0.5, 1.0), -20.0, Vec3::new(3.0, 0.25, -2.0)),
            cube,
        ),
        // Tall pillar; the sun throws its shadow toward +x +z.
        (
            place(Vec3::new(0.6, 6.0, 0.6), 0.0, Vec3::new(-4.0, 3.0, -1.0)),
            cube,
        ),
        // The glTF test model.
        (place(Vec3::ONE, 20.0, Vec3::new(3.0, 0.5, 3.5)), glb),
        // Corner at (6, -6): one wall faces +z (shaded), one faces -x (lit).
        (
            place(Vec3::new(6.0, 3.0, 0.2), 0.0, Vec3::new(3.0, 1.5, -6.1)),
            cube,
        ),
        (
            place(Vec3::new(0.2, 3.0, 6.0), 0.0, Vec3::new(6.1, 1.5, -3.0)),
            cube,
        ),
        // Sphere rows along x (rough to the right; dielectric on top, metal
        // below), lit from the camera side in the materials view.
        (place(Vec3::ONE, 0.0, Vec3::new(0.0, 0.0, 6.5)), spheres),
        // Normal-mapped panel facing -x -z, toward the sun.
        (place(Vec3::ONE, 225.0, Vec3::new(3.8, 1.05, 7.5)), panel),
        // Emissive cube (HDR orange glow) beside the checker cube, in the
        // overview and corner views.
        (
            place(Vec3::splat(0.6), 15.0, Vec3::new(1.8, 0.3, 1.4)),
            glow,
        ),
    ]
}

/// About 10,000 grey cubes and single spheres (`#mesh` of the sphere rows)
/// in a 1.2 m grid around the review floor, most of it outside the view.
fn crowd(server: &mut AssetServer) -> Vec<(Mat4, Handle<Model>)> {
    let cube: Handle<Model> = server.load("assets/models/review/grey_cube.glb");
    let sphere: Handle<Model> = server.load("assets/models/review/material_spheres.glb#mesh7");
    (0..100 * 100)
        .filter_map(|i| {
            let at = Vec3::new(
                -60.0 + 1.2 * (i % 100) as f32,
                0.4,
                -60.0 + 1.2 * (i / 100) as f32,
            );
            let on_floor = at.x.abs() < 11.0 && at.z.abs() < 11.0;
            let model = if i % 2 == 0 { cube } else { sphere };
            (!on_floor).then(|| {
                (
                    Mat4::from_scale_rotation_translation(Vec3::splat(0.8), Quat::IDENTITY, at),
                    model,
                )
            })
        })
        .collect()
}

/// The GI review room: grey floor, back wall and ceiling, red left wall,
/// green right wall (seen from the front), open toward -z where the sun
/// comes in. Interior 5 x 4 x 5 m centred on (-13, 0, -13), walls 0.4 m:
/// inside the GI volume and the sun's shadow box, just off the review floor.
fn cornell(server: &mut AssetServer) -> Vec<(Mat4, Handle<Model>)> {
    let grey: Handle<Model> = server.load("assets/models/review/grey_cube.glb");
    let red: Handle<Model> = server.load("assets/models/review/red_panel.glb");
    let green: Handle<Model> = server.load("assets/models/review/green_panel.glb");
    let slab = |size: Vec3, center: Vec3| {
        Mat4::from_scale_rotation_translation(size, Quat::IDENTITY, center)
    };
    // Walls end under the ceiling and at the back wall: overlapping slabs
    // share face planes, which z-fight (striped edges).
    vec![
        (
            slab(Vec3::new(5.8, 0.1, 7.0), Vec3::new(-13.0, -0.05, -14.0)),
            grey,
        ),
        (
            slab(Vec3::new(0.4, 4.0, 5.0), Vec3::new(-10.3, 2.0, -13.0)),
            red,
        ),
        (
            slab(Vec3::new(0.4, 4.0, 5.0), Vec3::new(-15.7, 2.0, -13.0)),
            green,
        ),
        (
            slab(Vec3::new(5.8, 4.0, 0.4), Vec3::new(-13.0, 2.0, -10.3)),
            grey,
        ),
        (
            slab(Vec3::new(5.8, 0.4, 5.4), Vec3::new(-13.0, 4.2, -12.8)),
            grey,
        ),
    ]
}

/// Named camera presets.
fn camera(view: &str) -> Result<Camera> {
    let (eye, center) = match view {
        // Three-quarter view from above.
        "overview" | "crowd" => (Vec3::new(-11.0, 9.0, 12.0), Vec3::new(0.5, 0.5, -1.0)),
        // Near the floor, looking toward the horizon.
        "low" => (Vec3::new(-9.0, 0.6, 9.0), Vec3::new(9.0, 1.2, -9.0)),
        // From the +x +z side, up toward the default sun (disc near the top
        // edge), the pillar and walls in view.
        "sun" => {
            let eye = Vec3::new(6.0, 0.3, 6.0);
            let pitch = 34f32.to_radians();
            let dir = Vec3::new(
                -pitch.cos() * 0.5f32.sqrt(),
                pitch.sin(),
                -pitch.cos() * 0.5f32.sqrt(),
            );
            (eye, eye + dir)
        }
        // From the sun's side onto the sphere rows and the panel.
        "materials" => (Vec3::new(0.0, 2.4, 0.0), Vec3::new(1.0, 0.9, 6.5)),
        // Close over the cubes at the origin toward the two-wall corner.
        "corner" => (Vec3::new(-2.5, 3.0, 4.0), Vec3::new(3.0, 0.2, -3.0)),
        // In front of the GI room's opening, looking in.
        "cornell" => (Vec3::new(-13.0, 2.0, -21.5), Vec3::new(-13.0, 1.9, -12.0)),
        _ => {
            bail!(
                "unknown view {view:?}; expected overview, low, sun, materials, corner, crowd or cornell"
            )
        }
    };
    Ok(Camera {
        eye,
        center,
        aspect: WIDTH as f32 / HEIGHT as f32,
        ..Camera::default()
    })
}

/// Copy the tightly packed `row` bytes out of each `padded` buffer row.
fn unpad_rows(data: &[u8], padded: usize, row: usize) -> Vec<u8> {
    data.chunks(padded)
        .flat_map(|r| &r[..row])
        .copied()
        .collect()
}

/// Frames timed per run, after one warm-up frame.
const TIMED_FRAMES: u32 = 100;

/// One frame: `render_scene`, submit, then wait for the GPU.
fn render_and_wait(
    renderer: &mut Renderer,
    frame: &FrameData,
    server: &mut AssetServer,
    target: &melon_render::ViewportTarget,
) -> Result<RenderStats> {
    let mut encoder = renderer
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("snapshot timing"),
        });
    let stats = renderer.render_scene(&mut encoder, frame, server, target, &[], &[]);
    renderer.queue().submit([encoder.finish()]);
    renderer
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .context("timing poll failed")?;
    Ok(stats)
}

/// Debug offset for `--no-shadow`: far outside the shadow box around the origin.
const NO_SHADOW_OFFSET: Vec3 = Vec3::new(200.0, 0.0, 0.0);

/// The GI mode named by `--gi <mode>`.
fn gi_mode(name: Option<&str>) -> Result<GiMode> {
    match name {
        Some("off") => Ok(GiMode::Off),
        Some("ssgi") => Ok(GiMode::Ssgi),
        Some("ddgi") => Ok(GiMode::Ddgi),
        other => bail!("--gi needs off, ssgi or ddgi, got {other:?}"),
    }
}

fn render(window: Arc<Window>, out: &str, view: &str, flags: &[String]) -> Result<()> {
    let mut gi = GiMode::Ddgi;
    let mut args = flags.iter();
    while let Some(flag) = args.next() {
        const FLAGS: [&str; 8] = [
            "--ao-only",
            "--no-ao",
            "--no-shadow",
            "--no-bloom",
            "--bloom-only",
            "--gi",
            "--no-gi",
            "--gi-only",
        ];
        match flag.as_str() {
            "--gi" => gi = gi_mode(args.next().map(String::as_str))?,
            "--no-gi" => gi = GiMode::Off,
            f if FLAGS.contains(&f) => {}
            _ => bail!("unknown flag {flag:?}; expected one of {FLAGS:?}"),
        }
    }
    let has = |f: &str| flags.iter().any(|x| x == f);
    let ao_only = has("--ao-only");
    let gi_only = has("--gi-only");
    let offset = if has("--no-shadow") {
        NO_SHADOW_OFFSET
    } else {
        Vec3::ZERO
    };
    let mut camera = camera(view)?;
    camera.eye += offset;
    camera.center += offset;
    let mut renderer = pollster::block_on(Renderer::new(window))?;
    renderer.set_gi_only(gi_only);
    let target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(WIDTH, HEIGHT));
    let mut server = AssetServer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..").into());
    let mut frame = FrameData {
        view_proj: camera.view_proj(),
        eye: camera.eye,
        light: Light::default(),
        environment: Environment {
            ao_strength: if has("--no-ao") { 0.0 } else { 1.0 },
            bloom_intensity: if has("--no-bloom") || gi_only {
                0.0
            } else if has("--bloom-only") {
                1.0
            } else {
                Environment::default().bloom_intensity
            },
            gi_mode: gi,
            ..Default::default()
        },
        models: review_scene(&mut server),
    };
    if view == "crowd" {
        frame.models.extend(crowd(&mut server));
    }
    if view == "cornell" {
        frame.models.extend(cornell(&mut server));
    }
    for (matrix, _) in &mut frame.models {
        *matrix = Mat4::from_translation(offset) * *matrix;
    }

    let device = renderer.device().clone();
    let queue = renderer.queue().clone();
    // The first frame loads the models; time the steady state after it.
    let mut stats = render_and_wait(&mut renderer, &frame, &mut server, &target)?;
    let start = std::time::Instant::now();
    for _ in 0..TIMED_FRAMES {
        stats = render_and_wait(&mut renderer, &frame, &mut server, &target)?;
    }
    println!(
        "{view}: {:?}, {} models, {:.2} ms per frame (render_scene + submit + wait, mean of {TIMED_FRAMES})",
        renderer.backend(),
        frame.models.len(),
        start.elapsed().as_secs_f64() * 1000.0 / f64::from(TIMED_FRAMES)
    );
    println!("{view}: {stats:?}");

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("snapshot"),
    });
    renderer.render_scene(&mut encoder, &frame, &mut server, &target, &[], &[]);

    let texture = if ao_only {
        target.ao_texture().clone()
    } else {
        target.color_texture_handle()
    };
    let row = WIDTH * texture.format().block_copy_size(None).unwrap_or(4);
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("snapshot readback"),
        size: (padded * HEIGHT) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |res| {
        let _ = tx.send(res);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .context("readback poll failed")?;
    rx.recv()?.context("readback map failed")?;
    let mut pixels = unpad_rows(&buffer.get_mapped_range(..), padded as usize, row as usize);

    // The display image is the sRGB surface format: RGBA or BGRA.
    let color_type = match texture.format() {
        wgpu::TextureFormat::Rgba8UnormSrgb => image::ColorType::Rgba8,
        wgpu::TextureFormat::Bgra8UnormSrgb => {
            pixels.chunks_mut(4).for_each(|p| p.swap(0, 2));
            image::ColorType::Rgba8
        }
        wgpu::TextureFormat::R8Unorm => image::ColorType::L8,
        other => bail!("unsupported viewport format {other:?}"),
    };
    if let Some(dir) = std::path::Path::new(out).parent() {
        std::fs::create_dir_all(dir)?;
    }
    image::save_buffer(out, &pixels, WIDTH, HEIGHT, color_type)
        .with_context(|| format!("failed to write {out}"))?;
    Ok(())
}

struct App {
    out: String,
    view: String,
    flags: Vec<String>,
    result: Option<Result<()>>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.result.is_none() {
            let attrs = Window::default_attributes()
                .with_title("melon snapshot")
                .with_visible(false);
            self.result = Some(
                event_loop
                    .create_window(attrs)
                    .context("failed to create hidden window")
                    .and_then(|w| render(Arc::new(w), &self.out, &self.view, &self.flags)),
            );
        }
        event_loop.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args.next().context(
        "usage: snapshot <out.png> [overview|low|sun|materials|corner|crowd|cornell] [--ao-only] [--no-ao] [--no-shadow] [--no-bloom] [--bloom-only] [--gi off|ssgi|ddgi] [--no-gi] [--gi-only]",
    )?;
    let view = args.next().unwrap_or_else(|| "overview".into());
    let flags = args.collect();

    #[cfg(target_os = "windows")]
    let event_loop = {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        EventLoop::builder().with_any_thread(true).build()?
    };
    #[cfg(not(target_os = "windows"))]
    let event_loop = EventLoop::new()?;

    let mut app = App {
        out: out.clone(),
        view,
        flags,
        result: None,
    };
    event_loop.run_app(&mut app)?;
    app.result.context("event loop exited before rendering")??;
    println!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpad_rows_drops_row_padding() {
        let data = [1, 2, 0, 0, 3, 4, 0, 0];
        assert_eq!(unpad_rows(&data, 4, 2), vec![1, 2, 3, 4]);
    }

    #[test]
    fn gi_flag_names_every_mode() {
        assert_eq!(gi_mode(Some("off")).unwrap(), GiMode::Off);
        assert_eq!(gi_mode(Some("ssgi")).unwrap(), GiMode::Ssgi);
        assert_eq!(gi_mode(Some("ddgi")).unwrap(), GiMode::Ddgi);
        assert!(gi_mode(Some("rtgi")).is_err());
        assert!(gi_mode(None).is_err());
    }

    #[test]
    fn unknown_view_is_an_error() {
        assert!(camera("sideways").is_err());
        for view in [
            "overview",
            "low",
            "sun",
            "materials",
            "corner",
            "crowd",
            "cornell",
        ] {
            assert!(camera(view).is_ok(), "{view}");
        }
    }
}
