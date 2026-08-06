use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use slint::ComponentHandle;

slint::include_modules!();

const DEFAULT_LEFT_PANE_WIDTH: f32 = 220.0;
const DEFAULT_RIGHT_PANE_WIDTH: f32 = 260.0;
const DEFAULT_BOTTOM_PANE_HEIGHT: f32 = 200.0;
const MIN_LEFT_PANE_WIDTH: f32 = 180.0;
const MAX_LEFT_PANE_WIDTH: f32 = 360.0;
const MIN_RIGHT_PANE_WIDTH: f32 = 200.0;
const MAX_RIGHT_PANE_WIDTH: f32 = 360.0;
const MIN_BOTTOM_PANE_HEIGHT: f32 = 72.0;
const MAX_BOTTOM_PANE_HEIGHT: f32 = 640.0;

/// Persistent, Rust-owned editor layout model.
///
/// Pane sizes are persisted as logical pixels and clamped to a *hard* range
/// here (a safety net on load/save). Live, per-window clamping happens in the
/// Slint UI (see `editor.slint`: `side-pane-available` / `bottom-pane-available`
/// and `clamp-panes`), which bounds the panes by the current window size so a
/// restored or dragged layout can never push the window past its own bounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct DockLayout {
    console_in_right: bool,
    left_pane_width: f32,
    right_pane_width: f32,
    bottom_pane_height: f32,
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            console_in_right: false,
            left_pane_width: DEFAULT_LEFT_PANE_WIDTH,
            right_pane_width: DEFAULT_RIGHT_PANE_WIDTH,
            bottom_pane_height: DEFAULT_BOTTOM_PANE_HEIGHT,
        }
    }
}

impl DockLayout {
    fn sanitized(self) -> Self {
        Self {
            console_in_right: self.console_in_right,
            left_pane_width: self
                .left_pane_width
                .clamp(MIN_LEFT_PANE_WIDTH, MAX_LEFT_PANE_WIDTH),
            right_pane_width: self
                .right_pane_width
                .clamp(MIN_RIGHT_PANE_WIDTH, MAX_RIGHT_PANE_WIDTH),
            bottom_pane_height: self
                .bottom_pane_height
                .clamp(MIN_BOTTOM_PANE_HEIGHT, MAX_BOTTOM_PANE_HEIGHT),
        }
    }
}

fn layout_path() -> PathBuf {
    PathBuf::from(".melon").join("slint-layout.ron")
}

fn load_layout() -> DockLayout {
    std::fs::read_to_string(layout_path())
        .ok()
        .and_then(|s| ron::from_str::<DockLayout>(&s).ok())
        .unwrap_or_default()
        .sanitized()
}

fn save_layout(layout: DockLayout) {
    if let Ok(ron) = ron::ser::to_string_pretty(&layout, ron::ser::PrettyConfig::default())
        && std::fs::create_dir_all(".melon").is_ok()
    {
        let _ = std::fs::write(layout_path(), ron);
    }
}

fn sync_layout(app: &EditorWindow, layout: &DockLayout) {
    app.set_console_in_right(layout.console_in_right);
    app.set_left_pane_width(layout.left_pane_width);
    app.set_right_pane_width(layout.right_pane_width);
    app.set_bottom_pane_height(layout.bottom_pane_height);
}

fn capture_layout(app: &EditorWindow, console_in_right: bool) -> DockLayout {
    DockLayout {
        console_in_right,
        left_pane_width: app.get_left_pane_width(),
        right_pane_width: app.get_right_pane_width(),
        bottom_pane_height: app.get_bottom_pane_height(),
    }
    .sanitized()
}

pub fn create_editor_window() -> Result<EditorWindow, slint::PlatformError> {
    let window = EditorWindow::new()?;
    let layout = Rc::new(RefCell::new(load_layout()));
    sync_layout(&window, &layout.borrow());

    let window_weak = window.as_weak();
    let layout_handle = layout.clone();
    window.on_layout_changed(move || {
        if let Some(app) = window_weak.upgrade() {
            let next = capture_layout(&app, layout_handle.borrow().console_in_right);
            *layout_handle.borrow_mut() = next;
            save_layout(next);
        }
    });

    // Track which tab (if any) is being dragged. Only Console can be docked
    // elsewhere in the spike.
    let dragging = Rc::new(RefCell::new(false));

    let window_weak = window.as_weak();
    let dragging_handle = dragging.clone();
    window.on_tab_pressed(move |tab| {
        if tab == "Console" {
            *dragging_handle.borrow_mut() = true;
        }
        if let Some(app) = window_weak.upgrade() {
            app.set_drag_active(*dragging_handle.borrow());
        }
    });

    let window_weak = window.as_weak();
    let layout_handle = layout.clone();
    let dragging_handle = dragging.clone();
    window.on_tab_released(move |tab| {
        let mut was_dragging = dragging_handle.borrow_mut();
        let is_console = tab == "Console";
        let should_relocate = *was_dragging && is_console;
        *was_dragging = false;

        if let Some(app) = window_weak.upgrade() {
            app.set_drag_active(false);

            if should_relocate {
                // Hit-test against the two dock groups while dragging. The
                // drop-target flags are driven by hover inside the .slint UI.
                let target_right = app.get_drop_target_right();
                let target_bottom = app.get_drop_target_bottom();
                if target_right != target_bottom {
                    let mut layout = layout_handle.borrow_mut();
                    layout.console_in_right = target_right;
                    save_layout(*layout);
                    sync_layout(&app, &layout);
                    if target_right {
                        app.set_right_active_tab("Console".into());
                    } else {
                        app.set_bottom_active_tab("Console".into());
                    }
                }
            }
        }
    });

    Ok(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_layout_uses_default_pane_sizes() {
        let layout: DockLayout = ron::from_str("(console_in_right:true)").unwrap();

        assert!(layout.console_in_right);
        assert_eq!(layout.left_pane_width, DEFAULT_LEFT_PANE_WIDTH);
        assert_eq!(layout.right_pane_width, DEFAULT_RIGHT_PANE_WIDTH);
        assert_eq!(layout.bottom_pane_height, DEFAULT_BOTTOM_PANE_HEIGHT);
    }

    #[test]
    fn sanitization_clamps_invalid_pane_sizes() {
        let layout = DockLayout {
            left_pane_width: -10.0,
            right_pane_width: 900.0,
            bottom_pane_height: f32::INFINITY,
            ..DockLayout::default()
        }
        .sanitized();

        assert_eq!(layout.left_pane_width, MIN_LEFT_PANE_WIDTH);
        assert_eq!(layout.right_pane_width, MAX_RIGHT_PANE_WIDTH);
        assert_eq!(layout.bottom_pane_height, MAX_BOTTOM_PANE_HEIGHT);
    }

    #[test]
    fn layout_round_trips() {
        let expected = DockLayout {
            console_in_right: true,
            left_pane_width: 240.0,
            right_pane_width: 300.0,
            bottom_pane_height: 180.0,
        };
        let serialized = ron::to_string(&expected).unwrap();
        let actual: DockLayout = ron::from_str(&serialized).unwrap();

        assert_eq!(actual.console_in_right, expected.console_in_right);
        assert_eq!(actual.left_pane_width, expected.left_pane_width);
        assert_eq!(actual.right_pane_width, expected.right_pane_width);
        assert_eq!(actual.bottom_pane_height, expected.bottom_pane_height);
    }
}
