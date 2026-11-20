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
    let text = match crate::pack::installed_read(path)
        .unwrap_or_else(|| std::fs::read(path))
        .and_then(|b| String::from_utf8(b).map_err(std::io::Error::other))
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(serde::Deserialize, Default, PartialEq, Debug)]
    struct Cfg {
        n: i32,
    }

    #[test]
    fn config_loads_from_the_installed_pack_not_the_disk() {
        let root = std::env::temp_dir().join(format!("melon_ron_pack_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/x.ron"), "(n: 7)").unwrap();
        crate::pack::build_pack(&root, &[], &root.join("game.pak")).unwrap();
        std::fs::remove_file(root.join("config/x.ron")).unwrap();

        let (missing, outcome) = load::<Cfg>(&root.join("config/x.ron"));
        assert_eq!((missing, outcome), (Cfg::default(), LoadOutcome::Missing));
        let pack = crate::pack::Pack::open(&root.join("game.pak")).unwrap();
        crate::pack::install(&root, Arc::new(pack));
        let (cfg, outcome) = load::<Cfg>(&root.join("config/x.ron"));
        assert_eq!((cfg, outcome), (Cfg { n: 7 }, LoadOutcome::Loaded));
        let _ = std::fs::remove_dir_all(&root);
    }
}
