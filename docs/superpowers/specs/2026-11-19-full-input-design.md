# Full input, actions and Project Settings

Roadmap Tier 0: "Full input: all keys, `just_pressed` / `just_released`, mouse
buttons and wheel, action mapping."

## Intent

Games read *actions* ("Jump", "MoveForward"), not keys, and players' controls
are data the designer edits in the editor and the game ships with — like
Unreal's and Unity's input settings. The engine underneath tracks every key
and mouse button with per-frame edges.

## Goals

- **Raw input (`melon_core::Input`):** every common key (letters, digits,
  F1–F12, arrows, Space, Enter, Escape, Tab, Backspace, Delete, Insert, Home,
  End, PageUp, PageDown, both Shift / Ctrl / Alt, punctuation keys, numpad
  digits), mouse buttons (Left, Right, Middle, Back, Forward), mouse delta
  (raw device motion counts), wheel delta. The cursor position is deferred to a
  future visible-cursor game mode (the game cursor is hidden and confined).
  `pressed(x)`, `just_pressed(x)`, `just_released(x)` for keys and mouse
  buttons; edges last one frame (`Update`).
- **Actions:** `InputMap` resource (serialisable) of named actions.
  - Button action: any number of bindings (keys or mouse buttons);
    `pressed` / `just_pressed` / `just_released` = any binding.
  - Axis action: any number of (positive, negative) binding pairs; value in
    −1..1 (sum of pairs, clamped).
  - `Actions` resource answering `pressed(name)`, `just_pressed(name)`,
    `just_released(name)`, `axis(name)`, filled each frame by a system in a
    new `PreUpdate` stage (before `FixedUpdate` / `Update`). Unknown names
    answer false / 0.0 and log one warning per name.
- **Persistence:** `config/input.ron` under the project root. Missing file →
  built-in defaults (MoveForward W/S, MoveRight D/A, Jump Space, Fire mouse
  Left); a broken file → defaults plus an error in the Console. The broken
  file is copied to `config/input.ron.broken` before the first save
  overwrites it, so a hand-edited mapping is never lost.
- **Editor: Edit → Project Settings…** opens a window (focus stays
  `Editor` while it is used). Left: a section list
  (only "Input Mapping" for now). Right: the selected section.
  Input Mapping: one row per action — name (editable), kind (Button / Axis),
  its bindings as chips; add / remove binding, add / remove action.
  Clicking "+" on a binding (or an existing chip) waits for the next key or
  mouse button ("Press a key… Esc cancels"). Capture reads the host's raw
  winit-mapped stream (egui can't tell left / right modifiers or numpad
  digits apart), and the captured key is swallowed so it doesn't also
  trigger an editor shortcut. Changes apply live and save to
  `config/input.ron` immediately (atomic write). Duplicate or empty action
  names are refused with a status warning.
- **Input focus profiles (user: Unreal-style, Esc / click):** an
  `InputFocus` resource in `melon_core` with two states.
  - `Editor`: the editor owns keyboard and mouse (camera, W/E/R, Delete, F,
    gizmo, click-select). The game's `Input` and `Actions` stay empty.
  - `Game`: keyboard and mouse go to the game's `Input`; the editor camera,
    tool shortcuts, gizmo and picking are off, and the gizmo tool buttons
    are shown disabled. The OS cursor is hidden and confined to the window,
    and mouse movement comes from raw device motion (FPS-style look).
  - Transitions: F5 Play / resume → `Game`. Left-click on the Scene viewport
    while Playing → `Game`. **Esc** → `Editor` (the game keeps running).
    Pause, Step and Stop → `Editor`. Editing is always `Editor`.
  - Reserved for the editor while it runs a game: Esc, F5, F6, Shift+F5 —
    games can still bind Esc, it just doesn't reach them inside the editor
    (a shipped game has no editor and gets every key).
- **Raw input source:** the host records every winit keyboard / mouse event
  (and raw `DeviceEvent::MouseMotion`) into the raw input state regardless
  of whether egui used it; the game's `Input` receives it only when focus is
  `Game` and no editor text field has keyboard focus. The editor publishes
  that gate each frame; the host applies it to the next frame (one frame of
  latency at most when focus changes).

## Non-goals

- A game camera separate from the editor camera (the game moves `Camera`
  itself while it has focus); a separate Game view.
- Gamepads, touch, input contexts / layers, chords and modifiers on
  bindings, mouse-axis bindings, dead zones.
- A fixed-step-safe "pressed since last fixed step" (FixedUpdate readers use
  `pressed`; edges are for `Update`).
- A Preferences window (the window layout is reusable for it later).

## Anti-goals

- A key edge that is missed or seen twice in `Update`.
- Game input firing while the user types in an editor text field.
- A mapping edit that is lost, or a broken file that wipes the user's
  mapping.

## Approaches

| Option | Picked |
|---|---|
| Raw `Input` only, games map keys themselves | No |
| **Raw `Input` + data-driven `InputMap` / `Actions`, edited in Project Settings** | **Yes** (user) |
| Unreal Enhanced Input (contexts, modifiers, triggers) | Later; this design grows into it |
| Mapping under Preferences (per user) | No (user: Project Settings, ships with the game) |

## Design notes

- `KeyCode` and `MouseButton` stay engine types in `melon_core` (no winit
  dependency); the host maps winit codes, the editor maps egui keys for the
  rebinding capture.
- `PreUpdate` stage runs once per frame before `FixedUpdate`; the existing
  `clear_input_deltas_system` clears edges, deltas and wheel in `PostUpdate`.
- `InputMap` is RON with a `version: 1` field (same pattern as scenes).
- The Project Settings window is an egui window owned by the editor
  (`project_settings.rs`), not a dock tab.

## Tests

- Edges: press then hold over two frames → `just_pressed` only on the first;
  release → `just_released` once; press and release in one frame → both
  edges that frame, `pressed` false after.
- Mouse buttons and wheel behave like keys / reset each frame.
- Actions: any binding presses a button action; axis W/S → +1 / −1 / 0 when
  both; unknown action → false / 0.0.
- RON round-trip; missing file → defaults; broken file → defaults, error,
  file untouched.
- Host key mapping covers every `KeyCode` variant (table test).
- Editor (headless): rebinding capture assigns the next key and Esc cancels;
  duplicate name refused; edit saves the file; broken file backed up before
  the first save.
- Focus: F5 → `Game`; Esc → `Editor` while still Playing; click on the
  viewport → `Game`; Pause / Stop → `Editor`; in `Game` the editor camera and
  W/E/R don't react and the game's `Input` gets keys; in `Editor` it gets
  none; a focused text field blocks game keys.

## My calls

- Default actions as listed; `config/input.ron` path.
- String action names (data-driven); typed handles can come later.
- Auto-save on change instead of a Save button (Unity-style project
  settings).
- F5 enters `Game` focus directly (as Unreal's Play In Editor); Pause, Step
  and Stop return to `Editor`.
- Cursor hidden and confined (not locked to centre) with raw device motion;
  winit `CursorGrabMode::Confined`, falling back to `Locked` where needed.

## Left to the builder

- Exact key list beyond the goals' groups; chip and window styling (from
  `theme`); module layout.
