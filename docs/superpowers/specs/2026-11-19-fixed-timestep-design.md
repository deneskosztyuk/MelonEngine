# Fixed timestep and frame stages

Roadmap Tier 0, item 1. Fixes bug 2 (physics speed tied to frame rate),
bug 3 (demo systems ignore `Time`) and an unlisted bug: the 7 host systems
share one `Schedule` with no ordering, so `sync_to_physics`, `step` and
`sync_from_physics` can run in any order.

## Intent

Simulation runs at the same speed on any monitor, and motion stays smooth at
any refresh rate. This is foundation for every later gameplay and physics item.

## Goals

- Physics and fixed-rate game logic step at a fixed 1/60 s, however fast or
  slow frames are.
- Rendered motion of fixed-step entities is smooth at 30, 60, 144 Hz.
- Systems run in a defined order, by stage.

## Non-goals

- Play / Pause / Stop UI and scene snapshot (roadmap item 2).
- Moving draw-list building out of `melon_host` (roadmap item 5).
- Plugin API for `melon_game` (roadmap item 7).

## Anti-goals

- Physics outcome that depends on frame rate, even slightly.
- Editor gizmo drags that lag or rubber-band because of interpolation.

## Approaches

| | Approach | Trade-off |
|---|---|---|
| A | Render the latest fixed step as-is | Simplest. Uneven motion on non-60 Hz displays. |
| **B** | **Blend previous and current step (interpolation)** | **Smooth at any rate. ~1 step (16 ms) of visual lag, one extra component.** |
| C | Fixed 120 Hz, no blending | Less stutter, not none. Doubles physics cost. |

Picked: **B**, by the user.

## Design

**Stages.** Bevy `ScheduleLabel`s in `melon_core`, run by `Core::update` in
this order:

1. `FixedUpdate`: run 0..N times per frame (see accumulator).
2. `Update`: once per frame.
3. `PostUpdate`: once per frame.

`Core::add_systems(label, systems)` registers into a stage.

**Time.** `Time` gains `fixed_delta` (1/60 s) and `overstep` (leftover
accumulated time). `Time::alpha()` = `overstep / fixed_delta`, in 0..1.
Frame delta is clamped to 0.25 s, so a long hitch runs at most 15 steps.

**Accumulator.** Each `Core::update`: add the frame delta to `overstep`, then
run `FixedUpdate` while `overstep >= fixed_delta`, subtracting each time.
While `SimulationState` is `Paused`, nothing accumulates and `overstep` is 0.

**Interpolation.** Opt-in component `PreviousTransform(Transform)`.

- First system in `FixedUpdate` copies `Transform` into `PreviousTransform`.
- `melon_physics` system `add_previous_transforms` (`PostUpdate`) inserts it
  on every dynamic `RigidBodyComponent` that lacks it. One system covers all
  spawn paths (default scene, scene load, editor), instead of patching each.
- `Transform::interpolate(prev, alpha)`: lerp translation and scale, slerp
  rotation.
- The host draw-list build blends entities that have `PreviousTransform`,
  only while `Playing`. When paused, it draws the current `Transform`, so
  gizmo drags show directly.

**System placement.**

| Stage | Systems, in order |
|---|---|
| `FixedUpdate` (only while `Playing`) | `store_previous_transforms`, then the physics chain: `sync_to_physics`, `step`, `sync_from_physics` |
| `Update` | `frame_counter`, `log_frame`, `fly_camera`, `rotation_system`, `bob_system` (the last two `run_if` `Playing`) |
| `PostUpdate` | `cleanup_deleted`, `add_previous_transforms` |

`step_system` sets rapier's `integration_parameters.dt` from
`Time::fixed_delta`, so the two never disagree.

**Demo systems.** `rotation_system`: 0.5 rad/s times `delta_seconds`.
`bob_system`: offset = `amplitude * (sin(t) - sin(t - dt))`, using
`elapsed_seconds`, so it has no drift. Both stay in `Update`; they are
per-frame visuals, not fixed-step state.

## Tests (all headless)

- One 1/30 s frame runs `FixedUpdate` twice; two 1/120 s frames run it once.
- `alpha()` matches the leftover; a 1 s frame is clamped to 15 steps.
- Paused: zero fixed steps, `overstep` stays 0.
- Bug 2 regression: a body dropped for 1 s of frames lands at the same height
  at 30 fps and 144 fps (within a small tolerance).
- Bug 3 regression: `bob_system` returns to its start height after one period.
- `Transform::interpolate` at alpha 0 and 1 returns prev and current.

## My calls (correct any)

- **No `PreUpdate` or `Extract` stage yet.** Neither has a system today.
  `Extract` comes with roadmap item 5, `PreUpdate` with the first system that
  needs it.
- **Interpolation is opt-in per entity**, not on every `Transform`. Blending an
  entity that also moves in `Update` would distort it.
- **Rotation and bob stay in `Update`**, frame-rate independent through
  `delta_seconds`.
- **Fixed rate 60 Hz**, matching rapier's default.
- Editor, gizmos and scene I/O stay outside the schedule, in the host, as now.

## Left to the builder

- Exact `SystemSet` names for the physics chain.
- Hot-reload: update the `hot_game` imports in `melon_host/src/main.rs` for the
  new demo-system signatures.
