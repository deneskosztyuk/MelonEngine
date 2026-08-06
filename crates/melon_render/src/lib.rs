use std::sync::Arc;

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
}

fn index_format_size(format: wgpu::IndexFormat) -> usize {
    match format {
        wgpu::IndexFormat::Uint16 => 2,
        wgpu::IndexFormat::Uint32 => 4,
    }
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

pub struct SceneRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_format: wgpu::TextureFormat,
    mesh_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    model_bind_group_layout: wgpu::BindGroupLayout,
    // Layout stored but not read after construction; kept for future
    // renderer extensions that need to rebind the light uniform.
    #[allow(dead_code)]
    light_bind_group_layout: wgpu::BindGroupLayout,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    light_buffer: wgpu::Buffer,
    light_bind_group: wgpu::BindGroup,
    // Kept to keep the texture alive for the lifetime of the renderer.
    #[allow(dead_code)]
    albedo_texture: wgpu::Texture,
    albedo_bind_group: wgpu::BindGroup,
    line_pipeline: wgpu::RenderPipeline,
    line_vertex_buffer: Option<wgpu::Buffer>,
    line_buffer_capacity: wgpu::BufferAddress,
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
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
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

fn uniform_buffer_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
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
        let (depth_texture, depth_view) = Self::create_depth_target(&scene.device, size);

        let renderer = Self {
            instance,
            surface,
            size,
            window,
            scene,
            depth_texture,
            depth_view,
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
        let camera_layout = uniform_buffer_layout(&device, "camera bind group layout");
        let model_layout = dynamic_uniform_buffer_layout(&device, "model bind group layout");
        let light_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
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

        let mesh_pipeline = Renderer::build_mesh_pipeline(
            &device,
            surface_format,
            &camera_layout,
            &model_layout,
            &light_layout,
            &texture_layout,
        );

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera uniform buffer"),
            size: std::mem::size_of::<CameraUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let light_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light uniform buffer"),
            size: std::mem::size_of::<LightUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("light bind group"),
            layout: &light_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: light_buffer.as_entire_binding(),
            }],
        });

        let (albedo_texture, albedo_bind_group) =
            Renderer::create_checkerboard_texture(&device, &queue, &texture_layout);

        let line_pipeline = Renderer::build_line_pipeline(&device, surface_format, &camera_layout);

        // Reusable model uniform buffer with dynamic offsets.
        // Stride = max(ModelUniform size, min_uniform_buffer_offset_alignment).
        // This avoids creating a new buffer+bind group per entity per frame (memory leak fix).
        const MAX_ENTITIES: usize = 1024;
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
            model_bind_group_layout: model_layout,
            light_bind_group_layout: light_layout,
            texture_bind_group_layout: texture_layout,
            camera_buffer,
            camera_bind_group,
            light_buffer,
            light_bind_group,
            albedo_texture,
            albedo_bind_group,
            line_pipeline,
            line_vertex_buffer: None,
            line_buffer_capacity: 0,
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
        surface_format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        model_layout: &wgpu::BindGroupLayout,
        light_layout: &wgpu::BindGroupLayout,
        texture_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/mesh.wgsl"));
        let target_format = surface_format.add_srgb_suffix();

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh pipeline layout"),
            bind_group_layouts: &[
                Some(camera_layout),
                Some(model_layout),
                Some(light_layout),
                Some(texture_layout),
            ],
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

    fn build_line_pipeline(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
    ) -> wgpu::RenderPipeline {
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("../../../assets/shaders/line.wgsl"));
        let target_format = surface_format.add_srgb_suffix();

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("line pipeline layout"),
            bind_group_layouts: &[Some(camera_layout)],
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
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
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
            sample_count: 1,
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
        ViewportTarget::new(&self.device, self.surface_format.add_srgb_suffix(), size)
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
}

