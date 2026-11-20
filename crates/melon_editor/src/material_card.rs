//! Inspector card for a selected `.mat` file: edits the factors, alpha mode
//! and texture paths and saves the file on every change (the renderer
//! hot-reloads it). Asset edits are not undoable.

use std::path::Path;

use melon_core::{AlphaMode, AssetServer, MaterialAsset, TextureRef};

/// A texture slot as one path text box; empty clears it.
fn texture_field(
    ui: &mut egui::Ui,
    label: &str,
    slot: &mut Option<TextureRef>,
    server: Option<&AssetServer>,
) -> bool {
    ui.label(label);
    let mut text = slot.as_ref().map(|t| t.path.clone()).unwrap_or_default();
    let changed = ui
        .add(egui::TextEdit::singleline(&mut text).hint_text("assets/textures/..."))
        .changed();
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
    ui.horizontal(|ui| {
        for c in &mut asset.base_color {
            changed |= drag(ui, c, 0.0..=1.0);
        }
    });
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
    ui.horizontal(|ui| {
        for c in &mut asset.emissive {
            changed |= drag(ui, c, 0.0..=1.0);
        }
    });
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

/// Read `path`, show the card, save on change. A broken file shows why.
pub fn show(ui: &mut egui::Ui, path: &Path, server: Option<&AssetServer>) {
    let mut asset = match MaterialAsset::read(path) {
        Ok(asset) => asset,
        Err(error) => {
            ui.colored_label(crate::theme::colors::ERROR, format!("{error:#}"));
            return;
        }
    };
    let mut changed = false;
    egui::Grid::new(("material_rows", path))
        .num_columns(2)
        .spacing([8.0, crate::theme::sizes::SPACING])
        .show(ui, |ui| changed = rows(ui, &mut asset, server));
    if changed && let Err(error) = asset.save(path) {
        tracing::error!("could not save {}: {error:#}", path.display());
    }
}
