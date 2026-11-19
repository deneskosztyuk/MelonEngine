# ADR-0001: Editor UI library

**Status:** Accepted (2026-11-19)
**Date:** 2026-11-19
**Deciders:** Denes (owner)

## Context

The editor should end up looking and working like Unity / Unreal / Godot:
docking panels with tabs, a menu bar, a toolbar, hierarchy, inspector,
content browser, console, and the scene viewport. Most of the code will be
written by an AI agent. Three UI libraries are in the repo today:

| | egui 0.35 | Dear ImGui (imgui-rs 0.12) | Slint 1.17.1 |
|---|---|---|---|
| State in repo | The working editor: hierarchy, inspector, gizmo, play/pause/stop, save/load, new viewport panel with Unity controls | `imgui-poc` feature: menu bar and 5 empty panels, scene image, no logic, frozen camera | `melon_slint_poc` binary: polished Unreal-like mock-up (menu, toolbar, panes, tabs, status bar), display-only data, own render loop |
| Code | ~1,600 lines Rust (`lib.rs`, `gizmo.rs`), plus UI-independent `viewport.rs` / `play.rs` / `scene.rs` | ~320 lines Rust | ~970 lines `.slint` + ~450 lines Rust |

Screenshots of all three were taken on 2026-11-19 at 2880×1755.

Constraints: Rust 2024, wgpu 29, winit 0.30, `unsafe_code` denied in our
crates, solo developer plus AI agent, Windows first.

## Decision

**egui**, with **`egui_dock` 0.20** (MIT, supports egui 0.35) for
docking and tabs, and a custom theme (colours, spacing, font, icons) modelled
on the Slint mock-up's look. Remove the Dear ImGui and Slint experiments once
the egui layout reaches the mock-up's level.

## Options considered

### A. egui + egui_dock

| Dimension | Assessment |
|---|---|
| Complexity | Low: all editor logic already lives here |
| Docking | `egui_dock` 0.20: tabs, drag-to-dock, splits; no OS-level floating windows |
| Look | Plain by default; fully restylable (`Style`, `Visuals`, fonts) |
| Build | Pure Rust, fast |
| Agent fit | High: immediate mode, plain Rust, no second language |
| Upgrade path | egui 0.36+ needs wgpu 30; `egui_dock` tracks egui releases |

**Pros:** nothing to port; viewport, gizmo and inspector already work; pure
Rust; easiest for an agent to extend; dynamic inspectors (any component) are
natural in immediate mode.
**Cons:** default look is "programmer UI" until themed; complex widgets (tree
view with drag-reorder, property grid) are hand-built.

### B. Dear ImGui (imgui-rs + docking branch)

| Dimension | Assessment |
|---|---|
| Complexity | Medium: port all editor logic from egui |
| Docking | Best in class: docking + multi-viewport (floating OS windows) |
| Look | Dated by default; themable but bitmap font look persists without work |
| Build | C++ compile via FFI; `unsafe` across the boundary |
| Agent fit | Good: immediate mode Rust |
| Upgrade path | imgui-rs releases are slow; `imgui-wgpu` must match each wgpu bump |

**Pros:** the docking every game-editor user knows; proven in shipped engines.
**Cons:** full port; C++ toolchain; binding maintenance risk; least modern look.

### C. Slint

| Dimension | Assessment |
|---|---|
| Complexity | High: port all logic and bridge every dynamic list/inspector through Slint models and callbacks |
| Docking | None built in; the mock-up hand-rolls one fixed tab move |
| Look | Best out of the box; declarative `.slint` with live preview |
| Build | Skia renderer, slow builds (~1.5 min incremental release) |
| Agent fit | Medium: a second language plus Rust glue for every feature |
| Upgrade path | wgpu integration is `unstable-wgpu-29`, pinned `=1.17.1`; each wgpu bump waits on Slint |
| Licence | GPLv3, or royalty-free for desktop with attribution, or paid |

**Pros:** the nicest result for the least styling effort; accessibility.
**Cons:** no docking; retained mode fights an inspector that shows whatever
components an entity has; Slint owns the window and event loop, so the host
loop changes; licence attribution; pinned unstable wgpu feature.

## Trade-off analysis

The real choice is **look out of the box (Slint)** vs **the working editor and
low glue cost (egui)**. Docking, the feature Unity/Unreal/Godot users notice
most, is available for egui today via `egui_dock`; Slint has none. Dear ImGui's
docking is better (floating OS windows), but costs a full port, a C++
dependency and the most dated look.

The look gap is closable in egui: the Slint mock-up's colours, spacing and
font can be reproduced with egui's style system. The other gaps (Slint's
docking, ImGui's port and FFI) are structural.

## Consequences

- Easier: every new editor feature is plain Rust in one place; the agent adds
  panels as `egui_dock` tabs.
- Harder: matching the mock-up's polish takes deliberate theme work; floating
  panels as separate OS windows are not available.
- Revisit: if floating multi-window docking becomes essential, re-evaluate
  Dear ImGui.

## Action items

1. [ ] Add `egui_dock` 0.20 and move Hierarchy, Inspector, Scene, Console,
       Content Browser into dock tabs; persist the layout.
2. [ ] Menu bar + toolbar (tool modes, Play/Pause/Step) + status bar,
       following the Slint mock-up.
3. [ ] Theme: colours and spacing from `ui/theme.slint`, Inter-style font,
       icon set.
4. [ ] Remove the `imgui-poc` and `slint-poc` features, their code and
       dependencies.
