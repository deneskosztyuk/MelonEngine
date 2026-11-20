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
use serde::{Deserialize, Serialize};

use crate::Time;

/// Asset type tag for 3D models (glTF files, or `builtin:cube`).
pub struct Model;

/// Asset type tag for textures (png, jpg, hdr files).
pub struct Texture;

/// Asset type tag for materials (`.mat` RON files, see `material`).
pub struct Material;

/// Asset type tag for environments (`.env` RON files, see
/// `environment_asset`).
pub struct EnvironmentFile;

/// Prefix for assets that have no file (e.g. `builtin:cube`).
pub const BUILTIN_PREFIX: &str = "builtin:";

/// Stable id of an asset file, kept in its `.meta` sidecar so references
/// survive a rename or move. Serialised as the hyphenated lower-case UUID.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetGuid(uuid::Uuid);

impl AssetGuid {
    /// A new random (v4) GUID.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn to_bytes(self) -> [u8; 16] {
        *self.0.as_bytes()
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(uuid::Uuid::from_bytes(bytes))
    }
}

impl std::fmt::Display for AssetGuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl std::str::FromStr for AssetGuid {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

/// Version of the `.meta` file format.
pub const META_VERSION: u32 = 1;

/// Contents of `<asset file name>.meta`. `melon_core` only reads these; the
/// editor writes them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AssetMeta {
    pub version: u32,
    pub guid: AssetGuid,
}

impl AssetMeta {
    /// Parse a `.meta` file; any version other than `META_VERSION` is an error.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let meta: Self = ron::from_str(&text)?;
        anyhow::ensure!(
            meta.version == META_VERSION,
            "unsupported .meta version {} (expected {META_VERSION})",
            meta.version
        );
        Ok(meta)
    }

    pub fn to_ron(&self) -> String {
        format!(
            "(version: {}, guid: \"{}\")
",
            self.version, self.guid
        )
    }
}

/// The sidecar of `asset`: its full file name plus `.meta`.
pub fn meta_path(asset: &Path) -> PathBuf {
    let mut name = asset.file_name().unwrap_or_default().to_os_string();
    name.push(".meta");
    asset.with_file_name(name)
}

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
    part_bounds: Vec<(Vec3, Vec3)>,
}

/// Registry of every asset path the project uses.
#[derive(Resource)]
pub struct AssetServer {
    root: PathBuf,
    entries: Vec<AssetEntry>,
    by_path: HashMap<String, u32>,
    guid_to_path: HashMap<AssetGuid, String>,
    path_to_guid: HashMap<String, AssetGuid>,
    /// Bumped whenever the GUID index changes, so anything that resolved
    /// GUIDs to handles knows to resolve them again.
    index_epoch: u32,
    since_poll: f32,
    pack: Option<std::sync::Arc<crate::pack::Pack>>,
}

/// Seconds between hot-reload checks.
const POLL_INTERVAL: f32 = 0.5;

