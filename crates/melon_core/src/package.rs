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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::PlayerSettings;
use crate::pack::build_pack;

/// File name of the pack beside the executable.
pub const PACK_FILE: &str = "game.pak";

/// Written into every packaged folder; only a folder holding it (and the
/// pack) is ever replaced by a later build.
pub const MANIFEST_FILE: &str = "build.manifest";

/// `p` made absolute, without `.` / `..`, with the deepest existing part
/// resolved (symlinks, true case), so two spellings of a folder compare equal.
fn resolve(p: &Path) -> PathBuf {
    use std::path::Component;
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut clean = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other),
        }
    }
    let (mut tail, mut cur) = (Vec::new(), clean.clone());
    loop {
        if let Ok(mut found) = cur.canonicalize() {
            found.extend(tail.iter().rev());
            return found;
        }
        match (cur.file_name().map(|n| n.to_owned()), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                cur = parent.to_path_buf();
            }
            _ => return clean,
        }
    }
}

/// `path` equals or lies inside `base` (case-insensitive: Windows paths).
fn inside(path: &Path, base: &Path) -> bool {
    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    let (path, base) = (PathBuf::from(lower(path)), PathBuf::from(lower(base)));
    path.starts_with(base)
}

/// Refuse a target (the output folder or the game folder in it) that is the
/// project folder, holds it, or is / lies inside its `assets`, `config`,
/// `crates` or `.git`: packaging would write into, or delete, project files.
fn check_target(root: &Path, target: &Path) -> Result<()> {
    let (root, target) = (resolve(root), resolve(target));
    if inside(&root, &target) {
        bail!(
            "{} is the project folder or holds it; pick another output folder",
            target.display()
        );
    }
    for protected in ["assets", "config", "crates", ".git"] {
        if inside(&target, &root.join(protected)) {
            bail!(
                "{} is inside the project's {protected} folder; pick another output folder",
                target.display()
            );
        }
    }
    Ok(())
}

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
    check_target(root, out)?;
    check_target(root, &dir)?;
    if dir.exists() {
        if !dir.join(PACK_FILE).is_file() || !dir.join(MANIFEST_FILE).is_file() {
            bail!(
                "{} exists but was not written by a build; remove it or pick another output folder",
                dir.display()
            );
        }
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
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            "melon build 1
game: {}
version: {}
",
            settings.game_name, settings.version
        ),
    )?;
    Ok(PackageReport { dir, exe, files })
}

/// Pure part of `find_cargo`: `env_cargo` is `$CARGO`, `home` the user's home
/// folder, `on_path` whether `cargo` runs from PATH.
fn find_cargo_with(
    env_cargo: Option<PathBuf>,
    home: Option<PathBuf>,
    on_path: bool,
) -> Result<PathBuf> {
    if let Some(path) = env_cargo.filter(|p| p.is_file()) {
        return Ok(path);
    }
    if on_path {
        return Ok(PathBuf::from("cargo"));
    }
    let installed = home.map(|h| {
        h.join(".cargo")
            .join("bin")
            .join(format!("cargo{}", std::env::consts::EXE_SUFFIX))
    });
    if let Some(path) = installed.filter(|p| p.is_file()) {
        return Ok(path);
    }
    bail!(
        "cargo was not found: install Rust (https://rustup.rs) or set the CARGO environment variable to cargo's path"
    )
}

/// `$CARGO`, else `cargo` on PATH, else `~/.cargo/bin/cargo`.
pub fn find_cargo() -> Result<PathBuf> {
    let on_path = Command::new("cargo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    find_cargo_with(std::env::var_os("CARGO").map(PathBuf::from), home, on_path)
}

/// Run `cmd`, sending its stderr to `log` line by line. Setting `cancel`
/// kills the child and returns an error.
pub fn run_logged(mut cmd: Command, log: &mut dyn FnMut(&str), cancel: &AtomicBool) -> Result<()> {
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start the command")?;
    let stderr = child.stderr.take().context("no stderr")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => log(&line),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if cancel.load(Ordering::Relaxed) {
            // cargo starts rustc and linkers: end the whole tree on Windows.
            #[cfg(windows)]
            let _ = Command::new("taskkill")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            let _ = child.wait();
            bail!("cancelled");
        }
    }
    if !child
        .wait()
        .context("the command did not finish")?
        .success()
    {
        bail!("the command failed");
    }
    Ok(())
}

