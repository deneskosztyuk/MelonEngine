//! Dockable editor layout: which panels exist, the default arrangement, and
//! saving / restoring the user's arrangement between runs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use egui_dock::{DockState, NodeIndex};
use serde::{Deserialize, Serialize};

use crate::theme::sizes;

/// Every dockable editor panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EditorTab {
    Hierarchy,
    Scene,
    Inspector,
    ContentBrowser,
    Console,
}

impl EditorTab {
    pub const ALL: [EditorTab; 5] = [
        EditorTab::Hierarchy,
        EditorTab::Scene,
        EditorTab::Inspector,
        EditorTab::ContentBrowser,
        EditorTab::Console,
    ];

    pub fn title(self) -> &'static str {
        match self {
            EditorTab::Hierarchy => "Hierarchy",
            EditorTab::Scene => "Scene",
            EditorTab::Inspector => "Inspector",
            EditorTab::ContentBrowser => "Content Browser",
            EditorTab::Console => "Console",
        }
    }
}

/// Reference window size the default pane widths are fractions of.
const REFERENCE_WIDTH: f32 = 1440.0;
const REFERENCE_HEIGHT: f32 = 900.0;

/// Unity-style default: Content Browser + Console along the bottom; above it
/// Hierarchy on the left, Scene in the middle, Inspector on the right.
pub fn default_layout() -> DockState<EditorTab> {
    let mut state = DockState::new(vec![EditorTab::Scene]);
    let tree = state.main_surface_mut();
    // `split_*` returns [old, new]; `fraction` is the share of the first
    // (left / top) child, whichever of the two that is.
    let [top, _bottom] = tree.split_below(
        NodeIndex::root(),
        1.0 - sizes::BOTTOM_PANE / REFERENCE_HEIGHT,
        vec![EditorTab::ContentBrowser, EditorTab::Console],
    );
    let [scene, _left] = tree.split_left(
        top,
        sizes::LEFT_PANE / REFERENCE_WIDTH,
        vec![EditorTab::Hierarchy],
    );
    tree.split_right(
        scene,
        1.0 - sizes::RIGHT_PANE / (REFERENCE_WIDTH - sizes::LEFT_PANE),
        vec![EditorTab::Inspector],
    );
    state
}

/// Default location of the saved layout (git-ignored).
pub fn layout_path() -> PathBuf {
    PathBuf::from(".melon/editor_layout.ron")
}

/// Load a saved layout; a missing, unreadable or corrupt file gives the default.
pub fn load_layout(path: &Path) -> DockState<EditorTab> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| ron::from_str(&text).ok())
        .unwrap_or_else(default_layout)
}

/// Save the layout atomically (temp file + rename).
pub fn save_layout(state: &DockState<EditorTab>, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let text = ron::ser::to_string_pretty(state, ron::ser::PrettyConfig::default())
        .context("failed to serialize editor layout")?;
    let tmp = path.with_extension("ron.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Bring a panel to the front, or put it back if it was closed.
pub fn reopen(state: &mut DockState<EditorTab>, tab: EditorTab) {
    match state.find_tab(&tab) {
        Some(path) => {
            let _ = state.set_active_tab(path);
        }
        None => state.push_to_focused_leaf(tab),
    }
}

/// Hierarchy search: case-insensitive substring; a blank query matches all.
pub fn hierarchy_matches(name: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("melon_layout_{name}_{}.ron", std::process::id()))
    }

    fn open_tabs(state: &DockState<EditorTab>) -> Vec<EditorTab> {
        let mut tabs: Vec<EditorTab> = state.iter_all_tabs().map(|(_, t)| *t).collect();
        tabs.sort_by_key(|t| *t as u8);
        tabs
    }

    #[test]
    fn default_layout_has_all_tabs() {
        assert_eq!(open_tabs(&default_layout()), EditorTab::ALL.to_vec());
    }

    #[test]
    fn layout_roundtrip() {
        let path = temp_file("roundtrip");
        let mut state = default_layout();
        let console = state.find_tab(&EditorTab::Console).unwrap();
        state.remove_tab(console);
        save_layout(&state, &path).unwrap();
        let loaded = load_layout(&path);
        assert_eq!(open_tabs(&loaded), open_tabs(&state));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_layout_falls_back_to_default() {
        let path = temp_file("corrupt");
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(open_tabs(&load_layout(&path)), EditorTab::ALL.to_vec());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_layout_falls_back_to_default() {
        let path = temp_file("missing");
        let _ = std::fs::remove_file(&path);
        assert_eq!(open_tabs(&load_layout(&path)), EditorTab::ALL.to_vec());
    }

    #[test]
    fn reopen_closed_tab() {
        let mut state = default_layout();
        let console = state.find_tab(&EditorTab::Console).unwrap();
        state.remove_tab(console);
        assert!(state.find_tab(&EditorTab::Console).is_none());
        reopen(&mut state, EditorTab::Console);
        assert!(state.find_tab(&EditorTab::Console).is_some());
        // Reopening an open tab does not duplicate it.
        reopen(&mut state, EditorTab::Console);
        assert_eq!(
            state
                .iter_all_tabs()
                .filter(|(_, t)| **t == EditorTab::Console)
                .count(),
            1
        );
    }

    #[test]
    fn hierarchy_filter() {
        assert!(hierarchy_matches("Ground", "gro"));
        assert!(!hierarchy_matches("Ground", "x"));
        assert!(hierarchy_matches("Ground", ""));
        assert!(hierarchy_matches("Ground", "  "));
    }
}
