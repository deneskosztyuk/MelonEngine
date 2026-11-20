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

/// Folder navigation inside a fixed root (the assets folder).
pub struct ContentBrowser {
    root: PathBuf,
    current: PathBuf,
    search: String,
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

    /// Open a folder (ignored if it is outside the root).
    pub fn open(&mut self, dir: PathBuf) {
        if dir.starts_with(&self.root) {
            self.current = dir;
            self.refresh();
        }
    }

    /// Go to the parent folder, never above the root.
    pub fn up(&mut self) {
        if self.current != self.root
            && let Some(parent) = self.current.parent()
        {
            self.current = parent.to_path_buf();
            self.refresh();
        }
    }

    /// Re-read the disk: the current folder, or with a search every file
    /// below it whose name contains the search (case-insensitive).
    pub fn refresh(&mut self) {
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
}
