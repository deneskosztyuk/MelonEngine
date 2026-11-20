//! Content Browser panel: browse the project's `assets/` folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme;

/// What a browser item is, from its extension (case-insensitive).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AssetKind {
    Folder,
    Model,
    Texture,
    Shader,
    Scene,
    Other,
}

impl AssetKind {
    /// Every kind a file can be, in filter-chip order.
    pub const FILE_KINDS: [AssetKind; 5] = [
        AssetKind::Model,
        AssetKind::Texture,
        AssetKind::Shader,
        AssetKind::Scene,
        AssetKind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AssetKind::Folder => "Folder",
            AssetKind::Model => "Model",
            AssetKind::Texture => "Texture",
            AssetKind::Shader => "Shader",
            AssetKind::Scene => "Scene",
            AssetKind::Other => "Other",
        }
    }
}

pub fn asset_kind(path: &Path, is_dir: bool) -> AssetKind {
    if is_dir {
        return AssetKind::Folder;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "glb" | "gltf" => AssetKind::Model,
        "png" | "jpg" | "jpeg" | "hdr" => AssetKind::Texture,
        "wgsl" => AssetKind::Shader,
        "ron" => AssetKind::Scene,
        _ => AssetKind::Other,
    }
}

/// One file or folder in a listing.
#[derive(Clone, Debug)]
pub struct ContentEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: AssetKind,
    /// File size in bytes (0 for folders).
    pub size: u64,
}

/// Folders first, then files, each sorted case-insensitively. `.meta`
/// sidecars are hidden. A missing or unreadable folder lists as empty.
pub fn list_dir(path: &Path) -> Vec<ContentEntry> {
    let Ok(read) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<ContentEntry> = read
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().ends_with(".meta"))
        .map(|entry| {
            let meta = entry.metadata().ok();
            // `file_type` does not follow symlinks / junctions, so recursive
            // walks cannot loop.
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let path = entry.path();
            ContentEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: asset_kind(&path, is_dir),
                path,
                is_dir,
                size: meta.as_ref().map_or(0, |m| m.len()),
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

/// A file operation the browser asks the editor to run (the editor owns
/// the `AssetServer`, the confirm dialogs and the status bar).
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserRequest {
    /// Create "New Folder" inside this folder.
    NewFolder(PathBuf),
    Rename(PathBuf, String),
    Duplicate(PathBuf),
    /// Ask before deleting (the editor lists what uses it).
    AskDelete(PathBuf),
    /// Move the item into the folder.
    Move(PathBuf, PathBuf),
    /// Copy files from outside into the folder.
    Import(Vec<PathBuf>, PathBuf),
    Reveal(PathBuf),
    /// Open a file (scene or OS default app).
    Open(PathBuf),
}

/// An inline rename in progress.
#[derive(Clone, Debug)]
pub(crate) struct Renaming {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    /// The text field still needs focus and its selection set.
    pub(crate) fresh: bool,
}

/// What a Content Browser drag carries: the item, and for model files the
/// project-relative asset path the Scene and Hierarchy spawn.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetDrag {
    pub path: PathBuf,
    pub model: Option<String>,
}

/// What the Inspector shows for a file or folder picked in the browser.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetDetails {
    pub name: String,
    /// Project-relative, forward slashes.
    pub path: String,
    pub kind: AssetKind,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub guid: Option<melon_core::AssetGuid>,
}

/// Details of `path`; `None` if it is gone or outside the project.
pub fn asset_details(server: &melon_core::AssetServer, path: &Path) -> Option<AssetDetails> {
    let meta = std::fs::metadata(path).ok()?;
    let rel = melon_core::assets::project_relative(server.root(), path)?;
    let is_dir = meta.is_dir();
    Some(AssetDetails {
        name: path.file_name()?.to_string_lossy().into_owned(),
        kind: asset_kind(path, is_dir),
        size: if is_dir { 0 } else { meta.len() },
        modified: meta.modified().ok(),
        guid: server.guid_of(&rel),
        path: rel,
    })
}

