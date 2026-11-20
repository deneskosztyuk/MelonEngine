//! Drawing for the Content Browser (`ContentBrowser::ui`). State and logic
//! live in `content.rs`, so the same browser can be drawn in a dock tab or
//! the Content Drawer.

use std::path::{Path, PathBuf};

use egui::{Color32, Id, PointerButton, RichText, Sense};

use crate::content::{
    AssetKind, ContentBrowser, ContentEntry, format_size, is_model_path, list_dir,
};
use crate::theme::{colors, sizes};

/// What the user did this frame; applied after drawing so drawing can
/// borrow the browser immutably.
enum Action {
    Open(PathBuf),
    Back,
    Forward,
    Up,
    Refresh,
    Select(PathBuf),
    Search(String),
    ToggleKind(AssetKind),
    AddFavorite(PathBuf),
    RemoveFavorite(PathBuf),
}

fn kind_color(kind: AssetKind) -> Color32 {
    match kind {
        AssetKind::Folder => colors::WARNING,
        AssetKind::Model => colors::ACCENT_HOVER,
        AssetKind::Texture => colors::SUCCESS,
        AssetKind::Shader => colors::ERROR,
        AssetKind::Scene => colors::INFO,
        AssetKind::Other => colors::TEXT_MUTED,
    }
}

fn kind_glyph(kind: AssetKind) -> &'static str {
    if kind == AssetKind::Folder {
        "🗀"
    } else {
        "🗋"
    }
}

/// A selectable label with a fixed id (`ui.add` ids are positional), so
/// tests and other code can find it with `Context::read_response`. Painted
/// like egui's selectable label; it is the only widget on its rect, so it
/// gets hover.
fn id_label(
    ui: &mut egui::Ui,
    id: Id,
    text: impl Into<egui::WidgetText>,
    selected: bool,
) -> egui::Response {
    let padding = ui.spacing().button_padding;
    let galley = text.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let size = (galley.size() + 2.0 * padding).max(egui::vec2(0.0, ui.spacing().interact_size.y));
    let (_, rect) = ui.allocate_space(size);
    let response = ui.interact(rect, id, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() || response.has_focus() {
            ui.painter().rect(
                rect.expand(visuals.expansion),
                visuals.corner_radius,
                visuals.weak_bg_fill,
                visuals.bg_stroke,
                egui::StrokeKind::Inside,
            );
        }
        let text_pos = egui::pos2(
            rect.min.x + padding.x,
            rect.center().y - galley.size().y / 2.0,
        );
        ui.painter().galley(text_pos, galley, visuals.text_color());
    }
    response
}

