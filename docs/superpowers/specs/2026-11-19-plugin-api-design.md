# Plugin API with whole-plugin hot reload

Roadmap Tier 0: "Plugin API so `melon_game` registers its own systems.
Fixes bug 8." Bug 8: game systems are hard-coded in the host and their
signatures are duplicated in the host's `hot_module`.

## Intent

A game is a plugin: it registers its own systems, resources and actions. The
host never names a game function. In dev builds, rebuilding the game crate
reloads the whole plugin live while the scene (ECS state) stays as it is.
User picked whole-plugin reload (option A) as the north star.

## Goals

- `melon_core::Plugin` trait: `fn build(&self, app: &mut App)` where `App`
  is a registration handle that records systems by stage
  (`FixedUpdate`, `PreUpdate`, `Update`, `PostUpdate`) and can insert
  resources into the world.
- `Core::add_plugin(plugin)` builds it into plugin-owned schedules that
  `Core` runs after the engine's own systems in each stage.
- `melon_game` exports one entry point (`GamePlugin` plus a `#[no_mangle]`
  builder function for the dynamic path). `rotation_system` and `bob_system`
  register through it, with `.run_if(playing)` inside the plugin.
- **Release / ship build:** the host links `melon_game` statically and calls
  `core.add_plugin(melon_game::GamePlugin)`.
- **Dev build (`hot-reload` feature):** the host loads the `melon_game`
  dylib, calls its builder, and watches the file. When it changes: drop the
  plugin's schedules, load the new library, call the builder again — same
  `World`, same entities and resources.
- The `hot_game` module and its duplicated imports disappear from the host.
- A Console line on every reload ("Game plugin reloaded") and an error line
  (keeping the old plugin) when the new library fails to load.

## Non-goals

- Several game plugins, plugin dependencies / ordering between plugins.
- Reloading engine crates.
- Migrating game-defined component data across a reload when its layout
  changes (documented as unsupported).

## Anti-goals

- A crash on reload (dangling function pointers or drop glue into an
  unloaded library).
- Running two copies of a game system after a reload.
- Replacing `core.world` (bevy systems are bound to their world).

## Approaches

| Option | Picked |
|---|---|
| **A. Whole-plugin reload: drop the plugin's schedules, reload, build again** | **Yes** (user) |
| B. Plugins without live reload | No |
| C. Keep per-function wrappers (host still lists functions) | No |

## Design notes

- **Never unload old libraries.** Component / resource descriptors created
  by game code can hold drop functions inside the library, and bevy keeps
  descriptors for the life of the world. Each reload loads a fresh copy of
  the dylib under a unique file name and keeps old copies loaded (dev-only
  memory cost). Old schedules are dropped before building the new ones.
- Plugin-owned schedules: one per stage, separate from the engine's, so a
  reload can replace exactly the game's systems.
- `PreUpdate` comes from the input work, which lands first; plugins can use it.
- The builder symbol has a fixed name and a Rust-ABI signature
  `fn(&mut App, &tracing::Dispatch)`; the host and the game are built by the
  same compiler in one workspace.
- **Compatibility handshake.** The dylib also exports a fingerprint function;
  `melon_core::abi_fingerprint()` hashes the `TypeId`s of the types that
  cross the boundary (`App`, `World`, `Transform`, `Time`, `Input`, ...).
  The host compares the library's value with its own and refuses a mismatch
  (Console error, old plugin kept). This catches a game built with different
  dependency features than the host instead of crashing.
- **Logging.** The dylib has its own `tracing` statics; the builder receives
  the host's `Dispatch` and installs it as the dylib's global default, so
  game logs reach the Console in dev builds too.
- **Rebuild command.** A `just game` recipe rebuilds the game library with
  the same profile and features as `just dev-run` (the running host exe is
  locked on Windows, so only the game crate is rebuilt).
- **Unsafe.** Loading the library and resolving its symbols is `unsafe`.
  This is the second allowed exception: one host module (`game_loader`) with
  `#[allow(unsafe_code)]`, documented in AGENTS.md. `hot-lib-reloader`'s
  `LibReloader` closes old libraries on reload, so the loader uses
  `libloading` directly (copy to a unique name, load, keep loaded).
- Watch: poll the dylib's modified time from the host (same idea as the asset
  server); rebuild is the user's `cargo build -p melon_game`.

## Tests

- `add_plugin` registers a system that runs in its stage after engine
  systems; a resource inserted by the plugin exists.
- Rebuilding plugin schedules (simulated reload: drop + build again) runs
  each system once per frame, not twice, and keeps entities / resources.
- `run_if(playing)` from the plugin: no game system runs while Editing.
- `GamePlugin` builds `rotation_system` and `bob_system` (spin and bob still
  move entities while Playing).
- Host: no game function names outside the plugin entry (grep-style test or
  review), dev and release builds both compile.
- Manual (dev build): edit `ROTATION_SPEED`, rebuild `melon_game`, see the
  speed change without restarting; Console shows the reload line.

## My calls

- `App` as a thin registration handle over `Core` (not a full bevy `App`).
- ABI fingerprint handshake, `Dispatch` passed to the game, `just game`.
- A second `unsafe` exception (host `game_loader` module) — required by
  dynamic loading.
- Keep old libraries loaded; unique-name copies.
- Poll-based watch.

## Left to the builder

- File-copy naming; module layout.
