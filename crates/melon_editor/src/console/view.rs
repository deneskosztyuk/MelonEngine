//! Console panel: filtered, collapsed, selectable rows over a [`ConsoleLog`].
//!
//! Each frame copies only the lines pushed since the last frame into a
//! [`Mirror`] and appends them to the cached rows; the rows are rebuilt only
//! when the level toggles, search text or Collapse change.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Key, Sense, TextStyle};
use tracing::Level;

use super::{ConsoleLog, LogLine, Mirror, format_line, format_time};
use crate::theme::{self, colors, sizes};

/// One shown row: a line, or a run of identical consecutive lines (Collapse).
/// `first` identifies the row; `last` is the line drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    first: u64,
    last: u64,
    count: usize,
}

/// What the rows were built for; a change rebuilds them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RowKey {
    levels: [bool; 3],
    search: String,
    collapse: bool,
}

/// Clicked rows, by `Row::first`. `anchor` is the last plain click: the
/// start of a Shift+click range and the row the details strip shows.
#[derive(Default, Debug)]
struct Selection {
    anchor: Option<u64>,
    picked: BTreeSet<u64>,
}

impl Selection {
    /// Click on `rows[index]`: select it alone, or with `shift` the range
    /// from the anchor to it.
    fn click(&mut self, rows: &[Row], index: usize, shift: bool) {
        let anchor = self
            .anchor
            .filter(|_| shift)
            .and_then(|a| rows.binary_search_by_key(&a, |r| r.first).ok());
        self.picked.clear();
        match anchor {
            Some(a) => {
                let range = a.min(index)..=a.max(index);
                self.picked.extend(rows[range].iter().map(|r| r.first));
            }
            None => {
                self.anchor = Some(rows[index].first);
                self.picked.insert(rows[index].first);
            }
        }
    }

    fn select_all(&mut self, rows: &[Row]) {
        self.picked = rows.iter().map(|r| r.first).collect();
    }

    fn contains(&self, row: &Row) -> bool {
        self.picked.contains(&row.first)
    }
}

/// Console panel state: filters, toggles, cached rows, selection, scrolling.
pub struct ConsoleView {
    /// Level toggles: errors, warnings, info. The host captures INFO and
    /// above only, so there is no Debug toggle.
    levels: [bool; 3],
    search: String,
    collapse: bool,
    wrap: bool,
    mirror: Mirror,
    rows: Vec<Row>,
    /// Next sequence number to append to `rows`.
    scanned: u64,
    built_for: Option<RowKey>,
    selection: Selection,
    /// Lines added while scrolled up (the "Jump to latest" count).
    unseen: usize,
}

impl Default for ConsoleView {
    fn default() -> Self {
        Self {
            levels: [true; 3],
            search: String::new(),
            collapse: true,
            wrap: false,
            mirror: Mirror::default(),
            rows: Vec::new(),
            scanned: 0,
            built_for: None,
            selection: Selection::default(),
            unseen: 0,
        }
    }
}

impl ConsoleView {
    /// Sync with `log` and update the rows. Returns how many shown lines
    /// were added.
    fn refresh(&mut self, log: &ConsoleLog) -> usize {
        log.sync(&mut self.mirror);
        let key = RowKey {
            levels: self.levels,
            search: self.search.clone(),
            collapse: self.collapse,
        };
        if self.built_for.as_ref() != Some(&key) {
            self.rows.clear();
            self.scanned = 0;
            self.built_for = Some(key);
        }
        let start = self.mirror.start;
        // Rows are in sequence order; drop those whose lines are all gone.
        let gone = self.rows.partition_point(|r| r.last < start);
        self.rows.drain(..gone);

        let needle = self.search.to_lowercase();
        let mut added = 0;
        for seq in self.scanned.max(start)..self.mirror.end() {
            let Some(line) = self.mirror.get(seq) else {
                continue;
            };
            let level_on = match line.level {
                Level::ERROR => self.levels[0],
                Level::WARN => self.levels[1],
                Level::INFO => self.levels[2],
                _ => false,
            };
            let matches = needle.is_empty()
                || line.message.to_lowercase().contains(&needle)
                || line.target.to_lowercase().contains(&needle);
            if !level_on || !matches {
                continue;
            }
            added += 1;
            let repeat = self.collapse
                && self.rows.last().is_some_and(|row| {
                    self.mirror.get(row.last).is_some_and(|prev| {
                        prev.level == line.level
                            && prev.target == line.target
                            && prev.message == line.message
                    })
                });
            match self.rows.last_mut() {
                Some(row) if repeat => {
                    row.last = seq;
                    row.count += 1;
                }
                _ => self.rows.push(Row {
                    first: seq,
                    last: seq,
                    count: 1,
                }),
            }
        }
        self.scanned = self.mirror.end();
        added
    }

