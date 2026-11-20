//! Dev-build game plugin loader: loads the `melon_game` dylib, checks its ABI
//! fingerprint, builds it into the core and reloads it when the file changes.
//! Old libraries are never unloaded: bevy keeps component descriptors (with
//! drop functions inside the library) for the life of the world.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use melon_core::{App, Core};

/// Seconds between checks of the dylib's modified time.
const POLL_INTERVAL: f32 = 0.5;

pub struct GameLoader {
    source: PathBuf,
    loaded: Vec<libloading::Library>,
    modified: Option<SystemTime>,
    since_poll: f32,
}

impl GameLoader {
    pub fn new(source: PathBuf) -> Self {
        Self {
            source,
            loaded: Vec::new(),
            modified: None,
            since_poll: 0.0,
        }
    }

    /// Load the game dylib cargo builds next to the host exe (any profile or
    /// target dir). On failure the editor runs without a game plugin.
    pub fn start(core: &mut Core) -> Self {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let source = dir.join(libloading::library_filename("melon_game"));
        remove_stale_copies(&source);
        let mut loader = Self::new(source);
        match loader.load(core) {
            Ok(()) => tracing::info!("Game plugin loaded from {}", loader.source.display()),
            Err(error) => tracing::error!("Game plugin not loaded: {error}"),
        }
        loader
    }

    /// Copy the dylib to a fresh name (Windows locks loaded files, cargo must
    /// overwrite the original), load it, check its fingerprint and rebuild
    /// the plugin schedules. On failure the previous plugin stays, except when
    /// a resource changed its layout: then no plugin system runs.
    pub fn load(&mut self, core: &mut Core) -> Result<(), String> {
        self.load_expecting(core, melon_core::abi_fingerprint())
    }

    fn load_expecting(&mut self, core: &mut Core, expected: u64) -> Result<(), String> {
        let source = &self.source;
        let modified = std::fs::metadata(source)
            .and_then(|meta| meta.modified())
            .map_err(|e| format!("{}: {e}", source.display()))?;
        self.modified = Some(modified);
        let mut n = self.loaded.len();
        let mut copy = copy_name(source, n);
        // Another running host may still hold a copy from its own session.
        while copy.exists() && std::fs::remove_file(&copy).is_err() {
            n += 1;
            copy = copy_name(source, n);
        }
        std::fs::copy(source, &copy).map_err(|e| format!("copy to {}: {e}", copy.display()))?;
        // SAFETY: the library is melon_game built by this workspace; loading
        // runs only its Rust static initialisers.
        let library = unsafe { libloading::Library::new(&copy) }.map_err(|e| {
            format!(
                "{}: {e} (start dev builds through `cargo run` / `just dev-run`                  so Rust's std dll is on PATH)",
                copy.display()
            )
        })?;
        // Its code has run (initialisers, then `melon_game_abi`): never free it.
        self.loaded.push(library);
        let library = &self.loaded[self.loaded.len() - 1];
        // SAFETY: the symbol is `melon_game_abi`, declared `fn() -> u64`.
        let abi = unsafe { library.get::<fn() -> u64>(b"melon_game_abi") }
            .map_err(|e| format!("melon_game_abi: {e}"))?;
        if abi() != expected {
            return Err(
                "ABI fingerprint differs from the host; build the game with `just game`".into(),
            );
        }
        // SAFETY: the fingerprint matched, so `App` and `Dispatch` have the
        // host's layout and the symbol is `fn(&mut App, &Dispatch)`.
        let build =
            *unsafe { library.get::<fn(&mut App, &tracing::Dispatch)>(b"melon_game_build") }
                .map_err(|e| format!("melon_game_build: {e}"))?;
        core.rebuild_plugins(|app| build(app, &tracing::dispatcher::get_default(Clone::clone)))
    }

    /// Every `POLL_INTERVAL` seconds, reload the dylib if its modified time changed.
    pub fn poll(&mut self, dt: f32, core: &mut Core) {
        self.since_poll += dt;
        if self.since_poll < POLL_INTERVAL {
            return;
        }
        self.since_poll = 0.0;
        let modified = std::fs::metadata(&self.source).and_then(|meta| meta.modified());
        if modified.is_err() || modified.ok() == self.modified {
            return;
        }
        match self.load(core) {
            Ok(()) => tracing::info!("Game plugin reloaded"),
            Err(error) => {
                tracing::error!("Game plugin reload failed: {error}")
            }
        }
    }
}

impl Drop for GameLoader {
    /// Leak the libraries: the world (dropped later, or never) may still hold
    /// systems and drop functions that live in them.
    fn drop(&mut self) {
        self.loaded.drain(..).for_each(std::mem::forget);
    }
}

