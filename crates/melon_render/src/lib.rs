use std::sync::Arc;

mod models;

/// Model uniform slots: at most this many entities with a model are drawn.
pub const MAX_ENTITIES: usize = 1024;
pub use models::ModelCache;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use tracing::instrument;
use wgpu::util::DeviceExt;
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ModelUniform {
    model: [[f32; 4]; 4],
    normal_matrix: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightUniform {
    direction: [f32; 3],
    ambient: f32,
    color: [f32; 3],
    _pad: f32,
    light_view_proj: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct LineVertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
}

pub struct Mesh {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
    pub index_format: wgpu::IndexFormat,
}

/// A model loaded from a glTF file. Each primitive becomes one `Mesh` with an
/// optional albedo texture bind group. Draw by zipping `meshes` with
/// `albedo_bind_groups` (falling back to the renderer's checkerboard when an
/// entry is `None`).
pub struct LoadedModel {
    pub meshes: Vec<Mesh>,
    /// Per-primitive albedo bind group. `None` if the primitive had no
    /// base color texture; the caller should bind the renderer's default.
    pub albedo_bind_groups: Vec<Option<wgpu::BindGroup>>,
    /// Local-space bounds (min, max) of every part, node transforms included.
    pub bounds: (glam::Vec3, glam::Vec3),
}

/// Convert a decoded glTF image (`gltf::image::Data`) into an
/// `image::DynamicImage` the renderer can upload. glTF stores pixels as
/// 8-bit or 16-bit channels; we convert everything to RGBA8 for upload.
fn image_from_gltf_data(data: &gltf::image::Data) -> image::DynamicImage {
    use gltf::image::Format;
    use image::{ImageBuffer, Rgba};
    let (width, height) = (data.width, data.height);
    match data.format {
        Format::R8G8B8A8 => ImageBuffer::from_raw(width, height, data.pixels.clone())
            .map(image::DynamicImage::ImageRgba8)
            .expect("gltf R8G8B8A8 pixel buffer size mismatch"),
        Format::R8G8B8 => {
            let rgb: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
                ImageBuffer::from_raw(width, height, data.pixels.clone())
                    .expect("gltf R8G8B8 pixel buffer size mismatch");
            image::DynamicImage::ImageRgb8(rgb).to_rgba8().into()
        }
        Format::R8G8 => {
            let gray_alpha: ImageBuffer<image::LumaA<u8>, Vec<u8>> =
                ImageBuffer::from_raw(width, height, data.pixels.clone())
                    .expect("gltf R8G8 pixel buffer size mismatch");
            image::DynamicImage::ImageLumaA8(gray_alpha)
                .to_rgba8()
                .into()
        }
        Format::R8 => {
            let gray: ImageBuffer<image::Luma<u8>, Vec<u8>> =
                ImageBuffer::from_raw(width, height, data.pixels.clone())
                    .expect("gltf R8 pixel buffer size mismatch");
            image::DynamicImage::ImageLuma8(gray).to_rgba8().into()
        }
        // 16-bit and float formats: convert channel-by-channel to RGBA8.
        // Base color textures are rarely 16-bit; this is a safety net.
        _ => {
            let mut rgba = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(width, height);
            let channels = match data.format {
                Format::R16G16B16A16 => 4,
                Format::R16G16B16 => 3,
                Format::R16G16 => 2,
                Format::R16 => 1,
                Format::R32G32B32FLOAT => 3,
                _ => 1,
            };
            let stride = channels * 2; // 16-bit = 2 bytes per channel
            for (x, y, pixel) in rgba.enumerate_pixels_mut() {
                let offset = ((y * width + x) as usize) * stride;
                if offset + stride > data.pixels.len() {
                    break;
                }
                let read_ch = |c: usize| -> u8 {
                    let off = offset + c * 2;
                    if off + 1 < data.pixels.len() {
                        // Take the high byte of the 16-bit value.
                        data.pixels[off + 1]
                    } else {
                        0
                    }
                };
                match channels {
                    1 => *pixel = Rgba([read_ch(0), read_ch(0), read_ch(0), 255]),
                    2 => *pixel = Rgba([read_ch(0), read_ch(0), read_ch(0), read_ch(1)]),
                    3 => *pixel = Rgba([read_ch(0), read_ch(1), read_ch(2), 255]),
                    _ => *pixel = Rgba([read_ch(0), read_ch(1), read_ch(2), read_ch(3)]),
                }
            }
            image::DynamicImage::ImageRgba8(rgba)
        }
    }
}

/// Format of the scene colour target. The tonemap pass maps it to the display.
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// MSAA sample count of the 3D scene pass (colour and depth).
pub const MSAA_SAMPLES: u32 = 4;

/// Width and height of the sun's shadow map, in texels.
pub const SHADOW_MAP_SIZE: u32 = 2048;

/// World -> shadow clip space for a directional light. An orthographic box of
/// half-size `radius` around `center`, seen from `to_light`.
pub fn directional_light_view_proj(
    to_light: glam::Vec3,
    center: glam::Vec3,
    radius: f32,
) -> glam::Mat4 {
    use glam::camera::rh::{proj::directx, view};
    // look_at degenerates when the view direction is parallel to `up`.
    let up = if to_light.y.abs() > 0.99 {
        glam::Vec3::Z
    } else {
        glam::Vec3::Y
    };
    let eye = center + to_light * (2.0 * radius);
    directx::orthographic(-radius, radius, -radius, radius, 0.0, 4.0 * radius)
        * view::look_at_mat4(eye, center, up)
}

/// Offscreen HDR colour target that the 3D pass renders into. The pass
/// draws into the multisampled texture and resolves into the single-sample
/// one, which the tonemap pass reads.
pub struct HdrTarget {
    msaa_texture: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Binds this target as the tonemap pass input.
    bind_group: wgpu::BindGroup,
    size: winit::dpi::PhysicalSize<u32>,
}

impl HdrTarget {
    /// Resolve target: the single-sample image the tonemap pass reads.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Multisampled colour attachment of the 3D pass. Resolve into `view()`.
    pub fn msaa_view(&self) -> &wgpu::TextureView {
        &self.msaa_view
    }

    pub fn sample_count(&self) -> u32 {
        self.msaa_texture.sample_count()
    }

    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }
}

/// Growable vertex buffer for line lists. Each draw site that can appear in
/// the same frame needs its own: `queue.write_buffer` lands before the GPU
/// runs, so a shared buffer would hold only the last upload.
#[derive(Default)]
struct LineBuffer {
    buffer: Option<wgpu::Buffer>,
    capacity: wgpu::BufferAddress,
}

impl LineBuffer {
    /// Uploads `vertices` and returns the buffer and the byte length used.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &[LineVertex],
    ) -> (&wgpu::Buffer, wgpu::BufferAddress) {
        let needed = std::mem::size_of_val(vertices) as wgpu::BufferAddress;
        if needed > self.capacity {
            self.capacity = needed.max(self.capacity * 2);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("line vertex buffer"),
                size: self.capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buffer = self.buffer.as_ref().expect("allocated above");
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(vertices));
        (buffer, needed)
    }
}

