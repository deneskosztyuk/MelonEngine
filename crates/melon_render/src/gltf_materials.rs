//! Unpack the materials of a glTF file into `.mat` assets (and their
//! textures as png files) next to it, one per distinct material, in the
//! order of the model's mesh parts.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use melon_core::{AlphaMode, MaterialAsset, TextureRef};

fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Every primitive's material index, in the order `SceneRenderer::add_node`
/// builds the parts (nodes depth first).
fn part_materials(node: &gltf::Node, out: &mut Vec<Option<usize>>) {
    if let Some(mesh) = node.mesh() {
        out.extend(mesh.primitives().map(|p| p.material().index()));
    }
    for child in node.children() {
        part_materials(&child, out);
    }
}

/// Write one `.mat` per distinct material of `model` (and a png per used
/// image) next to it, named `<model>_<material>.mat`. `rel_dir` is the
/// project-relative folder of the model; the returned paths are
/// project-relative `.mat` paths, one per mesh part of the model (parts
/// without a material get a default one). Textures are referenced by path;
/// the caller adds `.meta`s and fills the GUIDs.
pub fn extract_materials(model: &Path, rel_dir: &str) -> Result<Vec<String>> {
    let (document, _, images) = gltf::import(model)
        .with_context(|| format!("failed to import glTF file: {}", model.display()))?;
    let dir = model.parent().unwrap_or_else(|| Path::new("."));
    let stem = file_safe(&model.file_stem().unwrap_or_default().to_string_lossy());
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .context("glTF file has no scene")?;
    let mut parts = Vec::new();
    for node in scene.nodes() {
        part_materials(&node, &mut parts);
    }

    let mut written_images: HashMap<usize, String> = HashMap::new();
    let mut files: HashMap<Option<usize>, String> = HashMap::new();
    let mut out = Vec::new();
    for material_index in parts {
        if let Some(path) = files.get(&material_index) {
            out.push(path.clone());
            continue;
        }
        let material = material_index.and_then(|i| document.materials().nth(i));
        let name = file_safe(
            material
                .as_ref()
                .and_then(|m| m.name())
                .unwrap_or("default"),
        );
        let mut texture = |tex: Option<gltf::Texture>| -> Result<Option<TextureRef>> {
            let Some(source) = tex.and_then(|t| t.source().map(|i| i.index())) else {
                return Ok(None);
            };
            let rel = match written_images.get(&source) {
                Some(rel) => rel.clone(),
                None => {
                    let file = format!("{stem}_image{source}.png");
                    crate::image_from_gltf_data(&images[source])
                        .to_rgba8()
                        .save(dir.join(&file))
                        .with_context(|| format!("could not write {file}"))?;
                    let rel = format!("{rel_dir}/{file}");
                    written_images.insert(source, rel.clone());
                    rel
                }
            };
            Ok(Some(TextureRef {
                guid: None,
                path: rel,
            }))
        };
        let asset = match &material {
            None => MaterialAsset::default(),
            Some(material) => {
                let pbr = material.pbr_metallic_roughness();
                MaterialAsset {
                    base_color: pbr.base_color_factor(),
                    base_color_texture: texture(pbr.base_color_texture().map(|t| t.texture()))?,
                    metallic: pbr.metallic_factor(),
                    roughness: pbr.roughness_factor(),
                    metallic_roughness_texture: texture(
                        pbr.metallic_roughness_texture().map(|t| t.texture()),
                    )?,
                    normal_scale: material.normal_texture().map_or(1.0, |n| n.scale()),
                    normal_texture: texture(material.normal_texture().map(|t| t.texture()))?,
                    emissive: material.emissive_factor(),
                    emissive_texture: texture(material.emissive_texture().map(|t| t.texture()))?,
                    emissive_strength: material.emissive_strength().unwrap_or(1.0),
                    alpha_mode: match material.alpha_mode() {
                        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                        gltf::material::AlphaMode::Mask => AlphaMode::Mask {
                            cutoff: material.alpha_cutoff().unwrap_or(0.5),
                        },
                        gltf::material::AlphaMode::Blend => AlphaMode::Blend,
                    },
                    double_sided: material.double_sided(),
                }
            }
        };
        let file = format!("{stem}_{name}.mat");
        asset.save(&dir.join(&file))?;
        let rel = format!("{rel_dir}/{file}");
        files.insert(material_index, rel.clone());
        out.push(rel);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::{AlphaMode, MaterialAsset};

    const GLTF: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1, 2]}],
        "nodes": [{"mesh": 0}, {"mesh": 1}, {"mesh": 0}],
        "meshes": [
            {"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]},
            {"primitives": [{"attributes": {"POSITION": 0}, "material": 1}]}
        ],
        "materials": [
            {"name": "Red Paint", "doubleSided": true,
             "pbrMetallicRoughness": {"baseColorFactor": [1, 0, 0, 1], "metallicFactor": 0.2}},
            {"name": "Glass", "alphaMode": "BLEND",
             "pbrMetallicRoughness": {"baseColorFactor": [0, 0, 1, 0.5]}}
        ],
        "buffers": [{"byteLength": 36,
            "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAA"}],
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3,
            "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]}]
    }"#;

    #[test]
    fn each_distinct_material_becomes_a_mat_file_in_part_order() {
        let dir = std::env::temp_dir().join(format!("melon_extract_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model = dir.join("thing.gltf");
        std::fs::write(&model, GLTF).unwrap();
        let parts = extract_materials(&model, "assets/models").unwrap();
        // Three parts (nodes 0, 1, 2); parts 0 and 2 share a material.
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], parts[2]);
        assert_ne!(parts[0], parts[1]);
        let red = MaterialAsset::read(&dir.join(parts[0].rsplit('/').next().unwrap())).unwrap();
        assert_eq!(red.base_color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(red.metallic, 0.2);
        assert!(red.double_sided);
        let glass = MaterialAsset::read(&dir.join(parts[1].rsplit('/').next().unwrap())).unwrap();
        assert_eq!(glass.alpha_mode, AlphaMode::Blend);
        assert!(parts[0].starts_with("assets/models/") && parts[0].ends_with(".mat"));
        assert!(parts[0].contains("Red Paint") || parts[0].contains("Red_Paint"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
