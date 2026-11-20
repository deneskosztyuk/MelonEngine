//! FBX import with ufbx, into the same CPU parts the glTF path uploads.

use std::cell::Cell;
use std::collections::HashMap;

use anyhow::{Context, Result};
use glam::{Mat4, Quat, Vec3, Vec4};
use melon_core::{AlphaMode, ImportSettings, MaterialAsset};

use crate::{GltfNode, Vertex};

thread_local! {
    static PARSES: Cell<usize> = const { Cell::new(0) };
}

/// Files parsed by ufbx on this thread so far (tests count parses).
pub fn parses_on_this_thread() -> usize {
    PARSES.with(Cell::get)
}

/// `path` (no `#mesh` suffix) is an FBX file.
pub(crate) fn is_fbx(path: &str) -> bool {
    path.len() >= 4 && path[path.len() - 4..].eq_ignore_ascii_case(".fbx")
}

/// Reads a file by project path.
pub(crate) type Reader<'a> = &'a dyn Fn(&str) -> std::io::Result<Vec<u8>>;

/// Reports a problem with a texture: (texture file name, message). The
/// caller decides how often a message is shown.
pub(crate) type Warn<'a> = &'a dyn Fn(&str, String);

/// Where a model's external textures come from: `read` fetches a project
/// path, `index` lists every project file (`/` separators) for the search.
pub(crate) struct Sources<'a> {
    pub read: Reader<'a>,
    pub index: &'a [String],
    pub warn: Warn<'a>,
}

pub(crate) fn log_warn(_: &str, message: String) {
    tracing::warn!("{message}");
}

impl<'a> Sources<'a> {
    /// Warnings go to the log.
    pub fn logging(read: Reader<'a>, index: &'a [String]) -> Self {
        Self {
            read,
            index,
            warn: &log_warn,
        }
    }
}

/// A texture file of a model: its name, the project path it was read from
/// (`None` when embedded) and its raw (still encoded) bytes.
pub(crate) struct FbxTexture {
    pub file_name: String,
    pub path: Option<String>,
    pub bytes: Vec<u8>,
}

/// One material: factors and, per slot (base colour, metallic-roughness,
/// normal, emissive), an index into `FbxModel::textures`.
pub(crate) struct FbxMaterial {
    pub name: String,
    pub asset: MaterialAsset,
    pub textures: [Option<usize>; 4],
}

/// Triangles of one material of one mesh, in mesh space.
pub(crate) struct FbxPart {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub material: Option<usize>,
}

pub(crate) struct FbxModel {
    /// Parts per scene mesh (`#mesh<N>`).
    pub meshes: Vec<Vec<FbxPart>>,
    pub materials: Vec<FbxMaterial>,
    pub textures: Vec<FbxTexture>,
    /// Depth-first node tree, and per node its world matrix.
    pub nodes: Vec<GltfNode>,
    pub worlds: Vec<glam::Mat4>,
}

/// A world matrix that flattens space (a zero scale): its geometry is not
/// drawn, and inverting it would give NaN normals.
fn degenerate(world: &Mat4) -> bool {
    let det = world.determinant();
    !det.is_finite() || det.abs() < 1e-18
}

impl FbxModel {
    /// Every mesh-carrying node (zero-scale ones left out) with its world matrix.
    fn drawn(&self) -> impl Iterator<Item = (&GltfNode, &Mat4)> {
        self.nodes
            .iter()
            .zip(&self.worlds)
            .filter(|(n, w)| n.mesh.is_some() && !degenerate(w))
    }

    /// Every part of every node's mesh with the node's world matrix baked
    /// in: (vertices, indices, material).
    pub fn baked_parts(&self) -> Vec<(Vec<Vertex>, Vec<u32>, Option<usize>)> {
        let mut out = Vec::new();
        for (node, world) in self.drawn() {
            for part in node.mesh.iter().flat_map(|&m| &self.meshes[m]) {
                let (vertices, indices) = bake(part, *world);
                out.push((vertices, indices, part.material));
            }
        }
        out
    }

    /// The material of every part `baked_parts` yields, in the same order.
    pub fn part_materials(&self) -> Vec<Option<usize>> {
        self.drawn()
            .flat_map(|(n, _)| n.mesh.iter().flat_map(|&m| &self.meshes[m]))
            .map(|p| p.material)
            .collect()
    }

    /// Bounds of the baked model, `None` without meshes.
    #[cfg(test)]
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut bounds: Option<(Vec3, Vec3)> = None;
        for (vertices, _, _) in self.baked_parts() {
            for v in vertices {
                let p = Vec3::from_array(v.position);
                bounds = Some(bounds.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))));
            }
        }
        bounds
    }
}

/// `part` under `world`: normals by the inverse transpose, tangents by the
/// matrix (the bitangent sign flips under a mirror, and so does the winding,
/// so back-face culling keeps the outside), like the glTF path.
fn bake(part: &FbxPart, world: Mat4) -> (Vec<Vertex>, Vec<u32>) {
    let normal_matrix = world.inverse().transpose();
    let mirrored = world.determinant() < 0.0;
    let vertices = part
        .vertices
        .iter()
        .map(|v| {
            let [x, y, z, w] = v.tangent;
            Vertex {
                position: world
                    .transform_point3(Vec3::from_array(v.position))
                    .to_array(),
                normal: normal_matrix
                    .transform_vector3(Vec3::from_array(v.normal))
                    .normalize_or_zero()
                    .to_array(),
                uv: v.uv,
                tangent: world
                    .transform_vector3(Vec3::new(x, y, z))
                    .normalize_or_zero()
                    .extend(if mirrored { -w } else { w })
                    .to_array(),
            }
        })
        .collect();
    let mut indices = part.indices.clone();
    if mirrored {
        for tri in indices.chunks_exact_mut(3) {
            tri.swap(1, 2);
        }
    }
    (vertices, indices)
}