impl Renderer {
    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }

    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }

    #[instrument(skip(self), level = "debug")]
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.configure_surface();
        let (depth_texture, depth_view) = Self::create_depth_target(&self.device, new_size);
        self.depth_texture = depth_texture;
        self.depth_view = depth_view;
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

    pub fn update_light(&self, direction: glam::Vec3, ambient: f32, color: glam::Vec3) {
        let uniform = LightUniform {
            direction: direction.to_array(),
            ambient,
            color: color.to_array(),
            _pad: 0.0,
        };
        self.queue
            .write_buffer(&self.light_buffer, 0, bytemuck::cast_slice(&[uniform]));
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

    /// Load a glTF (.glb or .gltf) file from disk and upload all primitives
    /// to the GPU. Each primitive becomes one entry in `LoadedModel::meshes`
    /// with a matching albedo bind group (or `None` if untextured).
    #[instrument(skip(self), level = "info")]
    pub fn load_gltf(&self, path: &str) -> Result<LoadedModel> {
        let (document, buffers, images) =
            gltf::import(path).with_context(|| format!("failed to import glTF file: {path}"))?;

        let mut meshes = Vec::new();
        let mut albedo_bind_groups = Vec::new();

        for mesh in document.meshes() {
            for primitive in mesh.primitives() {
                let reader =
                    primitive.reader(|buffer| buffers.get(buffer.index()).map(|d| d.as_ref()));

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

                let vertices: Vec<Vertex> = positions
                    .iter()
                    .zip(normals.iter())
                    .zip(uvs.iter())
                    .map(|((&pos, &norm), &uv)| Vertex {
                        position: pos,
                        normal: norm,
                        uv,
                    })
                    .collect();

                let (index_bytes, index_format): (Vec<u8>, wgpu::IndexFormat) =
                    match reader.read_indices() {
                        Some(indices) => {
                            let u32_indices: Vec<u32> = indices.into_u32().collect();
                            (
                                bytemuck::cast_slice(&u32_indices).to_vec(),
                                wgpu::IndexFormat::Uint32,
                            )
                        }
                        None => {
                            let u32_indices: Vec<u32> = (0..positions.len() as u32).collect();
                            (
                                bytemuck::cast_slice(&u32_indices).to_vec(),
                                wgpu::IndexFormat::Uint32,
                            )
                        }
                    };

                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("gltf vertex buffer"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("gltf index buffer"),
                            contents: &index_bytes,
                            usage: wgpu::BufferUsages::INDEX,
                        });

                meshes.push(Mesh {
                    vertex_buffer,
                    index_buffer,
                    index_count: (index_bytes.len() / index_format_size(index_format)) as u32,
                    index_format,
                });

                let albedo_bg = primitive
                    .material()
                    .pbr_metallic_roughness()
                    .base_color_texture()
                    .and_then(|info| info.texture().source())
                    .and_then(|image| images.get(image.index()))
                    .map(|img_data| {
                        let dyn_img = image_from_gltf_data(img_data);
                        let texture = self.create_texture_from_image(&dyn_img);
                        self.create_texture_bind_group(&texture)
                    });
                albedo_bind_groups.push(albedo_bg);
            }
        }

        if meshes.is_empty() {
            anyhow::bail!("glTF file {path} contained no meshes");
        }

        tracing::info!("loaded glTF {}: {} primitive(s)", path, meshes.len());

        Ok(LoadedModel {
            meshes,
            albedo_bind_groups,
        })
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
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(2, &self.light_bind_group, &[]);
        pass.set_bind_group(3, &self.albedo_bind_group, &[]);
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
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(2, &self.light_bind_group, &[]);
        for (entity_index, mesh, albedo) in draw_calls {
            let offset = self.model_uniform_offset(*entity_index);
            pass.set_bind_group(1, &self.model_bind_group, &[offset]);
            match albedo {
                Some(bg) => pass.set_bind_group(3, *bg, &[]),
                None => pass.set_bind_group(3, &self.albedo_bind_group, &[]),
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

        let uniform = CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[uniform]));

        let needed = std::mem::size_of_val(vertices) as wgpu::BufferAddress;
        if needed > self.line_buffer_capacity {
            let new_capacity = needed.max(self.line_buffer_capacity * 2);
            self.line_vertex_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("line vertex buffer"),
                size: new_capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.line_buffer_capacity = new_capacity;
        }

        let buffer = self.line_vertex_buffer.as_ref().unwrap();
        self.queue
            .write_buffer(buffer, 0, bytemuck::cast_slice(vertices));

        pass.set_pipeline(&self.line_pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..needed));
        pass.draw(0..vertices.len() as u32, 0..1);
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

    /// Convenience: begin frame, draw meshes, end frame in one call.
    /// For simple cases where no egui overlay is needed.
    pub fn render_meshes(
        &mut self,
        clear_color: wgpu::Color,
        matrices: &[glam::Mat4],
        draw_calls: &[(usize, &Mesh)],
    ) {
        let Some(mut frame) = self.begin_frame() else {
            return;
        };
        self.update_model_uniforms(matrices);
        {
            let mut pass = frame
                .encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("melon_render mesh pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &frame.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear_color),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.depth_view,
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
            self.draw_meshes(&mut pass, draw_calls);
        }
        self.end_frame(frame);
    }
}

pub struct ViewportTarget {
    color_texture: Arc<wgpu::Texture>,
    color_view: Arc<wgpu::TextureView>,
    _depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    size: winit::dpi::PhysicalSize<u32>,
}

impl ViewportTarget {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: winit::dpi::PhysicalSize<u32>,
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
            view_formats: &[],
        }));
        let color_view =
            Arc::new(color_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let (depth_texture, depth_view) = Renderer::create_depth_target(device, size);

        Self {
            color_texture,
            color_view,
            _depth_texture: depth_texture,
            depth_view,
            size,
        }
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
    fn light_uniform_size_is_32() {
        assert_eq!(std::mem::size_of::<LightUniform>(), 32);
    }

    // --- GPU tests via headless event loop harness ---
    // winit allows only ONE EventLoop per process, so all GPU assertions run
    // inside a single test that creates one loop + one renderer.

    type GpuAssertion = Box<dyn FnOnce(&Renderer) -> Result<(), String> + Send>;

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
            if let Some(renderer) = self.renderer.take() {
                while let Some(assertion) = self.assertions.pop() {
                    self.results.push(assertion(&renderer));
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
