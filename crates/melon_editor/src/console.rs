//! Console panel: engine `tracing` output captured into a bounded ring buffer.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::theme;

/// One captured log event.
#[derive(Clone, Debug)]
pub struct LogLine {
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// Shared, bounded log. Clone it to hand the same buffer to the tracing layer
/// and the console panel.
#[derive(Clone, Default)]
pub struct ConsoleLog(Arc<Mutex<VecDeque<LogLine>>>);

impl ConsoleLog {
    /// Oldest lines are dropped beyond this, so a system logging every frame
    /// cannot grow memory without bound.
    pub const CAPACITY: usize = 2_000;

    pub fn push(&self, line: LogLine) {
        let mut lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == Self::CAPACITY {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<LogLine> {
        let lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        lines.iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// A `tracing` layer that records every event into this log.
    pub fn layer<S: Subscriber>(&self) -> impl Layer<S> {
        ConsoleLayer(self.clone())
    }
}

struct ConsoleLayer(ConsoleLog);

impl<S: Subscriber> Layer<S> for ConsoleLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        self.0.push(LogLine {
            level: *meta.level(),
            target: meta.target().to_string(),
            message: visitor.message + &visitor.fields,
        });
    }
}

/// Collects the `message` field plus any other fields as ` name=value`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// Which levels the console panel shows. The host captures INFO and above
/// only (DEBUG would flood the buffer from per-frame spans), so there is no
/// Debug toggle.
#[derive(Clone, Copy, Debug)]
pub struct ConsoleFilter {
    pub error: bool,
    pub warn: bool,
    pub info: bool,
}

impl Default for ConsoleFilter {
    fn default() -> Self {
        Self {
            error: true,
            warn: true,
            info: true,
        }
    }
}

impl ConsoleFilter {
    pub fn shows(&self, level: Level) -> bool {
        match level {
            Level::ERROR => self.error,
            Level::WARN => self.warn,
            Level::INFO => self.info,
            _ => false,
        }
    }
}

/// Console panel: Clear, level toggles, and the log, scrolled to the newest line.
pub fn console_ui(ui: &mut egui::Ui, log: &ConsoleLog, filter: &mut ConsoleFilter) {
    let lines = log.snapshot();
    ui.horizontal(|ui| {
        if ui.button("Clear").clicked() {
            log.clear();
        }
        ui.separator();
        ui.toggle_value(&mut filter.error, "Errors");
        ui.toggle_value(&mut filter.warn, "Warnings");
        ui.toggle_value(&mut filter.info, "Info");
        ui.separator();
        ui.weak(format!("{} lines", lines.len()));
    });
    ui.separator();
    // Lay out only the visible rows: the log can hold thousands of lines.
    let shown: Vec<&LogLine> = lines.iter().filter(|l| filter.shows(l.level)).collect();
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show_rows(ui, row_height, shown.len(), |ui, rows| {
            for line in &shown[rows] {
                ui.label(
                    egui::RichText::new(format!(
                        "[{:<5}] {}: {}",
                        line.level, line.target, line.message
                    ))
                    .monospace()
                    .color(theme::level_color(line.level)),
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    fn line(i: usize) -> LogLine {
        LogLine {
            level: tracing::Level::INFO,
            target: "test".into(),
            message: format!("line {i}"),
        }
    }

    #[test]
    fn ring_buffer_caps_at_capacity() {
        let log = ConsoleLog::default();
        for i in 0..2_500 {
            log.push(line(i));
        }
        let lines = log.snapshot();
        assert_eq!(lines.len(), ConsoleLog::CAPACITY);
        assert_eq!(lines[0].message, "line 500");
        assert_eq!(lines.last().unwrap().message, "line 2499");
    }

    #[test]
    fn layer_captures_message_and_level() {
        let log = ConsoleLog::default();
        let subscriber = tracing_subscriber::registry().with(log.layer());
        tracing::subscriber::with_default(subscriber, || tracing::warn!("hello {}", 1));
        let lines = log.snapshot();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].level, tracing::Level::WARN);
        assert_eq!(lines[0].message, "hello 1");
    }

    #[test]
    fn clear_empties_the_log() {
        let log = ConsoleLog::default();
        log.push(line(0));
        log.clear();
        assert!(log.snapshot().is_empty());
    }
}
