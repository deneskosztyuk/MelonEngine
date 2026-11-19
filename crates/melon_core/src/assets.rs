//! Assets referenced by typed handles. The `AssetServer` maps project-relative
//! paths to handles and back, tracks a version per file (bumped when the file
//! changes on disk, for hot reload) and stores bounds the renderer measures.
//! No GPU code: `melon_render` turns handles into GPU data.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bevy_ecs::component::Component;
use bevy_ecs::resource::Resource;
use bevy_ecs::system::{Res, ResMut};
use glam::Vec3;

use crate::Time;

/// Asset type tag for 3D models (glTF files, or `builtin:cube`).
pub struct Model;

/// Prefix for assets that have no file (e.g. `builtin:cube`).
pub const BUILTIN_PREFIX: &str = "builtin:";

/// Typed reference to an asset registered in the `AssetServer`. Scenes store
/// the asset's path, never the id.
#[derive(Component)]
pub struct Handle<T: Send + Sync + 'static> {
    id: u32,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Send + Sync + 'static> Handle<T> {
    pub fn id(&self) -> u32 {
        self.id
    }
}

// Manual impls: derives would require `T` itself to be Copy / Eq / ...
impl<T: Send + Sync + 'static> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Send + Sync + 'static> Copy for Handle<T> {}
impl<T: Send + Sync + 'static> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<T: Send + Sync + 'static> Eq for Handle<T> {}
impl<T: Send + Sync + 'static> std::hash::Hash for Handle<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}
impl<T: Send + Sync + 'static> std::fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Handle({})", self.id)
    }
}

struct AssetEntry {
    path: String,
    version: u32,
    modified: Option<SystemTime>,
    bounds: Option<(Vec3, Vec3)>,
}

/// Registry of every asset path the project uses.
#[derive(Resource)]
pub struct AssetServer {
    root: PathBuf,
    entries: Vec<AssetEntry>,
    by_path: HashMap<String, u32>,
    since_poll: f32,
}

/// Seconds between hot-reload checks.
const POLL_INTERVAL: f32 = 0.5;

/// Forward slashes, no leading `./`, so one file always maps to one handle.
fn normalise(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.strip_prefix("./").unwrap_or(&path).to_string()
}

impl AssetServer {
    /// `root` is the project folder asset paths are relative to.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            entries: Vec::new(),
            by_path: HashMap::new(),
            since_poll: 0.0,
        }
    }

    /// Handle for `path`, registering it on first use. Only reads the file's
    /// modified time: the renderer loads the file when first drawn.
    pub fn load<T: Send + Sync + 'static>(&mut self, path: &str) -> Handle<T> {
        let path = normalise(path);
        let id = match self.by_path.get(&path) {
            Some(id) => *id,
            None => {
                let id = self.entries.len() as u32;
                let modified = self.modified_time(&path);
                self.entries.push(AssetEntry {
                    path: path.clone(),
                    version: 0,
                    modified,
                    bounds: None,
                });
                self.by_path.insert(path, id);
                id
            }
        };
        Handle {
            id,
            _marker: PhantomData,
        }
    }

    pub fn path<T: Send + Sync + 'static>(&self, handle: Handle<T>) -> &str {
        &self.entries[handle.id as usize].path
    }

    /// File on disk, or `None` for built-in assets.
    pub fn full_path<T: Send + Sync + 'static>(&self, handle: Handle<T>) -> Option<PathBuf> {
        let path = self.path(handle);
        (!path.starts_with(BUILTIN_PREFIX)).then(|| self.root.join(path))
    }

    /// Bumped every time the file changes on disk.
    pub fn version<T: Send + Sync + 'static>(&self, handle: Handle<T>) -> u32 {
        self.entries[handle.id as usize].version
    }

    /// Local-space bounds, written by the renderer after loading.
    pub fn set_bounds<T: Send + Sync + 'static>(
        &mut self,
        handle: Handle<T>,
        min: Vec3,
        max: Vec3,
    ) {
        self.entries[handle.id as usize].bounds = Some((min, max));
    }

    pub fn bounds<T: Send + Sync + 'static>(&self, handle: Handle<T>) -> Option<(Vec3, Vec3)> {
        self.entries[handle.id as usize].bounds
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Modified time of a file-backed asset, `None` if built-in or missing.
    fn modified_time(&self, path: &str) -> Option<SystemTime> {
        if path.starts_with(BUILTIN_PREFIX) {
            return None;
        }
        std::fs::metadata(self.root.join(path))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Check every file-backed asset's modified time; a change (or a file
    /// appearing) bumps its version. A missing file is not a change (the last
    /// load stays in use).
    pub fn poll_changes(&mut self) {
        for i in 0..self.entries.len() {
            let Some(modified) = self.modified_time(&self.entries[i].path) else {
                continue;
            };
            let entry = &mut self.entries[i];
            if entry.modified != Some(modified) {
                entry.version += 1;
                entry.modified = Some(modified);
            }
        }
    }
}

