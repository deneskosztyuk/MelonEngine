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
use melon_core::assets::EnvironmentFile;
use melon_core::bevy_reflect::{PartialReflect, ReflectRef, TypeInfo, Typed};
use melon_core::glam::Vec3;
use melon_core::{AssetRef, AssetServer, EditHint, EnvironmentAsset, EnvironmentPreview};

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

/// An unsaved edit and the frame it was last drawn on.
#[derive(Clone)]
struct Pending {
    frame: u64,
    asset: EnvironmentAsset,
}

/// The parsed file (or the one-line reason it has none) for one
/// `AssetServer` version.
#[derive(Clone)]
struct Cached {
    version: u32,
    result: Result<EnvironmentAsset, String>,
}

/// Editors per field: HDR colour pickers for `Vec3`, asset pickers for
/// `AssetRef`, drag values for numbers with an `EditHint`; every label in
/// sentence case.
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
        let hint = info
            .field_at(i)
            .and_then(|f| f.custom_attributes().get::<EditHint>());
        let kind = if field.try_downcast_ref::<Vec3>().is_some() {
            WidgetKind::HdrColor
        } else if field.try_downcast_ref::<AssetRef>().is_some() {
            WidgetKind::AssetRef {
                root: root.to_path_buf(),
                extensions: &TEXTURE_EXTENSIONS,
            }
        } else if let Some(h) = hint {
            WidgetKind::Drag {
                min: h.min,
                max: h.max,
                speed: h.speed,
                suffix: h.suffix,
                decimals: h.decimals,
            }
        } else {
            continue;
        };
        let label = Some(title(name).into());
        widgets.insert(name, FieldWidget { label, kind });
    }
    widgets
}