fn vec3(v: ufbx::Vec3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

fn mat4(m: &ufbx::Matrix) -> Mat4 {
    let c = |x: f64, y: f64, z: f64, w: f32| Vec4::new(x as f32, y as f32, z as f32, w);
    Mat4::from_cols(
        c(m.m00, m.m10, m.m20, 0.0),
        c(m.m01, m.m11, m.m21, 0.0),
        c(m.m02, m.m12, m.m22, 0.0),
        c(m.m03, m.m13, m.m23, 1.0),
    )
}

/// Last component of a path with `/` or `\` separators.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The path a file stores for a texture, whichever of its three it has.
fn stored_path(texture: &ufbx::Texture) -> &str {
    [
        &*texture.filename,
        &*texture.relative_filename,
        &*texture.absolute_filename,
    ]
    .into_iter()
    .find(|n| !n.is_empty())
    .unwrap_or_default()
}

/// The texture's file name from whichever path the file stores (some files
/// have only the absolute one, which ufbx keeps apart from `filename`).
fn texture_file_name(texture: &ufbx::Texture) -> Option<&str> {
    Some(file_name(stored_path(texture))).filter(|n| !n.is_empty())
}

/// Project files by lower-case path and by lower-case file name, for the
/// case-insensitive texture search.
struct Index<'a> {
    by_path: HashMap<String, &'a str>,
    by_name: HashMap<String, Vec<&'a str>>,
}

impl<'a> Index<'a> {
    fn new(files: &'a [String]) -> Self {
        let mut by_path = HashMap::new();
        let mut by_name: HashMap<String, Vec<&str>> = HashMap::new();
        for f in files {
            by_path.entry(f.to_lowercase()).or_insert(f.as_str());
            by_name
                .entry(file_name(f).to_lowercase())
                .or_default()
                .push(f);
        }
        Self { by_path, by_name }
    }
}

/// Where a texture of the FBX at `model` may live, in the order searched
/// (Unity's): the stored relative path, the stored file name in the FBX's
/// folder, then a `Textures` folder in the FBX's folder and in each parent
/// up to `assets`. (The absolute path another machine wrote is reduced to
/// its file name.)
fn texture_candidates(model: &str, relative: &str, name: &str) -> Vec<String> {
    let model = model.replace('\\', "/");
    let mut base: Vec<&str> = model.split('/').collect();
    base.pop();
    let join = |dir: &[&str], tail: &str| {
        let mut path = dir.join("/");
        if !path.is_empty() {
            path.push('/');
        }
        path + tail
    };
    let mut out = Vec::new();
    let relative = relative.replace('\\', "/");
    if !relative.is_empty() && !relative.contains(':') && !relative.starts_with('/') {
        let mut parts = base.clone();
        for part in relative.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        out.push(parts.join("/"));
    }
    out.push(join(&base, name));
    let mut dir = base;
    loop {
        out.push(join(&dir, &format!("Textures/{name}")));
        if dir.last().is_none_or(|d| d.eq_ignore_ascii_case("assets")) {
            break;
        }
        dir.pop();
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.to_lowercase()));
    out
}

/// The project path of texture `t` of the model at `model`: the first of the
/// candidates in the index, else the first file of that name anywhere.
/// `Err` holds the candidates searched.
fn find_texture(
    model: &str,
    t: &ufbx::Texture,
    index: &Index,
    warn: Warn,
) -> Result<String, Vec<String>> {
    let name = texture_file_name(t).unwrap_or_default();
    let candidates = texture_candidates(model, &t.relative_filename, name);
    if let Some(path) = candidates
        .iter()
        .find_map(|c| index.by_path.get(&c.to_lowercase()))
    {
        return Ok(path.to_string());
    }
    match index.by_name.get(&name.to_lowercase()).map(Vec::as_slice) {
        Some([first, rest @ ..]) => {
            if !rest.is_empty() {
                warn(
                    name,
                    format!(
                        "{model}: texture '{name}' exists in several folders, using {first} (also {})",
                        rest.join(", ")
                    ),
                );
            }
            Ok(first.to_string())
        }
        _ => Err(candidates),
    }
}

fn load_scene(bytes: &[u8], settings: &ImportSettings, content: bool) -> Result<ufbx::SceneRoot> {
    PARSES.with(|p| p.set(p.get() + 1));
    let opts = ufbx::LoadOpts {
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        // 0 keeps the file's own unit: its numbers are taken as metres.
        target_unit_meters: if settings.use_file_units { 1.0 } else { 0.0 },
        // Meshes and node transforms come out in metres, Y up, mesh by mesh,
        // so `#mesh<N>` and the node tree agree with a glTF of the same object.
        space_conversion: ufbx::SpaceConversion::ModifyGeometry,
        geometry_transform_handling: ufbx::GeometryTransformHandling::ModifyGeometry,
        inherit_mode_handling: ufbx::InheritModeHandling::Compensate,
        pivot_handling: ufbx::PivotHandling::AdjustToPivot,
        generate_missing_normals: true,
        // Animation is not played yet (skin data is kept): the animation area
        // turns this back on.
        ignore_animation: true,
        ignore_embedded: !content,
        ..Default::default()
    };
    ufbx::load_memory(bytes, opts).map_err(|e| match e.info() {
        "" => anyhow::anyhow!("{}", e.description),
        info => anyhow::anyhow!("{}: {info}", e.description),
    })
}

