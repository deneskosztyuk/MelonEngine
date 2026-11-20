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

/// Where a model's glTF bytes come from: a file on disk (dev builds, so
/// external buffers and textures resolve) or memory (the pack).
enum Source {
    File(std::path::PathBuf),
    Packed { name: String, bytes: Vec<u8> },
}

impl Source {
    fn of(server: &AssetServer, handle: Handle<Model>, path: &str) -> anyhow::Result<Self> {
        if server.has_pack() {
            let name = split_mesh_fragment(path).0.to_string();
            let bytes = server
                .read(&name)
                .map_err(|e| anyhow::anyhow!("{name} is not in the pack: {e}"))?;
            return Ok(Self::Packed { name, bytes });
        }
        server.full_path(handle).map(Self::File).ok_or_else(|| {
            anyhow::anyhow!("unknown built-in asset (expected {BUILTIN_PREFIX}cube)")
        })
    }

    fn model(&self, scene: &SceneRenderer, path: &str) -> anyhow::Result<LoadedModel> {
        match self {
            Self::File(file) => scene.load_gltf(&file.to_string_lossy()),
            Self::Packed { bytes, .. } => scene
                .load_gltf_bytes(bytes)
                .map_err(|e| e.context(format!("packed model {path}"))),
        }
    }

    fn meshes(&self, scene: &SceneRenderer) -> anyhow::Result<Vec<anyhow::Result<LoadedModel>>> {
        match self {
            Self::File(file) => scene.load_gltf_meshes(&file.to_string_lossy()),
            Self::Packed { name, bytes } => scene.load_gltf_meshes_bytes(name, bytes),
        }
    }
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
        source: &Source,
        mesh: usize,
    ) -> anyhow::Result<LoadedModel> {
        let mut ours = None;
        for (i, model) in source.meshes(scene)?.into_iter().enumerate() {
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

    /// `handle` has a loaded model in the cache.
    pub fn contains(&self, handle: Handle<Model>) -> bool {
        self.models.contains_key(&handle.id())
    }

    /// Drop `handle`'s GPU model (it loads again on its next use).
    pub fn evict(&mut self, handle: Handle<Model>) {
        self.models.remove(&handle.id());
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
            let result = match Source::of(server, handle, &path) {
                Ok(source) => match split_mesh_fragment(&path) {
                    (file_path, Some(mesh)) => {
                        self.load_fragments(scene, server, handle, file_path, &source, mesh)
                    }
                    (_, None) => source.model(scene, &path),
                },
                Err(error) => Err(error),
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