/// Build the player (release, no editor, static C runtime) in
/// `<workspace>/target/package` and return its path. `log` gets cargo's
/// output line by line; `cancel` stops the build. The separate target folder
/// keeps the flags change from rebuilding the normal release build.
pub fn build_player(
    workspace: &Path,
    log: &mut dyn FnMut(&str),
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let target = workspace.join("target").join("package");
    let mut cmd = Command::new(find_cargo()?);
    cmd.args([
        "build",
        "-p",
        "melon_host",
        "--release",
        "--no-default-features",
    ])
    .arg("--target-dir")
    .arg(&target)
    .current_dir(workspace)
    .env("RUSTFLAGS", "-C target-feature=+crt-static");
    run_logged(cmd, log, cancel).context("cargo build of the player")?;
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
        assert_eq!(names, ["Space Melon.exe", "build.manifest", "game.pak"]);

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

    fn project_with_player(name: &str) -> (PathBuf, PathBuf) {
        let root = temp(name);
        for d in ["assets", "config", "crates", ".git"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("keep.txt"), "precious").unwrap();
        }
        let exe = root.join("p.exe");
        std::fs::write(&exe, b"x").unwrap();
        (root, exe)
    }

    fn game(name: &str) -> PlayerSettings {
        PlayerSettings {
            game_name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn outputs_that_would_touch_project_folders_are_refused() {
        let (root, exe) = project_with_player("protect");
        let parent = root.parent().unwrap().to_path_buf();
        let root_name = root.file_name().unwrap().to_string_lossy().into_owned();
        let cases: Vec<(PathBuf, String)> = vec![
            (root.clone(), "assets".into()),
            (root.clone(), "config".into()),
            (root.clone(), "crates".into()),
            (root.clone(), ".git".into()),
            (root.join("assets"), "Game".into()),
            (root.join("assets/sub"), "Game".into()),
            (root.join(".git"), "Game".into()),
            (root.join("crates"), "Game".into()),
            (root.join("config"), "Game".into()),
            (root.join("out/../assets"), "Game".into()),
            (root.join("assets/.."), "assets".into()),
            (parent, root_name),
        ];
        for (out, name) in cases {
            let result = package_game(&root, &exe, &out, &game(&name));
            assert!(result.is_err(), "{} + {name} was accepted", out.display());
            for d in ["assets", "config", "crates", ".git"] {
                assert!(root.join(d).join("keep.txt").is_file(), "{d} lost");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_folder_a_previous_build_wrote_is_replaced() {
        let (root, exe) = project_with_player("marker");
        let out = root.join("Builds");
        std::fs::create_dir_all(out.join("Game")).unwrap();
        std::fs::write(out.join("Game/keep.txt"), "mine").unwrap();
        assert!(package_game(&root, &exe, &out, &game("Game")).is_err());
        assert!(out.join("Game/keep.txt").is_file());

        std::fs::remove_dir_all(out.join("Game")).unwrap();
        package_game(&root, &exe, &out, &game("Game")).unwrap();
        // The second build replaces the first one (it carries the marker).
        std::fs::write(out.join("Game/stale.txt"), "x").unwrap();
        package_game(&root, &exe, &out, &game("Game")).unwrap();
        assert!(!out.join("Game/stale.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cargo_is_found_from_env_then_path_then_the_home_folder() {
        let dir = temp("cargo");
        let named = dir.join("my-cargo");
        std::fs::write(&named, "").unwrap();
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".cargo/bin")).unwrap();
        let installed = home
            .join(".cargo/bin")
            .join(format!("cargo{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&installed, "").unwrap();

        assert_eq!(
            find_cargo_with(Some(named.clone()), Some(home.clone()), true).unwrap(),
            named
        );
        assert_eq!(
            find_cargo_with(None, Some(home.clone()), true).unwrap(),
            PathBuf::from("cargo")
        );
        assert_eq!(
            find_cargo_with(None, Some(home.clone()), false).unwrap(),
            installed
        );
        let message = find_cargo_with(Some(dir.join("gone")), Some(dir.join("nohome")), false)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("CARGO") && message.contains("Rust"),
            "{message}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn cancelling_kills_the_child_and_returns_quickly() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            flag.store(true, Ordering::Relaxed);
        });
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"]);
        let started = std::time::Instant::now();
        let error = run_logged(cmd, &mut |_| {}, &cancel).unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }

    #[cfg(windows)]
    #[test]
    fn child_output_goes_to_the_log_and_failure_is_an_error() {
        use std::sync::atomic::AtomicBool;
        let mut cmd = Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-Command",
            "[Console]::Error.WriteLine('hello'); exit 3",
        ]);
        let mut lines = Vec::new();
        let result = run_logged(
            cmd,
            &mut |l| lines.push(l.to_string()),
            &AtomicBool::new(false),
        );
        assert!(result.is_err());
        assert_eq!(lines, ["hello"]);
    }
}