/// The card of the `.env` file `path` (project-relative), saving when an
/// edit ends. A missing or broken file shows why and no fields.
pub fn show(ui: &mut egui::Ui, world: &mut World, project_root: &Path, path: &str) {
    // The server's form of the path (forward slashes, no `./`) and the
    // version of the file, which says when to parse it again.
    let (path, version) = match world.get_resource_mut::<AssetServer>() {
        Some(mut server) => {
            let handle = server.load::<EnvironmentFile>(path);
            (server.path(handle).to_string(), server.version(handle))
        }
        None => (path.to_string(), 0),
    };
    let path = path.as_str();
    let frame = ui.ctx().cumulative_frame_nr();
    let memory = egui::Id::new(("environment_edit", path));
    // A copy left from a frame this card was not drawn on is stale.
    let pending: Option<EnvironmentAsset> = ui
        .data(|d| d.get_temp::<Pending>(memory))
        .filter(|p| p.frame + 1 >= frame)
        .map(|p| p.asset);
    let file = project_root.join(path);
    let file_memory = egui::Id::new(("environment_file", path));
    let cached = ui
        .data(|d| d.get_temp::<Cached>(file_memory))
        .filter(|c| c.version == version)
        .unwrap_or_else(|| {
            let result = if file.is_file() {
                EnvironmentAsset::read(&file).map_err(|e| format!("Cannot read {path}: {e:#}"))
            } else {
                Err(format!("File missing: {path}"))
            };
            let cached = Cached { version, result };
            ui.data_mut(|d| d.insert_temp(file_memory, cached.clone()));
            cached
        });
    let mut asset = match pending.clone() {
        Some(asset) => asset,
        None => match cached.result {
            Ok(asset) => asset,
            Err(message) => {
                let line = message.lines().next().unwrap_or_default();
                ui.colored_label(crate::theme::colors::ERROR, line);
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
        ui.data_mut(|d| d.remove::<Pending>(memory));
        match asset.save(&file) {
            // The next core update must see the write, not the old version.
            Ok(()) => {
                if let Some(mut server) = world.get_resource_mut::<AssetServer>() {
                    server.refresh(path);
                }
            }
            Err(error) => tracing::error!("could not save {}: {error:#}", file.display()),
        }
    } else if dirty {
        ui.data_mut(|d| d.insert_temp(memory, Pending { frame, asset }));
    }
}

/// The "no environment" line of an empty reference and a New Environment
/// button: it writes `assets/environments/New Environment.env` (a free name)
/// and returns a reference to it.
pub fn show_empty(ui: &mut egui::Ui, world: &mut World, project_root: &Path) -> Option<AssetRef> {
    ui.colored_label(
        crate::theme::colors::TEXT_MUTED,
        "No environment: the default look is used",
    );
    if !ui.button("New Environment").clicked() {
        return None;
    }
    let dir = project_root.join("assets/environments");
    let created = create_in(world, project_root, &dir);
    if let Err(error) = &created {
        tracing::error!("could not create an environment: {error:#}");
    }
    created.ok()
}

fn create_in(world: &mut World, root: &Path, dir: &Path) -> anyhow::Result<AssetRef> {
    std::fs::create_dir_all(dir)?;
    let mut server = world
        .get_resource_mut::<AssetServer>()
        .ok_or_else(|| anyhow::anyhow!("no asset server"))?;
    let file = crate::asset_ops::create_environment(&mut server, dir)?;
    let rel = melon_core::assets::project_relative(root, &file)
        .ok_or_else(|| anyhow::anyhow!("{} is outside the project", file.display()))?;
    Ok(AssetRef::for_path(&server, &rel))
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

    /// A world whose asset server reads `root`.
    fn world_for(root: &Path) -> World {
        let mut world = melon_core::Core::new().world;
        world.insert_resource(AssetServer::new(root.to_path_buf()));
        world
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

    fn pending_now(ctx: &egui::Context, asset: EnvironmentAsset) {
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| d.insert_temp(pending_id(), Pending { frame, asset }));
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
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        let mut copy = EnvironmentAsset::default();
        copy.fog.density = 0.5;
        pending_now(&ctx, copy.clone());
        frame(&ctx, &mut world, &root, vec![]);
        let preview = world.resource::<EnvironmentPreview>();
        assert_eq!(preview.0, Some(("assets/a.env".to_string(), copy)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_pending_copy_is_dropped() {
        let root = project("stale");
        let file = root.join("assets/a.env");
        EnvironmentAsset::default().save(&file).unwrap();
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        let mut copy = EnvironmentAsset::default();
        copy.fog.density = 0.5;
        pending_now(&ctx, copy);
        // The card is not drawn for a few frames.
        for _ in 0..3 {
            let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        }
        frame(&ctx, &mut world, &root, vec![]);
        assert!(world.resource::<EnvironmentPreview>().0.is_none());
        assert_eq!(
            EnvironmentAsset::read(&file).unwrap(),
            EnvironmentAsset::default(),
            "the stale copy never reaches the file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preview_is_dropped_when_the_card_is_not_drawn() {
        let root = project("dropped");
        EnvironmentAsset::default()
            .save(&root.join("assets/a.env"))
            .unwrap();
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        let mut copy = EnvironmentAsset::default();
        copy.fog.density = 0.5;
        pending_now(&ctx, copy);
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

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn version_of(world: &mut World) -> u32 {
        let mut server = world.resource_mut::<AssetServer>();
        let handle = server.load::<EnvironmentFile>("assets/a.env");
        server.version(handle)
    }

    #[test]
    fn a_drag_previews_while_the_pointer_is_down_and_saves_when_it_is_up() {
        let root = project("drag");
        let file = root.join("assets/a.env");
        EnvironmentAsset::default().save(&file).unwrap();
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        frame(&ctx, &mut world, &root, vec![]);
        frame(&ctx, &mut world, &root, vec![]);
        // The sun's first field (elevation).
        let field = egui::Id::new(("environment_fields", "assets/a.env", "sun")).with("elevation");
        let rect = ctx.read_response(field).expect("elevation field").rect;
        let (from, to) = (rect.center(), rect.center() + egui::vec2(40.0, 0.0));
        let press = vec![egui::Event::PointerMoved(from), button(from, true)];
        frame(&ctx, &mut world, &root, press);
        frame(&ctx, &mut world, &root, vec![egui::Event::PointerMoved(to)]);
        let pending: Pending = ctx.data(|d| d.get_temp(pending_id())).expect("pending");
        let pending = pending.asset;
        assert_ne!(pending, EnvironmentAsset::default(), "the drag edited it");
        assert_eq!(
            EnvironmentAsset::read(&file).unwrap(),
            EnvironmentAsset::default(),
            "nothing written mid-drag"
        );
        let preview = world.resource::<EnvironmentPreview>().0.clone();
        assert_eq!(preview, Some(("assets/a.env".to_string(), pending.clone())));
        let before = version_of(&mut world);
        frame(&ctx, &mut world, &root, vec![button(to, false)]);
        assert_eq!(EnvironmentAsset::read(&file).unwrap(), pending, "saved");
        let left: Option<Pending> = ctx.data(|d| d.get_temp(pending_id()));
        assert!(left.is_none(), "pending cleared after the write");
        assert_eq!(version_of(&mut world), before + 1, "the save bumped it");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_file_is_parsed_once_per_version() {
        let root = project("cache");
        let file = root.join("assets/a.env");
        EnvironmentAsset::default().save(&file).unwrap();
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        frame(&ctx, &mut world, &root, vec![]);
        // No re-read while the version stands: deleting the file changes nothing.
        std::fs::remove_file(&file).unwrap();
        frame(&ctx, &mut world, &root, vec![]);
        frame(&ctx, &mut world, &root, vec![]);
        assert!(
            ctx.read_response(egui::Id::new(("environment_section", "sun")))
                .is_some()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dot_slash_path_matches_the_server_path() {
        let root = project("dotslash");
        let mut copy = EnvironmentAsset::default();
        copy.fog.density = 0.5;
        copy.save(&root.join("assets/a.env")).unwrap();
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        pending_now(&ctx, copy.clone());
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &root, "./assets/a.env")
        });
        let preview = world.resource::<EnvironmentPreview>().0.clone();
        assert_eq!(preview, Some(("assets/a.env".to_string(), copy)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_section_gets_a_header() {
        let root = project("headers");
        EnvironmentAsset::default()
            .save(&root.join("assets/a.env"))
            .unwrap();
        let mut world = world_for(&root);
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
    fn labels_are_sentence_case_and_hints_become_drags() {
        let sky = EnvironmentAsset::default().sky;
        let widgets = section_widgets(&sky, Path::new("."));
        let intensity = &widgets["texture_intensity"];
        assert_eq!(intensity.label.as_deref(), Some("Texture intensity"));
        assert!(matches!(
            intensity.kind,
            WidgetKind::Drag { max, .. } if max == 20.0
        ));
        assert!(matches!(widgets["zenith"].kind, WidgetKind::HdrColor));
        let sun = EnvironmentAsset::default().sun;
        let widgets = section_widgets(&sun, Path::new("."));
        assert!(matches!(
            widgets["elevation"].kind,
            WidgetKind::Drag { min, max, suffix, .. }
                if min == -90.0 && max == 90.0 && suffix == "\u{b0}"
        ));
    }

    fn drawn_text(ctx: &egui::Context, world: &mut World, root: &Path) -> String {
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, world, root, "assets/a.env")
        });
        out.shapes
            .iter()
            .map(|s| format!("{:?}", s.shape))
            .collect()
    }

    #[test]
    fn broken_file_shows_the_error_and_no_fields() {
        let root = project("broken");
        std::fs::write(root.join("assets/a.env"), "(fog: ").unwrap();
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        let shown = drawn_text(&ctx, &mut world, &root);
        assert!(shown.contains("Cannot read assets/a.env"), "{shown}");
        assert!(
            ctx.read_response(egui::Id::new(("environment_section", "sun")))
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_file_says_so() {
        let root = project("missing");
        let mut world = world_for(&root);
        let ctx = egui::Context::default();
        let shown = drawn_text(&ctx, &mut world, &root);
        assert!(shown.contains("File missing: assets/a.env"), "{shown}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_empty_state_creates_and_references_a_new_environment() {
        let root = project("empty");
        let mut world = world_for(&root);
        let dir = root.join("assets/environments");
        let reference = create_in(&mut world, &root, &dir).unwrap();
        assert_eq!(reference.path, "assets/environments/New Environment.env");
        assert!(root.join(&reference.path).is_file());
        assert!(!reference.guid.is_empty(), "the meta gave it a GUID");
        let _ = std::fs::remove_dir_all(&root);
    }
}
