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
    History,
}

impl EditorTab {
    pub const ALL: [EditorTab; 6] = [
        EditorTab::Hierarchy,
        EditorTab::Scene,
        EditorTab::Inspector,
        EditorTab::ContentBrowser,
        EditorTab::Console,
        EditorTab::History,
    ];

    pub fn title(self) -> &'static str {
        match self {
            EditorTab::Hierarchy => "Hierarchy",
            EditorTab::Scene => "Scene",
            EditorTab::Inspector => "Inspector",
            EditorTab::ContentBrowser => "Content Browser",
            EditorTab::Console => "Console",
            EditorTab::History => "History",
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
        vec![
            EditorTab::ContentBrowser,
            EditorTab::Console,
            EditorTab::History,
        ],
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

/// Saved-layout location, relative to the project root (git-ignored).
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

/// Bring a panel to the front, or put it back if it was closed. A closed
/// panel goes into the largest panel group (by last frame's size), so the
/// Scene doesn't land in a thin bottom strip just because it had focus.
pub fn reopen(state: &mut DockState<EditorTab>, tab: EditorTab) {
    if let Some(path) = state.find_tab(&tab) {
        let _ = state.set_active_tab(path);
        return;
    }
    let largest = state
        .iter_leaves()
        .map(|(path, leaf)| (path, leaf.rect.area()))
        .filter(|(_, area)| *area > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(path, _)| path);
    if let Some(path) = largest {
        state.set_focused_node_and_surface(path);
    }
    state.push_to_focused_leaf(tab);
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
    fn unknown_tab_variant_falls_back_to_default() {
        // A layout saved by a build with a panel this build doesn't know.
        let path = temp_file("unknown");
        let text = ron::ser::to_string(&default_layout())
            .unwrap()
            .replace("Console", "Profiler");
        std::fs::write(&path, text).unwrap();
        assert_eq!(open_tabs(&load_layout(&path)), EditorTab::ALL.to_vec());
        let _ = std::fs::remove_file(&path);
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
    fn reopen_goes_to_largest_leaf() {
        let mut state = default_layout();
        let scene = state.find_tab(&EditorTab::Scene).unwrap();
        state.remove_tab(scene);
        // Pretend last frame laid out the Hierarchy leaf largest.
        for (_, leaf) in state.iter_leaves_mut() {
            let big = leaf.tabs.contains(&EditorTab::Hierarchy);
            let side = if big { 800.0 } else { 100.0 };
            leaf.rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(side, side));
        }
        // Focus the bottom leaf, where a plain push would land.
        let console = state.find_tab(&EditorTab::Console).unwrap();
        state.set_focused_node_and_surface(console.node_path());
        reopen(&mut state, EditorTab::Scene);
        let scene = state.find_tab(&EditorTab::Scene).unwrap();
        let hierarchy = state.find_tab(&EditorTab::Hierarchy).unwrap();
        assert_eq!(scene.node_path(), hierarchy.node_path());
    }

    #[test]
    fn hierarchy_filter() {
        assert!(hierarchy_matches("Ground", "gro"));
        assert!(!hierarchy_matches("Ground", "x"));
        assert!(hierarchy_matches("Ground", ""));
        assert!(hierarchy_matches("Ground", "  "));
    }

    /// Fixture: a real `.melon/editor_layout.ron` written by `save_layout` (ron 0.8).
    #[test]
    fn editor_layout_fixture_loads() {
        let text = include_str!("../fixtures/editor_layout.ron");
        let state: DockState<EditorTab> = ron::from_str(text).unwrap();
        // Saved before the History panel existed (Window menu reopens it).
        assert_eq!(open_tabs(&state), EditorTab::ALL[..5].to_vec());
        let leaves: Vec<Vec<EditorTab>> = state
            .iter_leaves()
            .map(|(_, leaf)| leaf.tabs.clone())
            .collect();
        assert_eq!(
            leaves,
            [
                vec![EditorTab::ContentBrowser, EditorTab::Console],
                vec![EditorTab::Hierarchy],
                vec![EditorTab::Scene],
                vec![EditorTab::Inspector],
            ]
        );
    }
}