fn map_value(map: &ufbx::MaterialMap) -> Option<ufbx::Vec4> {
    map.has_value.then_some(map.value_vec4)
}

fn map_texture(map: &ufbx::MaterialMap) -> Option<&ufbx::Texture> {
    map.texture
        .as_ref()
        .filter(|t| map.texture_enabled && t.type_ == ufbx::TextureType::File)
        .map(|t| &**t)
}

/// A converted material plus what needs the loaded textures to finish.
struct Converted {
    material: FbxMaterial,
    /// An opacity / transparency texture is linked (its file name).
    opacity_texture: Option<String>,
}

/// Factors from ufbx's PBR mapping (which also maps Phong / Lambert), the
/// classic diffuse colour where that has nothing. Colour factors are read
/// as linear. Where a texture is bound its colour factor is white (Blender
/// writes 0.8 diffuse and black emission for unlinked sockets).
fn convert_material(
    model: &str,
    m: &ufbx::Material,
    texture: &mut dyn FnMut(&ufbx::Texture) -> Option<usize>,
    warn: Option<Warn>,
) -> Converted {
    let (pbr, fbx) = (&m.pbr, &m.fbx);
    let mut asset = MaterialAsset::new_default();
    let base = if pbr.base_color.has_value || pbr.base_color.texture.is_some() {
        &pbr.base_color
    } else {
        &fbx.diffuse_color
    };
    let glossy = m.features.roughness_as_glossiness.enabled;
    let (metalness, roughness) = (map_texture(&pbr.metalness), map_texture(&pbr.roughness));
    let metal_rough = match (metalness, roughness) {
        (Some(a), Some(b)) if a.element.element_id == b.element.element_id && !glossy => Some(a),
        _ => None,
    };
    let normal = [&pbr.normal_map, &fbx.normal_map, &fbx.bump]
        .into_iter()
        .find_map(map_texture);
    let textures = [
        map_texture(base),
        metal_rough,
        normal,
        map_texture(&pbr.emission_color),
    ]
    .map(|t| t.and_then(&mut *texture));
    if let Some(c) = map_value(base) {
        let factor = map_value(&pbr.base_factor).map_or(1.0, |f| f.x);
        let c = if textures[0].is_some() {
            [1.0; 3]
        } else {
            [c.x, c.y, c.z]
        };
        asset.base_color = [
            (c[0] * factor) as f32,
            (c[1] * factor) as f32,
            (c[2] * factor) as f32,
            1.0,
        ];
    }
    if let Some(v) = map_value(&pbr.metalness) {
        asset.metallic = v.x as f32;
    }
    if let Some(v) = map_value(&pbr.roughness) {
        asset.roughness = if glossy { 1.0 - v.x } else { v.x } as f32;
    }
    if let Some(c) = map_value(&pbr.emission_color) {
        let factor = map_value(&pbr.emission_factor).map_or(1.0, |f| f.x);
        let c = if textures[3].is_some() {
            [1.0; 3]
        } else {
            [c.x, c.y, c.z]
        };
        asset.emissive = [
            (c[0] * factor) as f32,
            (c[1] * factor) as f32,
            (c[2] * factor) as f32,
        ];
    }
    let opacity_texture = [
        &pbr.opacity,
        &fbx.transparency_color,
        &fbx.transparency_factor,
    ]
    .into_iter()
    .find_map(map_texture)
    .map(|t| texture_file_name(t).unwrap_or_default().to_string());
    // ufbx maps `pbr.opacity` for a few shaders only; classic files keep a
    // transparency factor (1 = see-through).
    let opacity = map_value(&pbr.opacity)
        .map(|v| v.x as f32)
        .or_else(|| map_value(&fbx.transparency_factor).map(|v| 1.0 - v.x as f32));
    if opacity_texture.is_some() {
        asset.alpha_mode = AlphaMode::Mask { cutoff: 0.5 };
    } else if let Some(v) = opacity.filter(|v| *v < 0.999) {
        asset.base_color[3] = v;
        asset.alpha_mode = AlphaMode::Blend;
    }
    asset.double_sided = m.features.double_sided.enabled;
    if let Some(warn) = warn
        && metal_rough.is_none()
    {
        let rough_kind = if glossy { "glossiness" } else { "roughness" };
        for (kind, t) in [("metalness", metalness), (rough_kind, roughness)] {
            if let Some(file) = t.and_then(texture_file_name) {
                warn(
                    file,
                    format!(
                        "{model}: material '{}': {kind} texture '{file}' is not used (only one texture packed like glTF, G roughness and B metallic, is read)",
                        m.element.name
                    ),
                );
            }
        }
    }
    Converted {
        material: FbxMaterial {
            name: m.element.name.to_string(),
            asset,
            textures,
        },
        opacity_texture,
    }
}
/// Triangles of `mesh`, one part per material slot used.
fn convert_mesh(mesh: &ufbx::Mesh) -> Vec<FbxPart> {
    let slots: Vec<(u32, Option<&ufbx::MeshPart>)> = if mesh.material_parts.is_empty() {
        vec![(0, None)]
    } else {
        mesh.material_parts
            .iter()
            .map(|p| (p.index, Some(p)))
            .collect()
    };
    let has_tangents = mesh.vertex_tangent.exists && mesh.vertex_bitangent.exists;
    let mut parts = Vec::new();
    let mut corners = Vec::new();
    for (slot, part) in slots {
        let faces: Vec<u32> = match part {
            Some(p) => p.face_indices.to_vec(),
            None => (0..mesh.num_faces as u32).collect(),
        };
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut seen: HashMap<[u32; 4], u32> = HashMap::new();
        for face in faces {
            ufbx::triangulate_face_vec(&mut corners, mesh, mesh.faces[face as usize]);
            for &c in &corners {
                let c = c as usize;
                let key = [
                    mesh.vertex_position.indices[c],
                    if mesh.vertex_normal.exists {
                        mesh.vertex_normal.indices[c]
                    } else {
                        0
                    },
                    if mesh.vertex_uv.exists {
                        mesh.vertex_uv.indices[c]
                    } else {
                        0
                    },
                    if has_tangents {
                        mesh.vertex_tangent.indices[c]
                    } else {
                        0
                    },
                ];
                let index = *seen.entry(key).or_insert_with(|| {
                    let normal = if mesh.vertex_normal.exists {
                        vec3(mesh.vertex_normal[c])
                    } else {
                        Vec3::Z
                    };
                    // FBX's V runs up from the bottom, the engine's down.
                    let uv = if mesh.vertex_uv.exists {
                        let uv = mesh.vertex_uv[c];
                        [uv.x as f32, 1.0 - uv.y as f32]
                    } else {
                        [0.0; 2]
                    };
                    let tangent = if has_tangents {
                        let t = vec3(mesh.vertex_tangent[c]);
                        let b = vec3(mesh.vertex_bitangent[c]);
                        let w = if normal.cross(t).dot(b) < 0.0 {
                            -1.0
                        } else {
                            1.0
                        };
                        t.extend(w).to_array()
                    } else {
                        [0.0; 4]
                    };
                    vertices.push(Vertex {
                        position: vec3(mesh.vertex_position[c]).to_array(),
                        normal: normal.to_array(),
                        uv,
                        tangent,
                    });
                    vertices.len() as u32 - 1
                });
                indices.push(index);
            }
        }
        if indices.is_empty() {
            continue;
        }
        if !has_tangents {
            crate::generate_tangents(&mut vertices, &indices);
        }
        parts.push(FbxPart {
            vertices,
            indices,
            material: mesh
                .materials
                .get(slot as usize)
                .map(|m| m.element.typed_id as usize),
        });
    }
    parts
}

