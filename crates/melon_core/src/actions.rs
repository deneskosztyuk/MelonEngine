use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Mutex,
};

use bevy_ecs::{
    resource::Resource,
    system::{Res, ResMut},
};
use serde::{Deserialize, Serialize};

use crate::input::{Input, InputButton, KeyCode, MouseButton};
pub use crate::ron_file::LoadOutcome;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionKind {
    Button,
    Axis,
}

/// One axis pair: `positive` adds +1, `negative` adds -1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisBinding {
    pub positive: InputButton,
    pub negative: InputButton,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    pub kind: ActionKind,
    pub buttons: Vec<InputButton>,
    pub axes: Vec<AxisBinding>,
}

/// Named actions and their bindings, saved as `config/input.ron`.
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputMap {
    pub version: u32,
    pub actions: Vec<Action>,
}

fn key(code: KeyCode) -> InputButton {
    InputButton::Key(code)
}

fn axis_action(name: &str, positive: KeyCode, negative: KeyCode) -> Action {
    Action {
        name: name.into(),
        kind: ActionKind::Axis,
        buttons: Vec::new(),
        axes: vec![AxisBinding {
            positive: key(positive),
            negative: key(negative),
        }],
    }
}

fn button_action(name: &str, button: InputButton) -> Action {
    Action {
        name: name.into(),
        kind: ActionKind::Button,
        buttons: vec![button],
        axes: Vec::new(),
    }
}

impl Default for InputMap {
    fn default() -> Self {
        Self {
            version: 1,
            actions: vec![
                axis_action("MoveForward", KeyCode::W, KeyCode::S),
                axis_action("MoveRight", KeyCode::D, KeyCode::A),
                button_action("Jump", key(KeyCode::Space)),
                button_action("Fire", InputButton::Mouse(MouseButton::Left)),
            ],
        }
    }
}

impl InputMap {
    /// Missing or unreadable files give the defaults; the file is left untouched.
    pub fn load(path: &Path) -> (InputMap, LoadOutcome) {
        let (mut map, outcome) = crate::ron_file::load::<InputMap>(path);
        if outcome != LoadOutcome::Loaded {
            return (map, outcome);
        }
        let mut seen = HashSet::new();
        let mut refused = Vec::new();
        map.actions.retain(|a| {
            let keep = !a.name.is_empty() && seen.insert(a.name.clone());
            if !keep {
                refused.push(format!("{:?}", a.name));
            }
            keep
        });
        if refused.is_empty() {
            (map, LoadOutcome::Loaded)
        } else {
            let msg = format!(
                "dropped empty or duplicate action names: {}",
                refused.join(", ")
            );
            (map, LoadOutcome::Broken(msg))
        }
    }