impl ContentBrowser {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        // Mouse buttons 4 / 5: read input first, test hover outside the closure.
        let (back, forward) = ui.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Extra1),
                i.pointer.button_pressed(PointerButton::Extra2),
            )
        });
        if ui.rect_contains_pointer(ui.max_rect()) {
            if back {
                actions.push(Action::Back);
            }
            if forward {
                actions.push(Action::Forward);
            }
        }

        self.toolbar(ui, &mut actions);
        ui.separator();
        let panel = egui::Panel::left(Id::new("content_tree"))
            .resizable(true)
            .default_size(self.tree_width)
            .min_size(sizes::CONTENT_TREE_MIN)
            .show(ui, |ui| self.tree(ui, &mut actions));
        self.tree_width = panel.response.rect.width();
        egui::ScrollArea::vertical()
            .id_salt("content_items")
            .auto_shrink(false)
            .show(ui, |ui| self.asset_view(ui, &mut actions));

        for action in actions {
            match action {
                Action::Open(dir) => self.open(dir),
                Action::Back => self.go_back(),
                Action::Forward => self.go_forward(),
                Action::Up => self.up(),
                Action::Refresh => self.refresh(),
                Action::Select(path) => self.select(Some(path)),
                Action::Search(text) => self.set_search(text),
                Action::ToggleKind(kind) => self.toggle_kind(kind),
                Action::AddFavorite(dir) => self.add_favorite(dir),
                Action::RemoveFavorite(dir) => self.remove_favorite(&dir),
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
            let nav = [
                (
                    "⬅",
                    "Back (mouse button 4)",
                    self.can_go_back(),
                    Action::Back,
                ),
                (
                    "➡",
                    "Forward (mouse button 5)",
                    self.can_go_forward(),
                    Action::Forward,
                ),
                (
                    "⬆",
                    "Parent folder",
                    self.current() != self.root(),
                    Action::Up,
                ),
                ("🔄", "Refresh", true, Action::Refresh),
            ];
            for (glyph, tip, enabled, action) in nav {
                if ui
                    .add_enabled(enabled, egui::Button::new(glyph))
                    .on_hover_text(tip)
                    .clicked()
                {
                    actions.push(action);
                }
            }
            ui.separator();
            for (i, (name, dir)) in self.path_segments().into_iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("/").color(colors::TEXT_MUTED));
                }
                if id_label(ui, Id::new(("content_crumb", i)), name, false).clicked() {
                    actions.push(Action::Open(dir));
                }
            }
            ui.separator();
            let mut search = self.search().to_string();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut search)
                        .hint_text("Search")
                        .desired_width(sizes::CONTENT_SEARCH),
                )
                .changed()
            {
                actions.push(Action::Search(search));
            }
            for kind in AssetKind::FILE_KINDS {
                let text = RichText::new(kind.label()).color(kind_color(kind));
                if ui
                    .selectable_label(self.kind_enabled(kind), text)
                    .on_hover_text("Show or hide this type")
                    .clicked()
                {
                    actions.push(Action::ToggleKind(kind));
                }
            }
            ui.add(
                egui::Slider::new(self.tile_size_mut(), sizes::TILE_MIN..=sizes::TILE_MAX)
                    .show_value(false),
            )
            .on_hover_text("Tile size (far left: list)");
        });
    }

    fn tree(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical()
            .id_salt("content_tree_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                if !self.favorites().is_empty() {
                    ui.label(RichText::new("Favorites").color(colors::TEXT_MUTED));
                    for dir in self.favorites().to_vec() {
                        let name = dir
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                        let row = id_label(
                            ui,
                            Id::new(("content_favorite", &dir)),
                            format!("★ {name}"),
                            dir == self.current(),
                        );
                        if row.clicked() {
                            actions.push(Action::Open(dir.clone()));
                        }
                        row.context_menu(|ui| {
                            if ui.button("Remove from Favorites").clicked() {
                                actions.push(Action::RemoveFavorite(dir.clone()));
                            }
                        });
                    }
                    ui.separator();
                }
                let root = self.root().to_path_buf();
                self.tree_node(ui, &root, 0, actions);
            });
    }

    fn tree_node(
        &mut self,
        ui: &mut egui::Ui,
        dir: &Path,
        depth: usize,
        actions: &mut Vec<Action>,
    ) {
        let children = self
            .tree_children
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                list_dir(dir)
                    .into_iter()
                    .filter(|e| e.is_dir)
                    .map(|e| e.path)
                    .collect()
            })
            .clone();
        let open_id = Id::new(("content_tree_open", dir));
        let ancestor = self.current().starts_with(dir);
        let mut open = ui.data_mut(|d| *d.get_temp_mut_or(open_id, ancestor));
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * sizes::SPACING * 2.0);
            if children.is_empty() {
                ui.add_space(sizes::SPACING * 3.0);
            } else if ui
                .add(egui::Button::new(if open { "⏷" } else { "⏵" }).frame(false))
                .clicked()
            {
                open = !open;
            }
            let name = dir
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let row = id_label(
                ui,
                Id::new(("content_tree_row", dir)),
                format!("🗀 {name}"),
                dir == self.current(),
            );
            if row.clicked() {
                actions.push(Action::Open(dir.to_path_buf()));
            }
            let favorite = self.favorites().iter().any(|f| f == dir);
            row.context_menu(|ui| {
                if favorite {
                    if ui.button("Remove from Favorites").clicked() {
                        actions.push(Action::RemoveFavorite(dir.to_path_buf()));
                    }
                } else if ui.button("Add to Favorites").clicked() {
                    actions.push(Action::AddFavorite(dir.to_path_buf()));
                }
            });
        });
        ui.data_mut(|d| d.insert_temp(open_id, open));
        if open {
            for child in children {
                self.tree_node(ui, &child, depth + 1, actions);
            }
        }
    }

    fn asset_view(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let entries = self.visible_entries();
        if entries.is_empty() {
            let unfiltered = AssetKind::FILE_KINDS.iter().all(|k| self.kind_enabled(*k));
            ui.weak(if self.search().is_empty() && unfiltered {
                "Empty folder"
            } else {
                "No assets match"
            });
            return;
        }
        if self.is_list() {
            for entry in entries {
                self.item(ui, entry, actions);
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                for entry in entries {
                    self.item(ui, entry, actions);
                }
            });
        }
    }

    /// One row (list) or tile; model files are drag sources carrying their
    /// project-relative path.
    fn item(&self, ui: &mut egui::Ui, entry: &ContentEntry, actions: &mut Vec<Action>) {
        let model_path = (!entry.is_dir && is_model_path(&entry.name))
            .then(|| {
                self.root()
                    .parent()
                    .and_then(|p| melon_core::assets::project_relative(p, &entry.path))
            })
            .flatten();
        let response = match model_path {
            Some(path) => {
                ui.dnd_drag_source(Id::new(("content_drag", &entry.path)), path, |ui| {
                    self.draw_item(ui, entry)
                })
                .inner
            }
            None => self.draw_item(ui, entry),
        };
        if response.clicked() {
            actions.push(Action::Select(entry.path.clone()));
        }
        if response.double_clicked() && entry.is_dir {
            actions.push(Action::Open(entry.path.clone()));
        }
    }

    fn draw_item(&self, ui: &mut egui::Ui, entry: &ContentEntry) -> egui::Response {
        let id = Id::new(("content_item", &entry.path));
        let selected = self.selected() == Some(entry.path.as_path());
        let folder = (!self.search().is_empty())
            .then(|| {
                entry
                    .path
                    .parent()
                    .and_then(|p| p.strip_prefix(self.root()).ok())
            })
            .flatten()
            .map(|p| p.to_string_lossy().replace('\\', "/"));
        let size = if self.is_list() {
            egui::vec2(ui.available_width(), sizes::ROW_HEIGHT)
        } else {
            let tile = self.tile_size();
            egui::vec2(tile, tile + sizes::TILE_LABEL)
        };
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let response = ui.interact(rect, id, Sense::click());
        let fill = if selected {
            colors::ACCENT_SOFT
        } else if response.hovered() {
            colors::SURFACE_RAISED
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, sizes::RADIUS, fill);
        let glyph = kind_glyph(entry.kind);
        let tint = kind_color(entry.kind);
        let text = colors::TEXT;
        let small = egui::TextStyle::Body.resolve(ui.style());
        if self.is_list() {
            let mut x = rect.left() + sizes::SPACING;
            let y = rect.center().y;
            let g = painter.text(
                egui::pos2(x, y),
                egui::Align2::LEFT_CENTER,
                glyph,
                small.clone(),
                tint,
            );
            x = g.right() + sizes::SPACING;
            let n = painter.text(
                egui::pos2(x, y),
                egui::Align2::LEFT_CENTER,
                &entry.name,
                small.clone(),
                text,
            );
            if let Some(folder) = &folder {
                painter.text(
                    egui::pos2(n.right() + sizes::SPACING, y),
                    egui::Align2::LEFT_CENTER,
                    folder,
                    small.clone(),
                    colors::TEXT_MUTED,
                );
            }
            let detail = if entry.is_dir {
                entry.kind.label().to_string()
            } else {
                format!("{}  {}", entry.kind.label(), format_size(entry.size))
            };
            painter.text(
                egui::pos2(rect.right() - sizes::SPACING, y),
                egui::Align2::RIGHT_CENTER,
                detail,
                small,
                colors::TEXT_MUTED,
            );
        } else {
            let icon_rect =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.width()));
            let icon_font = egui::FontId::proportional(rect.width() * 0.5);
            painter.text(
                icon_rect.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                icon_font,
                tint,
            );
            painter.text(
                egui::pos2(rect.center().x, rect.bottom() - sizes::TILE_LABEL / 2.0),
                egui::Align2::CENTER_CENTER,
                &entry.name,
                small,
                text,
            );
        }
        let mut hover = entry.name.clone();
        if let Some(folder) = folder {
            hover = format!("{folder}/{}", entry.name);
        }
        response.on_hover_text(hover)
    }
}

