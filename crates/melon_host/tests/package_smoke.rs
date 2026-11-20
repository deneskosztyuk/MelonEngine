//! End to end: build the player, package a small project, run the packaged
//! game for 30 frames and check it loads its startup scene and exits 0.
//! Needs a GPU and a full release build (minutes the first time), so it is
//! ignored by default: `cargo test -p melon_host --test package_smoke -- --ignored`.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use melon_core::PlayerSettings;
use melon_core::package::{build_player, package_game};

const SCENE: &str = r#"(
    version: 6,
    entities: [
        (
            name: "Ground",
            model: Some("builtin:cube"),
            components: {
                "melon_core::components::Transform": (
                    translation: (x: 0.0, y: -0.5, z: 0.0),
                    rotation: (x: 0.0, y: 0.0, z: 0.0, w: 1.0),
                    scale: (x: 4.0, y: 1.0, z: 4.0),
                ),
            },
        ),
    ],
    name_counter: 1,
)"#;

#[test]
#[ignore = "needs a GPU and a full release build; run with --ignored"]
fn packaged_game_starts_loads_its_scene_and_exits_cleanly() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exe = build_player(
        &workspace,
        &mut |line| eprintln!("{line}"),
        &AtomicBool::new(false),
    )
    .expect("player builds");

    let root = std::env::temp_dir().join(format!("melon_smoke_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("assets/scenes")).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("assets/scenes/main.ron"), SCENE).unwrap();
    let settings = PlayerSettings {
        game_name: "Smoke Game".into(),
        startup_scene: "assets/scenes/main.ron".into(),
        ..Default::default()
    };
    settings
        .save(&root.join("config/player.ron"), false)
        .unwrap();

    let report = package_game(&root, &exe, &root.join("out"), &settings).unwrap();
    assert!(report.dir.join("game.pak").is_file());

    // Run it from another folder, so nothing leans on the working directory.
    let mut child = Command::new(&report.exe)
        .args(["--frames", "30"])
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the packaged game starts");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(120) {
            child.kill().ok();
            panic!("the packaged game did not exit within 120 s");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut log = String::new();
    child.stdout.take().unwrap().read_to_string(&mut log).ok();
    assert!(status.success(), "exit {status}; log:\n{log}");
    assert!(
        log.contains("loaded scene assets/scenes/main.ron"),
        "log:\n{log}"
    );
    assert!(log.contains("rendered 30 frames"), "log:\n{log}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Package a project whose `config/player.ron` is `player_ron` and run it.
fn run_packaged(name: &str, player_ron: &str) -> (std::process::ExitStatus, String) {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exe =
        build_player(&workspace, &mut |_| {}, &AtomicBool::new(false)).expect("player builds");
    let root = std::env::temp_dir().join(format!("melon_smoke_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/player.ron"), player_ron).unwrap();
    let settings = PlayerSettings {
        game_name: name.into(),
        ..Default::default()
    };
    let report = package_game(&root, &exe, &root.join("out"), &settings).unwrap();
    let output = Command::new(&report.exe)
        .args(["--frames", "5"])
        .current_dir(std::env::temp_dir())
        .output()
        .expect("the packaged game starts");
    let _ = std::fs::remove_dir_all(&root);
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
#[ignore = "needs a GPU and a full release build; run with --ignored"]
fn missing_startup_scene_logs_an_error_and_runs_an_empty_world() {
    let (status, log) = run_packaged("NoScene", r#"(startup_scene: "assets/scenes/nope.ron")"#);
    assert!(
        status.success(),
        "exit {status}; log:
{log}"
    );
    assert!(
        log.contains("scene request assets/scenes/nope.ron failed"),
        "log:
{log}"
    );
    assert!(
        log.contains("rendered 5 frames"),
        "log:
{log}"
    );
}

#[test]
#[ignore = "needs a GPU and a full release build; run with --ignored"]
fn invalid_window_size_falls_back_to_defaults_and_still_starts() {
    let (status, log) = run_packaged("BadWindow", "(window_size: (0, 0))");
    assert!(
        status.success(),
        "exit {status}; log:
{log}"
    );
    assert!(
        log.contains("player.ron"),
        "the broken settings are reported; log:
{log}"
    );
    assert!(
        log.contains("rendered 5 frames"),
        "log:
{log}"
    );
}
