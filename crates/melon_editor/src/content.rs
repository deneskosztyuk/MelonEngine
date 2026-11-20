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
    /// File kinds shown; folders are always shown outside search.
    filter: HashSet<AssetKind>,
    /// What the asset view shows, rebuilt only when the folder, search or
    /// filter changes.
    visible: Vec<ContentEntry>,
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
            filter: AssetKind::FILE_KINDS.into_iter().collect(),
            visible: Vec::new(),
        };
        browser.refresh();
        browser
    }

    pub fn visible_entries(&self) -> &[ContentEntry] {
        &self.visible
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn set_search(&mut self, text: String) {
        if text != self.search {
            self.search = text;
            self.refresh();
        }
    }

    pub fn kind_enabled(&self, kind: AssetKind) -> bool {
        self.filter.contains(&kind)
    }

    pub fn toggle_kind(&mut self, kind: AssetKind) {
        if !self.filter.remove(&kind) {
            self.filter.insert(kind);
        }
        self.refresh();
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
            if dir.is_dir() {
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
    /// below it whose name contains the search (case-insensitive).
    pub fn refresh(&mut self) {
        // The folder may have been deleted or renamed outside the editor.
        while !self.current.is_dir() && self.current != self.root {
            match self.current.parent() {
                Some(parent) => self.current = parent.to_path_buf(),
                None => break,
            }
        }
        let shown = |e: &ContentEntry| e.is_dir || self.filter.contains(&e.kind);
        self.visible = if self.search.is_empty() {
            list_dir(&self.current).into_iter().filter(shown).collect()
        } else {
            let needle = self.search.to_lowercase();
            let mut files = Vec::new();
            walk_files(&self.current, &mut files);
            files.retain(|e| shown(e) && e.name.to_lowercase().contains(&needle));
            files.sort_by(|a, b| {
                (a.name.to_lowercase(), &a.path).cmp(&(b.name.to_lowercase(), &b.path))
            });
            files
        };
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.current != self.root, egui::Button::new("Up"))
                .clicked()
            {
                self.up();
            }
            if ui.button("Refresh").clicked() {
                self.refresh();
            }
            ui.separator();
            let root_name = self
                .root
                .file_name()
                .map_or("assets".into(), |n| n.to_string_lossy().into_owned());
            let mut crumb = root_name;
            if let Ok(rel) = self.current.strip_prefix(&self.root) {
                for part in rel.components() {
                    crumb.push_str(" / ");
                    crumb.push_str(&part.as_os_str().to_string_lossy());
                }
            }
            ui.label(egui::RichText::new(crumb).color(theme::colors::TEXT_MUTED));
        });
        ui.separator();
        if self.visible.is_empty() {
            ui.weak("Empty folder (or the assets folder was not found)");
            return;
        }
        let mut open = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for entry in &self.visible {
                    let (icon, detail) = if entry.is_dir {
                        ("🗀", String::new())
                    } else {
                        ("🗋", format_size(entry.size))
                    };
                    // Model files drag into the Scene or Hierarchy as their
                    // project-relative asset path.
                    let model_path = (!entry.is_dir && is_model_path(&entry.name))
                        .then(|| {
                            self.root
                                .parent()
                                .and_then(|p| project_relative(p, &entry.path))
                        })
                        .flatten();
                    let label = format!("{icon} {}", entry.name);
                    let row = ui.horizontal(|ui| {
                        let response = match model_path {
                            Some(path) => {
                                ui.dnd_drag_source(egui::Id::new(&entry.path), path, |ui| {
                                    ui.selectable_label(false, &label)
                                })
                                .inner
                            }
                            None => ui.selectable_label(false, &label),
                        };
                        if !detail.is_empty() {
                            ui.weak(detail);
                        }
                        response
                    });
                    if entry.is_dir && row.inner.double_clicked() {
                        open = Some(entry.path.clone());
                    }
                }
            });
        if let Some(dir) = open {
            self.open(dir);
        }
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

fn format_size(bytes: u64) -> String {
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
}