pub struct SceneRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_format: wgpu::TextureFormat,
    mesh_pipeline: wgpu::RenderPipeline,
    tonemap_pipeline: wgpu::RenderPipeline,
    tonemap_bind_group_layout: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    model_bind_group_layout: wgpu::BindGroupLayout,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    light_buffer: wgpu::Buffer,
    /// Group 0: per-frame data shared by every pipeline (camera, light,
    /// shadow map).
    frame_bind_group: wgpu::BindGroup,
    shadow_pipeline: wgpu::RenderPipeline,
    /// Light view-proj for the shadow pass (group 0 of `shadow_pipeline`).
    shadow_camera_buffer: wgpu::Buffer,
    shadow_bind_group: wgpu::BindGroup,
    // Kept alive for `shadow_view`.
    _shadow_texture: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    // Kept to keep the texture alive for the lifetime of the renderer.
    #[allow(dead_code)]
    albedo_texture: wgpu::Texture,
    albedo_bind_group: wgpu::BindGroup,
    line_pipeline: wgpu::RenderPipeline,
    lines: LineBuffer,
    /// Depth-tested lines inside the 3D pass (grid, axes, selection box).
    world_line_pipeline: wgpu::RenderPipeline,
    world_lines: LineBuffer,
    model_uniform_buffer: wgpu::Buffer,
    model_bind_group: wgpu::BindGroup,
    model_uniform_stride: wgpu::BufferAddress,
    model_staging: Vec<u8>,
}

pub struct Renderer {
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    size: winit::dpi::PhysicalSize<u32>,
    window: Arc<Window>,
    scene: SceneRenderer,
}

impl std::ops::Deref for Renderer {
    type Target = SceneRenderer;

    fn deref(&self) -> &Self::Target {
        &self.scene
    }
}

impl std::ops::DerefMut for Renderer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.scene
    }
}

fn dynamic_uniform_buffer_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

impl Renderer {
    #[instrument(skip(window), level = "info")]
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .context("no suitable wgpu adapter found")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits {
                    max_uniform_buffer_binding_size: 256 * 1024, // 256 KB for model uniform buffer
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .context("failed to request wgpu device")?;

        let size = window.inner_size();
        let surface = instance
            .create_surface(window.clone())
            .context("failed to create wgpu surface from window")?;
        let surface_format = surface
            .get_capabilities(&adapter)
            .formats
            .first()
            .copied()
            .context("surface reported no usable formats")?;

        let scene = SceneRenderer::new(device, queue, surface_format);

        let renderer = Self {
            instance,
            surface,
            size,
            window,
            scene,
        };
        renderer.configure_surface();
        Ok(renderer)
    }

    pub fn scene(&self) -> &SceneRenderer {
        &self.scene
    }

    pub fn scene_mut(&mut self) -> &mut SceneRenderer {
        &mut self.scene
    }
}

impl SceneRenderer {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let uniform_entry = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        // Group 0 holds everything that changes once per frame, so group 3
        // stays free for GI data.
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame bind group layout"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                uniform_entry(1, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let model_layout = dynamic_uniform_buffer_layout(&device, "model bind group layout");
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow camera bind group layout"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });
        let shadow_pipeline =
            Renderer::build_shadow_pipeline(&device, &shadow_layout, &model_layout);
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let mesh_pipeline =
            Renderer::build_mesh_pipeline(&device, &frame_layout, &model_layout, &texture_layout);

        let tonemap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tonemap bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let tonemap_pipeline =
            Renderer::build_tonemap_pipeline(&device, surface_format, &tonemap_layout);

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera uniform buffer"),
            size: std::mem::size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light uniform buffer"),
            size: std::mem::size_of::<LightUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow map"),
            size: wgpu::Extent3d {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_view = shadow_texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Linear filtering on a comparison sampler gives 2x2 hardware PCF per tap.
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow comparison sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let shadow_camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow camera uniform buffer"),
            size: std::mem::size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow camera bind group"),
            layout: &shadow_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: shadow_camera_buffer.as_entire_binding(),
            }],
        });

        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame bind group"),
            layout: &frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: light_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
            ],
        });

        let (albedo_texture, albedo_bind_group) =
            Renderer::create_checkerboard_texture(&device, &queue, &texture_layout);

        let line_pipeline = Renderer::build_line_pipeline(&device, surface_format, &frame_layout);
        let world_line_pipeline = Renderer::build_world_line_pipeline(&device, &frame_layout);

        // Reusable model uniform buffer with dynamic offsets.
        // Stride = max(ModelUniform size, min_uniform_buffer_offset_alignment).
        // This avoids creating a new buffer+bind group per entity per frame (memory leak fix).
        let model_uniform_size = std::mem::size_of::<ModelUniform>() as wgpu::BufferAddress;
        let min_alignment =
            device.limits().min_uniform_buffer_offset_alignment as wgpu::BufferAddress;
        let model_uniform_stride = model_uniform_size.max(min_alignment);
        let model_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model uniform buffer"),
            size: model_uniform_stride * MAX_ENTITIES as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let model_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model bind group"),
            layout: &model_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &model_uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(model_uniform_size),
                }),
            }],
        });

        Self {
            device,
            queue,
            surface_format,
            mesh_pipeline,
            tonemap_pipeline,
            tonemap_bind_group_layout: tonemap_layout,
            model_bind_group_layout: model_layout,
            texture_bind_group_layout: texture_layout,
            camera_buffer,
            light_buffer,
            frame_bind_group,
            shadow_pipeline,
            shadow_camera_buffer,
            shadow_bind_group,
            _shadow_texture: shadow_texture,
            shadow_view,
            albedo_texture,
            albedo_bind_group,
            line_pipeline,
            lines: LineBuffer::default(),
            world_line_pipeline,
            world_lines: LineBuffer::default(),
            model_uniform_buffer,
            model_bind_group,
            model_uniform_stride,
            model_staging: Vec::new(),
        }
    }
}