#[cfg(test)]
mod tests {
    use crate::content::ContentBrowser;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melon_view_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// One frame of the browser at time `time`, with `events`.
    fn frame(
        browser: &mut ContentBrowser,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 500.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| browser.ui(ui));
    }

    fn click(pos: egui::Pos2, button: egui::PointerButton) -> Vec<egui::Event> {
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![egui::Event::PointerMoved(pos), press(true), press(false)]
    }

    fn center(ctx: &egui::Context, id: egui::Id) -> egui::Pos2 {
        ctx.read_response(id)
            .expect("widget was drawn")
            .rect
            .center()
    }

    #[test]
    fn clicking_a_path_segment_opens_that_folder() {
        let dir = temp_dir("crumb");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a/b"));
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let pos = center(&ctx, egui::Id::new(("content_crumb", 1usize)));
        frame(
            &mut browser,
            &ctx,
            0.1,
            click(pos, egui::PointerButton::Primary),
        );
        assert_eq!(browser.current(), dir.join("a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mouse_back_button_goes_back() {
        let dir = temp_dir("mouse_back");
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a"));
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        frame(
            &mut browser,
            &ctx,
            0.1,
            click(egui::pos2(450.0, 300.0), egui::PointerButton::Extra1),
        );
        assert_eq!(browser.current(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_click_folder_tile_opens_it() {
        let dir = temp_dir("double");
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let pos = center(&ctx, egui::Id::new(("content_item", dir.join("a"))));
        frame(
            &mut browser,
            &ctx,
            0.1,
            click(pos, egui::PointerButton::Primary),
        );
        assert_eq!(browser.selected(), Some(dir.join("a").as_path()));
        assert_eq!(browser.current(), dir, "one click only selects");
        frame(
            &mut browser,
            &ctx,
            0.2,
            click(pos, egui::PointerButton::Primary),
        );
        assert_eq!(browser.current(), dir.join("a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hovered_path_segment_paints_a_highlight() {
        let dir = temp_dir("hover");
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.open(dir.join("a"));
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let rect = ctx
            .read_response(egui::Id::new(("content_crumb", 0usize)))
            .unwrap()
            .rect;
        let mut output = None;
        for t in [0.1, 0.2] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 500.0),
                )),
                time: Some(t),
                events: vec![egui::Event::PointerMoved(rect.center())],
                ..Default::default()
            };
            output = Some(ctx.run_ui(input, |ui| browser.ui(ui)));
        }
        let highlighted = output
            .unwrap()
            .shapes
            .iter()
            .any(|clipped| match &clipped.shape {
                egui::Shape::Rect(r) => {
                    r.fill != egui::Color32::TRANSPARENT
                        && r.rect.contains(rect.center())
                        && r.rect.width() <= rect.width() + 8.0
                }
                _ => false,
            });
        assert!(
            highlighted,
            "hovering a path segment paints a fill under it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
