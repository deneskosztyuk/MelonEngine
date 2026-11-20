# MelonEngine task runner. Run `just --list` to see all commands.

# Use PowerShell on Windows.
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# Default: show available commands.
default:
    @just --list

# Format all crates.
fmt:
    cargo fmt --all

# Check formatting without changing (gate).
fmt-check:
    cargo fmt --all --check

# Run clippy with warnings denied (gate).
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Compile-check all crates (gate).
check:
    cargo check --workspace

# Full gate: fmt-check + clippy + check. Run before commit and before run.
gate: fmt-check clippy check

# Run all tests. Runs the gate first, then cargo test.
test: gate
    cargo test --workspace

# Render the review scene offscreen to target/review/snapshot.png (no window).
snapshot view="overview":
    cargo run -p melon_render --example snapshot -- target/review/snapshot.png {{view}}

# Run the engine in release mode (game plugin linked statically, no live reload).
# Runs the gate first; only launches if gate passes.
run: gate
    cargo run -p melon_host --release --no-default-features --features editor

# Fast run — skip the gate, release build (use during active dev).
fast-run:
    cargo run -p melon_host --release --no-default-features --features editor

# Debug run — with live reload of the melon_game plugin. Use for game logic dev.
dev-run: gate
    cargo run -p melon_host

# Rebuild only the game plugin (same profile and features as dev-run); a running
# dev-run reloads it live. The running host exe is locked, so only the game crate
# rebuilds: the host's empty lib target builds its dependencies without linking
# the exe. `cargo build -p melon_game` alone resolves other dependency features,
# and the host refuses that library (ABI fingerprint mismatch).
game:
    cargo build -p melon_host --lib

# Ship build — release, live reload disabled (statically links melon_game).
ship:
    cargo build -p melon_host --release --no-default-features --features editor

# Run the ship build.
ship-run:
    cargo run -p melon_host --release --no-default-features --features editor

# Package the game: build the player (release, no editor, static C runtime) and
# write <out>/<GameName>/ with the executable and game.pak. The folder runs on a
# PC without Rust or the Windows SDK. Same as the editor's File > Build.
package out:
    cargo run -p melon_host --no-default-features --bin melon_package -- {{out}}
