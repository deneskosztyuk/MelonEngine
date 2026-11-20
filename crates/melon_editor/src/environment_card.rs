//! Inspector card for a `.env` file: one collapsing section per field of
//! `EnvironmentAsset` (drawn by reflection, so a new section or field shows
//! up by itself). Edits wait in egui memory while the pointer is down and
//! are saved when it is up; while a copy is pending it is also published as
//! `EnvironmentPreview`, so the Scene view shows the edit live. The editor
//! clears the preview at the start of every UI frame, so it lives only while
//! this card keeps drawing a pending copy. Asset edits are not undoable.

use std::path::Path;

use bevy_ecs::reflect::AppTypeRegistry;
use bevy_ecs::world::World;
use melon_core::bevy_reflect::{PartialReflect, ReflectRef, TypeInfo, Typed};
use melon_core::glam::Vec3;
use melon_core::{AssetRef, EnvironmentAsset, EnvironmentPreview};

use crate::inspect::{FieldWidget, FieldWidgets, WidgetKind, reflect_ui_with};
use crate::material_card::should_write;

/// Texture files a sky texture can use.
const TEXTURE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "hdr"];

/// `sky_texture` -> `Sky texture`.
fn title(name: &str) -> String {
    let mut chars = name.replace('_', " ").chars().collect::<Vec<_>>();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

/// Colour pickers for `Vec3` fields, asset pickers for `AssetRef` fields.
fn section_widgets(section: &dyn PartialReflect, root: &Path) -> FieldWidgets {
    let mut widgets = FieldWidgets::new();
    let Some(TypeInfo::Struct(info)) = section.get_represented_type_info() else {
        return widgets;
    };
    let ReflectRef::Struct(fields) = section.reflect_ref() else {
        return widgets;
    };
    for (i, name) in info.field_names().iter().enumerate() {
        let Some(field) = fields.field_at(i) else {
            continue;
        };
        let kind = if field.try_downcast_ref::<Vec3>().is_some() {
            WidgetKind::Color
        } else if field.try_downcast_ref::<AssetRef>().is_some() {
            WidgetKind::AssetRef {
                root: root.to_path_buf(),
                extensions: &TEXTURE_EXTENSIONS,
            }
        } else {
            continue;
        };
        widgets.insert(name, FieldWidget { label: None, kind });
    }
    widgets
}

/// The card of the `.env` file `path` (project-relative), saving when an
/// edit ends. A missing or broken file shows why.
pub fn show(ui: &mut egui::Ui, world: &mut World, project_root: &Path, path: &str) {
    let memory = egui::Id::new(("environment_edit", path));
    let pending: Option<EnvironmentAsset> = ui.data(|d| d.get_temp(memory));
    let file = project_root.join(path);
    let mut asset = match pending.clone() {
        Some(asset) => asset,
        None => match EnvironmentAsset::read(&file) {
            Ok(asset) => asset,
            Err(error) => {
                ui.colored_label(crate::theme::colors::ERROR, format!("{error:#}"));
                return;
            }
        },
    };
    let mut changed = false;
    if let Some(registry) = world.get_resource::<AppTypeRegistry>().cloned() {
        let registry = registry.read();
        let names: &[&str] = match EnvironmentAsset::type_info() {
            TypeInfo::Struct(info) => info.field_names(),
            _ => &[],
        };
        for (i, name) in names.iter().copied().enumerate() {
            let melon_core::bevy_reflect::ReflectMut::Struct(fields) = asset.reflect_mut() else {
                break;
            };
            let Some(section) = fields.field_at_mut(i) else {
                continue;
            };
            let widgets = section_widgets(&*section, project_root);
            let header = egui::CollapsingHeader::new(title(name))
                .id_salt(("environment_section", path, name))
                .default_open(matches!(name, "sun" | "sky"))
                .show(ui, |ui| {
                    let id = egui::Id::new(("environment_fields", path, name));
                    changed |= reflect_ui_with(ui, id, section, &registry, &widgets);
                });
            // Fixed id over the header (widget ids are positional).
            ui.interact(
                header.header_response.rect,
                egui::Id::new(("environment_section", name)),
                egui::Sense::hover(),
            );
        }
    }
    let pointer_down = ui.input(|i| i.pointer.any_down());
    let dirty = changed || pending.is_some();
    if dirty {
        world.insert_resource(EnvironmentPreview(Some((path.to_string(), asset.clone()))));
    }
    if should_write(dirty, pointer_down) {
        ui.data_mut(|d| d.remove::<EnvironmentAsset>(memory));
        if let Err(error) = asset.save(&file) {
            tracing::error!("could not save {}: {error:#}", file.display());
        }
    } else if changed {
        ui.data_mut(|d| d.insert_temp(memory, asset));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("melon_envcard_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).unwrap();
        root
    }

    fn frame(ctx: &egui::Context, world: &mut World, root: &Path, events: Vec<egui::Event>) {
        let _ = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| show(ui, world, root, "assets/a.env"),
        );
    }

    fn pending_id() -> egui::Id {
        egui::Id::new(("environment_edit", "assets/a.env"))
    }

    #[test]
    fn the_file_is_written_when_the_pointer_is_up_not_while_dragging() {
        assert!(!should_write(true, true), "mid-drag");
        assert!(should_write(true, false), "drag ended");
        assert!(!should_write(false, false), "nothing edited");
    }

    #[test]
    fn pending_edit_drives_the_preview() {
        let root = project("preview");
        EnvironmentAsset::default()
            .save(&root.join("assets/a.env"))
            .unwrap();
        let mut world = melon_core::Core::new().world;
        let ctx = egui::Context::default();
        let mut copy = EnvironmentAsset::default();
        copy.fog.density = 0.5;
        ctx.data_mut(|d| d.insert_temp(pending_id(), copy.clone()));
        frame(&ctx, &mut world, &root, vec![]);
        let preview = world.resource::<EnvironmentPreview>();
        assert_eq!(preview.0, Some(("assets/a.env".to_string(), copy)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_is_dropped_when_the_card_is_not_drawn() {
        let root = project("dropped");
        EnvironmentAsset::default()
            .save(&root.join("assets/a.env"))
            .unwrap();
        let mut world = melon_core::Core::new().world;
        let ctx = egui::Context::default();
        let mut copy = EnvironmentAsset::default();
        copy.fog.density = 0.5;
        ctx.data_mut(|d| d.insert_temp(pending_id(), copy));
        frame(&ctx, &mut world, &root, vec![]);
        assert!(world.resource::<EnvironmentPreview>().0.is_some());
        // The next editor frame draws no card.
        let mut editor = crate::Editor::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            editor.build_ui(&mut world, ui)
        });
        assert!(world.resource::<EnvironmentPreview>().0.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_section_gets_a_header() {
        let root = project("headers");
        EnvironmentAsset::default()
            .save(&root.join("assets/a.env"))
            .unwrap();
        let mut world = melon_core::Core::new().world;
        let ctx = egui::Context::default();
        frame(&ctx, &mut world, &root, vec![]);
        frame(&ctx, &mut world, &root, vec![]);
        for name in ["sun", "ambient", "sky", "fog", "ao", "gi", "bloom"] {
            assert!(
                ctx.read_response(egui::Id::new(("environment_section", name)))
                    .is_some(),
                "{name}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_file_shows_the_error_and_no_fields() {
        let root = project("broken");
        std::fs::write(root.join("assets/a.env"), "(fog: ").unwrap();
        let mut world = melon_core::Core::new().world;
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &root, "assets/a.env")
        });
        let shown: String = out
            .shapes
            .iter()
            .map(|s| format!("{:?}", s.shape))
            .collect();
        assert!(!shown.is_empty(), "the error is drawn");
        assert!(
            ctx.read_response(egui::Id::new(("environment_section", "sun")))
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
