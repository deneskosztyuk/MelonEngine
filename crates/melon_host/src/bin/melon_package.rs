//! `just package <out>`: build the player and write `<out>/<GameName>/` with
//! the executable and `game.pak` (the same steps as the editor's File > Build).
//! `--no-build` packages the player built last time.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use melon_core::package::{build_player, package_game};
use melon_core::{LoadOutcome, PlayerSettings};

/// The project: the source tree this binary was built from.
const PROJECT_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn main() -> ExitCode {
    let mut out = None;
    let mut build = true;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--no-build" => build = false,
            flag if flag.starts_with("--") => {
                eprintln!("unknown flag {flag}");
                return ExitCode::from(2);
            }
            _ => out = Some(PathBuf::from(arg)),
        }
    }
    let Some(out) = out else {
        eprintln!("usage: melon_package <output folder> [--no-build]");
        return ExitCode::from(2);
    };
    match run(&out, build) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("package failed: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(out: &Path, build: bool) -> anyhow::Result<()> {
    let root = std::path::absolute(Path::new(PROJECT_ROOT))?;
    let (settings, outcome) = PlayerSettings::load(&root.join("config/player.ron"));
    if let LoadOutcome::Broken(message) = outcome {
        anyhow::bail!("config/player.ron is broken ({message}): fix it or delete it");
    }
    let exe = if build {
        build_player(
            &root,
            &mut |line| eprintln!("{line}"),
            &std::sync::atomic::AtomicBool::new(false),
        )?
    } else {
        root.join("target/package/release")
            .join(format!("melon_host{}", std::env::consts::EXE_SUFFIX))
    };
    std::fs::create_dir_all(out)?;
    let report = package_game(&root, &exe, &std::path::absolute(out)?, &settings)?;
    println!(
        "packaged {} ({} files in game.pak)\nrun: {}",
        report.dir.display(),
        report.files,
        report.exe.display()
    );
    Ok(())
}
