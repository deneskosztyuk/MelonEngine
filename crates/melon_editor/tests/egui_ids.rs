//! egui's debug check (`warn_if_rect_changes_id`) logs a warning through
//! `log` whenever a widget keeps its rect but changes its id between frames.
//! In the dev build each warning also reaches the Console and the terminal,
//! so a widget whose id churns every frame costs frame rate. This file is
//! its own test binary: the global `log` logger it installs sees only it.

use std::sync::Mutex;

use melon_core::glam::{Quat, Vec3};
use melon_core::{AssetServer, Bob, Core, Handle, Model, Name, SimulationState, Spin, Transform};

static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        let text = record.args().to_string();
        if text.contains("changed id between passes") {
            WARNINGS.lock().unwrap().push(text);
        }
    }
    fn flush(&self) {}
}

static CAPTURE: Capture = Capture;

fn frame(ctx: &egui::Context, time: f64, ui: impl FnMut(&mut egui::Ui)) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 900.0),
        )),
        time: Some(time),
        ..Default::default()
    };
    let _ = ctx.run_ui(input, ui);
}

#[test]
fn moving_entities_keep_widget_ids_stable() {
    log::set_logger(&CAPTURE).expect("only test in this binary");
    log::set_max_level(log::LevelFilter::Warn);

    // The capture works: a rect whose id changes every frame is reported.
    let ctx = egui::Context::default();
    for n in 0..3 {
        frame(&ctx, f64::from(n), |ui| {
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(50.0, 20.0));
            ui.interact(rect, egui::Id::new(("churn", n)), egui::Sense::click());
        });
    }
    assert!(
        !WARNINGS.lock().unwrap().is_empty(),
        "capture sees egui's warnings"
    );
    WARNINGS.lock().unwrap().clear();

    // The editor while playing, a spinning entity and a selected bobbing one.
    let root = std::env::temp_dir().join(format!("melon_egui_ids_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("assets/models")).unwrap();
    let mut core = Core::new();
    core.world.insert_resource(AssetServer::new(root.clone()));
    core.world.insert_resource(SimulationState::Playing);
    let cube: Handle<Model> = core
        .world
        .resource_mut::<AssetServer>()
        .load("builtin:cube");
    let spin = core
        .world
        .spawn((Name("Spinner".into()), Transform::default(), Spin))
        .id();
    let bob = core
        .world
        .spawn((
            Name("Bobber".into()),
            Transform::default(),
            Bob,
            cube,
            melon_core::Aabb {
                half_extents: Vec3::splat(0.5),
            },
        ))
        .id();
    let mut editor = melon_editor::Editor::new();
    editor.set_project_root(&root);
    editor.set_selected(Some(bob));

    let ctx = egui::Context::default();
    for n in 0..10 {
        let t = n as f32 / 60.0;
        core.world.get_mut::<Transform>(spin).unwrap().rotation = Quat::from_rotation_y(t * 3.0);
        core.world.get_mut::<Transform>(bob).unwrap().translation.y = (t * 4.0).sin();
        if n == 4 {
            WARNINGS.lock().unwrap().clear();
        }
        frame(&ctx, f64::from(t), |ui| {
            editor.build_ui(&mut core.world, ui)
        });
    }
    let _ = std::fs::remove_dir_all(&root);
    let found = WARNINGS.lock().unwrap().clone();
    assert!(
        found.is_empty(),
        "{} id warnings, first: {:?}",
        found.len(),
        found.first()
    );
}
