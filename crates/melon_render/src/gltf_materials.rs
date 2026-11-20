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
    if crate::fbx::is_fbx(&model.to_string_lossy()) {
        return extract_fbx(model, rel_dir);
    }
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

/// A file name for `bytes` in `dir` that never overwrites a different file:
/// `file` itself (or the same file already there), then `<model>_<file>`,
/// then `<model>_<name>_<n><ext>`.
fn free_file_name(dir: &Path, model_stem: &str, file: &str, bytes: &[u8]) -> String {
    let (name, ext) = file
        .rsplit_once('.')
        .map_or((file, String::new()), |(n, e)| (n, format!(".{e}")));
    for n in 0.. {
        let candidate = match n {
            0 => file.to_string(),
            1 => format!("{model_stem}_{file}"),
            n => format!("{model_stem}_{name}_{n}{ext}"),
        };
        match std::fs::read(dir.join(&candidate)) {
            Ok(existing) if existing != bytes => {}
            _ => return candidate,
        }
    }
    unreachable!()
}

/// `extract_materials` for an FBX: textures are copied next to the model by
/// their own file name (an embedded one without a name becomes
/// `texture_<n>.<ext>`), kept as is when the same file is already there and
/// never overwriting a different one. The project root is `rel_dir` above
/// the model's folder; textures are searched in its `assets` folder.
fn extract_fbx(model: &Path, rel_dir: &str) -> Result<Vec<String>> {
    let dir = model.parent().unwrap_or_else(|| Path::new("."));
    let mut root = dir;
    for _ in rel_dir.split('/').filter(|p| !p.is_empty()) {
        root = root.parent().unwrap_or(root);
    }
    let file = model.file_name().unwrap_or_default().to_string_lossy();
    let name = format!("{rel_dir}/{file}");
    let bytes =
        std::fs::read(model).with_context(|| format!("could not read {}", model.display()))?;
    let settings = melon_core::ImportSettings::read(model);
    let index: Vec<String> = melon_core::assets::asset_dir_files(root)
        .iter()
        .filter_map(|f| melon_core::assets::project_relative(root, f))
        .filter(|p| !p.ends_with(".meta"))
        .collect();
    let read = |p: &str| std::fs::read(root.join(p));
    let sources = crate::fbx::Sources::logging(&read, &index);
    let fbx = crate::fbx::import(&name, &bytes, &settings, Some(&sources))?;
    let stem = file_safe(&model.file_stem().unwrap_or_default().to_string_lossy());

    let mut written: HashMap<(usize, bool), String> = HashMap::new();
    let mut files: HashMap<Option<usize>, String> = HashMap::new();
    let mut used_names = std::collections::HashSet::new();
    let mut out = Vec::new();
    for material_index in fbx.part_materials() {
        if let Some(path) = files.get(&material_index) {
            out.push(path.clone());
            continue;
        }
        let material = material_index.map(|i| &fbx.materials[i]);
        let mut asset = material.map_or_else(MaterialAsset::new_default, |m| m.asset.clone());
        if let Some(material) = material {
            let slots = [
                &mut asset.base_color_texture,
                &mut asset.metallic_roughness_texture,
                &mut asset.normal_texture,
                &mut asset.emissive_texture,
            ];
            for (i, (slot, texture)) in slots.into_iter().zip(material.textures).enumerate() {
                let Some(t) = texture else { continue };
                let flip = i == 2 && settings.normal_y_flip;
                let rel = match written.get(&(t, flip)) {
                    Some(rel) => rel.clone(),
                    None => {
                        let texture = &fbx.textures[t];
                        let mut file = texture.file_name.clone();
                        let mut bytes = texture.bytes.clone();
                        if file.is_empty() {
                            let ext = image::guess_format(&bytes)
                                .ok()
                                .and_then(|f| f.extensions_str().first().copied())
                                .unwrap_or("png");
                            file = format!("texture_{t}.{ext}");
                        }
                        if flip && let Ok(img) = image::load_from_memory(&bytes) {
                            let mut png = std::io::Cursor::new(Vec::new());
                            crate::flip_green(&img)
                                .write_to(&mut png, image::ImageFormat::Png)
                                .context("could not encode the flipped normal map")?;
                            bytes = png.into_inner();
                            file = format!("{}.png", file.rsplit_once('.').map_or(&*file, |p| p.0));
                        }
                        let file = free_file_name(dir, &stem, &file, &bytes);
                        std::fs::write(dir.join(&file), &bytes)
                            .with_context(|| format!("could not write {file}"))?;
                        let rel = format!("{rel_dir}/{file}");
                        written.insert((t, flip), rel.clone());
                        rel
                    }
                };
                *slot = Some(TextureRef {
                    guid: None,
                    path: rel,
                });
            }
        }
        let base = file_safe(
            material
                .map(|m| m.name.as_str())
                .filter(|n| !n.is_empty())
                .unwrap_or("default"),
        );
        let mut name = base.clone();
        let mut n = 1;
        while !used_names.insert(name.clone()) {
            n += 1;
            name = format!("{base}_{n}");
        }
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

    /// A project root with an empty `assets/models`; returns that folder.
    fn extract_dir(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("melon_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("assets/models");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png_bytes(colour: [u8; 4]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba(colour))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn fbx_materials_and_embedded_textures_are_written_next_to_the_model() {
        use crate::fbx::fixture::Cube;
        let dir = extract_dir("extract_fbx");
        let png = png_bytes([0, 255, 0, 255]);
        let cube = Cube {
            two_materials: true,
            texture: Some("t.png"),
            embedded: Some(png.clone()),
            ..Cube::default()
        };
        let model = dir.join("box.fbx");
        std::fs::write(&model, cube.ascii()).unwrap();
        let parts = extract_materials(&model, "assets/models").unwrap();
        assert_eq!(parts.len(), 2);
        assert_ne!(parts[0], parts[1]);
        let red = MaterialAsset::read(&dir.join(parts[0].rsplit('/').next().unwrap())).unwrap();
        let blue = MaterialAsset::read(&dir.join(parts[1].rsplit('/').next().unwrap())).unwrap();
        let texture = red.base_color_texture.unwrap();
        assert_eq!(texture.path, "assets/models/t.png");
        assert!(blue.base_color_texture.is_none());
        assert_eq!(blue.base_color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(std::fs::read(dir.join("t.png")).unwrap(), png);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fbx_texture_with_other_bytes_next_to_the_model_gets_the_model_name_prefix() {
        use crate::fbx::fixture::Cube;
        let dir = extract_dir("extract_fbx_clash");
        let theirs = png_bytes([1, 2, 3, 255]);
        let ours = png_bytes([9, 9, 9, 255]);
        std::fs::write(dir.join("t.png"), &theirs).unwrap();
        let cube = Cube {
            texture: Some("t.png"),
            embedded: Some(ours.clone()),
            ..Cube::default()
        };
        std::fs::write(dir.join("box.fbx"), cube.ascii()).unwrap();
        let parts = extract_materials(&dir.join("box.fbx"), "assets/models").unwrap();
        let mat = MaterialAsset::read(&dir.join(parts[0].rsplit('/').next().unwrap())).unwrap();
        assert_eq!(
            mat.base_color_texture.unwrap().path,
            "assets/models/box_t.png"
        );
        assert_eq!(std::fs::read(dir.join("t.png")).unwrap(), theirs);
        assert_eq!(std::fs::read(dir.join("box_t.png")).unwrap(), ours);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fbx_texture_already_next_to_the_model_is_kept_not_overwritten() {
        use crate::fbx::fixture::Cube;
        let dir = extract_dir("extract_fbx_beside");
        let png = png_bytes([1, 2, 3, 255]);
        std::fs::write(dir.join("t.png"), &png).unwrap();
        let cube = Cube {
            texture: Some("t.png"),
            ..Cube::default()
        };
        std::fs::write(dir.join("box.fbx"), cube.ascii()).unwrap();
        let parts = extract_materials(&dir.join("box.fbx"), "assets/models").unwrap();
        let mat = MaterialAsset::read(&dir.join(parts[0].rsplit('/').next().unwrap())).unwrap();
        assert_eq!(mat.base_color_texture.unwrap().path, "assets/models/t.png");
        assert_eq!(std::fs::read(dir.join("t.png")).unwrap(), png);
        assert!(!dir.join("box_t.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn first_mat(dir: &std::path::Path, parts: &[String]) -> MaterialAsset {
        MaterialAsset::read(&dir.join(parts[0].rsplit('/').next().unwrap())).unwrap()
    }

    #[test]
    fn fbx_embedded_texture_without_a_file_name_becomes_texture_n() {
        use crate::fbx::fixture::Cube;
        let dir = extract_dir("extract_fbx_noname");
        let png = png_bytes([5, 6, 7, 255]);
        let cube = Cube {
            texture: Some(""),
            embedded: Some(png.clone()),
            ..Cube::default()
        };
        std::fs::write(dir.join("box.fbx"), cube.ascii()).unwrap();
        let parts = extract_materials(&dir.join("box.fbx"), "assets/models").unwrap();
        let texture = first_mat(&dir, &parts).base_color_texture.unwrap();
        assert_eq!(texture.path, "assets/models/texture_0.png");
        assert_eq!(std::fs::read(dir.join("texture_0.png")).unwrap(), png);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fbx_texture_never_overwrites_any_different_file() {
        use crate::fbx::fixture::Cube;
        let dir = extract_dir("extract_fbx_unique");
        let (a, b, ours) = (
            png_bytes([1, 1, 1, 255]),
            png_bytes([2, 2, 2, 255]),
            png_bytes([9, 9, 9, 255]),
        );
        std::fs::write(dir.join("t.png"), &a).unwrap();
        std::fs::write(dir.join("box_t.png"), &b).unwrap();
        let cube = Cube {
            texture: Some("t.png"),
            embedded: Some(ours.clone()),
            ..Cube::default()
        };
        std::fs::write(dir.join("box.fbx"), cube.ascii()).unwrap();
        let parts = extract_materials(&dir.join("box.fbx"), "assets/models").unwrap();
        let texture = first_mat(&dir, &parts).base_color_texture.unwrap();
        assert_eq!(texture.path, "assets/models/box_t_2.png");
        assert_eq!(std::fs::read(dir.join("t.png")).unwrap(), a);
        assert_eq!(std::fs::read(dir.join("box_t.png")).unwrap(), b);
        assert_eq!(std::fs::read(dir.join("box_t_2.png")).unwrap(), ours);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fbx_materials_with_one_name_get_distinct_files() {
        use crate::fbx::fixture::Cube;
        let dir = extract_dir("extract_fbx_names");
        let cube = Cube {
            two_materials: true,
            ..Cube::default()
        };
        let text = cube.ascii().replace("Material::Blue", "Material::Red");
        std::fs::write(dir.join("box.fbx"), text).unwrap();
        let parts = extract_materials(&dir.join("box.fbx"), "assets/models").unwrap();
        assert_eq!(parts.len(), 2);
        assert_ne!(parts[0], parts[1]);
        assert!(
            parts
                .iter()
                .all(|p| dir.join(p.rsplit('/').next().unwrap()).is_file())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fbx_texture_is_searched_in_the_projects_textures_folder() {
        use crate::fbx::fixture::Cube;
        let dir = extract_dir("extract_fbx_search");
        let png = png_bytes([3, 4, 5, 255]);
        let textures = dir.parent().unwrap().join("Textures");
        std::fs::create_dir_all(&textures).unwrap();
        std::fs::write(textures.join("T.PNG"), &png).unwrap();
        let cube = Cube {
            texture: Some("t.png"),
            ..Cube::default()
        };
        std::fs::write(dir.join("box.fbx"), cube.ascii()).unwrap();
        let parts = extract_materials(&dir.join("box.fbx"), "assets/models").unwrap();
        let texture = first_mat(&dir, &parts).base_color_texture.unwrap();
        assert_eq!(
            std::fs::read(dir.join("t.png")).unwrap(),
            png,
            "{texture:?}"
        );
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn fbx_extract_flips_the_normal_map_when_the_meta_says_so() {
        use crate::fbx::fixture::Cube;
        use melon_core::{AssetGuid, AssetMeta, ImportSettings, META_VERSION, meta_path};
        let dir = extract_dir("extract_fbx_flip");
        let cube = Cube {
            texture: Some("n.png"),
            embedded: Some(png_bytes([10, 20, 30, 255])),
            slots: vec!["NormalMap"],
            ..Cube::default()
        };
        let model = dir.join("box.fbx");
        std::fs::write(&model, cube.ascii()).unwrap();
        let meta = AssetMeta {
            version: META_VERSION,
            guid: AssetGuid::new(),
        };
        std::fs::write(meta_path(&model), meta.to_ron()).unwrap();
        ImportSettings {
            normal_y_flip: true,
            ..Default::default()
        }
        .write(&model)
        .unwrap();
        let parts = extract_materials(&model, "assets/models").unwrap();
        let texture = first_mat(&dir, &parts).normal_texture.unwrap();
        let written = image::open(dir.join(texture.path.rsplit('/').next().unwrap())).unwrap();
        assert_eq!(written.to_rgba8().get_pixel(0, 0).0, [10, 235, 30, 255]);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }
}