/// Split `<file>#mesh<N>` into the file and the glTF mesh index. Any other
/// path (no fragment, or not a `#mesh<N>` one) comes back whole with `None`.
pub fn split_mesh_fragment(path: &str) -> (&str, Option<usize>) {
    if let Some((file, fragment)) = path.rsplit_once('#')
        && let Some(index) = fragment.strip_prefix("mesh")
        && let Ok(index) = index.parse()
    {
        return (file, Some(index));
    }
    (path, None)
}

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
            guid_to_path: HashMap::new(),
            path_to_guid: HashMap::new(),
            index_epoch: 0,
            since_poll: 0.0,
            pack: None,
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
                    part_bounds: Vec::new(),
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

    /// File on disk (without any `#mesh<N>` fragment), or `None` for
    /// built-in assets.
    pub fn full_path<T: Send + Sync + 'static>(&self, handle: Handle<T>) -> Option<PathBuf> {
        let path = self.path(handle);
        (!path.starts_with(BUILTIN_PREFIX)).then(|| self.root.join(split_mesh_fragment(path).0))
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

    /// Local-space bounds of each mesh part (material slot), written by the
    /// renderer after loading; empty until then.
    pub fn set_part_bounds<T: Send + Sync + 'static>(
        &mut self,
        handle: Handle<T>,
        parts: Vec<(Vec3, Vec3)>,
    ) {
        self.entries[handle.id as usize].part_bounds = parts;
    }

    pub fn part_bounds<T: Send + Sync + 'static>(&self, handle: Handle<T>) -> &[(Vec3, Vec3)] {
        &self.entries[handle.id as usize].part_bounds
    }

    pub fn bounds<T: Send + Sync + 'static>(&self, handle: Handle<T>) -> Option<(Vec3, Vec3)> {
        self.entries[handle.id as usize].bounds
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read an asset's bytes: from the pack when one is set (and nothing else:
    /// a path the pack lacks is `NotFound`), else from `<root>/<path>`.
    pub fn read(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let path = normalise(path);
        let file = split_mesh_fragment(&path).0;
        match &self.pack {
            Some(pack) => pack
                .read(file)
                .unwrap_or_else(|| Err(std::io::ErrorKind::NotFound.into())),
            None => std::fs::read(self.root.join(file)),
        }
    }

    /// Serve every read and the GUID index from `pack`; no hot reload then.
    pub fn set_pack(&mut self, pack: std::sync::Arc<crate::pack::Pack>) {
        self.index_epoch += 1;
        self.guid_to_path.clear();
        self.path_to_guid.clear();
        for (path, guid) in pack.entries() {
            if let Some(guid) = guid {
                self.guid_to_path.insert(guid, path.to_string());
                self.path_to_guid.insert(path.to_string(), guid);
            }
        }
        self.pack = Some(pack);
    }

    pub fn has_pack(&self) -> bool {
        self.pack.is_some()
    }

    /// Rebuild the GUID index from every `.meta` under `<root>/assets`.
    /// Read-only. Broken `.meta` files are logged and skipped, as are ones
    /// whose asset is missing; of two sharing a GUID the first in path order
    /// wins (the editor's reconcile fixes duplicates before scanning).
    pub fn scan_metas(&mut self) {
        if self.pack.is_some() {
            return; // the pack index holds the GUIDs
        }
        self.index_epoch += 1;
        self.guid_to_path.clear();
        self.path_to_guid.clear();
        for file in asset_dir_files(&self.root) {
            let Some(asset) = file
                .to_str()
                .and_then(|f| f.strip_suffix(".meta"))
                .map(PathBuf::from)
            else {
                continue;
            };
            if !asset.is_file() {
                continue;
            }
            let meta = match AssetMeta::read(&file) {
                Ok(meta) => meta,
                Err(error) => {
                    tracing::warn!("skipping {}: {error}", file.display());
                    continue;
                }
            };
            let Some(path) = project_relative(&self.root, &asset) else {
                continue;
            };
            if self.guid_to_path.contains_key(&meta.guid) {
                continue;
            }
            self.guid_to_path.insert(meta.guid, path.clone());
            self.path_to_guid.insert(path, meta.guid);
        }
    }

    /// Changes whenever the GUID index does (`scan_metas`, `set_pack`, a
    /// rename): cached `resolve` results older than it are stale.
    pub fn index_epoch(&self) -> u32 {
        self.index_epoch
    }

    /// GUID of a file-backed asset path (a `#mesh<N>` fragment is ignored).
    pub fn guid_of(&self, path: &str) -> Option<AssetGuid> {
        let path = normalise(path);
        self.path_to_guid.get(split_mesh_fragment(&path).0).copied()
    }

    /// Current project-relative path of `guid`.
    pub fn path_of_guid(&self, guid: AssetGuid) -> Option<&str> {
        self.guid_to_path.get(&guid).map(String::as_str)
    }

    /// Every indexed GUID and its path.
    pub fn guid_index(&self) -> impl Iterator<Item = (AssetGuid, &str)> {
        self.guid_to_path.iter().map(|(g, p)| (*g, p.as_str()))
    }

    /// Handle for the asset `guid` names (plus `#mesh<N>`), `None` if unknown.
    pub fn load_guid<T: Send + Sync + 'static>(
        &mut self,
        guid: AssetGuid,
        fragment: Option<usize>,
    ) -> Option<Handle<T>> {
        let path = self.path_of_guid(guid)?.to_string();
        Some(match fragment {
            Some(n) => self.load(&format!("{path}#mesh{n}")),
            None => self.load(&path),
        })
    }

    /// A file moved from `old` to `new`: its entries (and `#mesh<N>` entries)
    /// take the new path and keep their handle ids; the GUID index follows.
    pub fn rename_path(&mut self, old: &str, new: &str) {
        let (old, new) = (normalise(old), normalise(new));
        for (id, entry) in self.entries.iter_mut().enumerate() {
            let (file, fragment) = split_mesh_fragment(&entry.path);
            if file != old {
                continue;
            }
            let renamed = match fragment {
                Some(n) => format!("{new}#mesh{n}"),
                None => new.clone(),
            };
            self.by_path.remove(&entry.path);
            self.by_path.insert(renamed.clone(), id as u32);
            entry.path = renamed;
        }
        if let Some(guid) = self.path_to_guid.remove(&old) {
            self.index_epoch += 1;
            self.guid_to_path.insert(guid, new.clone());
            self.path_to_guid.insert(new, guid);
        }
    }

    /// Modified time of a file-backed asset, `None` if built-in or missing.
    fn modified_time(&self, path: &str) -> Option<SystemTime> {
        if path.starts_with(BUILTIN_PREFIX) || self.pack.is_some() {
            return None;
        }
        std::fs::metadata(self.root.join(split_mesh_fragment(path).0))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Check every file-backed asset's modified time; a change (or a file
    /// appearing) bumps its version. A missing file is not a change (the last
    /// load stays in use).
    pub fn poll_changes(&mut self) {
        for i in 0..self.entries.len() {
            self.refresh_entry(i);
        }
    }

    /// `poll_changes` for one path (after the editor wrote the file). An
    /// unknown path is a no-op.
    pub fn refresh(&mut self, path: &str) {
        if let Some(&id) = self.by_path.get(&normalise(path)) {
            self.refresh_entry(id as usize);
        }
    }

    fn refresh_entry(&mut self, i: usize) {
        let Some(modified) = self.modified_time(&self.entries[i].path) else {
            return;
        };
        let entry = &mut self.entries[i];
        if entry.modified != Some(modified) {
            entry.version += 1;
            entry.modified = Some(modified);
        }
    }
}

