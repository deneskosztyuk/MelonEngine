//! Editor look, ported from the Slint mock-up's `theme.slint` design tokens.

use egui::{Color32, CornerRadius, Stroke, vec2};

/// Colour tokens.
pub mod colors {
    use egui::Color32;

    pub const CHROME: Color32 = Color32::from_rgb(0x11, 0x13, 0x16);
    pub const CHROME_RAISED: Color32 = Color32::from_rgb(0x18, 0x1b, 0x20);
    pub const SURFACE: Color32 = Color32::from_rgb(0x1c, 0x20, 0x26);
    pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x24, 0x29, 0x32);
    pub const INSPECTOR_CARDS: [Color32; 2] = [
        Color32::from_rgb(0x1a, 0x1d, 0x22),
        Color32::from_rgb(0x23, 0x27, 0x2d),
    ];
    pub const INSPECTOR_ROWS: [Color32; 2] = [
        Color32::from_rgb(0x30, 0x34, 0x3a),
        Color32::from_rgb(0x39, 0x3d, 0x43),
    ];
    pub const VIEWPORT: Color32 = Color32::from_rgb(0x34, 0x3b, 0x46);
    pub const BORDER: Color32 = Color32::from_rgb(0x30, 0x36, 0x40);
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x41, 0x49, 0x56);
    pub const TEXT: Color32 = Color32::from_rgb(0xe1, 0xe5, 0xeb);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x89, 0x91, 0x9d);
    pub const ACCENT: Color32 = Color32::from_rgb(0x2f, 0x79, 0xcf);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x3e, 0x8b, 0xe2);
    pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0x23, 0x47, 0x6f);
    pub const SUCCESS: Color32 = Color32::from_rgb(0x51, 0xb3, 0x7f);
    pub const WARNING: Color32 = Color32::from_rgb(0xd9, 0xa4, 0x41);
    pub const ERROR: Color32 = Color32::from_rgb(0xe0, 0x56, 0x56);
    pub const INFO: Color32 = Color32::from_rgb(0x6c, 0xb2, 0xe6);
    /// Axis labels in the inspector (X red, Y green, Z blue).
    pub const AXIS_X: Color32 = ERROR;
    pub const AXIS_Y: Color32 = SUCCESS;
    pub const AXIS_Z: Color32 = ACCENT_HOVER;
}

/// Size tokens, in points.
pub mod sizes {
    pub const ABOUT_LOGO: f32 = 96.0;
    pub const MENU_HEIGHT: f32 = 28.0;
    pub const TOOL_BUTTON: egui::Vec2 = egui::vec2(32.0, 26.0);
    pub const TOOLBAR_HEIGHT: f32 = 38.0;
    pub const PANEL_HEADER_HEIGHT: f32 = 30.0;
    pub const STATUS_HEIGHT: f32 = 23.0;
    pub const SPACING: f32 = 6.0;
    pub const RADIUS: u8 = 3;
    pub const INPUT_HEIGHT: f32 = 24.0;
    pub const ROW_HEIGHT: f32 = 26.0;
    pub const LEFT_PANE: f32 = 220.0;
    pub const RIGHT_PANE: f32 = 260.0;
    pub const BOTTOM_PANE: f32 = 200.0;
    pub const SETTINGS_WINDOW: egui::Vec2 = egui::vec2(720.0, 420.0);
    pub const SETTINGS_SECTIONS: f32 = 140.0;
    pub const SETTINGS_NAME: f32 = 140.0;
    /// Content Browser folder tree width (default).
    pub const CONTENT_TREE: f32 = 200.0;
    /// Content Browser tile sizes; below `TILE_LIST_BELOW` the view is a list.
    pub const TILE_MIN: f32 = 32.0;
    pub const TILE_DEFAULT: f32 = 72.0;
    pub const TILE_MAX: f32 = 128.0;
    pub const TILE_LIST_BELOW: f32 = 48.0;
}

/// Apply the editor theme (dark) to every egui style.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let radius = CornerRadius::same(sizes::RADIUS);
        style.spacing.item_spacing = vec2(sizes::SPACING, sizes::SPACING);
        style.spacing.button_padding = vec2(8.0, 3.0);
        style.spacing.interact_size.y = sizes::INPUT_HEIGHT;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(colors::TEXT);
        v.weak_text_color = Some(colors::TEXT_MUTED);
        v.panel_fill = colors::SURFACE;
        v.window_fill = colors::SURFACE_RAISED;
        v.window_stroke = Stroke::new(1.0, colors::BORDER_STRONG);
        v.window_corner_radius = radius;
        v.menu_corner_radius = radius;
        v.extreme_bg_color = colors::CHROME;
        v.faint_bg_color = colors::CHROME_RAISED;
        v.code_bg_color = colors::CHROME_RAISED;
        v.hyperlink_color = colors::INFO;
        v.warn_fg_color = colors::WARNING;
        v.error_fg_color = colors::ERROR;
        v.selection.bg_fill = colors::ACCENT_SOFT;
        v.selection.stroke = Stroke::new(1.0, colors::ACCENT);

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = colors::SURFACE;
        w.noninteractive.weak_bg_fill = colors::SURFACE;
        w.noninteractive.bg_stroke = Stroke::new(1.0, colors::BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, colors::TEXT);
        for (state, fill) in [
            (&mut w.inactive, colors::SURFACE_RAISED),
            (&mut w.hovered, colors::BORDER_STRONG),
            (&mut w.active, colors::ACCENT),
            (&mut w.open, colors::SURFACE_RAISED),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.corner_radius = radius;
            state.fg_stroke = Stroke::new(1.0, colors::TEXT);
        }
        w.inactive.bg_stroke = Stroke::NONE;
        w.hovered.bg_stroke = Stroke::new(1.0, colors::ACCENT_HOVER);
        w.active.bg_stroke = Stroke::new(1.0, colors::ACCENT_HOVER);
        w.open.bg_stroke = Stroke::new(1.0, colors::BORDER_STRONG);
    });
}

/// Colour for a log/status level: error, warning, info, success.
pub fn level_color(level: tracing::Level) -> Color32 {
    match level {
        tracing::Level::ERROR => colors::ERROR,
        tracing::Level::WARN => colors::WARNING,
        tracing::Level::INFO => colors::TEXT,
        _ => colors::TEXT_MUTED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_theme_slint() {
        assert_eq!(colors::CHROME, egui::Color32::from_rgb(0x11, 0x13, 0x16));
        assert_eq!(colors::ACCENT, egui::Color32::from_rgb(0x2f, 0x79, 0xcf));
        assert_eq!(colors::ERROR, egui::Color32::from_rgb(0xe0, 0x56, 0x56));
        assert_eq!(sizes::MENU_HEIGHT, 28.0);
        assert_eq!(sizes::STATUS_HEIGHT, 23.0);
    }

    #[test]
    fn apply_sets_panel_fill_and_accent() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let style = ctx.global_style();
        assert_eq!(style.visuals.panel_fill, colors::SURFACE);
        assert_eq!(style.visuals.selection.bg_fill, colors::ACCENT_SOFT);
        assert_eq!(style.spacing.item_spacing, egui::vec2(6.0, 6.0));
    }
}
