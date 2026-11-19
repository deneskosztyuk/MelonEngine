//! Frame stages. `Core::update` runs them in this order each frame:
//! `PreUpdate`, `FixedUpdate` (0..N times, at `Time::fixed_delta`), `Update`,
//! `PostUpdate`.

use bevy_ecs::schedule::ScheduleLabel;

/// Once per frame, before the fixed steps: turn raw input into game state.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreUpdate;

/// Fixed-rate simulation: physics and anything that must not depend on frame rate.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FixedUpdate;

/// Once per frame. Read `Time::delta_seconds` for frame-rate independence.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Update;

/// Once per frame, after `Update`: cleanup and bookkeeping.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PostUpdate;
