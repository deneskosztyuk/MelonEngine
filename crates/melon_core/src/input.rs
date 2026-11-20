use std::collections::HashSet;

use bevy_ecs::resource::Resource;
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// Raw keyboard and mouse state for one frame. The host feeds it with
/// `press` / `release`; `clear_frame` (end of frame) resets the edges and deltas.
#[derive(Resource, Default, Debug, Clone)]
pub struct Input {
    pressed: HashSet<InputButton>,
    just_pressed: HashSet<InputButton>,
    just_released: HashSet<InputButton>,
    /// Raw mouse motion this frame, in device counts (`DeviceEvent::MouseMotion`, unaccelerated).
    pub mouse_delta: Vec2,
    /// Wheel notches this frame; positive = away from the user.
    pub wheel: f32,
}

/// Who owns keyboard and mouse inside the editor. `Game` only while playing.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFocus {
    #[default]
    Editor,
    Game,
}

/// True when the game's `Input` should receive raw input this frame: focus is
/// `Game`, the simulation is Playing and no text field has keyboard focus.
/// The editor writes it every frame; the host reads it.
#[derive(Resource, Default, Clone, Copy)]
pub struct GameInputGate(pub bool);

/// Every button pressed this frame (key repeats excluded), whatever the focus.
/// The host fills it and clears it after the editor UI (not in `PostUpdate`,
/// which runs before the editor), so the rebinding capture sees this frame's presses.
#[derive(Resource, Default, Debug)]
pub struct RawPresses(pub Vec<InputButton>);

/// A physical key, named by its position on a US layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyCode {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Space,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    Backquote,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
}

impl KeyCode {
    pub const ALL: &[KeyCode] = &[
        Self::A,
        Self::B,
        Self::C,
        Self::D,
        Self::E,
        Self::F,
        Self::G,
        Self::H,
        Self::I,
        Self::J,
        Self::K,
        Self::L,
        Self::M,
        Self::N,
        Self::O,
        Self::P,
        Self::Q,
        Self::R,
        Self::S,
        Self::T,
        Self::U,
        Self::V,
        Self::W,
        Self::X,
        Self::Y,
        Self::Z,
        Self::Digit0,
        Self::Digit1,
        Self::Digit2,
        Self::Digit3,
        Self::Digit4,
        Self::Digit5,
        Self::Digit6,
        Self::Digit7,
        Self::Digit8,
        Self::Digit9,
        Self::F1,
        Self::F2,
        Self::F3,
        Self::F4,
        Self::F5,
        Self::F6,
        Self::F7,
        Self::F8,
        Self::F9,
        Self::F10,
        Self::F11,
        Self::F12,
        Self::ArrowUp,
        Self::ArrowDown,
        Self::ArrowLeft,
        Self::ArrowRight,
        Self::Space,
        Self::Enter,
        Self::Escape,
        Self::Tab,
        Self::Backspace,
        Self::Delete,
        Self::Insert,
        Self::Home,
        Self::End,
        Self::PageUp,
        Self::PageDown,
        Self::ShiftLeft,
        Self::ShiftRight,
        Self::ControlLeft,
        Self::ControlRight,
        Self::AltLeft,
        Self::AltRight,
        Self::Minus,
        Self::Equal,
        Self::BracketLeft,
        Self::BracketRight,
        Self::Backslash,
        Self::Semicolon,
        Self::Quote,
        Self::Comma,
        Self::Period,
        Self::Slash,
        Self::Backquote,
        Self::Numpad0,
        Self::Numpad1,
        Self::Numpad2,
        Self::Numpad3,
        Self::Numpad4,
        Self::Numpad5,
        Self::Numpad6,
        Self::Numpad7,
        Self::Numpad8,
        Self::Numpad9,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub const ALL: &[MouseButton] = &[
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Back,
        Self::Forward,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputButton {
    Key(KeyCode),
    Mouse(MouseButton),
}

impl Input {
    /// A press of a button already held (key repeat) is not a new edge.
    pub fn press(&mut self, button: InputButton) {
        if self.pressed.insert(button) {
            self.just_pressed.insert(button);
        }
    }

    pub fn release(&mut self, button: InputButton) {
        if self.pressed.remove(&button) {
            self.just_released.insert(button);
        }
    }

    /// Release every held button (focus lost), with `just_released` edges.
    pub fn release_all(&mut self) {
        self.just_released.extend(self.pressed.drain());
    }

    pub fn pressed(&self, button: InputButton) -> bool {
        self.pressed.contains(&button)
    }

    pub fn just_pressed(&self, button: InputButton) -> bool {
        self.just_pressed.contains(&button)
    }

    pub fn just_released(&self, button: InputButton) -> bool {
        self.just_released.contains(&button)
    }

    pub fn key(&self, code: KeyCode) -> bool {
        self.pressed(InputButton::Key(code))
    }

    /// End of frame: edges, mouse delta and wheel start over. Held buttons stay.
    pub fn clear_frame(&mut self) {
        self.just_pressed.clear();
        self.just_released.clear();
        self.mouse_delta = Vec2::ZERO;
        self.wheel = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: InputButton = InputButton::Key(KeyCode::W);

    #[test]
    fn press_gives_one_frame_just_pressed() {
        let mut i = Input::default();
        i.press(W);
        assert!(i.just_pressed(W));
        assert!(i.key(KeyCode::W));
        i.clear_frame();
        assert!(i.pressed(W));
        assert!(!i.just_pressed(W));
    }

    #[test]
    fn release_gives_one_frame_just_released() {
        let mut i = Input::default();
        i.press(W);
        i.clear_frame();
        i.release(W);
        assert!(i.just_released(W));
        assert!(!i.pressed(W));
        i.clear_frame();
        assert!(!i.just_released(W));
    }

    #[test]
    fn press_and_release_in_one_frame_keeps_both_edges() {
        let mut i = Input::default();
        i.press(W);
        i.release(W);
        assert!(i.just_pressed(W));
        assert!(i.just_released(W));
        assert!(!i.pressed(W));
    }

    #[test]
    fn pressing_a_held_key_again_is_not_a_new_edge() {
        let mut i = Input::default();
        i.press(W);
        i.clear_frame();
        i.press(W);
        assert!(!i.just_pressed(W));
        assert!(i.pressed(W));
    }

    #[test]
    fn release_all_releases_with_edges() {
        let mut i = Input::default();
        let rmb = InputButton::Mouse(MouseButton::Right);
        i.press(W);
        i.press(rmb);
        i.clear_frame();
        i.release_all();
        assert!(!i.pressed(W) && !i.pressed(rmb));
        assert!(i.just_released(W) && i.just_released(rmb));
    }

    #[test]
    fn clear_frame_resets_delta_and_wheel() {
        let mut i = Input {
            mouse_delta: Vec2::new(10.0, 20.0),
            wheel: 2.0,
            ..Default::default()
        };
        i.clear_frame();
        assert_eq!(i.mouse_delta, Vec2::ZERO);
        assert_eq!(i.wheel, 0.0);
    }
}
