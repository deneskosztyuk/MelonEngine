// Generates a minimal GLB: a unit cube (24 verts, 36 indices) with normals,
// UVs, and a 4x4 RGBA8 base color texture (melon pink, PNG-encoded).
// Output: assets/models/cube.glb
// Run: cargo run -p melon_render --example gen_cube_glb

use std::fs;

use image::ImageEncoder;

fn main() {
    let vertices: [[f32; 3]; 24] = [
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [0.5, 0.5, 0.5],
        [0.5, -0.5, 0.5],
        [-0.5, -0.5, 0.5],
        [-0.5, 0.5, 0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, 0.5, 0.5],
        [0.5, 0.5, 0.5],
        [0.5, -0.5, 0.5],
        [-0.5, -0.5, 0.5],
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
        [-0.5, -0.5, 0.5],
        [-0.5, -0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [0.5, 0.5, -0.5],
        [0.5, -0.5, -0.5],
    ];
    let normals: [[f32; 3]; 24] = [
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -1.0],
    ];
    let uvs: [[f32; 2]; 24] = [
        [0.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 0.0],
        [0.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 0.0],
        [0.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 0.0],
        [0.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 0.0],
        [0.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 0.0],
        [0.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 0.0],
    ];
    let indices: [u16; 36] = [
        0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17,
        18, 16, 18, 19, 20, 21, 22, 20, 22, 23,
    ];

    let tex_size: u32 = 4;
    let tex_pixels: Vec<u8> = (0..(tex_size * tex_size * 4))
        .map(|i| match i % 4 {
            0 => 220u8,
            1 => 60u8,
            2 => 180u8,
            _ => 255u8,
        })
        .collect();
    let mut png_buf = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(&mut png_buf);
        encoder
            .write_image(
                &tex_pixels,
                tex_size,
                tex_size,
                image::ExtendedColorType::Rgba8,
            )
            .expect("failed to encode PNG");
    }
    eprintln!("PNG texture: {} bytes", png_buf.len());

    let pos_bytes: Vec<u8> = vertices
        .iter()
        .flat_map(|v| v.iter().copied())
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let nrm_bytes: Vec<u8> = normals
        .iter()
        .flat_map(|v| v.iter().copied())
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let uv_bytes: Vec<u8> = uvs
        .iter()
        .flat_map(|v| v.iter().copied())
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let idx_bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let tex_bytes = &png_buf;

    let pos_offset = 0usize;
    let pos_len = pos_bytes.len();
    let nrm_offset = pos_offset + pos_len;
    let nrm_len = nrm_bytes.len();
    let uv_offset = nrm_offset + nrm_len;
    let uv_len = uv_bytes.len();
    let idx_offset = uv_offset + uv_len;
    let idx_len = idx_bytes.len();
    let tex_offset = (idx_offset + idx_len + 3) & !3;
    let tex_len = tex_bytes.len();

    let mut bin = Vec::with_capacity(tex_offset + tex_len);
    bin.extend_from_slice(&pos_bytes);
    bin.extend_from_slice(&nrm_bytes);
    bin.extend_from_slice(&uv_bytes);
    bin.extend_from_slice(&idx_bytes);
    while bin.len() < tex_offset {
        bin.push(0);
    }
    bin.extend_from_slice(tex_bytes);
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let bin_len = bin.len();

    let json = format!(
        r#"{{"asset":{{"version":"2.0","generator":"MelonEngine GLB generator"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},"indices":3,"material":0}}]}}],"materials":[{{"pbrMetallicRoughness":{{"baseColorTexture":{{"index":0}},"metallicFactor":0.0,"roughnessFactor":0.8}}}}],"textures":[{{"source":0}}],"images":[{{"bufferView":4,"mimeType":"image/png"}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":24,"type":"VEC3","max":[0.5,0.5,0.5],"min":[-0.5,-0.5,-0.5]}},{{"bufferView":1,"componentType":5126,"count":24,"type":"VEC3"}},{{"bufferView":2,"componentType":5126,"count":24,"type":"VEC2"}},{{"bufferView":3,"componentType":5123,"count":36,"type":"SCALAR"}}],"bufferViews":[{{"buffer":0,"byteOffset":{pos_offset},"byteLength":{pos_len},"target":34962}},{{"buffer":0,"byteOffset":{nrm_offset},"byteLength":{nrm_len},"target":34962}},{{"buffer":0,"byteOffset":{uv_offset},"byteLength":{uv_len},"target":34962}},{{"buffer":0,"byteOffset":{idx_offset},"byteLength":{idx_len},"target":34963}},{{"buffer":0,"byteOffset":{tex_offset},"byteLength":{tex_len}}}],"buffers":[{{"byteLength":{bin_len}}}]}}"#,
    );

    let mut json_data = json.into_bytes();
    while json_data.len() % 4 != 0 {
        json_data.push(b' ');
    }
    let json_len = json_data.len();
    let total_len = 12 + 8 + json_len + 8 + bin_len;

    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(&0x46546C67u32.to_le_bytes());
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());
    glb.extend_from_slice(&(json_len as u32).to_le_bytes());
    glb.extend_from_slice(&0x4e4f534au32.to_le_bytes());
    glb.extend_from_slice(&json_data);
    glb.extend_from_slice(&(bin_len as u32).to_le_bytes());
    glb.extend_from_slice(&0x004e4942u32.to_le_bytes());
    glb.extend_from_slice(&bin);

    let out_path = "assets/models/cube.glb";
    fs::write(out_path, &glb).expect("failed to write GLB");
    eprintln!("wrote {} ({} bytes)", out_path, glb.len());
    eprintln!(
        "  vertices: 24, indices: 36, texture: {}x{} RGBA8 PNG",
        tex_size, tex_size
    );
}