/// `<stem>-<n>.<ext>` beside `source`.
fn copy_name(source: &Path, n: usize) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let ext = source.extension().unwrap_or_default().to_string_lossy();
    source.with_file_name(format!("{stem}-{n}.{ext}"))
}

/// Delete `<stem>-*.<ext>` copies left by earlier runs; locked ones stay.
fn remove_stale_copies(source: &Path) {
    let (Some(dir), Some(stem), Some(ext)) = (
        source.parent(),
        source.file_stem().and_then(|s| s.to_str()),
        source.extension().and_then(|s| s.to_str()),
    ) else {
        return;
    };
    let prefix = format!("{stem}-");
    let suffix = format!(".{ext}");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(&suffix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_names_are_unique() {
        let source = PathBuf::from("target/debug/melon_game.dll");
        let (a, b) = (copy_name(&source, 1), copy_name(&source, 2));
        assert_ne!(a, b);
        assert_eq!(a.parent(), source.parent());
        assert_eq!(b.parent(), source.parent());
    }

    #[test]
    fn missing_library_is_an_error_and_keeps_core_usable() {
        let mut core = Core::new();
        let mut loader = GameLoader::new("does/not/exist/melon_game.dll".into());
        assert!(loader.load(&mut core).is_err());
        assert!(loader.loaded.is_empty());
        core.update();
    }

    #[test]
    fn loads_and_reloads_the_built_game_dylib() {
        use melon_core::{SimulationState, Spin, Time, Transform};
        // cargo builds the dependency's dylib beside this test exe.
        let built = std::env::current_exe()
            .unwrap()
            .with_file_name(libloading::library_filename("melon_game"));
        let dir = std::env::temp_dir().join(format!("melon_load_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join(built.file_name().unwrap());
        std::fs::copy(&built, &source).unwrap();
        let mut core = Core::new();
        let mut loader = GameLoader::new(source);
        loader.load(&mut core).unwrap();
        loader.load(&mut core).unwrap();
        assert_eq!(loader.loaded.len(), 2);
        let e = core.world.spawn((Transform::default(), Spin)).id();
        core.world.insert_resource(SimulationState::Playing);
        core.world.resource_mut::<Time>().update(0.2);
        core.update();
        let rot = core.world.get::<Transform>(e).unwrap().rotation;
        // Only the second build's systems run: 0.5 rad/s * 0.2 s, once.
        assert!(rot.angle_between(melon_core::glam::Quat::from_rotation_y(0.1)) < 1e-5);
    }

    #[test]
    fn fingerprint_mismatch_keeps_library_and_old_plugin() {
        use melon_core::{SimulationState, Spin, Time, Transform};
        let built = std::env::current_exe()
            .unwrap()
            .with_file_name(libloading::library_filename("melon_game"));
        let dir = std::env::temp_dir().join(format!("melon_mismatch_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join(built.file_name().unwrap());
        std::fs::copy(&built, &source).unwrap();
        let mut core = Core::new();
        let mut loader = GameLoader::new(source);
        loader.load(&mut core).unwrap();
        let error = loader
            .load_expecting(&mut core, melon_core::abi_fingerprint() ^ 1)
            .unwrap_err();
        assert!(error.contains("fingerprint"), "{error}");
        // The mismatched library ran code, so it stays loaded.
        assert_eq!(loader.loaded.len(), 2);
        let e = core.world.spawn((Transform::default(), Spin)).id();
        core.world.insert_resource(SimulationState::Playing);
        core.world.resource_mut::<Time>().update(0.2);
        core.update();
        let rot = core.world.get::<Transform>(e).unwrap().rotation;
        assert!(rot.angle_between(melon_core::glam::Quat::from_rotation_y(0.1)) < 1e-5);
    }

    #[test]
    fn unloadable_library_error_hints_at_cargo_run() {
        let dir = std::env::temp_dir().join(format!("melon_garbage_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("melon_game.dll");
        std::fs::write(&source, b"not a library").unwrap();
        let mut core = Core::new();
        let error = GameLoader::new(source).load(&mut core).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(error.contains("cargo run"), "{error}");
    }

    #[test]
    fn poll_waits_for_interval() {
        let dir = std::env::temp_dir().join(format!("melon_poll_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("melon_game.dll");
        std::fs::write(&source, b"not a library").unwrap();
        let mut core = Core::new();
        let mut loader = GameLoader::new(source);
        loader.poll(0.2, &mut core);
        loader.poll(0.2, &mut core);
        // No stat yet: a stat records the file's modified time.
        assert!(loader.modified.is_none());
        loader.poll(0.2, &mut core);
        assert!(loader.modified.is_some());
        // The garbage file failed to load; nothing stays loaded.
        assert!(loader.loaded.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
