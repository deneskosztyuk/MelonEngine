//! Packaging: turn a project into a folder someone else can run.
//!
//! `<out>/<GameName>/` holds the player executable (renamed to the game
//! name) and `game.pak`. Nothing else is needed at runtime: the player is
//! built with a static C runtime and picks the DX12 FXC shader compiler,
//! which ships with Windows, so no Windows SDK, Rust or VC++ redistributable
//! has to be installed.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::PlayerSettings;
use crate::pack::build_pack;

/// File name of the pack beside the executable.
pub const PACK_FILE: &str = "game.pak";

/// What `package_game` wrote.
#[derive(Debug)]
pub struct PackageReport {
    /// `<out>/<GameName>`
    pub dir: PathBuf,
    pub exe: PathBuf,
    /// Files in the pack.
    pub files: usize,
}

/// Write `<out>/<GameName>/` with `player_exe` and the pack of `root`.
/// An existing folder of that name is replaced.
pub fn package_game(
    root: &Path,
    player_exe: &Path,
    out: &Path,
    settings: &PlayerSettings,
) -> Result<PackageReport> {
    settings.validate().map_err(anyhow::Error::msg)?;
    if !player_exe.is_file() {
        bail!(
            "the player executable {} does not exist",
            player_exe.display()
        );
    }
    let dir = out.join(&settings.game_name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("cannot replace {}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let exe_name = match player_exe.extension() {
        Some(ext) => format!("{}.{}", settings.game_name, ext.to_string_lossy()),
        None => settings.game_name.clone(),
    };
    let exe = dir.join(exe_name);
    std::fs::copy(player_exe, &exe).with_context(|| format!("cannot copy to {}", exe.display()))?;
    let files = build_pack(root, &settings.exclude, &dir.join(PACK_FILE))?;
    Ok(PackageReport { dir, exe, files })
}

/// Build the player (release, no editor, static C runtime) in
/// `<workspace>/target/package` and return its path. `log` gets cargo's
/// output line by line. The separate target folder keeps the flags change
/// from rebuilding the normal release build.
pub fn build_player(workspace: &Path, log: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    let target = workspace.join("target").join("package");
    let mut child = Command::new("cargo")
        .args([
            "build",
            "-p",
            "melon_host",
            "--release",
            "--no-default-features",
        ])
        .arg("--target-dir")
        .arg(&target)
        .current_dir(workspace)
        .env("RUSTFLAGS", "-C target-feature=+crt-static")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run cargo")?;
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            log(&line);
        }
    }
    if !child.wait().context("cargo did not finish")?.success() {
        bail!("cargo build of the player failed");
    }
    let name = format!("melon_host{}", std::env::consts::EXE_SUFFIX);
    Ok(target.join("release").join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::Pack;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_package_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn packaging_a_project_writes_the_expected_folder() {
        let root = temp("project");
        std::fs::create_dir_all(root.join("assets/scenes")).unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("assets/scenes/main.ron"), "(scene)").unwrap();
        std::fs::write(root.join("assets/notes.md"), "private").unwrap();
        std::fs::write(root.join("config/input.ron"), "()").unwrap();
        let exe = root.join("player.exe");
        std::fs::write(&exe, b"MZ fake").unwrap();
        let out = root.join("out");
        let settings = PlayerSettings {
            game_name: "Space Melon".into(),
            ..Default::default()
        };

        let report = package_game(&root, &exe, &out, &settings).unwrap();
        assert_eq!(report.dir, out.join("Space Melon"));
        assert_eq!(std::fs::read(&report.exe).unwrap(), b"MZ fake");
        assert_eq!(report.exe, out.join("Space Melon").join("Space Melon.exe"));
        let mut names: Vec<String> = std::fs::read_dir(&report.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["Space Melon.exe", "game.pak"]);

        let pack = Pack::open(&report.dir.join(PACK_FILE)).unwrap();
        assert_eq!(report.files, 2);
        assert_eq!(
            pack.read("assets/scenes/main.ron").unwrap().unwrap(),
            b"(scene)"
        );
        assert!(pack.contains("config/input.ron"));
        assert!(
            !pack.contains("assets/notes.md"),
            "excluded by the default **/*.md"
        );

        // Packaging again replaces the folder.
        std::fs::write(report.dir.join("stale.txt"), "x").unwrap();
        let again = package_game(&root, &exe, &out, &settings).unwrap();
        assert!(!again.dir.join("stale.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_project_and_default_excludes_package_cleanly() {
        let root = temp("empty");
        let exe = root.join("p.exe");
        std::fs::write(&exe, b"x").unwrap();
        let out = root.join("out");
        let settings = PlayerSettings::default();
        let report = package_game(&root, &exe, &out, &settings).unwrap();
        assert_eq!(report.files, 0);
        assert_eq!(
            Pack::open(&report.dir.join(PACK_FILE))
                .unwrap()
                .entries()
                .count(),
            0
        );

        std::fs::create_dir_all(root.join("assets/shaders")).unwrap();
        std::fs::write(root.join("assets/shaders/x.wgsl"), "x").unwrap();
        std::fs::write(root.join("assets/keep.bin"), "k").unwrap();
        let report = package_game(&root, &exe, &out, &settings).unwrap();
        let pack = Pack::open(&report.dir.join(PACK_FILE)).unwrap();
        assert!(pack.contains("assets/keep.bin") && !pack.contains("assets/shaders/x.wgsl"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_invalid_windows_name_is_refused_before_anything_is_written() {
        let root = temp("names");
        let exe = root.join("p.exe");
        std::fs::write(&exe, b"x").unwrap();
        let out = root.join("out");
        for name in [
            "CON", "aux.txt", "COM1", "a:b", "a<b", r"a\b", "..", "", " x", "x ", "a\nb",
        ] {
            let settings = PlayerSettings {
                game_name: name.into(),
                ..Default::default()
            };
            assert!(
                package_game(&root, &exe, &out, &settings).is_err(),
                "{name:?}"
            );
        }
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_player_or_bad_name_is_an_error_and_writes_nothing() {
        let root = temp("errors");
        let out = root.join("out");
        let settings = PlayerSettings::default();
        assert!(package_game(&root, &root.join("nope.exe"), &out, &settings).is_err());
        let exe = root.join("p.exe");
        std::fs::write(&exe, b"x").unwrap();
        let bad = PlayerSettings {
            game_name: "a/b".into(),
            ..Default::default()
        };
        assert!(package_game(&root, &exe, &out, &bad).is_err());
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