    /// Shown lines (repeats included).
    fn shown_lines(&self) -> usize {
        self.rows.iter().map(|r| r.count).sum()
    }

    /// `rows` as copied text, one [`format_line`] per row, ` (×N)` on repeats.
    fn copy_text<'a>(&self, rows: impl Iterator<Item = &'a Row>) -> String {
        let mut text = String::new();
        for row in rows {
            let Some(line) = self.mirror.get(row.last) else {
                continue;
            };
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format_line(line));
            if row.count > 1 {
                let _ = write!(text, " (×{})", row.count);
            }
        }
        text
    }

    fn anchor_row(&self) -> Option<Row> {
        let anchor = self.selection.anchor?;
        let i = self.rows.binary_search_by_key(&anchor, |r| r.first).ok()?;
        Some(self.rows[i])
    }
}

/// `crate::a::b::c` as `crate::c`; one or two segments stay.
fn short_target(target: &str) -> String {
    let parts: Vec<&str> = target.split("::").collect();
    match parts.as_slice() {
        [first, .., last] if parts.len() > 2 => format!("{first}::{last}"),
        _ => target.to_string(),
    }
}

/// Console panel: toolbar, rows, and the selected entry's details.
///
/// Keys while the pointer is over the panel: Ctrl+C copies the selection,
/// Ctrl+A selects all shown rows, Home / End scroll to the top / bottom.
pub fn console_ui(ui: &mut egui::Ui, log: &ConsoleLog, view: &mut ConsoleView) {
    let added = view.refresh(log);
    let mut copy = None;

    let hovered = ui.rect_contains_pointer(ui.max_rect());
    let typing = ui.ctx().egui_wants_keyboard_input();
    let (copy_key, select_all, home, end) = if hovered && !typing {
        ui.input(|i| {
            let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy))
                || (i.modifiers.command && i.key_pressed(Key::C));
            (
                copy,
                i.modifiers.command && i.key_pressed(Key::A),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
            )
        })
    } else {
        (false, false, false, false)
    };
    if select_all {
        view.selection.select_all(&view.rows);
    }
    if copy_key && !view.selection.picked.is_empty() {
        copy = Some(view.copy_text(view.rows.iter().filter(|r| view.selection.contains(r))));
    }

    ui.horizontal(|ui| {
        if ui.button("Clear").clicked() {
            log.clear();
            view.selection = Selection::default();
        }
        ui.separator();
        for (on, label) in view.levels.iter_mut().zip(["Errors", "Warnings", "Info"]) {
            ui.toggle_value(on, label);
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut view.search)
                .hint_text("Search")
                .desired_width(sizes::CONSOLE_SEARCH),
        );
        ui.toggle_value(&mut view.collapse, "Collapse")
            .on_hover_text("Merge identical consecutive lines into one row with a count");
        ui.toggle_value(&mut view.wrap, "Wrap")
            .on_hover_text("Show full messages instead of one line per row");
        ui.separator();
        let picked = !view.selection.picked.is_empty();
        if ui
            .add_enabled(picked, egui::Button::new("Copy"))
            .on_hover_text("Copy the selected rows (Ctrl+C)")
            .clicked()
        {
            copy = Some(view.copy_text(view.rows.iter().filter(|r| view.selection.contains(r))));
        }
        if ui
            .button("Copy all")
            .on_hover_text("Copy every shown row")
            .clicked()
        {
            copy = Some(view.copy_text(view.rows.iter()));
        }
        ui.weak(format!(
            "{} of {} lines",
            view.shown_lines(),
            view.mirror.lines.len()
        ));
    });
    if let Some(text) = copy {
        ui.ctx().copy_text(text);
    }
    ui.separator();

    if let Some(row) = view.anchor_row()
        && let Some(line) = view.mirror.get(row.last)
    {
        egui::Panel::bottom("console_details")
            .resizable(true)
            .default_size(sizes::CONSOLE_DETAILS)
            .show(ui, |ui| {
                details_ui(ui, line, row, view.mirror.get(row.first))
            });
    }

    let row_height = ui.text_style_height(&TextStyle::Monospace) + sizes::CONSOLE_ROW_PAD;
    let mut clicked = None;
    let output = ui
        .scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut area = egui::ScrollArea::vertical()
                .id_salt("console_rows")
                .auto_shrink(false)
                .stick_to_bottom(true);
            if home {
                area = area.vertical_scroll_offset(0.0);
            } else if end || ui.ctx().data(|d| d.get_temp::<bool>(jump_id(ui))).is_some() {
                ui.ctx().data_mut(|d| d.remove::<bool>(jump_id(ui)));
                area = area.vertical_scroll_offset(f32::MAX);
            }
            let mut draw = |ui: &mut egui::Ui, range: std::ops::Range<usize>| {
                for i in range {
                    let row = view.rows[i];
                    let Some(line) = view.mirror.get(row.last) else {
                        continue;
                    };
                    let selected = view.selection.contains(&row);
                    if row_ui(
                        ui, row.first, line, row.count, selected, view.wrap, row_height,
                    )
                    .clicked()
                    {
                        clicked = Some(i);
                    }
                }
            };
            let n = view.rows.len();
            // shortcut: Wrap lays out every row (rows differ in height); fine
            // at the 2 000-line buffer cap, needs per-row heights beyond that.
            if view.wrap {
                area.show(ui, |ui| draw(ui, 0..n))
            } else {
                area.show_rows(ui, row_height, n, draw)
            }
        })
        .inner;

    if let Some(i) = clicked {
        let shift = ui.input(|i| i.modifiers.shift);
        view.selection.click(&view.rows, i, shift);
    }

    let at_bottom =
        output.state.offset.y + output.inner_rect.height() >= output.content_size.y - row_height;
    if at_bottom {
        view.unseen = 0;
    } else {
        view.unseen += added;
    }
    if view.unseen > 0 {
        let size = egui::vec2(output.inner_rect.width().min(200.0), sizes::INPUT_HEIGHT);
        let rect = egui::Rect::from_center_size(
            output.inner_rect.center_bottom() - egui::vec2(0.0, size.y),
            size,
        );
        let label = format!("{} new lines ↓ Jump to latest", view.unseen);
        if ui.put(rect, egui::Button::new(label)).clicked() {
            ui.ctx().data_mut(|d| d.insert_temp(jump_id(ui), true));
        }
    }
}