/// The node tree (depth first) and each node's world matrix, with the
/// scale and forward flip of `settings` applied: positions scale, and the
/// roots turn about Y.
fn build_nodes(scene: &ufbx::Scene, settings: &ImportSettings) -> (Vec<GltfNode>, Vec<Mat4>) {
    let mut nodes = Vec::new();
    let mut worlds = Vec::new();
    let mut stack: Vec<(&ufbx::Node, Option<usize>)> = scene
        .root_node
        .children
        .iter()
        .rev()
        .map(|c| (&**c, None))
        .collect();
    while let Some((node, parent)) = stack.pop() {
        let t = &node.local_transform;
        let index = nodes.len();
        nodes.push(GltfNode {
            name: (!node.element.name.is_empty()).then(|| node.element.name.to_string()),
            parent,
            translation: vec3(t.translation),
            rotation: Quat::from_xyzw(
                t.rotation.x as f32,
                t.rotation.y as f32,
                t.rotation.z as f32,
                t.rotation.w as f32,
            ),
            scale: vec3(t.scale),
            mesh: node.mesh.as_ref().map(|m| m.element.typed_id as usize),
        });
        worlds.push(mat4(&node.geometry_to_world));
        stack.extend(node.children.iter().rev().map(|c| (&**c, Some(index))));
    }
    let flip = Quat::from_rotation_y(std::f32::consts::PI);
    for (node, world) in nodes.iter_mut().zip(&mut worlds) {
        node.translation *= settings.scale;
        world.w_axis = (world.w_axis.truncate() * settings.scale).extend(1.0);
        if settings.forward_flip {
            *world = Mat4::from_quat(flip) * *world;
            if node.parent.is_none() {
                node.translation = flip * node.translation;
                node.rotation = flip * node.rotation;
            }
        }
    }
    (nodes, worlds)
}

/// Nodes and bounds of the FBX in `bytes` without converting any mesh:
/// positions and world matrices only.
pub(crate) fn summary(bytes: &[u8], settings: &ImportSettings) -> Result<crate::ModelSummary> {
    let scene = load_scene(bytes, settings, false)?;
    let (nodes, worlds) = build_nodes(&scene, settings);
    let mut bounds: Option<(Vec3, Vec3)> = None;
    for (node, world) in nodes.iter().zip(&worlds) {
        let Some(mesh) = node.mesh.filter(|_| !degenerate(world)) else {
            continue;
        };
        for v in scene.meshes[mesh].vertex_position.values.iter() {
            let p = world.transform_point3(vec3(*v) * settings.scale);
            bounds = Some(bounds.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))));
        }
    }
    Ok(crate::ModelSummary { nodes, bounds })
}

