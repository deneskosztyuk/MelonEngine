use egui::{
    Button, Painter, Pos2, Rect, Response, Shape, Stroke, Ui, Vec2, WidgetInfo, WidgetType, vec2,
};

use crate::theme::sizes;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum ToolbarIcon {
    Move,
    Rotate,
    Scale,
    Play,
    Pause,
    Step,
    Stop,
}

impl ToolbarIcon {
    pub(super) fn button(self, ui: &mut Ui, enabled: bool, selected: Option<bool>) -> Response {
        let label = match self {
            Self::Move => "Move (W)",
            Self::Rotate => "Rotate (E)",
            Self::Scale => "Scale (R)",
            Self::Play => "Play / resume (F5)",
            Self::Pause => "Pause (F6)",
            Self::Step => "Step forward one fixed update",
            Self::Stop => "Stop (Shift+F5)",
        };
        ui.add_enabled(enabled, |ui: &mut Ui| {
            let active = selected.unwrap_or(false);
            let response = ui.add(
                Button::new("")
                    .min_size(sizes::TOOL_BUTTON)
                    .selected(active),
            );
            let color = ui
                .style()
                .interact_selectable(&response, active)
                .fg_stroke
                .color;
            self.paint(
                ui.painter(),
                response.rect.center(),
                Stroke::new(1.6, color),
            );
            response.widget_info(|| match selected {
                Some(value) => WidgetInfo::selected(
                    WidgetType::SelectableLabel,
                    response.enabled(),
                    value,
                    label,
                ),
                None => WidgetInfo::labeled(WidgetType::Button, response.enabled(), label),
            });
            response
        })
        .on_hover_text(label)
        .on_disabled_hover_text(label)
    }

    fn paint(self, painter: &Painter, center: Pos2, stroke: Stroke) {
        let point = |x, y| center + vec2(x, y);
        let bar = |x, w| {
            painter.rect_filled(
                Rect::from_center_size(point(x, 0.0), vec2(w, 12.0)),
                0.5,
                stroke.color,
            );
        };
        match self {
            Self::Move => {
                for axis in [Vec2::X, -Vec2::X, Vec2::Y, -Vec2::Y] {
                    painter.arrow(center - axis * 6.5, axis * 13.0, stroke);
                }
            }
            Self::Rotate => {
                let arc = (0..=20)
                    .map(|i| {
                        let angle = (-75.0 + 285.0 * i as f32 / 20.0).to_radians();
                        center + Vec2::angled(angle) * 6.0
                    })
                    .collect();
                painter.add(Shape::line(arc, stroke));
                painter.add(Shape::line(
                    vec![point(-6.6, 0.8), point(-5.2, -3.0), point(-1.4, -1.6)],
                    stroke,
                ));
            }
            Self::Scale => {
                painter.line_segment([point(-5.0, 5.0), point(4.5, -4.5)], stroke);
                for (p, size) in [(point(-5.0, 5.0), 4.0), (point(4.5, -4.5), 6.0)] {
                    painter.rect_filled(
                        Rect::from_center_size(p, Vec2::splat(size)),
                        0.5,
                        stroke.color,
                    );
                }
            }
            Self::Play | Self::Step => {
                let left = if self == Self::Step { -6.0 } else { -4.5 };
                painter.add(Shape::convex_polygon(
                    vec![point(left, -6.0), point(left + 10.0, 0.0), point(left, 6.0)],
                    stroke.color,
                    Stroke::NONE,
                ));
                if self == Self::Step {
                    bar(6.0, 2.0);
                }
            }
            Self::Pause => {
                bar(-3.5, 3.5);
                bar(3.5, 3.5);
            }
            Self::Stop => {
                painter.rect_filled(
                    Rect::from_center_size(center, Vec2::splat(12.0)),
                    1.0,
                    stroke.color,
                );
            }
        }
    }
}
