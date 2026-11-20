//! Content Drawer: the Content Browser in a panel that slides up from the
//! status bar over the dock area (Ctrl+Space or the status-bar button).

use bevy_ecs::world::World;

use crate::theme::{colors, sizes};
use crate::{Editor, content, layout};

#[derive(Default)]
pub(crate) struct DrawerState {
    pub(crate) open: bool,
    /// The on-screen part of the drawer last drawn, for the outside rules.
    pub(crate) rect: Option<egui::Rect>,
    /// The status-bar "Content Drawer" button: a press on it is not outside.
    pub(crate) button: Option<egui::Rect>,
}

impl Editor {
    /// Ctrl+Space and the status-bar button. With the Content Browser tab
    /// on screen, point at it instead, so one browser is drawn once.
    pub(crate) fn toggle_drawer(&mut self) {
        if self.drawer.open {
            self.drawer.open = false;
        } else if self.content_tab_shown {
            layout::reopen(&mut self.dock, layout::EditorTab::ContentBrowser);
            self.content.flash();
        } else {
            self.drawer.open = true;
        }
    }

    /// "Dock in Layout": the browser goes back to its tab.
    pub(crate) fn dock_drawer(&mut self) {
        layout::reopen(&mut self.dock, layout::EditorTab::ContentBrowser);
        self.drawer.open = false;
    }

    /// Runs after the dock, so `content_tab_shown` is this frame's.
    pub(crate) fn drawer_ui(&mut self, world: &mut World, ui: &egui::Ui) {
        let ctx = ui.ctx().clone();
        // The tab was clicked: it draws the browser now.
        if self.content_tab_shown {
            self.drawer.open = false;
        }
        if self.drawer.open && self.drawer_should_close(&ctx) {
            self.drawer.open = false;
        }
        let shown = ctx.animate_bool_with_time(
            egui::Id::new("content_drawer"),
            self.drawer.open,
            sizes::DRAWER_SLIDE_SECONDS,
        );
        if shown <= 0.0 || self.content_tab_shown {
            self.drawer.rect = None;
            return;
        }
        let screen = ctx.content_rect();
        let bottom = screen.bottom() - sizes::STATUS_HEIGHT;
        let height = screen.height() * sizes::DRAWER_HEIGHT_SHARE;
        let visible =
            egui::Rect::from_x_y_ranges(screen.x_range(), bottom - height * shown..=bottom);
        self.drawer.rect = Some(visible);
        self.content_shown = true;
        self.content.blocked = self.delete_confirm.is_some() || self.confirm_open.is_some();
        let mut dock = false;
        egui::Area::new(egui::Id::new("content_drawer"))
            .order(egui::Order::Foreground)
            // An area is placed by last frame's size: hanging it from its
            // bottom edge keeps a shrinking drawer off the status bar.
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(visible.left_bottom())
            .constrain(false)
            .show(&ctx, |ui| {
                // Grows rather than slides: the browser's panels set their
                // own clip rects, so content below `visible` would show.
                ui.set_clip_rect(visible);
                ui.scope_builder(egui::UiBuilder::new().max_rect(visible), |ui| {
                    egui::Frame::new()
                        .fill(colors::CHROME_RAISED)
                        .inner_margin(sizes::SPACING)
                        .show(ui, |ui| {
                            let size = visible.shrink(sizes::SPACING).size().max(egui::Vec2::ZERO);
                            ui.set_min_size(size);
                            ui.set_max_size(size);
                            ui.horizontal(|ui| {
                                ui.strong("Content Drawer");
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| dock = ui.button("Dock in Layout").clicked(),
                                );
                            });
                            ui.separator();
                            self.content.ui(ui);
                        });
                });
                // The theme's window stroke is BORDER_STRONG.
                let stroke = ui.visuals().window_stroke;
                ui.painter().hline(visible.x_range(), visible.top(), stroke);
            });
        self.apply_content_requests(world);
        if dock {
            self.dock_drawer();
        }
    }

    /// Esc (not while typing or renaming), a press outside the drawer, or
    /// an asset dragged out of it (the payload stays for the drop).
    fn drawer_should_close(&self, ctx: &egui::Context) -> bool {
        let (esc, press, pointer) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Escape),
                i.pointer
                    .any_pressed()
                    .then(|| i.pointer.interact_pos())
                    .flatten(),
                i.pointer.latest_pos(),
            )
        });
        let typing = ctx.text_edit_focused();
        let off_drawer = |p: egui::Pos2| self.drawer.rect.is_some_and(|r| !r.contains(p));
        // Popups and dialogs above the drawer (its context menus) are not outside.
        let under = press.and_then(|p| ctx.layer_id_at(p));
        let outside_press = press.is_some_and(|p| {
            off_drawer(p)
                && !self.drawer.button.is_some_and(|b| b.contains(p))
                && under.is_none_or(|l| l.order < egui::Order::Foreground)
        });
        let dragged_out = egui::DragAndDrop::has_payload_of_type::<content::AssetDrag>(ctx)
            && pointer.is_some_and(off_drawer);
        (esc && !typing && self.content.renaming.is_none()) || outside_press || dragged_out
    }
}
