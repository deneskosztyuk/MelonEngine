//! What the game wants from the mouse cursor and from the application
//! window, as plain data the host applies. Game code writes these resources;
//! the player and the editor's Play apply them (see `AGENTS.md`).

use bevy_ecs::resource::Resource;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorMode {
    /// Moves freely over the desktop.
    #[default]
    Free,
    /// Stays inside the window.
    Confined,
    /// Held in place; use `Input::mouse_delta` for look.
    Locked,
}

/// The cursor the game wants. Default: free and visible (as in Unity and
/// Godot); a first-person game sets `Cursor::locked()` when it starts.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub mode: CursorMode,
    pub visible: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            mode: CursorMode::Free,
            visible: true,
        }
    }
}

impl Cursor {
    /// Hidden and held: mouse look.
    pub fn locked() -> Self {
        Self {
            mode: CursorMode::Locked,
            visible: false,
        }
    }

    /// The host calls this with what it applied last (`None` = nothing
    /// yet, or the grab was dropped, e.g. on focus loss). Returns the cursor
    /// to apply now, if it differs from `applied` or `force` is set, and
    /// records it.
    pub fn to_apply(self, applied: &mut Option<Cursor>, force: bool) -> Option<Cursor> {
        if !force && *applied == Some(self) {
            return None;
        }
        *applied = Some(self);
        Some(self)
    }
}

/// Ask the host to quit: `world.resource_mut::<AppExit>().request()`. The
/// player closes the window at the end of the frame; the editor's Play
/// stops Play instead (the editor itself stays open).
#[derive(Resource, Default, Debug)]
pub struct AppExit {
    requested: bool,
}

impl AppExit {
    pub fn request(&mut self) {
        self.requested = true;
    }

    /// True once per request.
    pub fn take(&mut self) -> bool {
        std::mem::take(&mut self.requested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cursor_is_free_and_visible() {
        let c = Cursor::default();
        assert_eq!((c.mode, c.visible), (CursorMode::Free, true));
        assert_eq!(Cursor::locked().mode, CursorMode::Locked);
        assert!(!Cursor::locked().visible);
    }

    #[test]
    fn a_cursor_is_applied_only_when_it_changes_or_forced() {
        let mut applied = None;
        let free = Cursor::default();
        assert_eq!(
            free.to_apply(&mut applied, false),
            Some(free),
            "first frame"
        );
        assert_eq!(free.to_apply(&mut applied, false), None, "unchanged");
        assert_eq!(
            free.to_apply(&mut applied, true),
            Some(free),
            "focus regained"
        );
        let locked = Cursor::locked();
        assert_eq!(locked.to_apply(&mut applied, false), Some(locked));
        assert_eq!(locked.to_apply(&mut applied, false), None);
        assert_eq!(
            free.to_apply(&mut applied, false),
            Some(free),
            "game gave it back"
        );
    }

    #[test]
    fn a_quit_request_is_taken_once() {
        let mut exit = AppExit::default();
        assert!(!exit.take());
        exit.request();
        exit.request();
        assert!(exit.take());
        assert!(!exit.take());
    }
}