/// Hot reload: poll asset files every `POLL_INTERVAL` seconds of wall time,
/// in every simulation state.
pub fn hot_reload_system(time: Res<Time>, server: Option<ResMut<AssetServer>>) {
    let Some(mut server) = server else {
        return;
    };
    server.since_poll += time.delta_seconds;
    if server.since_poll >= POLL_INTERVAL {
        server.since_poll = 0.0;
        server.poll_changes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_assets_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets/models")).unwrap();
        dir
    }

    fn touch_later(path: &Path) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn same_path_same_handle() {
        let mut server = AssetServer::new(".".into());
        let a: Handle<Model> = server.load("assets/models/a.glb");
        let b: Handle<Model> = server.load("assets/models/a.glb");
        let c: Handle<Model> = server.load("assets/models/c.glb");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn windows_separators_normalised() {
        let mut server = AssetServer::new(".".into());
        let a: Handle<Model> = server.load("assets\\models\\a.glb");
        let b: Handle<Model> = server.load("./assets/models/a.glb");
        assert_eq!(a, b);
        assert_eq!(server.path(a), "assets/models/a.glb");
    }

    #[test]
    fn builtin_has_no_full_path() {
        let mut server = AssetServer::new(".".into());
        let cube: Handle<Model> = server.load("builtin:cube");
        assert!(server.full_path(cube).is_none());
    }

    #[test]
    fn full_path_joins_root() {
        let mut server = AssetServer::new(PathBuf::from("project"));
        let h: Handle<Model> = server.load("assets/models/a.glb");
        assert_eq!(
            server.full_path(h),
            Some(PathBuf::from("project").join("assets/models/a.glb"))
        );
    }

    #[test]
    fn poll_bumps_version_on_change() {
        let root = temp_root("poll");
        let file = root.join("assets/models/a.glb");
        std::fs::write(&file, "v1").unwrap();
        let mut server = AssetServer::new(root.clone());
        let h: Handle<Model> = server.load("assets/models/a.glb");
        server.poll_changes();
        assert_eq!(server.version(h), 0);
        touch_later(&file);
        server.poll_changes();
        assert_eq!(server.version(h), 1);
        server.poll_changes();
        assert_eq!(server.version(h), 1, "no change, no bump");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn poll_ignores_deleted_file() {
        let root = temp_root("deleted");
        let file = root.join("assets/models/a.glb");
        std::fs::write(&file, "v1").unwrap();
        let mut server = AssetServer::new(root.clone());
        let h: Handle<Model> = server.load("assets/models/a.glb");
        server.poll_changes();
        std::fs::remove_file(&file).unwrap();
        server.poll_changes();
        assert_eq!(server.version(h), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_appearing_after_load_bumps_version() {
        let root = temp_root("appears");
        let mut server = AssetServer::new(root.clone());
        let h: Handle<Model> = server.load("assets/models/a.glb");
        server.poll_changes();
        std::fs::write(root.join("assets/models/a.glb"), "v1").unwrap();
        server.poll_changes();
        assert_eq!(server.version(h), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn change_before_first_poll_bumps_version() {
        let root = temp_root("early");
        let file = root.join("assets/models/a.glb");
        std::fs::write(&file, "v1").unwrap();
        let mut server = AssetServer::new(root.clone());
        let h: Handle<Model> = server.load("assets/models/a.glb");
        touch_later(&file);
        server.poll_changes();
        assert_eq!(server.version(h), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bounds_round_trip() {
        let mut server = AssetServer::new(".".into());
        let h: Handle<Model> = server.load("builtin:cube");
        assert!(server.bounds(h).is_none());
        server.set_bounds(h, Vec3::splat(-1.0), Vec3::new(2.0, 1.0, 1.0));
        assert_eq!(
            server.bounds(h),
            Some((Vec3::splat(-1.0), Vec3::new(2.0, 1.0, 1.0)))
        );
    }

    #[test]
    fn hot_reload_system_polls_every_half_second() {
        let root = temp_root("system");
        let file = root.join("assets/models/a.glb");
        std::fs::write(&file, "v1").unwrap();
        let mut core = crate::Core::new();
        core.world.insert_resource(AssetServer::new(root.clone()));
        let h: Handle<Model> = core
            .world
            .resource_mut::<AssetServer>()
            .load("assets/models/a.glb");
        core.world.resource_mut::<AssetServer>().poll_changes();
        touch_later(&file);
        let frame = |core: &mut crate::Core| {
            core.world.resource_mut::<crate::Time>().update(0.25);
            core.update();
        };
        frame(&mut core);
        assert_eq!(
            core.world.resource::<AssetServer>().version(h),
            0,
            "polled too early"
        );
        frame(&mut core);
        assert_eq!(core.world.resource::<AssetServer>().version(h), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
