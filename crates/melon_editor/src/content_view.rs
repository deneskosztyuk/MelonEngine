//! Drawing for the Content Browser (`ContentBrowser::ui`). State and logic
//! live in `content.rs`, so the same browser can be drawn in a dock tab or
//! the Content Drawer.

use std::path::{Path, PathBuf};

use egui::{Color32, Id, PointerButton, RichText, Sense};

use crate::content::{
    AssetDrag, AssetKind, BrowserRequest, ContentBrowser, ContentEntry, format_size, is_model_path,
    list_dir,
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
    Request(BrowserRequest),
    StartRename(PathBuf),
    RenameText(String),
    /// Leave the inline rename; `true` commits the text.
    EndRename(bool),
    /// A model tile is on screen: it wants a thumbnail.
    WantThumbnail(String),
}

fn kind_color(kind: AssetKind) -> Color32 {
    match kind {
        AssetKind::Folder => colors::WARNING,
        AssetKind::Model => colors::ACCENT_HOVER,
        AssetKind::Texture => colors::SUCCESS,
        AssetKind::Shader => colors::ERROR,
        AssetKind::Scene => colors::INFO,
        AssetKind::Audio => colors::ACCENT,
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
pub(crate) fn id_label(
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

/// New Folder / Show in Explorer for a folder (right-click on a tree row
/// or on empty space).
fn folder_menu(ui: &mut egui::Ui, dir: &Path, actions: &mut Vec<Action>) {
    if ui.button("New Folder").clicked() {
        actions.push(Action::Request(BrowserRequest::NewFolder(
            dir.to_path_buf(),
        )));
    }
    if ui.button("Show in Explorer").clicked() {
        actions.push(Action::Request(BrowserRequest::Reveal(dir.to_path_buf())));
    }
}

/// A browser item released over `folder` moves into it.
fn drop_target(response: &egui::Response, folder: &Path, actions: &mut Vec<Action>) {
    if let Some(drag) = response.dnd_release_payload::<AssetDrag>()
        && drag.path != folder
    {
        actions.push(Action::Request(BrowserRequest::Move(
            drag.path.clone(),
            folder.to_path_buf(),
        )));
    }
}

impl ContentBrowser {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        self.thumbnails.begin_frame();
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
        // Focus follows the last press: inside the browser or not.
        let (pressed, press_pos) =
            ui.input(|i| (i.pointer.any_pressed(), i.pointer.interact_pos()));
        if pressed {
            self.focused = press_pos.is_some_and(|p| ui.max_rect().contains(p));
        }
        // A rename of an item that is not shown (other folder, filtered
        // out) could never finish and would hold the keys.
        if self
            .renaming
            .as_ref()
            .is_some_and(|r| !self.visible_entries().iter().any(|e| e.path == r.path))
        {
            self.renaming = None;
        }
        self.shortcut_keys(ui, &mut actions);
        // A ping flashes for PING_SECONDS from its first draw.
        let now = ui.input(|i| i.time);
        self.ping_scroll = false;
        if let Some((_, start)) = &mut self.pinged {
            match start {
                None => {
                    *start = Some(now);
                    self.ping_scroll = true;
                }
                Some(t) if now - *t >= sizes::PING_SECONDS => self.pinged = None,
                Some(_) => {}
            }
        }
        let browser_rect = ui.max_rect();
        if let Some(start) = &mut self.flashed {
            let start = *start.get_or_insert(now);
            if now - start >= sizes::PING_SECONDS {
                self.flashed = None;
            }
        }

        self.toolbar(ui, &mut actions);
        ui.separator();
        let tree_id = Id::new("content_tree");
        let resizing = ui
            .ctx()
            .read_response(tree_id.with("__resize"))
            .is_some_and(|r| r.dragged() || r.drag_stopped());
        if !resizing {
            // egui stores the width clamped by a narrow tab; start from ours.
            ui.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(tree_id));
        }
        let max = ui.available_width() * sizes::CONTENT_TREE_MAX_SHARE;
        let panel = egui::Panel::left(tree_id)
            .resizable(true)
            .default_size(self.tree_width)
            .min_size(sizes::CONTENT_TREE_MIN.min(max))
            .max_size(max)
            .show(ui, |ui| self.tree(ui, &mut actions));
        if resizing {
            self.tree_width = panel.response.rect.width();
        }
        // Empty space behind the items: right-click menu, click clears the selection.
        let background = ui.interact(
            ui.available_rect_before_wrap(),
            Id::new("content_items_background"),
            Sense::click(),
        );
        if background.clicked() {
            self.select(None);
        }
        let current = self.current().to_path_buf();
        background.context_menu(|ui| folder_menu(ui, &current, &mut actions));
        egui::ScrollArea::vertical()
            .id_salt("content_items")
            .auto_shrink(false)
            .show(ui, |ui| self.asset_view(ui, &mut actions));
        if self.flashing() {
            ui.painter().rect_stroke(
                browser_rect,
                sizes::RADIUS,
                egui::Stroke::new(sizes::PING_STROKE, colors::ACCENT),
                egui::StrokeKind::Inside,
            );
            ui.ctx().request_repaint();
        }

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
                Action::Request(request) => self.request(request),
                Action::WantThumbnail(path) => self.thumbnails.want(path),
                Action::StartRename(path) => {
                    self.select(Some(path.clone()));
                    self.start_rename(path);
                }
                Action::RenameText(text) => {
                    if let Some(renaming) = &mut self.renaming {
                        renaming.text = text;
                        renaming.fresh = false;
                    }
                }
                Action::EndRename(commit) => {
                    if let Some(renaming) = self.renaming.take()
                        && commit
                        && renaming
                            .path
                            .file_name()
                            .is_some_and(|n| *n != *renaming.text)
                    {
                        self.request(BrowserRequest::Rename(renaming.path, renaming.text));
                    }
                }
            }
        }
    }

    /// F2 / Ctrl+D / Delete / Enter on the selection, only while the browser
    /// has focus and no text field is being typed in.
    fn shortcut_keys(&self, ui: &egui::Ui, actions: &mut Vec<Action>) {
        if !self.focused || self.blocked || self.renaming.is_some() || ui.ctx().text_edit_focused()
        {
            return;
        }
        let Some(selected) = self.selected().map(Path::to_path_buf) else {
            return;
        };
        // Fresh presses only: a held Ctrl+D must not keep duplicating.
        let (rename, duplicate, delete, open) = ui.input(|i| {
            (
                crate::fresh_press(i, egui::Key::F2),
                i.modifiers.command && crate::fresh_press(i, egui::Key::D),
                crate::fresh_press(i, egui::Key::Delete),
                crate::fresh_press(i, egui::Key::Enter),
            )
        });
        if rename {
            actions.push(Action::StartRename(selected.clone()));
        }
        if duplicate {
            actions.push(Action::Request(BrowserRequest::Duplicate(selected.clone())));
        }
        if delete {
            actions.push(Action::Request(BrowserRequest::AskDelete(selected.clone())));
        }
        if open {
            actions.push(if selected.is_dir() {
                Action::Open(selected)
            } else {
                Action::Request(BrowserRequest::Open(selected))
            });
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
            if let Some((playing, request)) = self.audio_toggle() {
                let (glyph, tip) = if playing {
                    ("⏹ Stop", "Stop the preview")
                } else {
                    ("▶ Preview", "Play the selected clip (Ui bus)")
                };
                if ui.button(glyph).on_hover_text(tip).clicked() {
                    actions.push(Action::Request(request));
                }
                ui.separator();
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
                self.reveal = false;
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
        open |= self.reveal && ancestor && dir != self.current();
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * sizes::CONTENT_TREE_INDENT);
            if children.is_empty() {
                ui.add_space(sizes::CONTENT_TREE_ARROW);
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
            drop_target(&row, dir, actions);
            let favorite = self.favorites().iter().any(|f| f == dir);
            row.context_menu(|ui| {
                if favorite {
                    if ui.button("Remove from Favorites").clicked() {
                        actions.push(Action::RemoveFavorite(dir.to_path_buf()));
                    }
                } else if ui.button("Add to Favorites").clicked() {
                    actions.push(Action::AddFavorite(dir.to_path_buf()));
                }
                ui.separator();
                folder_menu(ui, dir, actions);
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
        if let Some(message) = self.empty_message() {
            ui.weak(message);
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
        let model = (!entry.is_dir && is_model_path(&entry.name))
            .then(|| {
                self.root()
                    .parent()
                    .and_then(|p| melon_core::assets::project_relative(p, &entry.path))
            })
            .flatten();
        let drag = AssetDrag {
            path: entry.path.clone(),
            model: model.clone(),
        };
        // Not `dnd_drag_source`: its drag-only widget on top hides this
        // item's clicks (egui hit test), so the item senses both itself.
        let response = self.draw_item(ui, entry, model.as_deref());
        if let Some(model) = &model
            && !self.is_list()
            && ui.is_rect_visible(response.rect)
        {
            actions.push(Action::WantThumbnail(model.clone()));
        }
        if response.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), drag);
        }
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if response.clicked() || response.secondary_clicked() {
            actions.push(Action::Select(entry.path.clone()));
        }
        if response.double_clicked() {
            actions.push(if entry.is_dir {
                Action::Open(entry.path.clone())
            } else {
                Action::Request(BrowserRequest::Open(entry.path.clone()))
            });
        }
        if entry.is_dir {
            drop_target(&response, &entry.path, actions);
        }
        let path = &entry.path;
        response.context_menu(|ui| {
            if ui.button("Open").clicked() {
                actions.push(if entry.is_dir {
                    Action::Open(path.clone())
                } else {
                    Action::Request(BrowserRequest::Open(path.clone()))
                });
            }
            if ui
                .add(egui::Button::new("Rename").shortcut_text("F2"))
                .clicked()
            {
                actions.push(Action::StartRename(path.clone()));
            }
            if ui
                .add(egui::Button::new("Duplicate").shortcut_text("Ctrl+D"))
                .clicked()
            {
                actions.push(Action::Request(BrowserRequest::Duplicate(path.clone())));
            }
            if ui
                .add(egui::Button::new("Delete").shortcut_text("Del"))
                .clicked()
            {
                actions.push(Action::Request(BrowserRequest::AskDelete(path.clone())));
            }
            ui.separator();
            if ui.button("Show in Explorer").clicked() {
                actions.push(Action::Request(BrowserRequest::Reveal(path.clone())));
            }
        });
        self.rename_field(ui, entry, &response, actions);
    }

    /// The inline rename text field over the item's name.
    fn rename_field(
        &self,
        ui: &mut egui::Ui,
        entry: &ContentEntry,
        item: &egui::Response,
        actions: &mut Vec<Action>,
    ) {
        let Some(renaming) = self.renaming.as_ref().filter(|r| r.path == entry.path) else {
            return;
        };
        let rect = if self.is_list() {
            item.rect
        } else {
            egui::Rect::from_min_max(
                egui::pos2(item.rect.left(), item.rect.bottom() - sizes::TILE_LABEL),
                item.rect.max,
            )
        };
        let id = Id::new("content_rename");
        let mut text = renaming.text.clone();
        let edit = ui.put(rect, egui::TextEdit::singleline(&mut text).id(id));
        if renaming.fresh {
            edit.request_focus();
            // Select the name without its extension, as Explorer does.
            let stem = Path::new(&renaming.text)
                .file_stem()
                .map_or(0, |s| s.to_string_lossy().chars().count());
            let stem = if entry.is_dir {
                text.chars().count()
            } else {
                stem
            };
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(stem),
                    )));
                state.store(ui.ctx(), id);
            }
            actions.push(Action::RenameText(text));
            return;
        }
        if edit.changed() {
            actions.push(Action::RenameText(text.clone()));
        }
        if edit.lost_focus() {
            let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
            actions.push(Action::RenameText(text));
            actions.push(Action::EndRename(!cancelled));
        }
    }

    /// `model`: the project-relative path of a model file (its thumbnail key).
    fn draw_item(
        &self,
        ui: &mut egui::Ui,
        entry: &ContentEntry,
        model: Option<&str>,
    ) -> egui::Response {
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
        let response = ui.interact(rect, id, Sense::click_and_drag());
        let drop_here = entry.is_dir
            && response.contains_pointer()
            && egui::DragAndDrop::payload::<AssetDrag>(ui.ctx())
                .is_some_and(|drag| drag.path != entry.path);
        let fill = if selected || drop_here {
            colors::ACCENT_SOFT
        } else if response.hovered() {
            colors::SURFACE_RAISED
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, sizes::RADIUS, fill);
        if self.pinged.as_ref().is_some_and(|(p, _)| *p == entry.path) {
            if self.ping_scroll {
                response.scroll_to_me(Some(egui::Align::Center));
            }
            painter.rect_stroke(
                rect,
                sizes::RADIUS,
                egui::Stroke::new(sizes::PING_STROKE, colors::ACCENT),
                egui::StrokeKind::Inside,
            );
            ui.ctx().request_repaint();
        }
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
            match model.and_then(|m| self.thumbnails.texture(m)) {
                Some(texture) => {
                    egui::Image::new((texture, icon_rect.size())).paint_at(ui, icon_rect);
                }
                None => {
                    let icon_font =
                        egui::FontId::proportional(rect.width() * sizes::TILE_ICON_SCALE);
                    painter.text(
                        icon_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        glyph,
                        icon_font,
                        tint,
                    );
                }
            }
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
    use crate::content::{AssetDrag, BrowserRequest, ContentBrowser};
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

    /// Like `frame`, with Ctrl / Cmd held (egui reads modifiers from the
    /// raw input, not from the key events).
    fn frame_ctrl(
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
            modifiers: egui::Modifiers::COMMAND,
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
    fn tree_reveals_the_current_folder_after_a_collapsed_draw() {
        let dir = temp_dir("reveal");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let row = |path: PathBuf| egui::Id::new(("content_tree_row", path));
        assert!(ctx.read_response(row(dir.join("a"))).is_some());
        assert!(
            ctx.read_response(row(dir.join("a/b"))).is_none(),
            "a is collapsed"
        );
        browser.open(dir.join("a/b"));
        frame(&mut browser, &ctx, 0.1, Vec::new());
        assert!(ctx.read_response(row(dir.join("a/b"))).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn narrow_tab_does_not_shrink_the_saved_tree_width() {
        let dir = temp_dir("narrow");
        let mut browser = ContentBrowser::new(dir.clone());
        browser.tree_width = 250.0;
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(100.0, 500.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| browser.ui(ui));
        assert_eq!(browser.tree_width, 250.0);
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

    #[test]
    fn clicking_a_model_tile_selects_it() {
        let dir = temp_dir("model_click");
        let model = dir.join("a.glb");
        std::fs::write(&model, b"x").unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let pos = center(&ctx, egui::Id::new(("content_item", model.clone())));
        frame(
            &mut browser,
            &ctx,
            0.1,
            click(pos, egui::PointerButton::Primary),
        );
        assert_eq!(browser.selected(), Some(model.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        let event = |pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        vec![event(true), event(false)]
    }

    /// A browser on `dir` with `a.glb` selected by a click (which also
    /// gives it focus). Returns the context and the time of the last frame.
    fn focused_on_a(dir: &std::path::Path) -> (ContentBrowser, egui::Context, f64) {
        std::fs::write(dir.join("a.glb"), b"x").unwrap();
        let mut browser = ContentBrowser::new(dir.to_path_buf());
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let pos = center(&ctx, egui::Id::new(("content_item", dir.join("a.glb"))));
        frame(
            &mut browser,
            &ctx,
            0.1,
            click(pos, egui::PointerButton::Primary),
        );
        (browser, ctx, 0.1)
    }

    #[test]
    fn f2_starts_rename_and_enter_requests_it() {
        let dir = temp_dir("rename_keys");
        let (mut browser, ctx, mut t) = focused_on_a(&dir);
        let mut step = |browser: &mut ContentBrowser, events| {
            t += 0.1;
            frame(browser, &ctx, t, events);
        };
        step(&mut browser, key(egui::Key::F2));
        step(&mut browser, Vec::new());
        step(&mut browser, vec![egui::Event::Text("b".into())]);
        step(&mut browser, key(egui::Key::Enter));
        assert_eq!(
            browser.take_requests(),
            [BrowserRequest::Rename(dir.join("a.glb"), "b.glb".into())]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn escape_cancels_rename() {
        let dir = temp_dir("rename_esc");
        let (mut browser, ctx, t) = focused_on_a(&dir);
        frame(&mut browser, &ctx, t + 0.1, key(egui::Key::F2));
        frame(&mut browser, &ctx, t + 0.2, Vec::new());
        frame(
            &mut browser,
            &ctx,
            t + 0.3,
            vec![egui::Event::Text("b".into())],
        );
        frame(&mut browser, &ctx, t + 0.4, key(egui::Key::Escape));
        frame(&mut browser, &ctx, t + 0.5, Vec::new());
        assert!(browser.take_requests().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_and_duplicate_keys_need_focus() {
        let dir = temp_dir("keys_focus");
        let (mut browser, ctx, t) = focused_on_a(&dir);
        frame(&mut browser, &ctx, t + 0.1, key(egui::Key::Delete));
        assert_eq!(
            browser.take_requests(),
            [BrowserRequest::AskDelete(dir.join("a.glb"))]
        );
        // Draw the browser in the left half only, so the right half is outside it.
        let half_frame = |browser: &mut ContentBrowser, time: f64, events: Vec<egui::Event>| {
            let ctrl = events
                .iter()
                .any(|e| matches!(e, egui::Event::Key { modifiers, .. } if modifiers.command));
            let input = egui::RawInput {
                modifiers: if ctrl {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::NONE
                },
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 500.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                let half = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(450.0, 500.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(half), |ui| browser.ui(ui));
            });
        };
        half_frame(
            &mut browser,
            t + 0.4,
            click(egui::pos2(700.0, 300.0), egui::PointerButton::Primary),
        );
        assert!(!browser.has_focus());
        half_frame(&mut browser, t + 0.5, key(egui::Key::Delete));
        let mut ctrl_d = key(egui::Key::D);
        for event in &mut ctrl_d {
            if let egui::Event::Key { modifiers, .. } = event {
                *modifiers = egui::Modifiers::COMMAND;
            }
        }
        half_frame(&mut browser, t + 0.6, ctrl_d);
        assert!(browser.take_requests().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_an_item_on_a_folder_requests_a_move() {
        let dir = temp_dir("drop_move");
        std::fs::create_dir_all(dir.join("c")).unwrap();
        std::fs::write(dir.join("x.glb"), b"x").unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let pos = center(&ctx, egui::Id::new(("content_item", dir.join("c"))));
        frame(
            &mut browser,
            &ctx,
            0.1,
            vec![egui::Event::PointerMoved(pos)],
        );
        egui::DragAndDrop::set_payload(
            &ctx,
            AssetDrag {
                path: dir.join("x.glb"),
                model: Some("assets/x.glb".into()),
            },
        );
        let release = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &mut browser,
            &ctx,
            0.2,
            vec![egui::Event::PointerMoved(pos), release],
        );
        assert_eq!(
            browser.take_requests(),
            [BrowserRequest::Move(dir.join("x.glb"), dir.join("c"))]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn held_ctrl_d_duplicates_once() {
        let dir = temp_dir("ctrl_d_repeat");
        let (mut browser, ctx, t) = focused_on_a(&dir);
        let press = |repeat| egui::Event::Key {
            key: egui::Key::D,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::COMMAND,
        };
        frame_ctrl(&mut browser, &ctx, t + 0.1, vec![press(false)]);
        assert_eq!(browser.take_requests().len(), 1);
        frame_ctrl(&mut browser, &ctx, t + 0.2, vec![press(true)]);
        frame_ctrl(&mut browser, &ctx, t + 0.3, vec![press(true)]);
        assert!(
            browser.take_requests().is_empty(),
            "key repeat does not duplicate again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_of_a_hidden_item_is_dropped() {
        let dir = temp_dir("rename_hidden");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        browser.start_rename(dir.join("a/b"));
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        assert!(browser.renaming.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tile_view_wants_thumbnails_list_view_does_not() {
        let dir = temp_dir("thumbs_wanted");
        std::fs::write(dir.join("a.glb"), b"x").unwrap();
        std::fs::write(dir.join("notes.md"), b"x").unwrap();
        let mut browser = ContentBrowser::new(dir.clone());
        let ctx = egui::Context::default();
        frame(&mut browser, &ctx, 0.0, Vec::new());
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(browser.thumbnails.wanted(), [format!("{name}/a.glb")]);
        *browser.tile_size_mut() = crate::theme::sizes::TILE_MIN;
        frame(&mut browser, &ctx, 0.1, Vec::new());
        assert!(browser.thumbnails.wanted().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