/// `import` with the default settings.
#[cfg(test)]
pub(crate) fn import_plain(
    name: &str,
    bytes: &[u8],
    sources: Option<&Sources>,
) -> Result<FbxModel> {
    import(name, bytes, &ImportSettings::default(), sources)
}

/// Any texel of the encoded image that is not fully opaque.
fn see_through(bytes: &[u8]) -> bool {
    image::load_from_memory(bytes)
        .is_ok_and(|i| i.color().has_alpha() && i.to_rgba8().pixels().any(|p| p[3] < 255))
}

/// Parse `bytes` (the FBX at project path `name`). With `sources`, textures
/// are loaded (embedded content first, then `find_texture`); a missing one
/// is reported through `Sources::warn` and stays empty.
pub(crate) fn import(
    name: &str,
    bytes: &[u8],
    settings: &ImportSettings,
    sources: Option<&Sources>,
) -> Result<FbxModel> {
    let scene =
        load_scene(bytes, settings, true).with_context(|| format!("failed to read FBX {name}"))?;
    let warn: Option<Warn> = sources.map(|s| s.warn);
    let index = Index::new(sources.map_or(&[], |s| s.index));
    let mut textures: Vec<FbxTexture> = Vec::new();
    let mut by_element: HashMap<u32, Option<usize>> = HashMap::new();
    let mut load_texture = |t: &ufbx::Texture| -> Option<usize> {
        let sources = sources?;
        *by_element.entry(t.element.element_id).or_insert_with(|| {
            let file = texture_file_name(t).unwrap_or_default().to_string();
            let (bytes, path) = if !t.content.is_empty() {
                (t.content.to_vec(), None)
            } else {
                match find_texture(name, t, &index, sources.warn) {
                    Ok(path) => match (sources.read)(&path) {
                        Ok(bytes) => (bytes, Some(path)),
                        Err(error) => {
                            (sources.warn)(
                                &file,
                                format!("{name}: texture '{file}' at {path} could not be read: {error}; using the default"),
                            );
                            return None;
                        }
                    },
                    Err(searched) => {
                        (sources.warn)(
                            &file,
                            format!(
                                "{name}: texture '{file}' not found (stored as '{}'); searched {}, then every folder of the project; using the default",
                                stored_path(t),
                                searched.join(", ")
                            ),
                        );
                        return None;
                    }
                }
            };
            textures.push(FbxTexture {
                file_name: file,
                path,
                bytes,
            });
            Some(textures.len() - 1)
        })
    };
    let converted: Vec<Converted> = scene
        .materials
        .iter()
        .map(|m| convert_material(name, m, &mut load_texture, warn))
        .collect();
    let mut alpha: HashMap<usize, bool> = HashMap::new();
    let materials = converted
        .into_iter()
        .map(|c| {
            let mut material = c.material;
            let base_alpha = material.textures[0]
                .is_some_and(|t| *alpha.entry(t).or_insert_with(|| see_through(&textures[t].bytes)));
            if base_alpha && material.asset.alpha_mode == AlphaMode::Opaque {
                material.asset.alpha_mode = AlphaMode::Mask { cutoff: 0.5 };
            }
            if let (Some(file), Some(warn)) = (c.opacity_texture, warn)
                && !base_alpha
            {
                warn(
                    &file,
                    format!(
                        "{name}: material '{}': opacity texture '{file}' is not used (cutouts read the base colour texture's alpha)",
                        material.name
                    ),
                );
            }
            material
        })
        .collect();
    let mut meshes: Vec<Vec<FbxPart>> = scene.meshes.iter().map(|m| convert_mesh(m)).collect();
    if settings.scale != 1.0 {
        for v in meshes.iter_mut().flatten().flat_map(|p| &mut p.vertices) {
            v.position = (Vec3::from_array(v.position) * settings.scale).to_array();
        }
    }
    let (nodes, worlds) = build_nodes(&scene, settings);
    let flat = nodes
        .iter()
        .zip(&worlds)
        .filter(|(n, w)| n.mesh.is_some() && degenerate(w))
        .count();
    if let (true, Some(warn)) = (flat > 0, warn) {
        warn(
            "(zero scale)",
            format!("{name}: {flat} node(s) with a zero scale are not drawn"),
        );
    }
    Ok(FbxModel {
        meshes,
        materials,
        textures,
        nodes,
        worlds,
    })
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Hand-written ASCII FBX: a 1 x 2 x 3 box.

    pub struct Cube {
        /// Z up, centimetres (else Y up, metres); same box either way.
        pub z_up_cm: bool,
        pub translation: [f64; 3],
        pub scaling: [f64; 3],
        /// Faces 0..3 use material 0, faces 3..6 material 1 (else all 0).
        pub two_materials: bool,
        /// Relative file name of a texture on material 0's diffuse.
        pub texture: Option<&'static str>,
        /// Embedded content of that texture.
        pub embedded: Option<Vec<u8>>,
        /// Material properties the texture is linked to (default: DiffuseColor).
        pub slots: Vec<&'static str>,
        /// Extra `P:` lines of material 0 (without the leading tab).
        pub props: Vec<String>,
        /// Number of mesh nodes (copies of the box sharing the materials).
        pub meshes: usize,
    }

    impl Default for Cube {
        fn default() -> Self {
            Self {
                z_up_cm: false,
                translation: [1.0, 2.0, 3.0],
                scaling: [1.0; 3],
                two_materials: false,
                texture: None,
                embedded: None,
                slots: Vec::new(),
                props: Vec::new(),
                meshes: 1,
            }
        }
    }

    /// Corner indices of each face, counter-clockwise seen from outside.
    const FACES: [[usize; 4]; 6] = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
    ];
    const NORMALS: [[f64; 3]; 6] = [
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];

    pub fn base64(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16
                | (*c.get(1).unwrap_or(&0) as u32) << 8
                | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn list(values: impl IntoIterator<Item = f64>) -> String {
        let v: Vec<String> = values.into_iter().map(|v| v.to_string()).collect();
        format!("*{} {{ a: {} }}", v.len(), v.join(","))
    }

    impl Cube {
        /// Y-up metres to the file's space (Z up, cm: a quarter turn about X).
        fn conv(&self, p: [f64; 3]) -> [f64; 3] {
            if self.z_up_cm {
                [p[0] * 100.0, -p[2] * 100.0, p[1] * 100.0]
            } else {
                p
            }
        }

        fn conv_dir(&self, n: [f64; 3]) -> [f64; 3] {
            if self.z_up_cm { [n[0], -n[2], n[1]] } else { n }
        }

        pub fn ascii(&self) -> String {
            // UnitScaleFactor is centimetres per unit.
            let (up, front, front_sign, unit) = if self.z_up_cm {
                (2, 1, -1, 1)
            } else {
                (1, 2, 1, 100)
            };
            let corner = |i: usize| {
                self.conv([
                    if i & 1 == 0 { -0.5 } else { 0.5 },
                    if i & 2 == 0 { -1.0 } else { 1.0 },
                    if i & 4 == 0 { -1.5 } else { 1.5 },
                ])
            };
            let vertices = list((0..8).flat_map(corner));
            let mut indices = Vec::new();
            let mut normals = Vec::new();
            for (f, face) in FACES.iter().enumerate() {
                for (k, &c) in face.iter().enumerate() {
                    indices.push(if k == 3 { -(c as f64) - 1.0 } else { c as f64 });
                    normals.extend(self.conv_dir(NORMALS[f]));
                }
            }
            let (pvi, normals) = (list(indices), list(normals));
            let uvs = list([0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0]);
            let uv_index = list((0..6).flat_map(|_| [0.0, 1.0, 2.0, 3.0]));
            let (mapping, materials) = if self.two_materials {
                ("ByPolygon", list([0.0, 0.0, 0.0, 1.0, 1.0, 1.0]))
            } else {
                ("AllSame", list([0.0]))
            };
            let [tx, ty, tz] = self.conv(self.translation);
            let [sx, sy, sz] = self.scaling;
            let props: String = self.props.iter().map(|p| format!("\t\t\t{p}\n")).collect();
            let mut text = format!(
                r#"; FBX 7.3.0 project file
FBXHeaderExtension:  {{
	FBXHeaderVersion: 1003
	FBXVersion: 7300
}}
GlobalSettings:  {{
	Version: 1000
	Properties70:  {{
		P: "UpAxis", "int", "Integer", "",{up}
		P: "UpAxisSign", "int", "Integer", "",1
		P: "FrontAxis", "int", "Integer", "",{front}
		P: "FrontAxisSign", "int", "Integer", "",{front_sign}
		P: "CoordAxis", "int", "Integer", "",0
		P: "CoordAxisSign", "int", "Integer", "",1
		P: "UnitScaleFactor", "double", "Number", "",{unit}
	}}
}}
Objects:  {{
	Geometry: 1000, "Geometry::Box", "Mesh" {{
		Vertices: {vertices}
		PolygonVertexIndex: {pvi}
		GeometryVersion: 124
		LayerElementNormal: 0 {{
			Version: 101
			Name: ""
			MappingInformationType: "ByPolygonVertex"
			ReferenceInformationType: "Direct"
			Normals: {normals}
		}}
		LayerElementUV: 0 {{
			Version: 101
			Name: "UVMap"
			MappingInformationType: "ByPolygonVertex"
			ReferenceInformationType: "IndexToDirect"
			UV: {uvs}
			UVIndex: {uv_index}
		}}
		LayerElementMaterial: 0 {{
			Version: 101
			Name: ""
			MappingInformationType: "{mapping}"
			ReferenceInformationType: "IndexToDirect"
			Materials: {materials}
		}}
		Layer: 0 {{
			Version: 100
			LayerElement:  {{
				Type: "LayerElementNormal"
				TypedIndex: 0
			}}
			LayerElement:  {{
				Type: "LayerElementMaterial"
				TypedIndex: 0
			}}
			LayerElement:  {{
				Type: "LayerElementUV"
				TypedIndex: 0
			}}
		}}
	}}
	Model: 2000, "Model::Box", "Mesh" {{
		Version: 232
		Properties70:  {{
			P: "Lcl Translation", "Lcl Translation", "", "A",{tx},{ty},{tz}
			P: "Lcl Scaling", "Lcl Scaling", "", "A",{sx},{sy},{sz}
		}}
	}}
	Material: 3000, "Material::Red", "" {{
		Version: 102
		ShadingModel: "phong"
		Properties70:  {{
			P: "DiffuseColor", "Color", "", "A",0.8,0.2,0.1
			P: "EmissiveColor", "Color", "", "A",0.1,0.2,0.3
{props}
		}}
	}}
"#
            );
            let mut extra_connections = String::new();
            if self.meshes > 1 {
                let start = text.find("\tGeometry: 1000").unwrap();
                let end = text.find("\tMaterial: 3000").unwrap();
                let segment = text[start..end].to_string();
                for k in 1..self.meshes as i64 {
                    let copy = segment
                        .replace("Geometry: 1000", &format!("Geometry: {}", 1000 + 10 * k))
                        .replace("Model: 2000", &format!("Model: {}", 2000 + 10 * k));
                    text.insert_str(end, &copy);
                    extra_connections += &format!(
                        "\tC: \"OO\",{m},0\n\tC: \"OO\",{g},{m}\n\tC: \"OO\",3000,{m}\n",
                        m = 2000 + 10 * k,
                        g = 1000 + 10 * k
                    );
                    if self.two_materials {
                        extra_connections += &format!("\tC: \"OO\",3001,{}\n", 2000 + 10 * k);
                    }
                }
            }
            if self.two_materials {
                text += r#"	Material: 3001, "Material::Blue", "" {
		Version: 102
		ShadingModel: "lambert"
		Properties70:  {
			P: "DiffuseColor", "Color", "", "A",0.0,0.0,1.0
		}
	}
"#;
            }
            if let Some(file) = self.texture {
                text += &format!(
                    "\tTexture: 4000, \"Texture::t\", \"\" {{\n\t\tType: \"TextureVideoClip\"\n\t\tVersion: 202\n\t\tTextureName: \"Texture::t\"\n\t\tMedia: \"Video::t\"\n\t\tFileName: \"C:\\\\somewhere\\\\{file}\"\n\t\tRelativeFilename: \"{file}\"\n\t}}\n"
                );
                if let Some(data) = &self.embedded {
                    text += &format!(
                        "\tVideo: 5000, \"Video::t\", \"Clip\" {{\n\t\tType: \"Clip\"\n\t\tFileName: \"C:\\\\somewhere\\\\{file}\"\n\t\tRelativeFilename: \"{file}\"\n\t\tContent: \"{}\"\n\t}}\n",
                        base64(data)
                    );
                }
            }
            text += "}\nConnections:  {\n\tC: \"OO\",2000,0\n\tC: \"OO\",1000,2000\n\tC: \"OO\",3000,2000\n";
            text += &extra_connections;
            if self.two_materials {
                text += "\tC: \"OO\",3001,2000\n";
            }
            if self.texture.is_some() {
                let slots: &[&str] = if self.slots.is_empty() {
                    &["DiffuseColor"]
                } else {
                    &self.slots
                };
                for slot in slots {
                    text += &format!("\tC: \"OP\",4000,3000, \"{slot}\"\n");
                }
                if self.embedded.is_some() {
                    text += "\tC: \"OO\",5000,4000\n";
                }
            }
            text + "}\n"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::Cube;
    use super::*;
    use glam::Vec3;

    fn load(cube: &Cube) -> FbxModel {
        import_plain("assets/models/box.fbx", cube.ascii().as_bytes(), None).unwrap()
    }

    /// World-baked vertices of every part, with their triangles.
    fn baked(model: &FbxModel) -> Vec<(Vec<Vertex>, Vec<u32>)> {
        model
            .baked_parts()
            .into_iter()
            .map(|p| (p.0, p.1))
            .collect()
    }

    fn bounds(model: &FbxModel) -> (Vec3, Vec3) {
        model.bounds().unwrap()
    }

    #[test]
    fn cube_has_counts_and_bounds_in_metres_y_up() {
        let model = load(&Cube::default());
        let parts = baked(&model);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0.len(), 24);
        assert_eq!(parts[0].1.len(), 36);
        let (lo, hi) = bounds(&model);
        assert!(lo.abs_diff_eq(Vec3::new(0.5, 1.0, 1.5), 1e-4), "{lo}");
        assert!(hi.abs_diff_eq(Vec3::new(1.5, 3.0, 4.5), 1e-4), "{hi}");
    }

    #[test]
    fn centimetre_z_up_file_matches_its_metre_y_up_twin() {
        let (lo, hi) = bounds(&load(&Cube::default()));
        let (zlo, zhi) = bounds(&load(&Cube {
            z_up_cm: true,
            ..Cube::default()
        }));
        assert!(lo.abs_diff_eq(zlo, 1e-3), "{lo} vs {zlo}");
        assert!(hi.abs_diff_eq(zhi, 1e-3), "{hi} vs {zhi}");
    }

    /// Every triangle faces away from the box centre and agrees with its
    /// vertex normals.
    fn assert_outward(model: &FbxModel) {
        let (lo, hi) = bounds(model);
        let centre = (lo + hi) / 2.0;
        for (vertices, indices) in baked(model) {
            for tri in indices.chunks(3) {
                let p = |i: u32| Vec3::from_array(vertices[i as usize].position);
                let n = (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]));
                let mid = (p(tri[0]) + p(tri[1]) + p(tri[2])) / 3.0;
                assert!(n.dot(mid - centre) > 0.0, "triangle faces inward");
                let vn = Vec3::from_array(vertices[tri[0] as usize].normal);
                assert!(n.dot(vn) > 0.0, "winding disagrees with the normal");
            }
        }
    }

    #[test]
    fn mirrored_node_keeps_outward_faces() {
        assert_outward(&load(&Cube::default()));
        assert_outward(&load(&Cube {
            scaling: [-1.0, 1.0, 1.0],
            ..Cube::default()
        }));
        assert_outward(&load(&Cube {
            z_up_cm: true,
            ..Cube::default()
        }));
    }

    #[test]
    fn faces_split_into_one_part_per_material() {
        let model = load(&Cube {
            two_materials: true,
            ..Cube::default()
        });
        assert_eq!(model.materials.len(), 2);
        let parts = &model.meshes[0];
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].indices.len(), 18);
        assert_eq!(parts[1].indices.len(), 18);
        assert_ne!(parts[0].material, parts[1].material);
        let blue = &model.materials[parts[1].material.unwrap()];
        assert_eq!(blue.asset.base_color, [0.0, 0.0, 1.0, 1.0]);
        let red = &model.materials[parts[0].material.unwrap()];
        assert!((red.asset.base_color[0] - 0.8).abs() < 1e-6);
        assert!((red.asset.emissive[2] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn nodes_list_the_hierarchy_with_mesh_indices() {
        let model = load(&Cube::default());
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.nodes[0].mesh, Some(0));
        assert_eq!(model.nodes[0].parent, None);
        assert!(
            model.nodes[0]
                .translation
                .abs_diff_eq(Vec3::new(1.0, 2.0, 3.0), 1e-4)
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(import_plain("a.fbx", b"this is not an fbx file at all", None).is_err());
        assert!(import_plain("a.fbx", b"", None).is_err());
    }

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    /// The box with texture `tex/t.png` imported with `files` as the project
    /// (every file reads as its own path); the model's texture and the warnings.
    fn resolve(
        model: &str,
        embedded: Option<Vec<u8>>,
        files: &[&str],
    ) -> (Option<FbxTexture>, Vec<String>) {
        let cube = Cube {
            texture: Some("tex/t.png"),
            embedded,
            ..Cube::default()
        };
        let index: Vec<String> = files.iter().map(|f| f.to_string()).collect();
        let warnings = std::cell::RefCell::new(Vec::new());
        let read = |p: &str| {
            if index.iter().any(|f| f == p) {
                Ok(p.as_bytes().to_vec())
            } else {
                Err(std::io::ErrorKind::NotFound.into())
            }
        };
        let warn = |_: &str, m: String| warnings.borrow_mut().push(m);
        let sources = Sources {
            read: &read,
            index: &index,
            warn: &warn,
        };
        let model = import_plain(model, cube.ascii().as_bytes(), Some(&sources)).unwrap();
        let slot = model.materials[0].textures[0];
        let texture = slot.map(|t| model.textures.into_iter().nth(t).unwrap());
        (texture, warnings.into_inner())
    }

    fn found(model: &str, files: &[&str]) -> Option<String> {
        resolve(model, None, files).0?.path
    }

    #[test]
    fn texture_embedded_content_wins() {
        let (texture, _) = resolve(
            "assets/models/box.fbx",
            Some(png()),
            &["assets/models/tex/t.png"],
        );
        let texture = texture.unwrap();
        assert_eq!(texture.bytes, png());
        assert_eq!(texture.path, None);
    }

    #[test]
    fn texture_is_found_relative_to_the_fbx() {
        assert_eq!(
            found("assets/models/box.fbx", &["assets/models/tex/t.png"]).as_deref(),
            Some("assets/models/tex/t.png")
        );
    }

    #[test]
    fn texture_falls_back_to_the_same_name_next_to_the_fbx() {
        assert_eq!(
            found("assets/models/box.fbx", &["assets/models/t.png"]).as_deref(),
            Some("assets/models/t.png")
        );
    }

    #[test]
    fn texture_search_ignores_case_in_folders_and_names() {
        assert_eq!(
            found("assets/models/box.fbx", &["assets/Models/TEX/T.PNG"]).as_deref(),
            Some("assets/Models/TEX/T.PNG")
        );
    }

    #[test]
    fn texture_search_tries_textures_folders_up_to_assets() {
        for file in [
            "assets/models/Textures/t.png",
            "assets/models/textures/t.png",
            "assets/Textures/t.png",
        ] {
            assert_eq!(
                found("assets/models/box.fbx", &[file]).as_deref(),
                Some(file)
            );
        }
        // The nearest `Textures` folder wins over a farther one.
        assert_eq!(
            found(
                "assets/models/x/box.fbx",
                &["assets/Textures/t.png", "assets/models/Textures/t.png"]
            )
            .as_deref(),
            Some("assets/models/Textures/t.png")
        );
    }

    #[test]
    fn texture_search_falls_back_to_the_first_file_of_that_name_in_the_project() {
        let (texture, warnings) = resolve(
            "assets/models/box.fbx",
            None,
            &["assets/props/a/T.png", "assets/props/b/t.png"],
        );
        assert_eq!(
            texture.unwrap().path.as_deref(),
            Some("assets/props/a/T.png")
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("several folders"), "{warnings:?}");
        let (texture, warnings) = resolve("assets/models/box.fbx", None, &["assets/props/a/t.png"]);
        assert!(texture.is_some() && warnings.is_empty());
    }

    #[test]
    fn missing_texture_names_the_model_the_stored_path_and_what_was_searched() {
        let (texture, warnings) = resolve("assets/models/box.fbx", None, &["assets/other.png"]);
        assert!(texture.is_none());
        assert_eq!(warnings.len(), 1);
        let w = &warnings[0];
        for part in [
            "assets/models/box.fbx",
            "'t.png'",
            "tex/t.png",
            "assets/models/tex/t.png",
            "assets/models/Textures/t.png",
            "assets/Textures/t.png",
            "every folder of the project",
        ] {
            assert!(w.contains(part), "{part} not in {w}");
        }
    }
}
