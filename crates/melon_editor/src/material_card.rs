//! Inspector card for a selected `.mat` file: edits the factors, alpha mode
//! and textures (pickers, drops from the Content Browser) and colours
//! (colour pickers). The file is saved when a drag or picker ends, not on
//! every tick (the renderer hot-reloads it). Asset edits are not undoable.

use std::path::Path;

use melon_core::{AlphaMode, AssetServer, MaterialAsset, TextureRef};

/// Texture files a material can use.
const TEXTURE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];

/// A texture slot: a picker over the project's textures that also takes a
/// texture dragged from the Content Browser; None clears it. Without a
/// server (no project folder) a path text box.
fn texture_field(
    ui: &mut egui::Ui,
    label: &str,
    slot: &mut Option<TextureRef>,
    server: Option<&AssetServer>,
) -> bool {
    ui.label(label);
    let mut text = slot.as_ref().map(|t| t.path.clone()).unwrap_or_default();
    let changed = match server {
        Some(server) => crate::inspect::asset_field(
            ui,
            egui::Id::new(("material_texture", label)),
            &mut text,
            server.root(),
            &TEXTURE_EXTENSIONS,
        ),
        None => ui
            .add(egui::TextEdit::singleline(&mut text).hint_text("assets/textures/..."))
            .changed(),
    };
    if changed {
        *slot = (!text.is_empty()).then(|| match server {
            Some(server) => TextureRef::for_path(server, &text),
            None => TextureRef {
                guid: None,
                path: text,
            },
        });
    }
    ui.end_row();
    changed
}

/// Edits wait in memory while a pointer button is down (a drag, an open
/// colour picker) and the file is written once it is up.
pub(crate) fn should_write(dirty: bool, pointer_down: bool) -> bool {
    dirty && !pointer_down
}

fn drag(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>) -> bool {
    ui.add(
        egui::DragValue::new(value)
            .speed(0.01)
            .range(range)
            .max_decimals(3),
    )
    .changed()
}

/// The card's rows for `asset`; true when something changed.
pub fn rows(ui: &mut egui::Ui, asset: &mut MaterialAsset, server: Option<&AssetServer>) -> bool {
    let mut changed = false;
    ui.label("Base colour");
    changed |= ui
        .color_edit_button_rgba_unmultiplied(&mut asset.base_color)
        .changed();
    ui.end_row();
    changed |= texture_field(ui, "Base texture", &mut asset.base_color_texture, server);
    ui.label("Metallic");
    changed |= drag(ui, &mut asset.metallic, 0.0..=1.0);
    ui.end_row();
    ui.label("Roughness");
    changed |= drag(ui, &mut asset.roughness, 0.0..=1.0);
    ui.end_row();
    changed |= texture_field(
        ui,
        "Metal / rough tex",
        &mut asset.metallic_roughness_texture,
        server,
    );
    changed |= texture_field(ui, "Normal texture", &mut asset.normal_texture, server);
    ui.label("Normal scale");
    changed |= drag(ui, &mut asset.normal_scale, 0.0..=4.0);
    ui.end_row();
    ui.label("Emissive");
    changed |= ui.color_edit_button_rgb(&mut asset.emissive).changed();
    ui.end_row();
    changed |= texture_field(ui, "Emissive texture", &mut asset.emissive_texture, server);
    ui.label("Emissive strength");
    changed |= drag(ui, &mut asset.emissive_strength, 0.0..=100.0);
    ui.end_row();
    ui.label("Alpha mode");
    ui.horizontal(|ui| {
        let name = |m: &AlphaMode| match m {
            AlphaMode::Opaque => "Opaque",
            AlphaMode::Mask { .. } => "Mask",
            AlphaMode::Blend => "Blend",
        };
        egui::ComboBox::from_id_salt("material_alpha_mode")
            .selected_text(name(&asset.alpha_mode))
            .show_ui(ui, |ui| {
                for mode in [
                    AlphaMode::Opaque,
                    AlphaMode::Mask { cutoff: 0.5 },
                    AlphaMode::Blend,
                ] {
                    let selected = name(&mode) == name(&asset.alpha_mode);
                    if ui.selectable_label(selected, name(&mode)).clicked() && !selected {
                        asset.alpha_mode = mode;
                        changed = true;
                    }
                }
            });
        if let AlphaMode::Mask { cutoff } = &mut asset.alpha_mode {
            changed |= drag(ui, cutoff, 0.0..=1.0);
        }
    });
    ui.end_row();
    ui.label("Double sided");
    changed |= ui.checkbox(&mut asset.double_sided, "").changed();
    ui.end_row();
    changed
}

/// Show the card of `path`, saving when an edit ends. The edited copy lives
/// in egui memory meanwhile, so the file is not read back mid-drag. A broken
/// file shows why.
pub fn show(ui: &mut egui::Ui, path: &Path, server: Option<&AssetServer>) {
    let memory = egui::Id::new(("material_edit", path));
    let pending: Option<MaterialAsset> = ui.data(|d| d.get_temp(memory));
    let mut asset = match pending.clone() {
        Some(asset) => asset,
        None => match MaterialAsset::read(path) {
            Ok(asset) => asset,
            Err(error) => {
                ui.colored_label(crate::theme::colors::ERROR, format!("{error:#}"));
                return;
            }
        },
    };
    let mut changed = false;
    egui::Grid::new(("material_rows", path))
        .num_columns(2)
        .spacing([8.0, crate::theme::sizes::SPACING])
        .show(ui, |ui| changed = rows(ui, &mut asset, server));
    let pointer_down = ui.input(|i| i.pointer.any_down());
    if should_write(changed || pending.is_some(), pointer_down) {
        ui.data_mut(|d| d.remove::<MaterialAsset>(memory));
        if let Err(error) = asset.save(path) {
            tracing::error!("could not save {}: {error:#}", path.display());
        }
    } else if changed {
        ui.data_mut(|d| d.insert_temp(memory, asset));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_written_when_the_pointer_is_up_not_while_dragging() {
        assert!(!should_write(true, true), "mid-drag");
        assert!(should_write(true, false), "drag ended");
        assert!(!should_write(false, false), "nothing edited");
    }

    #[test]
    fn a_drag_is_saved_once_when_it_ends() {
        let dir = std::env::temp_dir().join(format!("melon_matcard_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.mat");
        MaterialAsset::default().save(&path).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut frame = |events: Vec<egui::Event>| {
            time += 0.05;
            let _ = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &path, None),
            );
        };
        frame(vec![]);
        let memory = egui::Id::new(("material_edit", path.as_path()));
        let pending = |ctx: &egui::Context| ctx.data(|d| d.get_temp::<MaterialAsset>(memory));
        // Press and drag at spots down the card until a drag widget edits.
        let mut release = None;
        'scan: for y in (20..400).step_by(6) {
            for x in [150.0f32, 200.0, 250.0] {
                let pos = egui::pos2(x, y as f32);
                let button = |pressed, pos| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                let moved = pos + egui::vec2(8.0, 0.0);
                frame(vec![egui::Event::PointerMoved(pos)]);
                frame(vec![button(true, pos)]);
                frame(vec![egui::Event::PointerMoved(moved)]);
                if pending(&ctx).is_some() {
                    release = Some(button(false, moved));
                    break 'scan;
                }
                frame(vec![button(false, moved)]);
                frame(vec![]);
            }
        }
        let release = release.expect("some drag widget edits the material");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "nothing is written during the drag"
        );
        frame(vec![release]);
        frame(vec![]);
        assert_ne!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "written when the drag ends"
        );
        assert!(pending(&ctx).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