/// Memory flag asking the next frame to scroll to the bottom.
fn jump_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("console_jump")
}

/// Row widget id (one Console panel per editor).
fn row_id(first: u64) -> egui::Id {
    egui::Id::new(("console_row", first))
}

/// One row: `time LEVEL ×N target: message`, truncated with `…` unless `wrap`.
fn row_ui(
    ui: &mut egui::Ui,
    seq: u64,
    line: &LogLine,
    count: usize,
    selected: bool,
    wrap: bool,
    row_height: f32,
) -> egui::Response {
    let font = TextStyle::Monospace.resolve(ui.style());
    let width = ui.available_width();
    let level_color = theme::level_color(line.level);
    let mut job = LayoutJob::default();
    let mut add = |text: &str, color| {
        job.append(text, 0.0, TextFormat::simple(font.clone(), color));
    };
    add(&format_time(line.time), colors::TEXT_MUTED);
    add(&format!(" {:<5} ", level_name(line.level)), level_color);
    if count > 1 {
        add(&format!("×{count} "), colors::ACCENT_HOVER);
    }
    add(
        &format!("{}: ", short_target(&line.target)),
        colors::TEXT_MUTED,
    );
    add(&line.message, level_color);
    job.wrap = TextWrapping {
        max_width: width - 2.0 * sizes::CONSOLE_ROW_INSET,
        max_rows: if wrap { usize::MAX } else { 1 },
        break_anywhere: !wrap,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let height = (galley.size().y + sizes::CONSOLE_ROW_PAD).max(row_height);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let response = ui.interact(rect, row_id(seq), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 0.0, colors::ACCENT_SOFT);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, colors::SURFACE_RAISED);
    }
    let pos = rect.min + egui::vec2(sizes::CONSOLE_ROW_INSET, sizes::CONSOLE_ROW_PAD / 2.0);
    ui.painter().galley(pos, galley, colors::TEXT);
    response
}

