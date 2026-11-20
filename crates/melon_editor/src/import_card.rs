//! The Inspector's "Import Settings" card of a selected model file: edits
//! wait in egui memory until Apply writes them into the model's `.meta` (the
//! model then reloads), Revert drops them.

use std::path::Path;

use melon_core::ImportSettings;

/// Where the card keeps the edits of `path` until Apply or Revert.
pub(crate) fn memory_id(path: &Path) -> egui::Id {
    egui::Id::new(("import_edit", path))
}

/// Id of the Apply button's response (for tests).
pub(crate) fn apply_id(path: &Path) -> egui::Id {
    egui::Id::new(("import_apply", path))
}

/// Id of the Revert button's response (for tests).
pub(crate) fn revert_id(path: &Path) -> egui::Id {
    egui::Id::new(("import_revert", path))
}

/// Files the settings apply to (FBX; glTF does not read them yet).
pub(crate) fn has_settings(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("fbx"))
}

/// Draw the card of the model file `path`.
pub(crate) fn show(ui: &mut egui::Ui, fills: &mut [egui::Color32; 2], path: &Path) {
    let memory = memory_id(path);
    let saved = ImportSettings::read(path);
    let mut edited: ImportSettings = ui.data(|d| d.get_temp(memory)).unwrap_or(saved);
    let (mut apply, mut revert) = (false, false);
    crate::inspector_card(ui, fills, "Import Settings", |ui| {
        ui.label("Scale");
        ui.add(
            egui::DragValue::new(&mut edited.scale)
                .speed(0.01)
                .range(0.0001..=100_000.0)
                .max_decimals(4),
        )
        .on_hover_text("Multiplies the model after the unit conversion");
        ui.end_row();
        ui.label("Use file units");
        ui.checkbox(&mut edited.use_file_units, "")
            .on_hover_text("Off: the file's numbers are metres");
        ui.end_row();
        ui.label("Forward flip");
        ui.checkbox(&mut edited.forward_flip, "")
            .on_hover_text("Turn the model 180 degrees about Y");
        ui.end_row();
        ui.label("Normal Y flip");
        ui.checkbox(&mut edited.normal_y_flip, "")
            .on_hover_text("Invert the green channel of normal maps (DirectX)");
        ui.end_row();
        let dirty = edited != saved;
        ui.label("");
        ui.horizontal(|ui| {
            let button = ui.add_enabled(dirty, egui::Button::new("Apply"));
            ui.interact(button.rect, apply_id(path), egui::Sense::hover());
            apply = button.clicked();
            let button = ui.add_enabled(dirty, egui::Button::new("Revert"));
            ui.interact(button.rect, revert_id(path), egui::Sense::hover());
            revert = button.clicked();
        });
        ui.end_row();
    });
    let mut applied = false;
    if apply {
        match edited.write(path) {
            Ok(()) => {
                applied = true;
                tracing::info!("import settings of {} applied", path.display());
            }
            Err(error) => tracing::error!(
                "could not save the import settings of {}: {error:#}",
                path.display()
            ),
        }
    }
    if applied || revert || edited == saved {
        ui.data_mut(|d| d.remove::<ImportSettings>(memory));
    } else {
        ui.data_mut(|d| d.insert_temp(memory, edited));
    }
}