/// Every file under `<root>/assets`, recursively, `.meta` files included.
/// Files and folders whose name starts with `.` are skipped. Sorted.
pub fn asset_dir_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("assets"), &mut out);
    out.sort();
    out
}

/// `path` relative to `root` with forward slashes (asset path form).
pub fn project_relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
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
    fn split_mesh_fragment_parses_only_mesh_fragments() {
        assert_eq!(split_mesh_fragment("a.glb#mesh2"), ("a.glb", Some(2)));
        assert_eq!(split_mesh_fragment("a.glb"), ("a.glb", None));
        assert_eq!(split_mesh_fragment("a.glb#meshX"), ("a.glb#meshX", None));
    }

    #[test]
    fn fragments_bump_with_their_file() {
        let root = temp_root("fragments");
        let file = root.join("assets/models/a.glb");
        std::fs::write(&file, "v1").unwrap();
        let mut server = AssetServer::new(root.clone());
        let m0: Handle<Model> = server.load("assets/models/a.glb#mesh0");
        let m1: Handle<Model> = server.load("assets/models/a.glb#mesh1");
        touch_later(&file);
        server.poll_changes();
        assert_eq!((server.version(m0), server.version(m1)), (1, 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn full_path_drops_fragment() {
        let mut server = AssetServer::new(PathBuf::from("project"));
        let h: Handle<Model> = server.load("assets/models/a.glb#mesh3");
        assert_eq!(
            server.full_path(h),
            Some(PathBuf::from("project").join("assets/models/a.glb"))
        );
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

    #[test]
    fn meta_round_trips_and_guid_is_a_string() {
        let meta = AssetMeta {
            version: META_VERSION,
            guid: AssetGuid::new(),
        };
        let text = meta.to_ron();
        assert!(text.contains(&format!("\"{}\"", meta.guid)), "{text}");
        let dir = temp_root("meta_rt");
        let file = dir.join("a.glb.meta");
        std::fs::write(&file, &text).unwrap();
        assert_eq!(AssetMeta::read(&file).unwrap(), meta);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meta_with_other_version_is_an_error() {
        let dir = temp_root("meta_ver");
        let file = dir.join("a.glb.meta");
        std::fs::write(
            &file,
            format!("(version: 2, guid: \"{}\")", AssetGuid::new()),
        )
        .unwrap();
        assert!(AssetMeta::read(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meta_path_appends_meta() {
        assert_eq!(
            meta_path(Path::new("assets/models/cube.glb")),
            PathBuf::from("assets/models/cube.glb.meta")
        );
    }

    #[test]
    fn guids_differ_and_parse_back() {
        let (a, b) = (AssetGuid::new(), AssetGuid::new());
        assert_ne!(a, b);
        assert_eq!(a.to_string().parse::<AssetGuid>().unwrap(), a);
    }

    fn write_asset_with_meta(root: &Path, rel: &str) -> AssetGuid {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let guid = AssetGuid::new();
        let meta = AssetMeta {
            version: META_VERSION,
            guid,
        };
        std::fs::write(meta_path(&file), meta.to_ron()).unwrap();
        guid
    }

    #[test]
    fn scan_metas_indexes_guids_both_ways() {
        let root = temp_root("scan");
        let g = write_asset_with_meta(&root, "assets/models/sub/a.glb");
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        assert_eq!(server.guid_of("assets/models/sub/a.glb"), Some(g));
        assert_eq!(server.guid_of("assets/models/sub/a.glb#mesh1"), Some(g));
        assert_eq!(server.path_of_guid(g), Some("assets/models/sub/a.glb"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_metas_skips_broken_meta_and_writes_nothing() {
        let root = temp_root("scan_broken");
        let file = root.join("assets/models/a.glb");
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(meta_path(&file), "not ron").unwrap();
        std::fs::write(root.join("assets/models/b.glb"), b"x").unwrap();
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        assert_eq!(server.guid_of("assets/models/a.glb"), None);
        assert_eq!(
            std::fs::read_to_string(meta_path(&file)).unwrap(),
            "not ron"
        );
        assert!(!meta_path(&root.join("assets/models/b.glb")).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_guid_resolves_current_path_with_fragment() {
        let root = temp_root("load_guid");
        let g = write_asset_with_meta(&root, "assets/models/a.glb");
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        let h: Handle<Model> = server.load_guid(g, Some(2)).unwrap();
        assert_eq!(server.path(h), "assets/models/a.glb#mesh2");
        assert!(server.load_guid::<Model>(AssetGuid::new(), None).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_path_moves_fragments_and_keeps_ids() {
        let root = temp_root("rename");
        let g = write_asset_with_meta(&root, "assets/models/a.glb");
        let mut server = AssetServer::new(root.clone());
        server.scan_metas();
        let whole: Handle<Model> = server.load("assets/models/a.glb");
        let part: Handle<Model> = server.load("assets/models/a.glb#mesh1");
        server.rename_path("assets/models/a.glb", "assets/models/b.glb");
        assert_eq!(server.path(whole), "assets/models/b.glb");
        assert_eq!(server.path(part), "assets/models/b.glb#mesh1");
        assert_eq!(server.load::<Model>("assets/models/b.glb#mesh1"), part);
        assert_eq!(server.path_of_guid(g), Some("assets/models/b.glb"));
        assert_eq!(server.guid_of("assets/models/a.glb"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn packed_guid_follows_an_asset_renamed_with_its_meta() {
        let root = temp_root("pack_rename");
        let guid = write_asset_with_meta(&root, "assets/models/a.glb");
        for ext in ["", ".meta"] {
            std::fs::rename(
                root.join(format!("assets/models/a.glb{ext}")),
                root.join(format!("assets/models/sub dir b.glb{ext}")),
            )
            .unwrap();
        }
        let out = root.join("game.pak");
        crate::pack::build_pack(&root, &[], &out).unwrap();
        let mut server = AssetServer::new(root.clone());
        server.set_pack(std::sync::Arc::new(crate::pack::Pack::open(&out).unwrap()));
        assert_eq!(
            server.path_of_guid(guid),
            Some("assets/models/sub dir b.glb")
        );
        assert!(server.read("assets/models/a.glb").is_err());
        assert!(
            server.read(r"assets\models\sub dir b.glb").is_ok(),
            "backslashes are normalised"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Fixture: a `.meta` file as `AssetMeta::to_ron` writes it.

    #[test]
    fn pack_wins_over_disk_and_serves_guids_without_hot_reload() {
        let root = temp_root("pack");
        let guid = write_asset_with_meta(&root, "assets/models/a.glb");
        std::fs::write(root.join("assets/models/a.glb"), b"packed").unwrap();
        let out = root.join("game.pak");
        crate::pack::build_pack(&root, &[], &out).unwrap();
        std::fs::write(root.join("assets/models/a.glb"), b"on disk now").unwrap();
        std::fs::remove_file(meta_path(&root.join("assets/models/a.glb"))).unwrap();

        let mut server = AssetServer::new(root.clone());
        assert_eq!(server.read("assets/models/a.glb").unwrap(), b"on disk now");
        server.set_pack(std::sync::Arc::new(crate::pack::Pack::open(&out).unwrap()));
        assert!(server.has_pack());
        assert_eq!(server.read("assets/models/a.glb").unwrap(), b"packed");
        assert_eq!(
            server.read("./assets/models/a.glb#mesh1").unwrap(),
            b"packed",
            "paths are normalised, fragments ignored"
        );
        assert_eq!(
            server.read("assets/missing.glb").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(server.path_of_guid(guid), Some("assets/models/a.glb"));
        assert_eq!(server.guid_of("assets/models/a.glb"), Some(guid));

        let handle: Handle<Model> = server.load("assets/models/a.glb");
        touch_later(&root.join("assets/models/a.glb"));
        server.poll_changes();
        assert_eq!(server.version(handle), 0, "packed games do not hot reload");
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn meta_fixture_loads() {
        let root = temp_root("meta_fixture");
        let path = root.join("assets/models/cube.glb.meta");
        std::fs::write(
            &path,
            "(version: 1, guid: \"334087e7-ba37-4ff4-b11f-bc731349dbfa\")
",
        )
        .unwrap();
        let meta = AssetMeta::read(&path).unwrap();
        assert_eq!(
            meta,
            AssetMeta {
                version: 1,
                guid: "334087e7-ba37-4ff4-b11f-bc731349dbfa".parse().unwrap(),
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