/// Details strip: full message (selectable), target, origin, repeat count.
fn details_ui(ui: &mut egui::Ui, line: &LogLine, row: Row, first: Option<&LogLine>) {
    egui::ScrollArea::vertical()
        .id_salt("console_details_scroll")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(theme::level_color(line.level), level_name(line.level));
                ui.weak(format_time(line.time));
                ui.label(&line.target);
                if let (Some(file), Some(n)) = (&line.file, line.line) {
                    ui.weak(format!("{file}:{n}"));
                }
                if let Some(module) = &line.module
                    && module != &line.target
                {
                    ui.weak(module);
                }
                if row.count > 1 {
                    let since = first.map_or(String::new(), |f| {
                        format!(", first at {}", format_time(f.time))
                    });
                    ui.weak(format!("×{}{since}", row.count));
                }
            });
            ui.add(
                egui::Label::new(egui::RichText::new(&line.message).monospace()).selectable(true),
            );
        });
}

fn level_name(level: Level) -> &'static str {
    level.as_str()
}

#[cfg(test)]
mod tests {
    use super::super::tests::log_line;
    use super::*;

    fn push(log: &ConsoleLog, level: Level, target: &str, message: &str) {
        log.push(log_line(level, target, message));
    }

    fn rows(view: &ConsoleView) -> Vec<(u64, usize)> {
        view.rows.iter().map(|r| (r.first, r.count)).collect()
    }

    #[test]
    fn collapse_merges_consecutive_repeats_and_keeps_counting() {
        let log = ConsoleLog::default();
        let mut view = ConsoleView::default();
        for _ in 0..3 {
            push(&log, Level::WARN, "a", "same");
        }
        push(&log, Level::WARN, "b", "same");
        push(&log, Level::INFO, "a", "same");
        assert_eq!(view.refresh(&log), 5);
        assert_eq!(rows(&view), [(0, 3), (3, 1), (4, 1)]);
        // A later repeat of the last line extends its row across frames.
        push(&log, Level::INFO, "a", "same");
        assert_eq!(view.refresh(&log), 1);
        assert_eq!(rows(&view), [(0, 3), (3, 1), (4, 2)]);
        assert_eq!(view.shown_lines(), 6);
        view.collapse = false;
        view.refresh(&log);
        assert_eq!(view.rows.len(), 6);
    }

    #[test]
    fn collapsed_row_keeps_its_count_when_its_first_line_is_evicted() {
        let log = ConsoleLog::default();
        let mut view = ConsoleView::default();
        for _ in 0..ConsoleLog::CAPACITY {
            push(&log, Level::WARN, "a", "flood");
        }
        view.refresh(&log);
        for _ in 0..10 {
            push(&log, Level::WARN, "a", "flood");
        }
        view.refresh(&log);
        assert_eq!(rows(&view), [(0, ConsoleLog::CAPACITY + 10)]);
        push(&log, Level::INFO, "a", "other");
        for _ in 0..ConsoleLog::CAPACITY {
            push(&log, Level::INFO, "a", "x");
        }
        view.refresh(&log);
        assert_eq!(view.rows.len(), 1, "flood row evicted: {:?}", rows(&view));
    }

    #[test]
    fn search_and_level_toggles_combine() {
        let log = ConsoleLog::default();
        let mut view = ConsoleView::default();
        push(&log, Level::ERROR, "render", "Shader failed");
        push(&log, Level::WARN, "render", "shader slow");
        push(&log, Level::INFO, "physics::step", "ok");
        push(&log, Level::INFO, "shaders", "loaded");
        view.search = "SHADER".into();
        view.refresh(&log);
        assert_eq!(rows(&view), [(0, 1), (1, 1), (3, 1)]);
        view.levels = [true, false, true];
        view.refresh(&log);
        assert_eq!(rows(&view), [(0, 1), (3, 1)]);
        view.search = "physics".into();
        view.refresh(&log);
        assert_eq!(rows(&view), [(2, 1)]);
    }