    /// Atomic write (temp file + rename). With `backup_broken`, an existing
    /// file is first copied to `<path>.broken` so a hand edit is never lost.
    pub fn save(&self, path: &Path, backup_broken: bool) -> anyhow::Result<()> {
        crate::ron_file::save(self, path, backup_broken)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ActionState {
    pressed: bool,
    just_pressed: bool,
    just_released: bool,
    axis: f32,
}

/// This frame's action values, filled by `update_actions_system`.
#[derive(Resource, Default)]
pub struct Actions {
    states: HashMap<String, ActionState>,
    warned: Mutex<HashSet<String>>,
}

impl Actions {
    pub fn pressed(&self, name: &str) -> bool {
        self.state(name).pressed
    }

    pub fn just_pressed(&self, name: &str) -> bool {
        self.state(name).just_pressed
    }

    pub fn just_released(&self, name: &str) -> bool {
        self.state(name).just_released
    }

    pub fn axis(&self, name: &str) -> f32 {
        self.state(name).axis
    }

    fn state(&self, name: &str) -> ActionState {
        if let Some(state) = self.states.get(name) {
            return *state;
        }
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        if warned.insert(name.to_owned()) {
            tracing::warn!("unknown input action {name:?}");
        }
        ActionState::default()
    }
}

pub fn update_actions_system(input: Res<Input>, map: Res<InputMap>, mut actions: ResMut<Actions>) {
    let held = |b: InputButton| if input.pressed(b) { 1.0 } else { 0.0 };
    actions.states = map
        .actions
        .iter()
        .map(|a| {
            let any = |f: fn(&Input, InputButton) -> bool| a.buttons.iter().any(|&b| f(&input, b));
            let axis: f32 = a
                .axes
                .iter()
                .map(|p| held(p.positive) - held(p.negative))
                .sum();
            let state = ActionState {
                pressed: any(Input::pressed),
                just_pressed: any(Input::just_pressed),
                just_released: any(Input::just_released),
                axis: axis.clamp(-1.0, 1.0),
            };
            (a.name.clone(), state)
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Input, InputButton, KeyCode, MouseButton};
    use bevy_ecs::{schedule::Schedule, world::World};
    use std::path::PathBuf;

    const W: InputButton = InputButton::Key(KeyCode::W);
    const S: InputButton = InputButton::Key(KeyCode::S);
    const UP: InputButton = InputButton::Key(KeyCode::ArrowUp);
    const LMB: InputButton = InputButton::Mouse(MouseButton::Left);

    fn run(map: InputMap, input: Input) -> World {
        let mut world = World::new();
        world.insert_resource(map);
        world.insert_resource(input);
        world.init_resource::<Actions>();
        let mut schedule = Schedule::default();
        schedule.add_systems(update_actions_system);
        schedule.run(&mut world);
        world
    }

    fn pressing(buttons: &[InputButton]) -> Input {
        let mut input = Input::default();
        for &b in buttons {
            input.press(b);
        }
        input
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("melon_actions_{name}_{}", std::process::id(),));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn any_binding_presses_a_button_action() {
        let mut map = InputMap::default();
        map.actions
            .iter_mut()
            .find(|a| a.name == "Jump")
            .unwrap()
            .buttons
            .push(LMB);
        let world = run(map, pressing(&[LMB]));
        let actions = world.resource::<Actions>();
        assert!(actions.pressed("Jump"));
        assert!(actions.just_pressed("Jump"));
        assert!(!actions.just_released("Jump"));
    }

    #[test]
    fn axis_sums_pairs_and_clamps() {
        let axis = |buttons: &[InputButton]| {
            run(InputMap::default(), pressing(buttons))
                .resource::<Actions>()
                .axis("MoveForward")
        };
        assert_eq!(axis(&[W]), 1.0);
        assert_eq!(axis(&[S]), -1.0);
        assert_eq!(axis(&[W, S]), 0.0);

        let mut map = InputMap::default();
        map.actions[0].axes.push(AxisBinding {
            positive: UP,
            negative: InputButton::Key(KeyCode::ArrowDown),
        });
        let world = run(map, pressing(&[W, UP]));
        assert_eq!(world.resource::<Actions>().axis("MoveForward"), 1.0);
    }

    #[test]
    fn unknown_action_is_false_and_zero() {
        let world = run(InputMap::default(), pressing(&[W]));
        let actions = world.resource::<Actions>();
        assert!(!actions.pressed("Nope"));
        assert!(!actions.just_pressed("Nope"));
        assert!(!actions.just_released("Nope"));
        assert_eq!(actions.axis("Nope"), 0.0);
    }

    #[test]
    fn ron_round_trip() {
        let map = InputMap::default();
        let text = ron::to_string(&map).unwrap();
        let back: InputMap = ron::from_str(&text).unwrap();
        assert_eq!(back, map);
        assert_eq!(back.version, 1);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        let (map, outcome) = InputMap::load(&dir.join("config/input.ron"));
        assert_eq!(outcome, LoadOutcome::Missing);
        assert_eq!(map.actions.len(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_file_gives_defaults_and_keeps_file() {
        let dir = temp_dir("broken");
        let path = dir.join("input.ron");
        std::fs::write(&path, "not ron {{").unwrap();
        let (map, outcome) = InputMap::load(&path);
        assert!(matches!(outcome, LoadOutcome::Broken(_)), "{outcome:?}");
        assert_eq!(map, InputMap::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not ron {{");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn duplicate_names_keep_first() {
        let dir = temp_dir("dup");
        let path = dir.join("input.ron");
        let mut map = InputMap::default();
        let mut second = map.actions[2].clone();
        second.buttons = vec![LMB];
        map.actions.push(second);
        std::fs::write(&path, ron::to_string(&map).unwrap()).unwrap();

        let (loaded, outcome) = InputMap::load(&path);
        let jumps: Vec<_> = loaded.actions.iter().filter(|a| a.name == "Jump").collect();
        assert_eq!(jumps.len(), 1);
        assert_eq!(jumps[0].buttons, [InputButton::Key(KeyCode::Space)]);
        match outcome {
            LoadOutcome::Broken(msg) => assert!(msg.contains("Jump"), "{msg}"),
            other => panic!("expected Broken, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_backs_up_broken_file() {
        let dir = temp_dir("backup");
        let path = dir.join("config/input.ron");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "garbage").unwrap();

        InputMap::default().save(&path, true).unwrap();
        let backup = dir.join("config/input.ron.broken");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "garbage");
        let (map, outcome) = InputMap::load(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(map, InputMap::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Fixture: `config/input.ron` as `InputMap::save` writes the defaults (ron 0.8).
    #[test]
    fn input_ron_fixture_loads() {
        let ron = r#"(
    version: 1,
    actions: [
        (
            name: "MoveForward",
            kind: Axis,
            buttons: [],
            axes: [
                (
                    positive: Key(W),
                    negative: Key(S),
                ),
            ],
        ),
        (
            name: "MoveRight",
            kind: Axis,
            buttons: [],
            axes: [
                (
                    positive: Key(D),
                    negative: Key(A),
                ),
            ],
        ),
        (
            name: "Jump",
            kind: Button,
            buttons: [
                Key(Space),
            ],
            axes: [],
        ),
        (
            name: "Fire",
            kind: Button,
            buttons: [
                Mouse(Left),
            ],
            axes: [],
        ),
    ],
)"#;
        let map: InputMap = ron::from_str(ron).unwrap();
        assert_eq!(map, InputMap::default());
    }
}
