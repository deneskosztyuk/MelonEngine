//! GPU models per asset handle: loaded on first use, reloaded when the
//! `AssetServer` bumps the file's version (hot reload).

use std::collections::{HashMap, HashSet};

use melon_core::assets::{BUILTIN_PREFIX, split_mesh_fragment};
use melon_core::{AssetServer, Handle, Model};

use crate::{LoadedModel, SceneRenderer};

struct CachedModel {
    version: u32,
    model: LoadedModel,
}

pub struct ModelCache {
    models: HashMap<u32, CachedModel>,
    builtin_cube: LoadedModel,
    /// (handle id, version) pairs that failed to load, so each failure is
    /// reported once and not retried every frame.
    failed: HashSet<(u32, u32)>,
}

impl ModelCache {
    /// Import `file` once and build all its meshes: return mesh `mesh` (for
    /// `handle`) and cache every other mesh under its own `#mesh<i>` handle,
    /// so a model's fragments don't each import the whole file.
    fn load_fragments(
        &mut self,
        scene: &SceneRenderer,
        server: &mut AssetServer,
        handle: Handle<Model>,
        file_path: &str,
        file: &std::path::Path,
        mesh: usize,
    ) -> anyhow::Result<LoadedModel> {
        let mut ours = None;
        for (i, model) in scene
            .load_gltf_meshes(&file.to_string_lossy())?
            .into_iter()
            .enumerate()
        {
            if i == mesh {
                ours = Some(model);
                continue;
            }
            let Ok(model) = model else { continue };
            let sibling: Handle<Model> = server.load(&format!("{file_path}#mesh{i}"));
            if sibling == handle {
                continue;
            }
            server.set_bounds(sibling, model.bounds.0, model.bounds.1);
            let version = server.version(sibling);
            self.models
                .insert(sibling.id(), CachedModel { version, model });
        }
        ours.unwrap_or_else(|| Err(anyhow::anyhow!("{file_path} has no mesh {mesh}")))
    }

    pub fn new(scene: &SceneRenderer) -> Self {
        Self {
            models: HashMap::new(),
            builtin_cube: scene.builtin_cube_model(),
            failed: HashSet::new(),
        }
    }

    /// `prepare` then `model`.
    pub fn get(
        &mut self,
        scene: &SceneRenderer,
        server: &mut AssetServer,
        handle: Handle<Model>,
    ) -> &LoadedModel {
        self.prepare(scene, server, handle);
        self.model(server, handle)
    }

    /// The model to draw for `handle` (after `prepare`): the loaded model,
    /// or the built-in cube for `builtin:cube` and for anything not loaded.
    pub fn model(&self, server: &AssetServer, handle: Handle<Model>) -> &LoadedModel {
        if server.path(handle) == "builtin:cube" {
            return &self.builtin_cube;
        }
        self.models
            .get(&handle.id())
            .map_or(&self.builtin_cube, |m| &m.model)
    }

    /// Load `handle` on first use or after its file changed. On a load error
    /// the previous version stays in use, or the built-in cube if there is
    /// none yet; the error is logged once per file version.
    pub fn prepare(
        &mut self,
        scene: &SceneRenderer,
        server: &mut AssetServer,
        handle: Handle<Model>,
    ) {
        let id = handle.id();
        let path = server.path(handle).to_string();
        if path == "builtin:cube" {
            if server.bounds(handle).is_none() {
                let (min, max) = self.builtin_cube.bounds;
                server.set_bounds(handle, min, max);
            }
            return;
        }

        let version = server.version(handle);
        let stale = self.models.get(&id).is_none_or(|m| m.version != version);
        if stale && !self.failed.contains(&(id, version)) {
            let result = match server.full_path(handle) {
                Some(file) => match split_mesh_fragment(&path) {
                    (file_path, Some(mesh)) => {
                        self.load_fragments(scene, server, handle, file_path, &file, mesh)
                    }
                    (_, None) => scene.load_gltf(&file.to_string_lossy()),
                },
                None => Err(anyhow::anyhow!(
                    "unknown built-in asset (expected {BUILTIN_PREFIX}cube)"
                )),
            };
            match result {
                Ok(model) => {
                    server.set_bounds(handle, model.bounds.0, model.bounds.1);
                    self.models.insert(id, CachedModel { version, model });
                }
                Err(error) => {
                    tracing::warn!("could not load model {path}: {error:#}");
                    self.failed.insert((id, version));
                }
            }
        }
    }
}
