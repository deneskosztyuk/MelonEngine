//! GPU models per asset handle: loaded on first use, reloaded when the
//! `AssetServer` bumps the file's version (hot reload) or the version of a
//! texture file the model read.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use melon_core::assets::{BUILTIN_PREFIX, split_mesh_fragment};
use melon_core::{AssetServer, Handle, Model, Texture};

use crate::fbx::{Sources, is_fbx};
use crate::{LoadedModel, SceneRenderer};

struct CachedModel {
    version: u32,
    /// The texture files the model read and their versions then.
    textures: Vec<(Handle<Texture>, u32)>,
    model: LoadedModel,
}

/// Where a model's glTF bytes come from: a file on disk (dev builds, so
/// external buffers and textures resolve) or memory (the pack).
enum Source {
    File(std::path::PathBuf),
    Packed {
        name: String,
        bytes: Vec<u8>,
        /// External buffers and images of a `.gltf`, by pack path.
        files: HashMap<String, Vec<u8>>,
    },
}

/// (model path, texture file name) pairs already warned about.
pub(crate) type Warned = RefCell<HashSet<(String, String)>>;

/// Run `f` with the texture sources of the FBX at project path `model`:
/// reads and the file index through `server`, each warning shown once per
/// (model, texture).
fn with_sources<R>(
    server: &AssetServer,
    warned: &Warned,
    model: &str,
    f: impl FnOnce(&Sources) -> R,
) -> R {
    let index = server.file_paths();
    let read = |p: &str| server.read(p);
    let warn = |texture: &str, message: String| {
        if warned
            .borrow_mut()
            .insert((model.to_string(), texture.to_string()))
        {
            tracing::warn!("{message}");
        }
    };
    f(&Sources {
        read: &read,
        index: &index,
        warn: &warn,
    })
}

type FbxBytes<'a> = Option<(String, std::borrow::Cow<'a, [u8]>)>;

impl Source {
    fn of(server: &AssetServer, handle: Handle<Model>, path: &str) -> anyhow::Result<Self> {
        if server.has_pack() {
            let name = split_mesh_fragment(path).0.to_string();
            let bytes = server
                .read(&name)
                .map_err(|e| anyhow::anyhow!("{name} is not in the pack: {e}"))?;
            // An FBX reads its textures through the server while it loads.
            let files = if is_fbx(&name) {
                HashMap::new()
            } else {
                crate::packed_gltf::external_files(&name, &bytes, &|p| server.read(p))?
            };
            return Ok(Self::Packed { name, bytes, files });
        }
        server.full_path(handle).map(Self::File).ok_or_else(|| {
            anyhow::anyhow!("unknown built-in asset (expected {BUILTIN_PREFIX}cube)")
        })
    }

    /// The FBX behind this source: its project path and bytes.
    fn fbx(&self, path: &str) -> anyhow::Result<FbxBytes<'_>> {
        let name = split_mesh_fragment(path).0.to_string();
        Ok(match self {
            Self::File(file) if is_fbx(&file.to_string_lossy()) => Some((
                name,
                std::fs::read(file)
                    .map_err(|e| anyhow::anyhow!("could not read {}: {e}", file.display()))?
                    .into(),
            )),
            Self::Packed { name, bytes, .. } if is_fbx(name) => {
                Some((name.clone(), bytes.as_slice().into()))
            }
            _ => None,
        })
    }

    /// The model and the texture files it read (project paths).
    fn model(
        &self,
        scene: &SceneRenderer,
        server: &AssetServer,
        warned: &Warned,
        path: &str,
    ) -> anyhow::Result<(LoadedModel, Vec<String>)> {
        if let Some((name, bytes)) = self.fbx(path)? {
            let settings = server.import_settings(&name);
            return with_sources(server, warned, &name, |s| {
                scene.load_fbx(&name, &bytes, &settings, s)
            });
        }
        match self {
            Self::File(file) => scene.load_gltf(&file.to_string_lossy()),
            Self::Packed { name, bytes, files } => scene
                .load_gltf_packed(name, bytes, files)
                .map_err(|e| e.context(format!("packed model {path}"))),
        }
        .map(|m| (m, Vec::new()))
    }

    #[allow(clippy::type_complexity)]
    fn meshes(
        &self,
        scene: &SceneRenderer,
        server: &AssetServer,
        warned: &Warned,
        path: &str,
    ) -> anyhow::Result<(Vec<anyhow::Result<LoadedModel>>, Vec<String>)> {
        if let Some((name, bytes)) = self.fbx(path)? {
            let settings = server.import_settings(&name);
            return with_sources(server, warned, &name, |s| {
                scene.load_fbx_meshes(&name, &bytes, &settings, s)
            });
        }
        match self {
            Self::File(file) => scene.load_gltf_meshes(&file.to_string_lossy()),
            Self::Packed { name, bytes, files } => scene.load_gltf_meshes_bytes(name, bytes, files),
        }
        .map(|m| (m, Vec::new()))
    }
}

