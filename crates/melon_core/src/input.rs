use std::collections::HashSet;

use bevy_ecs::resource::Resource;
use glam::Vec2;

#[derive(Resource, Default, Debug, Clone)]
pub struct Input {
    pub keys_pressed: HashSet<KeyCode>,
    pub mouse_delta: Vec2,
    pub right_mouse_held: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    W,
    A,
    S,
    D,
    Space,
    LeftShift,
}

impl Input {
    pub fn key(&self, code: KeyCode) -> bool {
        self.keys_pressed.contains(&code)
    }

    pub fn take_mouse_delta(&mut self) -> Vec2 {
        let d = self.mouse_delta;
        self.mouse_delta = Vec2::ZERO;
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_default_is_empty() {
        let i = Input::default();
        assert!(i.keys_pressed.is_empty());
        assert_eq!(i.mouse_delta, Vec2::ZERO);
        assert!(!i.right_mouse_held);
    }

    #[test]
    fn input_key_returns_pressed_state() {
        let mut i = Input::default();
        i.keys_pressed.insert(KeyCode::W);
        assert!(i.key(KeyCode::W));
        assert!(!i.key(KeyCode::A));
    }

    #[test]
    fn take_mouse_delta_clears() {
        let mut i = Input {
            mouse_delta: Vec2::new(10.0, 20.0),
            ..Default::default()
        };
        assert_eq!(i.take_mouse_delta(), Vec2::new(10.0, 20.0));
        assert_eq!(i.take_mouse_delta(), Vec2::ZERO);
    }
}
