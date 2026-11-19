# Plugin API Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `melon_game` registers its own systems through a `Plugin`; dev builds reload the whole plugin live on the same `World`; the host names no game function (bug 8).

**Architecture:** `melon_core` gains `Plugin`, an `App` registration handle and plugin-owned schedules that `Core` runs after the engine's own schedule in each stage. Release builds call `core.add_plugin(GamePlugin)`. Dev builds load the `melon_game` dylib through one `unsafe` host module, check an ABI fingerprint, pass the host's `tracing::Dispatch`, and rebuild the plugin schedules when the file changes, never unloading old library copies.

**Tech Stack:** Rust 2024, bevy_ecs 0.19, libloading 0.8, tracing.

**Spec:** `docs/superpowers/specs/2026-11-19-plugin-api-design.md`

**Depends on:** `docs/superpowers/plans/2026-11-19-full-input.md` (the `PreUpdate` stage) — execute after it.

## Global Constraints

- `just gate` before every commit; `just test` at the end of each task. Conventional Commits, no trailers or tool credits, commit on `main`.
- `melon_game` depends only on `melon_core`. Never replace `core.world`.
- `unsafe_code` stays denied except: `#[unsafe(no_mangle)]` entry points in `melon_game`, and the host's `game_loader` module (`#[allow(unsafe_code)]`), both documented in AGENTS.md.
- Never unload a game library while the process runs; old schedules are dropped before the new build.
- Entry symbols: `melon_game_build: fn(&mut App, &tracing::Dispatch)` and `melon_game_abi: fn() -> u64`.

## Review Focus

- Library built with different features than the host: fingerprint mismatch → Console error, old plugin kept, no crash (test in Task 3).
- Rebuild while the old copy is in use: the reload never touches the file cargo writes (copies to a unique name first; test in Task 3).
- A reload must not run a game system twice per frame (test in Task 1).
- Broken / missing dylib at startup in a dev build: the editor still opens without the game plugin, error in the Console (test in Task 3).
- Release (`--no-default-features`) build compiles and runs the game systems statically (Task 3 verification).

---

### Task 1: `Plugin`, `App` and plugin schedules

**Files:**
- Create: `crates/melon_core/src/plugin.rs`
- Modify: `crates/melon_core/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub trait Plugin { fn build(&self, app: &mut App); }`
  - `pub struct App<'a> { world: &'a mut World, schedules: &'a mut Schedules }` with `add_systems<M>(&mut self, label: impl ScheduleLabel, systems: impl IntoScheduleConfigs<ScheduleSystem, M>) -> &mut Self`, `insert_resource<R: Resource>(&mut self, r: R) -> &mut Self`, `world(&mut self) -> &mut World`.
  - `Core` field `plugin_schedules: Schedules` (one schedule per stage: `PreUpdate`, `FixedUpdate`, `Update`, `PostUpdate`); `run_stage` runs the engine schedule, then the plugin schedule of the same label.
  - `Core::add_plugin(&mut self, plugin: &dyn Plugin)` and `Core::rebuild_plugins(&mut self, build: impl FnOnce(&mut App))` (replaces `plugin_schedules` with fresh empty schedules, then builds).
  - `pub fn abi_fingerprint() -> u64` — hash (`std::hash::DefaultHasher`) of the `TypeId`s of `App<'static>`, `World`, `Schedules`, `Transform`, `Time`, `Input`, `Actions`, `SimulationState`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn plugin_system_runs_in_its_stage_after_engine_systems() // engine Update system pushes "engine", plugin pushes "plugin": order == ["engine", "plugin"]
#[test] fn plugin_resource_is_inserted()
#[test] fn rebuild_runs_each_system_once()   // add_plugin, rebuild_plugins with same build, one update: counter == 1
#[test] fn rebuild_keeps_entities_and_resources()
#[test] fn abi_fingerprint_is_stable()       // two calls equal, non-zero
```

- [ ] **Step 2: Run** `cargo test -p melon_core --lib plugin` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): Plugin trait, App handle and plugin-owned schedules`

### Task 2: `GamePlugin`

**Files:**
- Modify: `crates/melon_game/src/lib.rs`, `crates/melon_game/Cargo.toml` (`tracing.workspace = true` if missing)

