//! Shared load/save for the RON config files (`config/input.ron`,
//! `config/quality.ron`, quality profiles).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    Loaded,
    Missing,
    Broken(String),
}

impl LoadOutcome {
    /// Log a missing or broken file (`what` names it, e.g. "quality settings");
    /// true when broken, so the first save backs the file up.
    pub fn log(&self, path: &Path, what: &str) -> bool {
        match self {
            LoadOutcome::Loaded => false,
            LoadOutcome::Missing => {
                tracing::info!("{} not found; using the default {what}", path.display());
                false
            }
            LoadOutcome::Broken(message) => {
                tracing::error!(
                    "{} is broken ({message}); using the default {what}",
                    path.display()
                );
                true
            }
        }
    }
}

/// Missing or unreadable files give `T::default()`; the file is left untouched.
pub(crate) fn load<T: DeserializeOwned + Default>(path: &Path) -> (T, LoadOutcome) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (T::default(), LoadOutcome::Missing);
        }
        Err(e) => return (T::default(), LoadOutcome::Broken(e.to_string())),
    };
    match ron::from_str(&text) {
        Ok(value) => (value, LoadOutcome::Loaded),
        Err(e) => (T::default(), LoadOutcome::Broken(e.to_string())),
    }
}

/// Atomic write (temp file + rename). With `backup_broken`, an existing
/// file is first copied to `<path>.broken` so a hand edit is never lost.
pub(crate) fn save<T: Serialize>(
    value: &T,
    path: &Path,
    backup_broken: bool,
) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if backup_broken && path.exists() {
        let backup = with_suffix(path, ".broken");
        std::fs::copy(path, &backup)
            .with_context(|| format!("backing up to {}", backup.display()))?;
    }
    let text = ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())?;
    let tmp = with_suffix(path, ".tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}