/// `YYYY-MM-DD HH:MM UTC` (no date crate: civil date from days since 1970).
pub fn format_modified(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rest = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A scene file: `.ron` (any case) under `<project_root>/assets`.
pub fn is_scene_path(project_root: &Path, path: &Path) -> bool {
    path.starts_with(project_root.join("assets"))
        && path
            .extension()
            .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("ron"))
}

/// A file the asset pipeline can load as a model.
pub fn is_model_path(path: &str) -> bool {
    let path = path.to_lowercase();
    path.ends_with(".glb") || path.ends_with(".gltf")
}

/// Every model the Inspector can assign: `builtin:cube`, then each
/// `.glb` / `.gltf` under `<project>/assets/models` (recursive) as a
/// project-relative, forward-slash path, sorted.
pub fn model_list(project_root: &Path) -> Vec<String> {
    let mut models = Vec::new();
    collect_models(
        &project_root.join("assets/models"),
        project_root,
        &mut models,
    );
    models.sort();
    models.insert(0, "builtin:cube".to_string());
    models
}

fn collect_models(dir: &Path, project_root: &Path, out: &mut Vec<String>) {
    for entry in list_dir(dir) {
        if entry.is_dir {
            collect_models(&entry.path, project_root, out);
        } else if is_model_path(&entry.name)
            && let Some(path) = project_relative(project_root, &entry.path)
        {
            out.push(path);
        }
    }
}

/// `path` relative to `root`, with forward slashes (asset path form).
fn project_relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Where the browser's settings live, relative to the project root.
pub fn settings_path() -> PathBuf {
    PathBuf::from(".melon/content_browser.ron")
}

const SETTINGS_VERSION: u32 = 1;

/// Saved per project. Folders are root-relative with `/` (`""` = root).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ContentSettings {
    version: u32,
    favorites: Vec<String>,
    tile_size: f32,
    last_folder: Option<String>,
    tree_width: f32,
}

impl Default for ContentSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            favorites: Vec::new(),
            tile_size: theme::sizes::TILE_DEFAULT,
            last_folder: None,
            tree_width: theme::sizes::CONTENT_TREE,
        }
    }
}

/// Folder navigation inside a fixed root (the assets folder).
pub struct ContentBrowser {
    root: PathBuf,
    current: PathBuf,
    search: String,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    selected: Option<PathBuf>,
    favorites: Vec<PathBuf>,
    tile_size: f32,
    /// Folder tree width, written back by the view.
    pub(crate) tree_width: f32,
    /// Sub-folders per folder for the tree view, cleared by `refresh`.
    pub(crate) tree_children: std::collections::HashMap<PathBuf, Vec<PathBuf>>,
    /// Set by `refresh` (so whenever the current folder changes): the next
    /// tree draw expands every ancestor of the current folder.
    pub(crate) reveal: bool,
    /// File kinds shown; folders are always shown outside search.
    filter: HashSet<AssetKind>,
    /// The current folder as read by the last `refresh`.
    listing: Vec<ContentEntry>,
    /// Every file below the current folder, sorted; read from disk by the
    /// first search after a `refresh`, so typing filters it in memory.
    files: Option<Vec<ContentEntry>>,
    /// What the asset view shows, rebuilt only when the folder, search or
    /// filter changes.
    visible: Vec<ContentEntry>,
    requests: Vec<BrowserRequest>,
    /// The last click landed inside the browser: its keys (F2, Ctrl+D,
    /// Delete, Enter) act on the selection.
    pub(crate) focused: bool,
    pub(crate) renaming: Option<Renaming>,
    /// An editor dialog is open: the browser's keys wait.
    pub(crate) blocked: bool,
}