pub struct ModelCache {
    models: HashMap<u32, CachedModel>,
    builtin_cube: LoadedModel,
    builtin_sphere: LoadedModel,
    /// (handle id, version) pairs that failed to load, so each failure is
    /// reported once and not retried every frame.
    failed: HashSet<(u32, u32)>,
    pub(crate) warned: Warned,
}

impl ModelCache {
    /// The texture files `paths` with their current versions, registered
    /// with the server so changes to them are noticed.
    fn texture_versions(server: &mut AssetServer, paths: &[String]) -> Vec<(Handle<Texture>, u32)> {
        paths
            .iter()
            .map(|p| {
                let handle: Handle<Texture> = server.load(p);
                (handle, server.version(handle))
            })
            .collect()
    }

    /// Import `file` once and build all its meshes: return mesh `mesh` (for
    /// `handle`) and cache every other mesh under its own `#mesh<i>` handle,
    /// so a model's fragments don't each import the whole file. Also returns
    /// the texture files read.
    fn load_fragments(
        &mut self,
        scene: &SceneRenderer,
        server: &mut AssetServer,
        handle: Handle<Model>,
        file_path: &str,
        source: &Source,
        mesh: usize,
    ) -> anyhow::Result<(LoadedModel, Vec<String>)> {
        let (models, deps) = source.meshes(scene, server, &self.warned, file_path)?;
        let textures = Self::texture_versions(server, &deps);
        let mut ours = None;
        for (i, model) in models.into_iter().enumerate() {
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
            server.set_part_bounds(sibling, model.part_bounds.clone());
            let version = server.version(sibling);
            self.models.insert(
                sibling.id(),
                CachedModel {
                    version,
                    textures: textures.clone(),
                    model,
                },
            );
        }
        ours.unwrap_or_else(|| Err(anyhow::anyhow!("{file_path} has no mesh {mesh}")))
            .map(|m| (m, deps))
    }

    pub fn new(scene: &SceneRenderer) -> Self {
        Self {
            models: HashMap::new(),
            builtin_cube: scene.builtin_cube_model(),
            builtin_sphere: scene.builtin_sphere_model(),
            failed: HashSet::new(),
            warned: RefCell::default(),
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
        match server.path(handle) {
            "builtin:cube" => return &self.builtin_cube,
            "builtin:sphere" => return &self.builtin_sphere,
            _ => {}
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
        if path == "builtin:cube" || path == "builtin:sphere" {
            if server.bounds(handle).is_none() {
                let (min, max) = if path == "builtin:cube" {
                    self.builtin_cube.bounds
                } else {
                    self.builtin_sphere.bounds
                };
                server.set_bounds(handle, min, max);
                let parts = if path == "builtin:cube" {
                    self.builtin_cube.part_bounds.clone()
                } else {
                    self.builtin_sphere.part_bounds.clone()
                };
                server.set_part_bounds(handle, parts);
            }
            return;
        }

        let version = server.version(handle);
        let stale = self.models.get(&id).is_none_or(|m| {
            m.version != version || m.textures.iter().any(|(t, v)| server.version(*t) != *v)
        });
        if stale && !self.failed.contains(&(id, version)) {
            let result = match Source::of(server, handle, &path) {
                Ok(source) => match split_mesh_fragment(&path) {
                    (file_path, Some(mesh)) => {
                        self.load_fragments(scene, server, handle, file_path, &source, mesh)
                    }
                    (_, None) => source.model(scene, server, &self.warned, &path),
                },
                Err(error) => Err(error),
            };
            match result {
                Ok((model, deps)) => {
                    server.set_bounds(handle, model.bounds.0, model.bounds.1);
                    server.set_part_bounds(handle, model.part_bounds.clone());
                    let textures = Self::texture_versions(server, &deps);
                    self.models.insert(
                        id,
                        CachedModel {
                            version,
                            textures,
                            model,
                        },
                    );
                }
                Err(error) => {
                    tracing::warn!("could not load model {path}: {error:#}");
                    self.failed.insert((id, version));
                }
            }
        }
    }
}
