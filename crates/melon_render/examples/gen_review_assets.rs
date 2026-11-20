// Generates the small embedded GLBs the snapshot review scene uses for
// materials (assets/models/review/):
// - grey_cube.glb: unit cube, base colour 0.6 grey, roughness 0.8, no textures.
// - material_spheres.glb: two rows of five UV spheres, roughness 0.05 to 1;
//   top row a red dielectric, bottom row gold metal. One node and material each.
// - normal_panel.glb: 2x2 m panel facing +Z with a tiled-bump normal map and
//   no TANGENT attribute, so the loader generates tangents.
// - emissive_cube.glb: unit cube, dark base, emissive (2.5, 0.25, 0.03) HDR
//   (emissiveFactor x KHR_materials_emissive_strength), for bloom.
// Run from the repository root: cargo run -p melon_render --example gen_review_assets

use std::f32::consts::PI;
use std::fmt::Write as _;
use std::fs;

use image::ImageEncoder;

/// Vertex data of one mesh.
struct MeshData {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

/// Accessor indices of an uploaded mesh.
struct MeshAccessors {
    position: usize,
    normal: usize,
    uv: usize,
    indices: usize,
}

/// One GLB being built: a binary chunk plus the JSON arrays that index it.
#[derive(Default)]
struct Glb {
    bin: Vec<u8>,
    buffer_views: Vec<String>,
    accessors: Vec<String>,
}

impl Glb {
    fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let target = target.map_or(String::new(), |t| format!(r#","target":{t}"#));
        self.buffer_views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{}{target}}}"#,
            self.bin.len(),
            bytes.len()
        ));
        self.bin.extend_from_slice(bytes);
        self.buffer_views.len() - 1
    }

    fn floats<const N: usize>(&mut self, data: &[[f32; N]], ty: &str, bounds: bool) -> usize {
        let bytes: Vec<u8> = data
            .iter()
            .flatten()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let view = self.view(&bytes, Some(34962));
        let mut bounds_json = String::new();
        if bounds {
            let (mut min, mut max) = ([f32::MAX; N], [f32::MIN; N]);
            for v in data {
                for i in 0..N {
                    min[i] = min[i].min(v[i]);
                    max[i] = max[i].max(v[i]);
                }
            }
            bounds_json = format!(r#","min":{min:?},"max":{max:?}"#);
        }
        self.accessors.push(format!(
            r#"{{"bufferView":{view},"componentType":5126,"count":{},"type":"{ty}"{bounds_json}}}"#,
            data.len()
        ));
        self.accessors.len() - 1
    }

    fn mesh(&mut self, mesh: &MeshData) -> MeshAccessors {
        let position = self.floats(&mesh.positions, "VEC3", true);
        let normal = self.floats(&mesh.normals, "VEC3", false);
        let uv = self.floats(&mesh.uvs, "VEC2", false);
        let bytes: Vec<u8> = mesh.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let view = self.view(&bytes, Some(34963));
        self.accessors.push(format!(
            r#"{{"bufferView":{view},"componentType":5125,"count":{},"type":"SCALAR"}}"#,
            mesh.indices.len()
        ));
        MeshAccessors {
            position,
            normal,
            uv,
            indices: self.accessors.len() - 1,
        }
    }

    /// Writes the GLB. `rest` holds the remaining top-level JSON members
    /// (scene, nodes, meshes, materials, ...), without surrounding braces.
    fn write(mut self, path: &str, rest: &str) {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let mut json = format!(
            r#"{{"asset":{{"version":"2.0","generator":"MelonEngine review asset generator"}},{rest},"bufferViews":[{}],"accessors":[{}],"buffers":[{{"byteLength":{}}}]}}"#,
            self.buffer_views.join(","),
            self.accessors.join(","),
            self.bin.len()
        )
        .into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 12 + 8 + json.len() + 8 + self.bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(self.bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&self.bin);
        fs::write(path, &glb).unwrap_or_else(|e| panic!("failed to write {path}: {e}"));
        eprintln!("wrote {path} ({} bytes)", glb.len());
    }
}