    #[test]
    fn clear_drops_all_rows() {
        let log = ConsoleLog::default();
        let mut view = ConsoleView::default();
        push(&log, Level::INFO, "a", "x");
        view.refresh(&log);
        log.clear();
        view.refresh(&log);
        assert!(view.rows.is_empty());
    }

    fn sample_rows() -> Vec<Row> {
        [0, 3, 4, 9]
            .into_iter()
            .map(|s| Row {
                first: s,
                last: s,
                count: 1,
            })
            .collect()
    }

    fn picked(sel: &Selection) -> Vec<u64> {
        sel.picked.iter().copied().collect()
    }

    #[test]
    fn click_selects_one_shift_click_a_range_either_way() {
        let rows = sample_rows();
        let mut sel = Selection::default();
        sel.click(&rows, 1, false);
        assert_eq!(picked(&sel), [3]);
        sel.click(&rows, 3, true);
        assert_eq!(picked(&sel), [3, 4, 9]);
        sel.click(&rows, 0, true);
        assert_eq!(picked(&sel), [0, 3]);
        assert_eq!(sel.anchor, Some(3));
        sel.click(&rows, 2, false);
        assert_eq!(picked(&sel), [4]);
        sel.select_all(&rows);
        assert_eq!(picked(&sel), [0, 3, 4, 9]);
    }

    #[test]
    fn shift_click_without_a_visible_anchor_selects_one() {
        let rows = sample_rows();
        let mut sel = Selection {
            anchor: Some(7),
            ..Default::default()
        };
        sel.click(&rows, 2, true);
        assert_eq!(picked(&sel), [4]);
    }

    #[test]
    fn copy_text_is_one_formatted_line_per_row_with_repeat_count() {
        let log = ConsoleLog::default();
        let mut view = ConsoleView::default();
        push(&log, Level::WARN, "naga", "bad");
        push(&log, Level::WARN, "naga", "bad");
        push(&log, Level::INFO, "melon", "ok");
        view.refresh(&log);
        assert_eq!(
            view.copy_text(view.rows.iter()),
            "00:00:00.000 WARN  naga: bad (×2)\n00:00:00.000 INFO  melon: ok"
        );
    }

    #[test]
    fn short_target_keeps_crate_and_last_segment() {
        assert_eq!(
            short_target("wgpu_core::device::resource"),
            "wgpu_core::resource"
        );
        assert_eq!(short_target("melon_editor::scene"), "melon_editor::scene");
        assert_eq!(short_target("naga"), "naga");
    }

    /// One console frame; `modifiers` are held (egui reads them from the raw
    /// input, not from the key events).
    fn frame(
        ctx: &egui::Context,
        log: &ConsoleLog,
        view: &mut ConsoleView,
        modifiers: egui::Modifiers,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 400.0),
            )),
            modifiers,
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |ui| console_ui(ui, log, view))
    }

    fn copied(output: &egui::FullOutput) -> Option<&str> {
        output
            .platform_output
            .commands
            .iter()
            .find_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
    }

    #[test]
    fn shift_click_then_ctrl_c_copies_the_selected_rows() {
        let log = ConsoleLog::default();
        for m in ["one", "two", "three"] {
            push(&log, Level::INFO, "t", m);
        }
        let mut view = ConsoleView::default();
        let ctx = egui::Context::default();
        let none = egui::Modifiers::NONE;
        let click = |seq: u64, modifiers| {
            let pos = ctx
                .read_response(row_id(seq))
                .expect("row was drawn")
                .rect
                .center();
            let press = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            };
            vec![egui::Event::PointerMoved(pos), press(true), press(false)]
        };
        frame(&ctx, &log, &mut view, none, Vec::new());
        frame(&ctx, &log, &mut view, none, click(1, none));
        let shift = egui::Modifiers::SHIFT;
        frame(&ctx, &log, &mut view, shift, click(2, shift));
        let out = frame(&ctx, &log, &mut view, none, vec![egui::Event::Copy]);
        assert_eq!(
            copied(&out),
            Some(
                "00:00:00.000 INFO  t: two
00:00:00.000 INFO  t: three"
            )
        );
    }
}
