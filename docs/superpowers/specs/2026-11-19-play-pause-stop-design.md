# Play / Pause / Stop

Roadmap Tier 0, item 2. Fixes bug 1 (nothing sets `SimulationState::Paused`)
and the three minors deferred from the fixed-timestep review.

## Intent

Build a scene, press Play to test it, press Stop to get the scene back
exactly as built. Testing is repeatable and never damages the scene.

## Goals

- The editor opens in an edit mode where nothing simulates.
- Play snapshots the scene. Stop restores it. Pause freezes and resumes.
- Edits made while playing are thrown away on Stop (user's choice: approach A).

## Non-goals

- Undo/redo.
- Keeping selected entities across Play/Stop.
- A real ship build without the editor.

## Anti-goals

- Stop that leaves anything from the play session behind.
- Edits made in edit mode that Play silently reverts.
- Visible jumps on pause or resume.

## Approaches

| | Approach | Trade-off |
|---|---|---|
| **1** | **In-memory scene snapshot: split save/load into `scene_to_data` / `apply_scene`** | **Reuses the scene format and the in-place load fixed in bug 10. Restores only what scenes save (velocities reset, as wanted).** |
| 2 | Clone the `World` | Needs `bevy_reflect`; swapping worlds is bug 10's crash. |
| 3 | Copy components by hand | A third list of saved components; a forgotten one silently loses data. |
| 4 | Snapshot to a temp file | Works, but adds disk I/O and cleanup for nothing. |

Picked: **1** (one sensible way; the user approved moving on to design).

## Design

**States.** `SimulationState { Editing (default), Playing, Paused }`.

| | Fixed steps | `Update` game systems | `Time::elapsed_seconds` | `overstep` | Drawn pose |
|---|---|---|---|---|---|
| `Editing` | no | no | 0 | 0 | current `Transform` |
| `Playing` | yes | yes | advances | accumulates | blended |
| `Paused` | no | no | frozen | frozen | blended (same as last frame) |

- `elapsed_seconds` becomes play time: `Core::update` advances it only while
  `Playing`. `Time::update` sets `delta_seconds` only. Fixes bob drift across
  a pause.
- `overstep` is frozen while `Paused` (not zeroed), so the blended pose stays
  put and resume continues smoothly. It is always under one step, so resume
  cannot burst. Fixes the pause/resume jump.

**Transitions** live in a new module `melon_editor/src/play.rs`, as plain
functions on `&mut World`, so they are testable without a window:

- `play(world)`: from `Editing`: `scene_to_data` into a `PlaySnapshot`
  resource, then `apply_scene` from it (rebuilds physics, so rapier matches
  every edit-mode change, from gizmo or inspector), state `Playing`. From
  `Paused`: state `Playing` only.
- `pause(world)`: `Playing` to `Paused`.
- `stop(world)`: from `Playing` or `Paused`: `apply_scene` from the snapshot,
  remove `PlaySnapshot`, reset `elapsed_seconds` and `overstep` to 0, state
  `Editing`.
- `PhysicsWorld` is a resource, so these borrow it with `world.resource_scope`.

**Scene module split** (`scene.rs`): `scene_to_data(world) -> SceneData` and
`apply_scene(world, physics, &SceneData) -> Result<()>` (validation, then the
in-place clear). `save_scene` / `load_scene` become file wrappers.

**Editor UI.** The top bar's state label becomes buttons plus a label:
`▶ Play` / `⏸ Pause` / `⏹ Stop`, enabled per state, with `EDITING` (grey),
`PLAYING` (green), `PAUSED` (yellow). Shortcuts: F5 Play/Resume, F6 Pause,
Shift+F5 Stop. Save and Load are enabled only in `Editing`. Play and Stop
clear the selection (entity ids change).

**Gizmo physics write.** `write_to_physics_body` writes to rapier in every
state, not only `Playing`. Fixes fixed bodies going stale while paused, and
gizmo edits on dynamic bodies snapping back on resume.

**Startup.** Always `Editing`. `melon_host --play` calls `play` right after
the default scene spawns, for quick game runs.

**Rendering.** `render_matrix` blends when `Playing` or `Paused`, draws the
current `Transform` in `Editing`.

## Tests (all headless)

- `play` then move an entity then `stop`: original transform back; state `Editing`.
- `stop` removes entities spawned during play and restores despawned ones.
- `play` after an edit-mode move of a dynamic body: rapier body at the new spot.
- `elapsed_seconds` advances only while `Playing`; `stop` resets it to 0.
- `Paused` keeps `overstep`; resume runs at most 1 step; `Editing` zeroes it.
- `bob_system` has no drift across a pause (through `Core`).
- Gizmo physics write while `Paused` moves the rapier body.
- `render_matrix`: blends when `Paused`, current when `Editing`.
- `play` / `pause` / `stop` from the wrong state change nothing.

## My calls (correct any)

- **Play rebuilds the scene from the snapshot**, instead of syncing edits into
  rapier. It catches every edit path at once. Cost: selection clears on Play.
- **Shortcuts F5 / F6 / Shift+F5.**
- **Save and Load only in `Editing`**, so a physics-scrambled scene cannot be
  saved by accident.
- **`--play` flag** instead of a build feature. `just run` and `just ship`
  share the same build today, so a feature could not tell them apart.
- **The imgui POC** only shows the state; it gets the new variant, no buttons.

## Left to the builder

- Error handling if `apply_scene` fails during `play`/`stop` (it should not:
  the data came from the same world). Log it and stay in the current state.