impl ContentBrowser {
    pub fn new(root: PathBuf) -> Self {
        let mut browser = Self {
            current: root.clone(),
            root,
            search: String::new(),
            back: Vec::new(),
            forward: Vec::new(),
            selected: None,
            favorites: Vec::new(),
            tile_size: theme::sizes::TILE_DEFAULT,
            tree_width: theme::sizes::CONTENT_TREE,
            tree_children: std::collections::HashMap::new(),
            reveal: true,
            filter: AssetKind::FILE_KINDS.into_iter().collect(),
            listing: Vec::new(),
            files: None,
            visible: Vec::new(),
            requests: Vec::new(),
            focused: false,
            renaming: None,
            blocked: false,
        };
        browser.refresh();
        browser
    }

    /// File operations asked for since the last call.
    pub fn take_requests(&mut self) -> Vec<BrowserRequest> {
        std::mem::take(&mut self.requests)
    }

    pub(crate) fn request(&mut self, request: BrowserRequest) {
        self.requests.push(request);
    }

    /// The browser owns the keyboard shortcuts it shares with other panels.
    pub fn has_focus(&self) -> bool {
        self.focused
    }

    /// Show a text field in place of the item's name.
    pub fn start_rename(&mut self, path: PathBuf) {
        let text = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.renaming = Some(Renaming {
            path,
            text,
            fresh: true,
        });
    }

    pub fn visible_entries(&self) -> &[ContentEntry] {
        &self.visible
    }

    /// What the asset view says when it shows nothing; `None` if it shows
    /// something.
    pub fn empty_message(&self) -> Option<&'static str> {
        if !self.visible.is_empty() {
            None
        } else if self.search.is_empty() && self.listing.is_empty() {
            Some("Empty folder")
        } else {
            Some("No assets match")
        }
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn set_search(&mut self, text: String) {
        if text != self.search {
            self.search = text;
            self.update_visible();
        }
    }

    pub fn kind_enabled(&self, kind: AssetKind) -> bool {
        self.filter.contains(&kind)
    }

    pub fn toggle_kind(&mut self, kind: AssetKind) {
        if !self.filter.remove(&kind) {
            self.filter.insert(kind);
        }
        self.update_visible();
    }