**Interfaces:**
- Consumes: Task 1.
- Produces: `pub struct GamePlugin;` (`build`: `rotation_system` and `bob_system` in `Update` with `.run_if(playing)`); `#[allow(unsafe_code)] #[unsafe(no_mangle)] pub fn melon_game_build(app: &mut App, log: &tracing::Dispatch)` (installs `log` as this library's global default with `tracing::dispatcher::set_global_default`, ignoring "already set", then `GamePlugin.build(app)`); `#[allow(unsafe_code)] #[unsafe(no_mangle)] pub fn melon_game_abi() -> u64 { melon_core::abi_fingerprint() }`. The systems drop their `#[unsafe(no_mangle)]`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn game_plugin_spins_while_playing()     // Core + add_plugin(&GamePlugin), Spin entity, Playing, one frame with dt: rotation changed
#[test] fn game_plugin_idle_while_editing()       // Editing: rotation unchanged
#[test] fn abi_entry_matches_core()               // melon_game_abi() == melon_core::abi_fingerprint()
```

- [ ] **Step 2: Run** `cargo test -p melon_game` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p melon_game` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(game): GamePlugin registers the game's own systems`

### Task 3: Host loading and live reload

**Files:**
- Create: `crates/melon_host/src/game_loader.rs`
- Modify: `crates/melon_host/src/main.rs` (remove `hot_game` and `hot-lib-reloader`), `crates/melon_host/Cargo.toml` (`libloading.workspace = true` under the `hot-reload` feature, drop `hot-lib-reloader`), root `Cargo.toml` (`libloading = "0.8"`, remove `hot-lib-reloader`), `justfile` (`game` recipe), `AGENTS.md`, `README.md`, `docs/ROADMAP.md`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces (`game_loader`, compiled with `hot-reload`):
  - `pub struct GameLoader { source: PathBuf, loaded: Vec<libloading::Library>, modified: Option<SystemTime>, since_poll: f32 }`
  - `GameLoader::new(source: PathBuf) -> Self`; `load(&mut self, core: &mut Core) -> Result<(), String>`: copy `source` to `<stem>-<n>.dll` beside it, load, read `melon_game_abi` and compare with `melon_core::abi_fingerprint()`, then `core.rebuild_plugins(|app| build(app, &tracing::dispatcher::get_default(Clone::clone)))`; on any failure return `Err` and leave the previous plugin and libraries in place. Every loaded `Library` is pushed into `loaded` and never dropped.
  - `poll(&mut self, dt: f32, core: &mut Core)`: every 0.5 s stat `source`; on a newer modified time call `load` and log "Game plugin reloaded" (info) or the error (error).
  - Release / `--no-default-features`: `core.add_plugin(&melon_game::GamePlugin)`.
  - `just game`: `cargo build -p melon_game` with the same profile as `just dev-run` (comment: the running host exe is locked, so only the game crate rebuilds).

- [ ] **Step 1: Write the failing tests** (in `game_loader.rs`, `#[cfg(feature = "hot-reload")]`)

```rust
#[test] fn copy_names_are_unique()                // copy_name(stem, 1) != copy_name(stem, 2), both beside the source
#[test] fn missing_library_is_an_error_and_keeps_core_usable() // load() on a missing path: Err, core.update() still runs
#[test] fn poll_waits_for_interval()              // poll(0.2) twice: no stat-triggered load before 0.5 s (inject a fake clock / counter)
```

- [ ] **Step 2: Run** `cargo test -p melon_host` — Expected: FAIL.
- [ ] **Step 3: Implement.** AGENTS.md: replace the "Hot reload signatures" gotcha with the plugin flow (`GamePlugin`, `just game`, unsafe exception, no game-defined component layout changes across reloads). README: hot reload section. ROADMAP: tick Plugin API, mark bug 8 fixed.
- [ ] **Step 4: Run** `just test` and `cargo build -p melon_host --release --no-default-features` — Expected: both PASS. Manual dev check: run `cargo run -p melon_host`, Play, change `ROTATION_SPEED`, `just game`; the Console shows "Game plugin reloaded" and the speed changes.
- [ ] **Step 5: Commit** `feat(host): load and live-reload the game plugin` and `docs: plugin API`