/// `{"attributes":{...},"indices":..,"material":..}` for a mesh.
fn primitive(a: &MeshAccessors, material: usize) -> String {
    format!(
        r#"{{"attributes":{{"POSITION":{},"NORMAL":{},"TEXCOORD_0":{}}},"indices":{},"material":{material}}}"#,
        a.position, a.normal, a.uv, a.indices
    )
}

/// Unit cube centred on the origin, 4 vertices per face.
fn cube() -> MeshData {
    let mut mesh = MeshData {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::new(),
    };
    let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for (i, n) in axes.iter().enumerate() {
        for sign in [1.0f32, -1.0] {
            let n = n.map(|c| c * sign);
            // Two axes spanning the face, so that u x v = n (counter-clockwise).
            let u = axes[(i + 1) % 3].map(|c| c * sign);
            let v = axes[(i + 2) % 3];
            let base = mesh.positions.len() as u32;
            for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                mesh.positions.push(std::array::from_fn(|k| {
                    0.5 * (n[k] + su * u[k] + sv * v[k])
                }));
                mesh.normals.push(n);
                mesh.uvs.push([(su + 1.0) / 2.0, (1.0 - sv) / 2.0]);
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|k| base + k));
        }
    }
    mesh
}

/// UV sphere of `radius`, `segments` around and `rings` from pole to pole.
fn sphere(radius: f32, segments: u32, rings: u32) -> MeshData {
    let mut mesh = MeshData {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::new(),
    };
    for r in 0..=rings {
        let theta = PI * r as f32 / rings as f32;
        for s in 0..=segments {
            let phi = 2.0 * PI * s as f32 / segments as f32;
            let n = [
                theta.sin() * phi.cos(),
                theta.cos(),
                -theta.sin() * phi.sin(),
            ];
            mesh.positions.push(n.map(|c| c * radius));
            mesh.normals.push(n);
            mesh.uvs
                .push([s as f32 / segments as f32, r as f32 / rings as f32]);
        }
    }
    let row = segments + 1;
    for r in 0..rings {
        for s in 0..segments {
            let (a, b) = (r * row + s, (r + 1) * row + s);
            mesh.indices.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    mesh
}

fn png(pixels: &[u8], size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(pixels, size, size, image::ExtendedColorType::Rgba8)
        .expect("failed to encode PNG");
    out
}

/// One tile of the bump normal map: a raised square with bevelled edges.
/// Tangent space as glTF stores it: red = +U, green = up the image (-V).
fn tile_normal_map(size: u32) -> Vec<u8> {
    let height = |x: f32, y: f32| {
        let edge = x.min(1.0 - x).min(y).min(1.0 - y);
        let t = (edge / 0.15).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let step = 1.0 / size as f32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for row in 0..size {
        for col in 0..size {
            let (x, y) = ((col as f32 + 0.5) * step, (row as f32 + 0.5) * step);
            let dx = (height(x + step, y) - height(x - step, y)) / (2.0 * step);
            let dy = (height(x, y + step) - height(x, y - step)) / (2.0 * step);
            // Bevel depth: 0.05 of a tile.
            let n = glam::Vec3::new(-0.05 * dx, 0.05 * dy, 1.0).normalize();
            pixels.extend(
                n.to_array()
                    .map(|c| ((c * 0.5 + 0.5) * 255.0).round() as u8),
            );
            pixels.push(255);
        }
    }
    pixels
}

fn grey_cube(path: &str) {
    let mut glb = Glb::default();
    let a = glb.mesh(&cube());
    glb.write(
        path,
        &format!(
            r#""scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":"GreyCube"}}],"meshes":[{{"primitives":[{}]}}],"materials":[{{"name":"Grey","pbrMetallicRoughness":{{"baseColorFactor":[0.6,0.6,0.6,1.0],"metallicFactor":0.0,"roughnessFactor":0.8}}}}]"#,
            primitive(&a, 0)
        ),
    );
}

fn emissive_cube(path: &str) {
    let mut glb = Glb::default();
    let a = glb.mesh(&cube());
    glb.write(
        path,
        &format!(
            r#""extensionsUsed":["KHR_materials_emissive_strength"],"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":"EmissiveCube"}}],"meshes":[{{"primitives":[{}]}}],"materials":[{{"name":"Glow","pbrMetallicRoughness":{{"baseColorFactor":[0.05,0.05,0.05,1.0],"metallicFactor":0.0,"roughnessFactor":0.6}},"emissiveFactor":[1.0,0.1,0.012],"extensions":{{"KHR_materials_emissive_strength":{{"emissiveStrength":2.5}}}}}}]"#,
            primitive(&a, 0)
        ),
    );
}

fn material_spheres(path: &str) {
    let mut glb = Glb::default();
    let a = glb.mesh(&sphere(0.4, 48, 24));
    let (mut nodes, mut meshes, mut materials) = (Vec::new(), Vec::new(), Vec::new());
    for (row, (metallic, base, y)) in [
        (0.0, [0.7, 0.08, 0.06], 1.35), // dielectric (red plastic)
        (1.0, [1.0, 0.77, 0.34], 0.45), // gold
    ]
    .into_iter()
    .enumerate()
    {
        for step in 0..5 {
            let roughness = 0.05 + 0.95 * step as f32 / 4.0;
            let i = materials.len();
            let x = step as f32 - 2.0;
            let kind = if row == 0 { "Dielectric" } else { "Metal" };
            let mut material = String::new();
            write!(
                material,
                r#"{{"name":"{kind}{step}","pbrMetallicRoughness":{{"baseColorFactor":[{},{},{},1.0],"metallicFactor":{metallic},"roughnessFactor":{roughness}}}}}"#,
                base[0], base[1], base[2]
            )
            .unwrap();
            materials.push(material);
            meshes.push(format!(r#"{{"primitives":[{}]}}"#, primitive(&a, i)));
            nodes.push(format!(
                r#"{{"mesh":{i},"name":"{kind}{step}","translation":[{x},{y},0.0]}}"#
            ));
        }
    }
    let roots: Vec<String> = (0..nodes.len()).map(|i| i.to_string()).collect();
    glb.write(
        path,
        &format!(
            r#""scene":0,"scenes":[{{"nodes":[{}]}}],"nodes":[{}],"meshes":[{}],"materials":[{}]"#,
            roots.join(","),
            nodes.join(","),
            meshes.join(","),
            materials.join(",")
        ),
    );
}

fn normal_panel(path: &str) {
    // UVs run 0..4 across the panel: 16 tiles of 0.5 m, the map repeating.
    let mesh = MeshData {
        positions: vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; 4],
        uvs: vec![[0.0, 4.0], [4.0, 4.0], [4.0, 0.0], [0.0, 0.0]],
        indices: vec![0, 1, 2, 0, 2, 3],
    };
    let mut glb = Glb::default();
    let a = glb.mesh(&mesh);
    let image = glb.view(&png(&tile_normal_map(64), 64), None);
    glb.write(
        path,
        &format!(
            r#""scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"name":"NormalPanel"}}],"meshes":[{{"primitives":[{}]}}],"materials":[{{"name":"Tiles","pbrMetallicRoughness":{{"baseColorFactor":[0.7,0.7,0.7,1.0],"metallicFactor":0.0,"roughnessFactor":0.5}},"normalTexture":{{"index":0,"scale":1.0}},"doubleSided":false}}],"textures":[{{"source":0}}],"images":[{{"bufferView":{image},"mimeType":"image/png"}}]"#,
            primitive(&a, 0)
        ),
    );
}

fn main() {
    let dir = "assets/models/review";
    fs::create_dir_all(dir).expect("failed to create assets/models/review");
    grey_cube(&format!("{dir}/grey_cube.glb"));
    material_spheres(&format!("{dir}/material_spheres.glb"));
    normal_panel(&format!("{dir}/normal_panel.glb"));
    emissive_cube(&format!("{dir}/emissive_cube.glb"));
}
