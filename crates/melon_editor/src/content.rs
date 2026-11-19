//! Content Browser panel: browse the project's `assets/` folder.

use std::path::{Path, PathBuf};

use crate::theme;

/// One file or folder in a listing.
#[derive(Clone, Debug)]
pub struct ContentEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// File size in bytes (0 for folders).
    pub size: u64,
}

/// Folders first, then files, each sorted case-insensitively.
/// A missing or unreadable folder lists as empty.
pub fn list_dir(path: &Path) -> Vec<ContentEntry> {
    let Ok(read) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<ContentEntry> = read
        .filter_map(Result::ok)
        .map(|entry| {
            let meta = entry.metadata().ok();
            ContentEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path(),
                is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
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

/// Folder navigation inside a fixed root (the assets folder).
pub struct ContentBrowser {
    root: PathBuf,
    current: PathBuf,
    entries: Vec<ContentEntry>,
}

impl ContentBrowser {
    pub fn new(root: PathBuf) -> Self {
        let entries = list_dir(&root);
        Self {
            current: root.clone(),
            root,
            entries,
        }
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

    pub fn refresh(&mut self) {
        self.entries = list_dir(&self.current);
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
        if self.entries.is_empty() {
            ui.weak("Empty folder (or the assets folder was not found)");
            return;
        }
        let mut open = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for entry in &self.entries {
                    let (icon, detail) = if entry.is_dir {
                        ("🗀", String::new())
                    } else {
                        ("🗋", format_size(entry.size))
                    };
                    let row = ui.horizontal(|ui| {
                        let response = ui.selectable_label(false, format!("{icon} {}", entry.name));
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
}
