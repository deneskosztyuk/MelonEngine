//! GPU models per asset handle: loaded on first use, reloaded when the
//! `AssetServer` bumps the file's version (hot reload).

use std::collections::{HashMap, HashSet};

use melon_core::assets::BUILTIN_PREFIX;
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
    pub fn new(scene: &SceneRenderer) -> Self {
        Self {
            models: HashMap::new(),
            builtin_cube: scene.builtin_cube_model(),
            failed: HashSet::new(),
        }
    }

    /// The model to draw for `handle`. Loads it on first use or after its file
    /// changed. On a load error the previous version stays in use, or the
    /// built-in cube if there is none yet; the error is logged once.
    pub fn get(
        &mut self,
        scene: &SceneRenderer,
        server: &mut AssetServer,
        handle: Handle<Model>,
    ) -> &LoadedModel {
        let id = handle.id();
        let path = server.path(handle).to_string();
        if path == "builtin:cube" {
            if server.bounds(handle).is_none() {
                let (min, max) = self.builtin_cube.bounds;
                server.set_bounds(handle, min, max);
            }
            return &self.builtin_cube;
        }

        let version = server.version(handle);
        let stale = self.models.get(&id).is_none_or(|m| m.version != version);
        if stale && !self.failed.contains(&(id, version)) {
            let result = match server.full_path(handle) {
                Some(file) => scene.load_gltf(&file.to_string_lossy()),
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
        self.models
            .get(&id)
            .map_or(&self.builtin_cube, |m| &m.model)
    }
}
