//! Triangles of an entity's model, for mesh colliders. Reads the glTF file
//! again (positions and indices only); a broken or missing model warns and
//! gives `None`.

use std::path::Path;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use glam::{Mat4, Vec3};
use melon_core::assets::split_mesh_fragment;
use melon_core::{AssetServer, Handle, Model};

type Triangles = (Vec<Vec3>, Vec<[u32; 3]>);

/// The model's triangles in the entity's local space (before its scale).
/// `x.glb#meshN` gives that mesh unbaked; a whole file gives every mesh of
/// its default scene with the node transforms applied.
pub(crate) fn triangles(world: &World, entity: Entity) -> Option<Triangles> {
    let (Some(handle), Some(server)) = (
        world.get::<Handle<Model>>(entity).copied(),
        world.get_resource::<AssetServer>(),
    ) else {
        tracing::warn!("mesh collider of entity {entity}: it has no model");
        return None;
    };
    let path = server.path(handle);
    if path == "builtin:cube" {
        return Some(unit_cube());
    }
    let result = server
        .full_path(handle)
        .ok_or_else(|| format!("no mesh data for {path}"))
        .and_then(|file| read_gltf(&file, split_mesh_fragment(path).1));
    match result {
        Ok(t) if !t.1.is_empty() => Some(t),
        Ok(_) => {
            tracing::warn!("mesh collider of entity {entity}: {path} has no triangles");
            None
        }
        Err(error) => {
            tracing::warn!("mesh collider of entity {entity}: {error}");
            None
        }
    }
}

fn unit_cube() -> Triangles {
    let vertices = (0..8)
        .map(|i| {
            Vec3::new(
                if i & 1 == 0 { -0.5 } else { 0.5 },
                if i & 2 == 0 { -0.5 } else { 0.5 },
                if i & 4 == 0 { -0.5 } else { 0.5 },
            )
        })
        .collect();
    let quad = |a, b, c, d| [[a, b, c], [a, c, d]];
    let indices = [
        quad(0, 1, 3, 2),
        quad(4, 6, 7, 5),
        quad(0, 4, 5, 1),
        quad(2, 3, 7, 6),
        quad(0, 2, 6, 4),
        quad(1, 5, 7, 3),
    ]
    .into_iter()
    .flatten()
    .collect();
    (vertices, indices)
}

fn read_gltf(file: &Path, mesh: Option<usize>) -> Result<Triangles, String> {
    let gltf::Gltf { document, blob } =
        gltf::Gltf::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let buffers = gltf::import_buffers(&document, file.parent(), blob)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let mut out = (Vec::new(), Vec::new());
    match mesh {
        Some(n) => {
            let m = document
                .meshes()
                .nth(n)
                .ok_or_else(|| format!("{} has no mesh {n}", file.display()))?;
            append(&mut out, &m, Mat4::IDENTITY, &buffers);
        }
        None => {
            let scene = document
                .default_scene()
                .or_else(|| document.scenes().next())
                .ok_or_else(|| format!("{} has no scene", file.display()))?;
            let mut stack: Vec<(gltf::Node, Mat4)> =
                scene.nodes().map(|n| (n, Mat4::IDENTITY)).collect();
            while let Some((node, parent)) = stack.pop() {
                let m = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
                if let Some(mesh) = node.mesh() {
                    append(&mut out, &mesh, m, &buffers);
                }
                stack.extend(node.children().map(|c| (c, m)));
            }
        }
    }
    Ok(out)
}

fn append(out: &mut Triangles, mesh: &gltf::Mesh, m: Mat4, buffers: &[gltf::buffer::Data]) {
    for primitive in mesh.primitives() {
        if primitive.mode() != gltf::mesh::Mode::Triangles {
            continue;
        }
        let reader = primitive.reader(|b| Some(&buffers[b.index()]));
        let Some(positions) = reader.read_positions() else {
            continue;
        };
        let base = out.0.len() as u32;
        out.0
            .extend(positions.map(|p| m.transform_point3(Vec3::from_array(p))));
        let count = out.0.len() as u32 - base;
        let indices: Vec<u32> = match reader.read_indices() {
            Some(i) => i.into_u32().collect(),
            None => (0..count).collect(),
        };
        out.1.extend(
            indices
                .chunks_exact(3)
                .map(|t| [base + t[0], base + t[1], base + t[2]]),
        );
    }
}