impl Renderer {
    fn build_mesh_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
        model_layout: &wgpu::BindGroupLayout,
        texture_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/mesh.wgsl"));

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh pipeline layout"),
            bind_group_layouts: &[Some(frame_layout), Some(model_layout), Some(texture_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render mesh pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,  // position
                        1 => Float32x3,  // normal
                        2 => Float32x2,  // uv
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn build_tonemap_pipeline(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        tonemap_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader = device
            .create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/tonemap.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tonemap pipeline layout"),
            bind_group_layouts: &[Some(tonemap_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render tonemap pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format.add_srgb_suffix(),
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn build_shadow_pipeline(
        device: &wgpu::Device,
        shadow_layout: &wgpu::BindGroupLayout,
        model_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/shadow.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow pipeline layout"),
            bind_group_layouts: &[Some(shadow_layout), Some(model_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render shadow pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                }],
            },
            primitive: wgpu::PrimitiveState {
                // Back faces (from the sun) store depth, so lit front faces
                // sit in front of their own stored depth: less acne.
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        })
    }

    fn build_line_pipeline(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        frame_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/line.wgsl"));
        let target_format = surface_format.add_srgb_suffix();

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("line pipeline layout"),
            bind_group_layouts: &[Some(frame_layout)],
            immediate_size: 0,
        });

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render line pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                cull_mode: None,
                ..Default::default()
            },
            // Gizmo lines always draw on top, in the tonemap pass, which has
            // no depth attachment.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Line pipeline for the 3D pass: HDR target, multisampled, depth-tested
    /// but not depth-writing, so lines hide behind geometry.
    fn build_world_line_pipeline(
        device: &wgpu::Device,
        frame_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/line.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world line pipeline layout"),
            bind_group_layouts: &[Some(frame_layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("melon_render world line pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x3,
                        1 => Float32x3,
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn create_depth_target(
        device: &wgpu::Device,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth texture"),
            size: wgpu::Extent3d {
                width: size.width.max(1),
                height: size.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn create_checkerboard_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture_layout: &wgpu::BindGroupLayout,
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        const SIZE: u32 = 64;
        const TILE: u32 = 8;
        let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let checker = ((x / TILE) + (y / TILE)).is_multiple_of(2);
                if checker {
                    pixels.extend_from_slice(&[220, 60, 180, 255]); // melon-pink
                } else {
                    pixels.extend_from_slice(&[255, 255, 255, 255]); // white
                }
            }
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("albedo checkerboard"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 4),
                rows_per_image: Some(SIZE),
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("albedo sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("albedo bind group"),
            layout: texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        (texture, bind_group)
    }
}

impl SceneRenderer {
    #[instrument(skip(self), level = "debug")]
    pub fn cube_mesh(&self) -> Mesh {
        // 24 vertices (6 faces x 4), 36 indices. Each face has a normal + UVs.
        // UVs map so the checkerboard repeats once per face.
        let vertices: [Vertex; 24] = [
            // +X (normal +X)
            Vertex {
                position: [0.5, -0.5, -0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [0.5, 0.5, -0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                position: [0.5, 0.5, 0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                position: [0.5, -0.5, 0.5],
                normal: [1.0, 0.0, 0.0],
                uv: [1.0, 0.0],
            },
            // -X (normal -X)
            Vertex {
                position: [-0.5, -0.5, 0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [-0.5, 0.5, 0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                position: [-0.5, 0.5, -0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                position: [-0.5, -0.5, -0.5],
                normal: [-1.0, 0.0, 0.0],
                uv: [1.0, 0.0],
            },
            // +Y (normal +Y)
            Vertex {
                position: [0.5, 0.5, -0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [-0.5, 0.5, -0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                position: [-0.5, 0.5, 0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                position: [0.5, 0.5, 0.5],
                normal: [0.0, 1.0, 0.0],
                uv: [1.0, 0.0],
            },
            // -Y (normal -Y)
            Vertex {
                position: [0.5, -0.5, 0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [-0.5, -0.5, 0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                position: [-0.5, -0.5, -0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                position: [0.5, -0.5, -0.5],
                normal: [0.0, -1.0, 0.0],
                uv: [1.0, 0.0],
            },
            // +Z (normal +Z)
            Vertex {
                position: [0.5, -0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [0.5, 0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                position: [-0.5, 0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                position: [-0.5, -0.5, 0.5],
                normal: [0.0, 0.0, 1.0],
                uv: [1.0, 0.0],
            },
            // -Z (normal -Z)
            Vertex {
                position: [-0.5, -0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                position: [-0.5, 0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                position: [0.5, 0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                position: [0.5, -0.5, -0.5],
                normal: [0.0, 0.0, -1.0],
                uv: [1.0, 0.0],
            },
        ];
        let indices: [u16; 36] = [
            0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16,
            17, 18, 16, 18, 19, 20, 21, 22, 20, 22, 23,
        ];

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("cube vertex buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("cube index buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        Mesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            index_format: wgpu::IndexFormat::Uint16,
        }
    }

    /// Write all model uniforms to the shared buffer in one call.
    /// Each matrix is written at stride `model_uniform_stride` (aligned to device's
    /// min_uniform_buffer_offset_alignment). Use `model_uniform_offset(index)` to
    /// get the dynamic offset for `set_bind_group`.
    pub fn update_model_uniforms(&mut self, matrices: &[glam::Mat4]) {
        let stride = self.model_uniform_stride as usize;
        self.model_staging.clear();
        self.model_staging.reserve(matrices.len() * stride);
        for mat in matrices {
            let normal = mat.inverse().transpose();
            let uniform = ModelUniform {
                model: mat.to_cols_array_2d(),
                normal_matrix: normal.to_cols_array_2d(),
            };
            let uniform_bytes: [u8; std::mem::size_of::<ModelUniform>()] = bytemuck::cast(uniform);
            self.model_staging.extend_from_slice(&uniform_bytes);
            // Pad to stride
            let pad = stride - uniform_bytes.len();
            self.model_staging.extend(std::iter::repeat_n(0u8, pad));
        }
        self.queue
            .write_buffer(&self.model_uniform_buffer, 0, &self.model_staging);
    }

    /// Get the dynamic offset for entity at `index` in the model uniform buffer.
    pub fn model_uniform_offset(&self, index: usize) -> u32 {
        (index as wgpu::BufferAddress * self.model_uniform_stride) as u32
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    pub fn create_viewport_target(&self, size: winit::dpi::PhysicalSize<u32>) -> ViewportTarget {
        ViewportTarget::new(
            &self.device,
            self.surface_format.add_srgb_suffix(),
            size,
            self.create_hdr_target(size),
        )
    }

    /// Creates an HDR scene colour target. Zero sizes clamp to 1x1.
    pub fn create_hdr_target(&self, size: winit::dpi::PhysicalSize<u32>) -> HdrTarget {
        let size = winit::dpi::PhysicalSize::new(size.width.max(1), size.height.max(1));
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr scene color texture"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let msaa_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr scene msaa texture"),
            size: texture.size(),
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let msaa_view = msaa_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tonemap bind group"),
            layout: &self.tonemap_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        HdrTarget {
            msaa_texture,
            msaa_view,
            _texture: texture,
            view,
            bind_group,
            size,
        }
    }

    /// Begins a pass on `target` that tonemaps `hdr` over it. The pass stays
    /// open so callers can draw overlays (gizmo lines) on the display image.
    pub fn tonemap_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        hdr: &HdrTarget,
        target: &wgpu::TextureView,
    ) -> wgpu::RenderPass<'e> {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("melon_render tonemap pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The fullscreen triangle overwrites every pixel.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.tonemap_pipeline);
        pass.set_bind_group(0, &hdr.bind_group, &[]);
        pass.draw(0..3, 0..1);
        pass
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
}

impl Renderer {
    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }

    #[instrument(skip(self), level = "debug")]
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.configure_surface();
    }

    fn configure_surface(&self) {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.surface_format,
            view_formats: vec![self.surface_format.add_srgb_suffix()],
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            width: self.size.width,
            height: self.size.height,
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::AutoVsync,
        };
        self.surface.configure(&self.device, &config);
    }
}

impl SceneRenderer {
    pub fn update_camera(&self, view_proj: glam::Mat4) {
        let uniform = CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[uniform]));
    }

    /// `to_light` points from the scene toward the light (world space, normalized).
    /// `light_view_proj` is the shadow box, see [`directional_light_view_proj`].
    pub fn update_light(
        &self,
        to_light: glam::Vec3,
        ambient: f32,
        color: glam::Vec3,
        light_view_proj: glam::Mat4,
    ) {
        let uniform = LightUniform {
            direction: to_light.to_array(),
            ambient,
            color: color.to_array(),
            _pad: 0.0,
            light_view_proj: light_view_proj.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.light_buffer, 0, bytemuck::cast_slice(&[uniform]));
        let shadow_camera = CameraUniform {
            view_proj: light_view_proj.to_cols_array_2d(),
        };
        self.queue.write_buffer(
            &self.shadow_camera_buffer,
            0,
            bytemuck::cast_slice(&[shadow_camera]),
        );
    }

    /// Renders every mesh into the sun's shadow map. Call before the 3D pass,
    /// after `update_light` and `update_model_uniforms`.
    pub fn render_shadow_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        draw_calls: &[(usize, &Mesh)],
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("melon_render shadow pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.shadow_view,
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
        pass.set_pipeline(&self.shadow_pipeline);
        pass.set_bind_group(0, &self.shadow_bind_group, &[]);
        for (entity_index, mesh) in draw_calls {
            let offset = self.model_uniform_offset(*entity_index);
            pass.set_bind_group(1, &self.model_bind_group, &[offset]);
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }

    /// Upload an arbitrary image to a wgpu texture. The caller picks the
    /// format; the bytes must match (`width * height * bytes_per_pixel`).
    #[instrument(skip(self), level = "debug")]
    pub fn create_texture_from_bytes(
        &self,
        pixels: &[u8],
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("loaded texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * format.block_copy_size(None).unwrap()),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    /// Convert an `image::DynamicImage` to RGBA8 and upload it as a texture.
    /// Returns the texture; caller creates the view + bind group via
    /// `create_texture_bind_group`.
    #[instrument(skip(self), level = "debug")]
    pub fn create_texture_from_image(&self, img: &image::DynamicImage) -> wgpu::Texture {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        self.create_texture_from_bytes(&rgba, width, height, wgpu::TextureFormat::Rgba8UnormSrgb)
    }

    /// Create a texture view + sampler + bind group for the given texture,
    /// using the renderer's existing texture bind group layout.
    #[instrument(skip(self), level = "debug")]
    pub fn create_texture_bind_group(&self, texture: &wgpu::Texture) -> wgpu::BindGroup {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("loaded texture sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("loaded texture bind group"),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        })
    }

    /// Access the texture bind group layout (needed by callers that want to
    /// create their own bind groups against the same layout).
    pub fn texture_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.texture_bind_group_layout
    }

    /// Load a glTF / GLB file. Every primitive reachable from the default
    /// scene's node tree becomes one `Mesh`, with its node's world transform
    /// baked into the vertices, plus its base colour texture (or `None`).
    #[instrument(skip(self), level = "info")]
    pub fn load_gltf(&self, path: &str) -> Result<LoadedModel> {
        let (document, buffers, images) =
            gltf::import(path).with_context(|| format!("failed to import glTF file: {path}"))?;
        let model = self.build_model(&document, &buffers, &images)?;
        tracing::info!("loaded glTF {}: {} part(s)", path, model.meshes.len());
        Ok(model)
    }

    /// Load a glTF / GLB from memory (embedded or data-URI buffers only).
    pub fn load_gltf_bytes(&self, bytes: &[u8]) -> Result<LoadedModel> {
        let (document, buffers, images) =
            gltf::import_slice(bytes).context("failed to import glTF data")?;
        self.build_model(&document, &buffers, &images)
    }

    /// The procedural unit cube with the checkerboard (`builtin:cube`).
    pub fn builtin_cube_model(&self) -> LoadedModel {
        LoadedModel {
            meshes: vec![self.cube_mesh()],
            albedo_bind_groups: vec![None],
            bounds: (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5)),
        }
    }

    fn build_model(
        &self,
        document: &gltf::Document,
        buffers: &[gltf::buffer::Data],
        images: &[gltf::image::Data],
    ) -> Result<LoadedModel> {
        let mut model = LoadedModel {
            meshes: Vec::new(),
            albedo_bind_groups: Vec::new(),
            bounds: (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN)),
        };
        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next())
            .context("glTF file has no scene")?;
        for node in scene.nodes() {
            self.add_node(&node, glam::Mat4::IDENTITY, buffers, images, &mut model)?;
        }
        if model.meshes.is_empty() {
            anyhow::bail!("glTF scene contains no meshes");
        }
        Ok(model)
    }

    fn add_node(
        &self,
        node: &gltf::Node,
        parent: glam::Mat4,
        buffers: &[gltf::buffer::Data],
        images: &[gltf::image::Data],
        model: &mut LoadedModel,
    ) -> Result<()> {
        let world = parent * glam::Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                self.add_primitive(&primitive, world, buffers, images, model)?;
            }
        }
        for child in node.children() {
            self.add_node(&child, world, buffers, images, model)?;
        }
        Ok(())
    }

    fn add_primitive(
        &self,
        primitive: &gltf::Primitive,
        world: glam::Mat4,
        buffers: &[gltf::buffer::Data],
        images: &[gltf::image::Data],
        model: &mut LoadedModel,
    ) -> Result<()> {
        let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|d| d.as_ref()));

        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .map(|it| it.collect())
            .unwrap_or_default();
        if positions.is_empty() {
            anyhow::bail!("primitive {} has no positions", primitive.index());
        }
        let normals: Vec<[f32; 3]> = reader
            .read_normals()
            .map(|it| it.collect())
            .unwrap_or_else(|| vec![[0.0, 0.0, 1.0]; positions.len()]);
        let uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .map(|it| it.into_f32().collect())
            .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);
        if positions.len() != normals.len() || positions.len() != uvs.len() {
            anyhow::bail!(
                "primitive {} attribute length mismatch: {} positions, {} normals, {} uvs",
                primitive.index(),
                positions.len(),
                normals.len(),
                uvs.len()
            );
        }

        // Bake the node transform; normals use the inverse transpose.
        let normal_matrix = world.inverse().transpose();
        let vertices: Vec<Vertex> = positions
            .iter()
            .zip(&normals)
            .zip(&uvs)
            .map(|((&pos, &norm), &uv)| {
                let p = world.transform_point3(glam::Vec3::from_array(pos));
                model.bounds.0 = model.bounds.0.min(p);
                model.bounds.1 = model.bounds.1.max(p);
                let n = normal_matrix
                    .transform_vector3(glam::Vec3::from_array(norm))
                    .normalize_or_zero();
                Vertex {
                    position: p.to_array(),
                    normal: n.to_array(),
                    uv,
                }
            })
            .collect();

        let mut indices: Vec<u32> = match reader.read_indices() {
            Some(indices) => indices.into_u32().collect(),
            None => (0..positions.len() as u32).collect(),
        };
        // A mirroring transform flips triangle winding; flip it back so back-face
        // culling keeps the outside.
        if world.determinant() < 0.0 {
            for tri in indices.chunks_exact_mut(3) {
                tri.swap(1, 2);
            }
        }

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gltf vertex buffer"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gltf index buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        model.meshes.push(Mesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            index_format: wgpu::IndexFormat::Uint32,
        });

        let albedo = primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_texture()
            .and_then(|info| info.texture().source())
            .and_then(|image| images.get(image.index()))
            .map(|img_data| {
                let texture = self.create_texture_from_image(&image_from_gltf_data(img_data));
                self.create_texture_bind_group(&texture)
            });
        model.albedo_bind_groups.push(albedo);
        Ok(())
    }
}

impl Renderer {
    /// Acquire the surface texture and begin a new frame.
    /// Returns `None` if the surface is lost/outdated/occluded.
    pub fn begin_frame(&mut self) -> Option<Frame> {
        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                drop(t);
                self.configure_surface();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.configure_surface();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .expect("failed to recreate lost surface");
                self.configure_surface();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return None;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                tracing::error!("wgpu surface validation error during get_current_texture");
                return None;
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.surface_format.add_srgb_suffix()),
                ..Default::default()
            });

        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        Some(Frame {
            surface_texture,
            view,
            encoder,
        })
    }
}

impl SceneRenderer {
    /// Draw 3D meshes into an existing render pass.
    /// Each draw call is `(entity_index, mesh)` where `entity_index` is the index
    /// into the model uniform buffer (see `update_model_uniforms`).
    pub fn draw_meshes(&self, pass: &mut wgpu::RenderPass<'_>, draw_calls: &[(usize, &Mesh)]) {
        pass.set_pipeline(&self.mesh_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_bind_group(2, &self.albedo_bind_group, &[]);
        for (entity_index, mesh) in draw_calls {
            let offset = self.model_uniform_offset(*entity_index);
            pass.set_bind_group(1, &self.model_bind_group, &[offset]);
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }

    /// Draw 3D meshes with per-draw-call albedo textures.
    /// Each draw call is `(entity_index, mesh, albedo_bind_group)`.
    pub fn draw_meshes_textured(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        draw_calls: &[(usize, &Mesh, Option<&wgpu::BindGroup>)],
    ) {
        pass.set_pipeline(&self.mesh_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        for (entity_index, mesh, albedo) in draw_calls {
            let offset = self.model_uniform_offset(*entity_index);
            pass.set_bind_group(1, &self.model_bind_group, &[offset]);
            match albedo {
                Some(bg) => pass.set_bind_group(2, *bg, &[]),
                None => pass.set_bind_group(2, &self.albedo_bind_group, &[]),
            }
            pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }

    /// Draw colored line segments for gizmo/debug visualization.
    /// Uses a reusable vertex buffer with 2x growth — no per-frame allocation.
    /// `vertices` must be in pairs (LineList topology).
    /// The camera uniform is updated with `view_proj` before drawing.
    pub fn draw_lines(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        vertices: &[LineVertex],
        view_proj: glam::Mat4,
    ) {
        if vertices.is_empty() {
            return;
        }
        self.write_line_camera(view_proj);
        let (buffer, len) = self.lines.upload(&self.device, &self.queue, vertices);
        pass.set_pipeline(&self.line_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..len));
        pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Depth-tested lines in the 3D (HDR, multisampled) pass: grid, axes,
    /// selection box. Hidden behind geometry, unlike `draw_lines`.
    pub fn draw_world_lines(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        vertices: &[LineVertex],
        view_proj: glam::Mat4,
    ) {
        if vertices.is_empty() {
            return;
        }
        self.write_line_camera(view_proj);
        let (buffer, len) = self.world_lines.upload(&self.device, &self.queue, vertices);
        pass.set_pipeline(&self.world_line_pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..len));
        pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Writes the shared frame camera buffer. Every write lands before the
    /// frame is submitted, so all passes see the last value: callers must pass
    /// this frame's camera `view_proj` (they all do), never a special one.
    fn write_line_camera(&self, view_proj: glam::Mat4) {
        let uniform = CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[uniform]));
    }
}

impl Renderer {
    /// Submit the frame's command encoder and present the surface texture.
    /// Calls `device.poll` after submit to force wgpu to GC completed buffers
    /// (workaround for wgpu #5397 — short-lived buffers leak without poll).
    pub fn end_frame(&self, frame: Frame) {
        self.queue.submit([frame.encoder.finish()]);
        self.device.poll(wgpu::PollType::Poll).ok();
        self.window.pre_present_notify();
        frame.surface_texture.present();
    }
}

pub struct ViewportTarget {
    color_texture: Arc<wgpu::Texture>,
    color_view: Arc<wgpu::TextureView>,
    ui_view: wgpu::TextureView,
    ui_view_format: wgpu::TextureFormat,
    _depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    hdr_target: HdrTarget,
    size: winit::dpi::PhysicalSize<u32>,
}

impl ViewportTarget {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: winit::dpi::PhysicalSize<u32>,
        hdr_target: HdrTarget,
    ) -> Self {
        let size = winit::dpi::PhysicalSize::new(size.width.max(1), size.height.max(1));
        let color_texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("editor viewport color texture"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[format.remove_srgb_suffix()],
        }));
        let color_view =
            Arc::new(color_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let ui_view_format = format.remove_srgb_suffix();
        let ui_view = color_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("editor viewport ui view"),
            format: Some(ui_view_format),
            ..Default::default()
        });
        let (depth_texture, depth_view) = Renderer::create_depth_target(device, size);

        Self {
            color_texture,
            color_view,
            ui_view,
            ui_view_format,
            _depth_texture: depth_texture,
            depth_view,
            hdr_target,
            size,
        }
    }

    /// Scene colour target the 3D pass renders into before tonemapping.
    pub fn hdr_target(&self) -> &HdrTarget {
        &self.hdr_target
    }

    pub fn color_texture(&self) -> Arc<wgpu::Texture> {
        self.color_texture.clone()
    }

    pub fn color_texture_handle(&self) -> wgpu::Texture {
        self.color_texture.as_ref().clone()
    }

    pub fn color_view(&self) -> Arc<wgpu::TextureView> {
        self.color_view.clone()
    }

    /// Non-sRGB view of the colour texture for UI libraries that treat texture
    /// samples as gamma-encoded (egui). Sampling the sRGB view would decode
    /// the image twice and darken it.
    pub fn ui_view(&self) -> &wgpu::TextureView {
        &self.ui_view
    }

    pub fn ui_view_format(&self) -> wgpu::TextureFormat {
        self.ui_view_format
    }

    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }

    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }
}

/// A frame in progress: holds the surface texture, its view, and a command encoder.
/// The host can insert additional render passes (e.g. for egui) between
/// `begin_frame` and `end_frame`.
pub struct Frame {
    surface_texture: wgpu::SurfaceTexture,
    pub view: wgpu::TextureView,
    pub encoder: wgpu::CommandEncoder,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId};

    #[test]
    fn vertex_size_is_32_bytes() {
        assert_eq!(std::mem::size_of::<Vertex>(), 32);
    }

    #[test]
    fn line_vertex_size_is_24_bytes() {
        assert_eq!(std::mem::size_of::<LineVertex>(), 24);
    }

    #[test]
    fn line_vertex_field_offsets() {
        let v = LineVertex {
            position: [1.0, 2.0, 3.0],
            color: [4.0, 5.0, 6.0],
        };
        let base = &v as *const _ as usize;
        let pos = &v.position as *const _ as usize - base;
        let col = &v.color as *const _ as usize - base;
        assert_eq!(pos, 0, "position should be at offset 0");
        assert_eq!(col, 12, "color should be at offset 12");
    }

    #[test]
    fn vertex_field_offsets() {
        let v = Vertex {
            position: [1.0, 2.0, 3.0],
            normal: [4.0, 5.0, 6.0],
            uv: [7.0, 8.0],
        };
        let base = &v as *const _ as usize;
        let pos = &v.position as *const _ as usize - base;
        let norm = &v.normal as *const _ as usize - base;
        let uv = &v.uv as *const _ as usize - base;
        assert_eq!(pos, 0, "position should be at offset 0");
        assert_eq!(norm, 12, "normal should be at offset 12");
        assert_eq!(uv, 24, "uv should be at offset 24");
    }

    #[test]
    fn camera_uniform_size_is_64() {
        assert_eq!(std::mem::size_of::<CameraUniform>(), 64);
    }

    #[test]
    fn model_uniform_size_is_128() {
        assert_eq!(std::mem::size_of::<ModelUniform>(), 128);
    }

    #[test]
    fn light_uniform_size_is_96() {
        assert_eq!(std::mem::size_of::<LightUniform>(), 96);
    }

    fn clip(m: glam::Mat4, p: glam::Vec3) -> glam::Vec3 {
        let c = m * p.extend(1.0);
        c.truncate() / c.w
    }

    #[test]
    fn light_view_proj_center_maps_to_middle() {
        let to_light = glam::Vec3::new(1.0, 1.0, 0.0).normalize();
        let c = clip(
            directional_light_view_proj(to_light, glam::Vec3::new(3.0, 0.0, -2.0), 20.0),
            glam::Vec3::new(3.0, 0.0, -2.0),
        );
        assert!(c.x.abs() < 1e-4 && c.y.abs() < 1e-4, "xy = {c:?}");
        assert!(c.z > 0.0 && c.z < 1.0, "z = {}", c.z);
    }

    #[test]
    fn light_view_proj_nearer_point_has_smaller_depth() {
        let to_light = glam::Vec3::new(1.0, 1.0, 0.0).normalize();
        let m = directional_light_view_proj(to_light, glam::Vec3::ZERO, 20.0);
        assert!(clip(m, to_light).z < clip(m, -to_light).z);
    }

    #[test]
    fn light_view_proj_vertical_light_is_finite() {
        let m = directional_light_view_proj(glam::Vec3::Y, glam::Vec3::ZERO, 20.0);
        assert!(m.to_cols_array().iter().all(|v| v.is_finite()), "{m:?}");
    }

    // --- Shared GPU test scene ---
    // Floor + small occluder seen from straight above, sun at 45 degrees from +X.
    // White light on white albedo, so every colour channel holds the same value.

    const TEST_SIZE: u32 = 64;
    /// World (-2, 0, 0): on the floor, inside the occluder's shadow.
    const SHADOW_PX: (u32, u32) = (16, 32);
    /// World (2, 0, 0): on the floor, in the open.
    const LIT_PX: (u32, u32) = (48, 32);
    /// World (-3.44, 0, 0): on the floor, in the open, left of the shadow.
    const FAR_LEFT_PX: (u32, u32) = (4, 32);

    /// Renders the test scene and returns TEST_SIZE x TEST_SIZE RGBA8 sRGB pixels.
    /// The shadow box has half-size `shadow_radius` around `shadow_center`.
    fn render_test_scene(
        r: &mut Renderer,
        shadow_center: glam::Vec3,
        shadow_radius: f32,
    ) -> Vec<u8> {
        use glam::camera::rh::{proj::directx, view};
        use glam::{Mat4, Quat, Vec3};

        let device = r.device().clone();
        let size = winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE);
        let color = test_color_texture(&device, r.surface_format().add_srgb_suffix());
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let (_depth, depth_view) = Renderer::create_depth_target(&device, size);

        r.update_camera(
            directx::orthographic(-4.0, 4.0, -4.0, 4.0, 0.1, 20.0)
                * view::look_at_mat4(Vec3::new(0.0, 10.0, 0.0), Vec3::ZERO, Vec3::NEG_Z),
        );
        let to_light = Vec3::new(1.0, 1.0, 0.0).normalize();
        r.update_light(
            to_light,
            0.2,
            Vec3::ONE,
            directional_light_view_proj(to_light, shadow_center, shadow_radius),
        );
        r.update_model_uniforms(&[
            Mat4::from_scale_rotation_translation(
                Vec3::new(8.0, 0.2, 8.0),
                Quat::IDENTITY,
                Vec3::new(0.0, -0.1, 0.0),
            ),
            Mat4::from_scale_rotation_translation(
                Vec3::splat(0.5),
                Quat::IDENTITY,
                Vec3::new(0.0, 2.0, 0.0),
            ),
        ]);
        let white =
            r.create_texture_from_bytes(&[255; 4], 1, 1, wgpu::TextureFormat::Rgba8UnormSrgb);
        let white_bg = r.create_texture_bind_group(&white);
        let cube = r.cube_mesh();

        let hdr = r.create_hdr_target(size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        r.render_shadow_pass(&mut encoder, &[(0, &cube), (1, &cube)]);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: hdr.msaa_view(),
                    depth_slice: None,
                    resolve_target: Some(hdr.view()),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
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
            r.draw_meshes_textured(
                &mut pass,
                &[(0, &cube, Some(&white_bg)), (1, &cube, Some(&white_bg))],
            );
        }
        r.tonemap_pass(&mut encoder, &hdr, &color_view);
        read_back(&device, r.queue(), encoder, &color)
    }

    /// Clears an HDR target to `value` (all channels), tonemaps it to sRGB8
    /// and returns the centre pixel.
    fn tonemap_value(r: &Renderer, value: f64) -> u8 {
        let device = r.device();
        let hdr = r.create_hdr_target(winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE));
        let color = test_color_texture(device, r.surface_format().add_srgb_suffix());
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test hdr clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: hdr.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: value,
                        g: value,
                        b: value,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        r.tonemap_pass(&mut encoder, &hdr, &color_view);
        let pixels = read_back(device, r.queue(), encoder, &color);
        value_at(&pixels, (TEST_SIZE / 2, TEST_SIZE / 2))
    }

    fn test_color_texture(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test color texture"),
            size: wgpu::Extent3d {
                width: TEST_SIZE,
                height: TEST_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Submits `encoder` with a copy of `texture` appended and returns its
    /// tightly packed RGBA8 rows.
    fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mut encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) -> Vec<u8> {
        let row = TEST_SIZE * 4;
        let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test readback buffer"),
            size: (padded * TEST_SIZE) as wgpu::BufferAddress,
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
        buffer.map_async(wgpu::MapMode::Read, .., |res| {
            res.expect("readback map failed")
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("readback poll failed");
        let data = buffer.get_mapped_range(..);
        data.chunks(padded as usize)
            .flat_map(|r| &r[..row as usize])
            .copied()
            .collect()
    }

    /// First channel at pixel `(x, y)`; all channels match in the test scene.
    /// Draws one white world line along X through a row centre, with the
    /// depth buffer cleared to `depth_clear`, and returns the pixels.
    fn render_world_line(r: &mut Renderer, depth_clear: f32) -> Vec<u8> {
        use glam::Vec3;
        use glam::camera::rh::{proj::directx, view};

        let device = r.device().clone();
        let size = winit::dpi::PhysicalSize::new(TEST_SIZE, TEST_SIZE);
        let color = test_color_texture(&device, r.surface_format().add_srgb_suffix());
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let (_depth, depth_view) = Renderer::create_depth_target(&device, size);
        let view_proj = directx::orthographic(-4.0, 4.0, -4.0, 4.0, 0.1, 20.0)
            * view::look_at_mat4(Vec3::new(0.0, 10.0, 0.0), Vec3::ZERO, Vec3::NEG_Z);
        let line = [
            LineVertex {
                position: [-4.0, 0.0, 0.0625],
                color: [1.0, 1.0, 1.0],
            },
            LineVertex {
                position: [4.0, 0.0, 0.0625],
                color: [1.0, 1.0, 1.0],
            },
        ];
        let hdr = r.create_hdr_target(size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world line test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: hdr.msaa_view(),
                    depth_slice: None,
                    resolve_target: Some(hdr.view()),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(depth_clear),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            r.draw_world_lines(&mut pass, &line, view_proj);
        }
        r.tonemap_pass(&mut encoder, &hdr, &color_view);
        read_back(&device, r.queue(), encoder, &color)
    }

    /// One triangle (0,0,0)-(1,0,0)-(0,1,0) used by two nodes at x = -2 and x = +3.
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

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_models_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets/models")).unwrap();
        dir
    }

    fn value_at(pixels: &[u8], (x, y): (u32, u32)) -> u8 {
        pixels[((y * TEST_SIZE + x) * 4) as usize]
    }

    // --- GPU tests via headless event loop harness ---
    // winit allows only ONE EventLoop per process, so all GPU assertions run
    // inside a single test that creates one loop + one renderer.

    type GpuAssertion = Box<dyn FnOnce(&mut Renderer) -> Result<(), String> + Send>;

    struct GpuTestRunner {
        assertions: Vec<GpuAssertion>,
        results: Vec<Result<(), String>>,
        window: Option<Arc<Window>>,
        renderer: Option<Renderer>,
        done: bool,
    }

    impl GpuTestRunner {
        fn new(assertions: Vec<GpuAssertion>) -> Self {
            Self {
                assertions,
                results: Vec::new(),
                window: None,
                renderer: None,
                done: false,
            }
        }
    }

    impl ApplicationHandler for GpuTestRunner {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("melon_render test")
                            .with_visible(false),
                    )
                    .expect("failed to create test window"),
            );
            let renderer = match pollster::block_on(Renderer::new(window.clone())) {
                Ok(r) => r,
                Err(e) => {
                    self.results
                        .push(Err(format!("renderer init failed: {e:?}")));
                    self.done = true;
                    event_loop.exit();
                    return;
                }
            };
            self.window = Some(window);
            self.renderer = Some(renderer);
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            if let WindowEvent::CloseRequested = event {
                event_loop.exit();
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.done {
                event_loop.exit();
                return;
            }
            if let Some(mut renderer) = self.renderer.take() {
                while let Some(assertion) = self.assertions.pop() {
                    self.results.push(assertion(&mut renderer));
                }
                self.done = true;
            }
            event_loop.exit();
        }
    }

    #[test]
    fn gpu_tests() {
        let assertions: Vec<GpuAssertion> = vec![
            Box::new(|renderer| {
                let model = renderer
                    .load_gltf_bytes(TWO_NODE_GLTF.as_bytes())
                    .map_err(|e| format!("gltf_two_nodes_load_whole: {e:?}"))?;
                let (min, max) = model.bounds;
                if model.meshes.len() == 2 && min.x <= -2.0 && max.x >= 3.0 {
                    Ok(())
                } else {
                    Err(format!(
                        "gltf_two_nodes_load_whole: {} meshes, bounds {min} .. {max}",
                        model.meshes.len()
                    ))
                }
            }),
            Box::new(|renderer| {
                use melon_core::{AssetServer, Handle, Model};
                let root = temp_project("missing");
                let mut server = AssetServer::new(root.clone());
                let h: Handle<Model> = server.load("assets/models/nope.glb");
                let mut cache = ModelCache::new(renderer.scene());
                let model = cache.get(renderer.scene(), &mut server, h);
                let ok = model.meshes.len() == 1
                    && model.bounds == (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5));
                let _ = std::fs::remove_dir_all(&root);
                if ok {
                    Ok(())
                } else {
                    Err("cache_missing_file_gives_placeholder".into())
                }
            }),
            Box::new(|renderer| {
                use melon_core::{AssetServer, Handle, Model};
                let root = temp_project("reload");
                let file = root.join("assets/models/m.gltf");
                std::fs::write(&file, TWO_NODE_GLTF).unwrap();
                let mut server = AssetServer::new(root.clone());
                let h: Handle<Model> = server.load("assets/models/m.gltf");
                server.poll_changes();
                let mut cache = ModelCache::new(renderer.scene());
                let first = cache.get(renderer.scene(), &mut server, h).meshes.len();
                std::fs::write(&file, "garbage").unwrap();
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&file)
                    .unwrap()
                    .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
                    .unwrap();
                server.poll_changes();
                let version = server.version(h);
                let after = cache.get(renderer.scene(), &mut server, h).meshes.len();
                let _ = std::fs::remove_dir_all(&root);
                if first == 2 && version == 1 && after == 2 {
                    Ok(())
                } else {
                    Err(format!(
                        "cache_keeps_previous_on_broken_reload: first {first}, version {version}, after {after}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let target = renderer.create_viewport_target(winit::dpi::PhysicalSize::new(8, 8));
                if target.ui_view_format().is_srgb() {
                    Err("viewport_ui_view_is_not_srgb: egui would decode it twice".into())
                } else {
                    Ok(())
                }
            }),
            Box::new(|renderer| {
                // Line at depth ~0.5: visible over a far-cleared depth buffer,
                // hidden behind a near-cleared one (depth test on).
                let brightest = |px: &[u8]| (31..=33).map(|y| value_at(px, (48, y))).max();
                let shown = brightest(&render_world_line(renderer, 1.0)).unwrap_or(0);
                let hidden = brightest(&render_world_line(renderer, 0.0)).unwrap_or(0);
                if shown > 150 && hidden < 30 {
                    Ok(())
                } else {
                    Err(format!(
                        "world_lines_depth_tested: shown={shown} (want > 150), hidden={hidden} (want < 30)"
                    ))
                }
            }),
            Box::new(|renderer| {
                let hdr = renderer.create_hdr_target(winit::dpi::PhysicalSize::new(8, 8));
                if hdr.sample_count() == MSAA_SAMPLES {
                    Ok(())
                } else {
                    Err(format!(
                        "hdr_target_is_multisampled: {} samples (want {MSAA_SAMPLES})",
                        hdr.sample_count()
                    ))
                }
            }),
            Box::new(|renderer| {
                let pixels = render_test_scene(renderer, glam::Vec3::ZERO, 20.0);
                let (shadow, lit) = (value_at(&pixels, SHADOW_PX), value_at(&pixels, LIT_PX));
                if shadow < 170 && lit > 200 {
                    Ok(())
                } else {
                    Err(format!(
                        "occluder_casts_shadow: SHADOW_PX={shadow} (want < 170), LIT_PX={lit} (want > 200)"
                    ))
                }
            }),
            Box::new(|renderer| {
                // The box edge nearest FAR_LEFT_PX runs through the occluder,
                // and FAR_LEFT_PX sits within the box's depth range but past
                // that edge. Without the uv-range check, clamp-to-edge would
                // read the occluder's depth there and shadow it.
                let pixels = render_test_scene(renderer, glam::Vec3::new(0.12, 0.0, 0.0), 1.5);
                let v = value_at(&pixels, FAR_LEFT_PX);
                if v > 200 {
                    Ok(())
                } else {
                    Err(format!(
                        "outside_shadow_box_is_lit: FAR_LEFT_PX={v}, want > 200"
                    ))
                }
            }),
            Box::new(|renderer| {
                let size = renderer
                    .create_hdr_target(winit::dpi::PhysicalSize::new(0, 0))
                    .size();
                if size == winit::dpi::PhysicalSize::new(1, 1) {
                    Ok(())
                } else {
                    Err(format!("hdr_target_clamps_zero_size: got {size:?}"))
                }
            }),
            Box::new(|renderer| {
                // Untonemapped, HDR 1.0 would read 255; a curve compresses it
                // and still keeps brighter values brighter.
                let one = tonemap_value(renderer, 1.0);
                let eight = tonemap_value(renderer, 8.0);
                if one < 250 && eight > one {
                    Ok(())
                } else {
                    Err(format!(
                        "tonemap_compresses_highlights: px(1.0)={one}, px(8.0)={eight}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let pixels = render_test_scene(renderer, glam::Vec3::ZERO, 20.0);
                let v = value_at(&pixels, LIT_PX);
                if v > 200 {
                    Ok(())
                } else {
                    Err(format!(
                        "lit_floor_is_bright: expected > 200 at LIT_PX, got {v}"
                    ))
                }
            }),
            Box::new(|renderer| {
                let mesh = renderer.cube_mesh();
                if mesh.index_count == 36 {
                    Ok(())
                } else {
                    Err(format!("expected 36 indices, got {}", mesh.index_count))
                }
            }),
            Box::new(|renderer| {
                let _ = renderer.surface_format();
                Ok(())
            }),
            Box::new(|renderer| {
                let _ = renderer.size();
                Ok(())
            }),
            Box::new(|renderer| {
                // Create a 2x2 RGBA8 texture from raw bytes and verify it can
                // be used to build a bind group (proves the texture + layout
                // path works end-to-end without panicking).
                let pixels: [u8; 16] = [255; 16];
                let texture = renderer.create_texture_from_bytes(
                    &pixels,
                    2,
                    2,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                );
                let _bg = renderer.create_texture_bind_group(&texture);
                Ok(())
            }),
            Box::new(|renderer| {
                // Load the sample GLB and verify it has 1 mesh with 36 indices
                // and an albedo bind group.
                let glb_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/models/cube.glb");
                let model = renderer
                    .load_gltf(glb_path)
                    .map_err(|e| format!("load_gltf failed: {e:?}"))?;
                if model.meshes.len() != 1 {
                    return Err(format!("expected 1 mesh, got {}", model.meshes.len()));
                }
                if model.meshes[0].index_count != 36 {
                    return Err(format!(
                        "expected 36 indices, got {}",
                        model.meshes[0].index_count
                    ));
                }
                if model.albedo_bind_groups.len() != 1 {
                    return Err(format!(
                        "expected 1 albedo bind group slot, got {}",
                        model.albedo_bind_groups.len()
                    ));
                }
                if model.albedo_bind_groups[0].is_none() {
                    return Err("expected albedo bind group to be Some, got None".to_string());
                }
                Ok(())
            }),
        ];

        #[cfg(target_os = "windows")]
        let event_loop = {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            let mut builder = EventLoop::builder();
            builder.with_any_thread(true);
            builder.build().expect("failed to create event loop")
        };
        #[cfg(not(target_os = "windows"))]
        let event_loop = EventLoop::new().expect("failed to create event loop");

        event_loop.set_control_flow(ControlFlow::Poll);
        let mut runner = GpuTestRunner::new(assertions);
        let _ = event_loop.run_app(&mut runner);

        for result in &runner.results {
            match result {
                Ok(()) => {}
                Err(msg) => panic!("{msg}"),
            }
        }
        assert!(
            !runner.results.is_empty(),
            "GPU tests did not run (renderer init may have failed)"
        );
    }
}
