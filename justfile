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

# Run the engine in release mode (optimized glam, no hot-reload overhead).
# Runs the gate first; only launches if gate passes.
run: gate
    cargo run -p melon_host --release --no-default-features

# Fast run — skip the gate, release build (use during active dev).
fast-run:
    cargo run -p melon_host --release --no-default-features

# Debug run — unoptimized, with hot-reload for melon_game. Use for game logic dev.
dev-run: gate
    cargo run -p melon_host

# Ship build — release, hot-reload disabled (statically links melon_game).
ship:
    cargo build -p melon_host --release --no-default-features

# Run the ship build.
ship-run:
    cargo run -p melon_host --release --no-default-features