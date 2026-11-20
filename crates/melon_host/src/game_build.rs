//! Empty on purpose: `just game` runs `cargo build -p melon_host --lib`, which
//! rebuilds the melon_game dylib with the host's dependency features (so its ABI
//! fingerprint matches) without relinking the running, locked host exe.