    pub fn current(&self) -> &Path {
        &self.current
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Inside the root, with no `..` that could climb out of it.
    fn inside_root(&self, dir: &Path) -> bool {
        dir.starts_with(&self.root)
            && !dir.strip_prefix(&self.root).is_ok_and(|rel| {
                rel.components()
                    .any(|c| c == std::path::Component::ParentDir)
            })
    }

    /// Open a folder: ignored outside the root or if already open; otherwise
    /// recorded in history, and the search and selection are cleared.
    pub fn open(&mut self, dir: PathBuf) {
        if !self.inside_root(&dir) || dir == self.current {
            return;
        }
        let previous = std::mem::replace(&mut self.current, dir);
        self.back.push(previous);
        self.forward.clear();
        self.search.clear();
        self.selected = None;
        self.refresh();
    }

    /// Go to the parent folder, never above the root.
    pub fn up(&mut self) {
        if self.current != self.root
            && let Some(parent) = self.current.parent()
        {
            self.open(parent.to_path_buf());
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Previous folder in history, skipping folders deleted since.
    pub fn go_back(&mut self) {
        Self::step(&mut self.back, &mut self.forward, &mut self.current);
        self.refresh();
    }

    pub fn go_forward(&mut self) {
        Self::step(&mut self.forward, &mut self.back, &mut self.current);
        self.refresh();
    }

    /// Pop `from` until an existing folder; the old current goes onto `to`.
    fn step(from: &mut Vec<PathBuf>, to: &mut Vec<PathBuf>, current: &mut PathBuf) {
        while let Some(dir) = from.pop() {
            // A refresh walk-up can leave the current folder on the stack.
            if dir.is_dir() && dir != *current {
                to.push(std::mem::replace(current, dir));
                return;
            }
        }
    }

    /// `(name, folder)` from the root down to the current folder.
    pub fn path_segments(&self) -> Vec<(String, PathBuf)> {
        let name = |p: &Path| {
            p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        let mut segments = vec![(name(&self.root), self.root.clone())];
        let mut path = self.root.clone();
        if let Ok(rel) = self.current.strip_prefix(&self.root) {
            for part in rel.components() {
                path.push(part);
                segments.push((name(&path), path.clone()));
            }
        }
        segments
    }

    pub fn favorites(&self) -> &[PathBuf] {
        &self.favorites
    }

    /// Pin a folder inside the root (files and duplicates are ignored).
    pub fn add_favorite(&mut self, dir: PathBuf) {
        if self.inside_root(&dir) && dir.is_dir() && !self.favorites.contains(&dir) {
            self.favorites.push(dir);
        }
    }

    pub fn remove_favorite(&mut self, dir: &Path) {
        self.favorites.retain(|f| f != dir);
    }

    pub fn is_list(&self) -> bool {
        self.tile_size < theme::sizes::TILE_LIST_BELOW
    }

    pub fn tile_size(&self) -> f32 {
        self.tile_size
    }

    pub fn tile_size_mut(&mut self) -> &mut f32 {
        &mut self.tile_size
    }

    /// `dir` relative to the root with `/`.
    fn relative(&self, dir: &Path) -> Option<String> {
        let rel = dir.strip_prefix(&self.root).ok()?;
        Some(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }

    /// An existing folder from a saved relative path; `None` for anything
    /// with `..`, a root or a drive, or a folder that is gone.
    fn saved_folder(&self, rel: &str) -> Option<PathBuf> {
        let rel = Path::new(rel);
        let plain = rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        let dir = self.root.join(rel);
        (plain && dir.is_dir()).then_some(dir)
    }

    /// Apply saved settings; a missing or broken file keeps the defaults.
    pub fn load_settings(&mut self, file: &Path) {
        let settings = match std::fs::read_to_string(file) {
            Err(_) => ContentSettings::default(),
            Ok(text) => match ron::from_str::<ContentSettings>(&text) {
                Ok(s) if s.version == SETTINGS_VERSION => s,
                Ok(s) => {
                    tracing::warn!("{}: unsupported version {}", file.display(), s.version);
                    ContentSettings::default()
                }
                Err(error) => {
                    tracing::warn!("{}: {error}; using defaults", file.display());
                    ContentSettings::default()
                }
            },
        };
        let mut favorites = Vec::new();
        for dir in settings
            .favorites
            .iter()
            .filter_map(|f| self.saved_folder(f))
        {
            if !favorites.contains(&dir) {
                favorites.push(dir);
            }
        }
        self.favorites = favorites;
        self.tile_size = if settings.tile_size.is_finite() {
            settings
                .tile_size
                .clamp(theme::sizes::TILE_MIN, theme::sizes::TILE_MAX)
        } else {
            theme::sizes::TILE_DEFAULT
        };
        self.tree_width = if settings.tree_width.is_finite() && settings.tree_width > 0.0 {
            settings.tree_width
        } else {
            theme::sizes::CONTENT_TREE
        };
        if let Some(dir) = settings
            .last_folder
            .as_deref()
            .and_then(|f| self.saved_folder(f))
        {
            self.current = dir;
        }
        self.refresh();
    }

    /// Save atomically (temp file + rename), creating the folder.
    pub fn save_settings(&self, file: &Path) -> anyhow::Result<()> {
        use anyhow::Context;
        let settings = ContentSettings {
            version: SETTINGS_VERSION,
            favorites: self
                .favorites
                .iter()
                .filter_map(|f| self.relative(f))
                .collect(),
            tile_size: self.tile_size,
            last_folder: self.relative(&self.current),
            tree_width: self.tree_width,
        };
        if let Some(dir) = file.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        let text = ron::ser::to_string_pretty(&settings, ron::ser::PrettyConfig::default())?;
        let tmp = file.with_extension("ron.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, file)
            .with_context(|| format!("failed to write {}", file.display()))?;
        Ok(())
    }

    pub fn select(&mut self, path: Option<PathBuf>) {
        self.selected = path;
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Re-read the disk: the current folder, or with a search every file
    /// below it whose name contains the search (case-insensitive). Clears
    /// the tree cache.
    pub fn refresh(&mut self) {
        if self.selected.as_ref().is_some_and(|p| !p.exists()) {
            self.selected = None;
        }
        // The folder may have been deleted or renamed outside the editor.
        while !self.current.is_dir() && self.current != self.root {
            match self.current.parent() {
                Some(parent) => self.current = parent.to_path_buf(),
                None => break,
            }
        }
        self.tree_children.clear();
        self.reveal = true;
        self.listing = list_dir(&self.current);
        self.files = None;
        self.update_visible();
    }

    /// Rebuild `visible` from the cached listing (no disk read, except the
    /// first search after a `refresh`).
    fn update_visible(&mut self) {
        let filter = &self.filter;
        let shown = |e: &ContentEntry| e.is_dir || filter.contains(&e.kind);
        self.visible = if self.search.is_empty() {
            self.listing.iter().filter(|e| shown(e)).cloned().collect()
        } else {
            let files = self.files.get_or_insert_with(|| {
                let mut files = Vec::new();
                walk_files(&self.current, &mut files);
                files.sort_by(|a, b| {
                    (a.name.to_lowercase(), &a.path).cmp(&(b.name.to_lowercase(), &b.path))
                });
                files
            });
            let needle = self.search.to_lowercase();
            files
                .iter()
                .filter(|e| shown(e) && e.name.to_lowercase().contains(&needle))
                .cloned()
                .collect()
        };
    }
}

/// Every file under `dir`, recursively (folders are not included).
fn walk_files(dir: &Path, out: &mut Vec<ContentEntry>) {
    for entry in list_dir(dir) {
        if entry.is_dir {
            walk_files(&entry.path, out);
        } else {
            out.push(entry);
        }
    }
}

pub(crate) fn format_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1 << 20)),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / f64::from(1 << 10)),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_content_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn list_dir_dirs_first_sorted() {
        let dir = temp_dir("sorted");
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::create_dir(dir.join("A")).unwrap();
        std::fs::create_dir(dir.join("c")).unwrap();
        let names: Vec<String> = list_dir(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["A", "c", "a.txt", "b.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_model_path_accepts_glb_gltf_only() {
        assert!(is_model_path("assets/models/a.GLB"));
        assert!(is_model_path("assets/models/b.gltf"));
        assert!(!is_model_path("assets/README.md"));
        assert!(!is_model_path("assets/shaders/mesh.wgsl"));
    }

    #[test]
    fn model_list_sorted_builtin_first() {
        let root = temp_dir("models");
        let models = root.join("assets/models");
        std::fs::create_dir_all(models.join("sub")).unwrap();
        std::fs::write(models.join("b.glb"), "").unwrap();
        std::fs::write(models.join("sub/a.gltf"), "").unwrap();
        std::fs::write(models.join("notes.txt"), "").unwrap();
        assert_eq!(
            model_list(&root),
            [
                "builtin:cube",
                "assets/models/b.glb",
                "assets/models/sub/a.gltf"
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_dir_missing_is_empty() {
        assert!(list_dir(Path::new("this/path/does/not/exist")).is_empty());
    }

    #[test]
    fn navigation_stays_inside_root() {
        let dir = temp_dir("nav");
        std::fs::create_dir(dir.join("models")).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("models"));
        assert_eq!(browser.current(), dir.join("models"));
        browser.up();
        assert_eq!(browser.current(), dir);
        browser.up();
        assert_eq!(browser.current(), dir, "cannot leave the root");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_dir_hides_meta_files() {
        let dir = temp_dir("meta");
        std::fs::write(dir.join("a.glb"), "a").unwrap();
        std::fs::write(dir.join("a.glb.meta"), "m").unwrap();
        let names: Vec<String> = list_dir(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a.glb"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Create `rel` under `dir`: a folder if it ends with `/`, else a file.
    fn touch(dir: &Path, rel: &str) {
        let path = dir.join(rel.trim_end_matches('/'));
        if rel.ends_with('/') {
            std::fs::create_dir_all(&path).unwrap();
        } else {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x").unwrap();
        }
    }

    fn names(browser: &ContentBrowser) -> Vec<String> {
        browser
            .visible_entries()
            .iter()
            .map(|e| e.name.clone())
            .collect()
    }

    #[test]
    fn asset_kind_by_extension_case_insensitive() {
        assert_eq!(asset_kind(Path::new("a/B.GLB"), false), AssetKind::Model);
        assert_eq!(asset_kind(Path::new("a/b.gltf"), false), AssetKind::Model);
        assert_eq!(asset_kind(Path::new("t.JPeG"), false), AssetKind::Texture);
        assert_eq!(asset_kind(Path::new("s.wgsl"), false), AssetKind::Shader);
        assert_eq!(asset_kind(Path::new("level.ron"), false), AssetKind::Scene);
        assert_eq!(asset_kind(Path::new("README.md"), false), AssetKind::Other);
        assert_eq!(asset_kind(Path::new("models"), true), AssetKind::Folder);
    }

    #[test]
    fn filter_hides_files_but_keeps_folders() {
        let dir = temp_dir("filter");
        for rel in ["models/", "a.glb", "b.png"] {
            touch(&dir, rel);
        }
        let mut browser = ContentBrowser::new(dir.clone());
        browser.toggle_kind(AssetKind::Model);
        assert!(!browser.kind_enabled(AssetKind::Model));
        assert_eq!(names(&browser), ["models", "b.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_is_recursive_case_insensitive_files_only() {
        let dir = temp_dir("search");
        for rel in [
            "Crate.glb",
            "models/sub/crate_big.glb",
            "models/crates/",
            "notes.md",
        ] {
            touch(&dir, rel);
        }
        let mut browser = ContentBrowser::new(dir.clone());
        browser.set_search("CRATE".into());
        assert_eq!(browser.search(), "CRATE");
        assert_eq!(names(&browser), ["Crate.glb", "crate_big.glb"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_respects_filter_and_hides_meta() {
        let dir = temp_dir("search_filter");
        for rel in ["a.glb", "a.glb.meta", "a.png"] {
            touch(&dir, rel);
        }
        let mut browser = ContentBrowser::new(dir.clone());
        browser.toggle_kind(AssetKind::Model);
        browser.set_search("a".into());
        assert_eq!(names(&browser), ["a.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn visible_entries_all_filters_off_is_empty() {
        let dir = temp_dir("filters_off");
        for rel in ["models/", "a.glb"] {
            touch(&dir, rel);
        }
        let mut browser = ContentBrowser::new(dir.clone());
        for kind in AssetKind::FILE_KINDS {
            browser.toggle_kind(kind);
        }
        browser.set_search("a".into());
        assert!(browser.visible_entries().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_and_filter_reuse_the_last_disk_read() {
        let dir = temp_dir("search_cache");
        touch(&dir, "a.glb");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.set_search("a".into());
        browser
            .tree_children
            .insert(dir.clone(), vec![dir.join("kept")]);
        touch(&dir, "ab.glb");
        browser.set_search("ab".into());
        browser.toggle_kind(AssetKind::Model);
        browser.toggle_kind(AssetKind::Model);
        assert!(browser.visible_entries().is_empty(), "no disk read");
        assert!(browser.tree_children.contains_key(&dir), "tree kept");
        browser.refresh();
        assert_eq!(names(&browser), ["ab.glb"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_message_tells_empty_folder_from_filtered() {
        let dir = temp_dir("empty_message");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.toggle_kind(AssetKind::Model);
        assert_eq!(browser.empty_message(), Some("Empty folder"));
        touch(&dir, "a.glb");
        browser.refresh();
        assert_eq!(browser.empty_message(), Some("No assets match"));
        browser.toggle_kind(AssetKind::Model);
        assert_eq!(browser.empty_message(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn search_does_not_follow_a_junction_loop() {
        let dir = temp_dir("junction");
        touch(&dir, "a/file.glb");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("a/loop"))
            .arg(&dir)
            .output()
            .is_ok_and(|o| o.status.success());
        if !made {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let mut browser = ContentBrowser::new(dir.clone());
        browser.set_search("file".into());
        assert_eq!(names(&browser), ["file.glb"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn back_and_forward_walk_history() {
        let dir = temp_dir("history");
        touch(&dir, "a/b/");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a"));
        browser.open(dir.join("a/b"));
        browser.go_back();
        assert_eq!(browser.current(), dir.join("a"));
        browser.go_back();
        assert_eq!(browser.current(), dir);
        assert!(!browser.can_go_back());
        browser.go_forward();
        assert_eq!(browser.current(), dir.join("a"));
        assert!(browser.can_go_forward());
        browser.open(dir.clone());
        assert!(!browser.can_go_forward());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_same_folder_adds_no_history() {
        let dir = temp_dir("same");
        touch(&dir, "a/");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a"));
        browser.open(dir.join("a"));
        browser.go_back();
        assert_eq!(browser.current(), dir);
        assert!(!browser.can_go_back());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn back_skips_missing_folders() {
        let dir = temp_dir("skip");
        for rel in ["a/", "b/", "c/"] {
            touch(&dir, rel);
        }
        let mut browser = ContentBrowser::new(dir.clone());
        for name in ["a", "b", "c"] {
            browser.open(dir.join(name));
        }
        std::fs::remove_dir_all(dir.join("a")).unwrap();
        std::fs::remove_dir_all(dir.join("b")).unwrap();
        browser.go_back();
        assert_eq!(browser.current(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_falls_back_to_existing_ancestor() {
        let dir = temp_dir("ancestor");
        touch(&dir, "a/b/c/");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a/b/c"));
        std::fs::remove_dir_all(dir.join("a/b")).unwrap();
        browser.refresh();
        assert_eq!(browser.current(), dir.join("a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_segments_from_root_to_current() {
        let dir = temp_dir("crumbs");
        touch(&dir, "a/b/");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a/b"));
        let segments = browser.path_segments();
        let names: Vec<String> = segments.iter().map(|(n, _)| n.clone()).collect();
        let root_name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(names, [root_name, "a".to_string(), "b".to_string()]);
        assert_eq!(segments[1].1, dir.join("a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_clears_search_and_selection() {
        let dir = temp_dir("open_clears");
        touch(&dir, "a/x.glb");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.set_search("x".into());
        browser.select(Some(dir.join("a/x.glb")));
        browser.open(dir.join("a"));
        assert_eq!(browser.search(), "");
        assert_eq!(browser.selected(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_round_trip() {
        let dir = temp_dir("settings");
        touch(&dir, "a/");
        let file = dir.join("settings/content_browser.ron");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.add_favorite(dir.join("a"));
        *browser.tile_size_mut() = 100.0;
        browser.tree_width = 250.0;
        browser.open(dir.join("a"));
        browser.save_settings(&file).unwrap();

        let mut fresh = ContentBrowser::new(dir.clone());
        fresh.load_settings(&file);
        assert_eq!(fresh.favorites(), [dir.join("a")]);
        assert_eq!(fresh.current(), dir.join("a"));
        assert!(!fresh.can_go_back());
        assert!(!fresh.is_list());
        assert_eq!(fresh.tree_width, 250.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_broken_file_gives_defaults() {
        let dir = temp_dir("settings_broken");
        let file = dir.join("content_browser.ron");
        std::fs::write(&file, "(((not ron").unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.load_settings(&file);
        assert!(browser.favorites().is_empty());
        assert_eq!(browser.current(), dir);
        assert_eq!(*browser.tile_size_mut(), theme::sizes::TILE_DEFAULT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_drop_missing_and_outside_favorites() {
        let dir = temp_dir("settings_drop");
        touch(&dir, "a/");
        let file = dir.join("content_browser.ron");
        std::fs::write(
            &file,
            r#"(version: 1, favorites: ["a", "gone", "../outside", "a/.."], tile_size: 20.0, last_folder: Some("gone"), tree_width: 200.0)"#,
        )
        .unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.load_settings(&file);
        assert_eq!(browser.favorites(), [dir.join("a")]);
        assert_eq!(browser.current(), dir);
        assert_eq!(*browser.tile_size_mut(), theme::sizes::TILE_MIN);
        assert!(browser.is_list());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_favorite_ignores_duplicates_and_files() {
        let dir = temp_dir("favorites");
        touch(&dir, "a/");
        touch(&dir, "x.glb");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.add_favorite(dir.join("a"));
        browser.add_favorite(dir.join("a"));
        browser.add_favorite(dir.join("x.glb"));
        assert_eq!(browser.favorites(), [dir.join("a")]);
        browser.remove_favorite(&dir.join("a"));
        assert!(browser.favorites().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_missing_field_keeps_the_rest() {
        let dir = temp_dir("settings_partial");
        touch(&dir, "a/");
        let file = dir.join("content_browser.ron");
        std::fs::write(&file, r#"(version: 1, favorites: ["a"])"#).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.load_settings(&file);
        assert_eq!(browser.favorites(), [dir.join("a")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn back_after_refresh_walk_up_skips_the_current_folder() {
        let dir = temp_dir("refresh_dup");
        touch(&dir, "a/x/");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a"));
        browser.open(dir.join("a/x"));
        std::fs::remove_dir_all(dir.join("a/x")).unwrap();
        browser.refresh();
        assert_eq!(browser.current(), dir.join("a"));
        browser.go_back();
        assert_eq!(browser.current(), dir, "Back leaves the folder we are in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_drops_a_vanished_selection() {
        let dir = temp_dir("vanished");
        touch(&dir, "x.glb");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.select(Some(dir.join("x.glb")));
        std::fs::remove_file(dir.join("x.glb")).unwrap();
        browser.refresh();
        assert_eq!(browser.selected(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asset_details_for_a_model() {
        let root = temp_dir("details");
        touch(&root, "assets/models/x.glb");
        let mut server = melon_core::AssetServer::new(root.clone());
        crate::asset_db::reconcile(&mut server);
        let g = server.guid_of("assets/models/x.glb");
        let x = root.join("assets/models/x.glb");
        let d = asset_details(&server, &x).unwrap();
        assert_eq!(
            (d.name.as_str(), d.path.as_str(), d.kind),
            ("x.glb", "assets/models/x.glb", AssetKind::Model)
        );
        assert_eq!(d.guid, g);
        assert!(d.guid.is_some());
        assert_eq!(d.size, 1);
        assert!(d.modified.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn asset_details_none_for_missing_or_outside() {
        let root = temp_dir("details_none");
        let server = melon_core::AssetServer::new(root.clone());
        assert!(asset_details(&server, &root.join("assets/gone.glb")).is_none());
        let outside =
            std::env::temp_dir().join(format!("melon_outside_{}.glb", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        assert!(asset_details(&server, &outside).is_none());
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn format_modified_is_utc_civil_time() {
        // 2025-11-20 12:00:00 UTC
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_763_640_000);
        assert_eq!(format_modified(t), "2025-11-20 12:00 UTC");
        let leap = std::time::UNIX_EPOCH + std::time::Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(format_modified(leap), "2000-02-29 00:00 UTC");
    }
}
