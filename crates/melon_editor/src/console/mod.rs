//! Console: engine `tracing` output captured into a bounded ring buffer, shown
//! in the Console panel (`view`) and written to a plain-text log file.

mod view;

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

pub use view::{ConsoleView, console_ui};

/// Former name of [`ConsoleView`] (the editor's field still uses it).
pub type ConsoleFilter = ConsoleView;

/// Log times count from the first captured event (engine start-up).
static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// One captured log event.
#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    pub level: Level,
    pub target: String,
    pub message: String,
    /// Time since the first captured event.
    pub time: Duration,
    pub module: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl LogLine {
    fn from_event(event: &Event<'_>) -> Self {
        let time = START.elapsed();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        // tracing-log events carry the `log` record's origin as `log.*` fields.
        Self {
            level: *meta.level(),
            target: visitor.target.unwrap_or_else(|| meta.target().to_string()),
            message: visitor.message + &visitor.fields,
            time,
            module: visitor.module.or(meta.module_path().map(str::to_string)),
            file: visitor.file.or(meta.file().map(str::to_string)),
            line: visitor.line.or(meta.line()),
        }
    }
}

/// `HH:MM:SS.mmm LEVEL target: message`, the format of copied rows and of the
/// log file. Continuation lines of a multi-line message are indented.
pub fn format_line(line: &LogLine) -> String {
    format!(
        "{} {:<5} {}: {}",
        format_time(line.time),
        line.level.as_str(),
        line.target,
        line.message.replace('\n', "\n    ")
    )
}

/// `HH:MM:SS.mmm`.
fn format_time(time: Duration) -> String {
    let ms = time.as_millis();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1_000 % 60,
        ms % 1_000
    )
}

/// Ring buffer plus the sequence number of the next pushed line, so readers
/// can fetch only what is new.
#[derive(Default)]
struct Buffer {
    lines: VecDeque<LogLine>,
    next: u64,
}

impl Buffer {
    /// Sequence number of the oldest buffered line.
    fn start(&self) -> u64 {
        self.next - self.lines.len() as u64
    }
}

/// Shared, bounded log. Clone it to hand the same buffer to the tracing layer
/// and the console panel.
#[derive(Clone, Default)]
pub struct ConsoleLog(Arc<Mutex<Buffer>>);

impl ConsoleLog {
    /// Oldest lines are dropped beyond this, so a system logging every frame
    /// cannot grow memory without bound.
    pub const CAPACITY: usize = 2_000;

    fn lock(&self) -> std::sync::MutexGuard<'_, Buffer> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, line: LogLine) {
        let mut buf = self.lock();
        if buf.lines.len() == Self::CAPACITY {
            buf.lines.pop_front();
        }
        buf.lines.push_back(line);
        buf.next += 1;
    }

    pub fn snapshot(&self) -> Vec<LogLine> {
        self.lock().lines.iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.lock().lines.clear();
    }

    /// Bring `mirror` up to date: copy the lines pushed since its last sync
    /// and drop the ones the buffer has dropped (or cleared).
    fn sync(&self, mirror: &mut Mirror) {
        let buf = self.lock();
        let start = buf.start();
        if mirror.end() < start {
            // Everything the mirror holds has been dropped.
            mirror.lines.clear();
            mirror.start = start;
        }
        let skip = (mirror.end() - start) as usize;
        mirror.lines.extend(buf.lines.iter().skip(skip).cloned());
        drop(buf);
        while mirror.start < start {
            mirror.lines.pop_front();
            mirror.start += 1;
        }
    }

    /// A `tracing` layer that records every event into this log.
    pub fn layer<S: Subscriber>(&self) -> impl Layer<S> {
        ConsoleLayer(self.clone())
    }
}

/// A reader's copy of the log, addressed by sequence number.
#[derive(Default)]
struct Mirror {
    lines: VecDeque<LogLine>,
    /// Sequence number of `lines[0]`.
    start: u64,
}

impl Mirror {
    fn end(&self) -> u64 {
        self.start + self.lines.len() as u64
    }

    fn get(&self, seq: u64) -> Option<&LogLine> {
        let index = seq.checked_sub(self.start)?;
        self.lines.get(usize::try_from(index).ok()?)
    }
}

/// What the Console captures: INFO and above, minus the Vulkan debug
/// messenger (`wgpu_hal::vulkan::instance`), which forwards driver and loader
/// chatter (e.g. a stale third-party layer entry) at ERROR level. Those lines
/// still reach the terminal.
pub fn capture_filter() -> tracing_subscriber::filter::Targets {
    tracing_subscriber::filter::Targets::new()
        .with_default(Level::INFO)
        .with_target(
            "wgpu_hal::vulkan::instance",
            tracing_subscriber::filter::LevelFilter::OFF,
        )
}

struct ConsoleLayer(ConsoleLog);

impl<S: Subscriber> Layer<S> for ConsoleLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        self.0.push(LogLine::from_event(event));
    }
}

/// A `tracing` layer writing every event to `path` in [`format_line`] format,
/// one write per line so a reader always sees complete lines. Creates the
/// directory and truncates the file.
pub fn file_layer<S: Subscriber>(path: &Path) -> std::io::Result<impl Layer<S>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::File::create(path)?;
    Ok(FileLayer(Mutex::new(std::io::LineWriter::new(file))))
}

struct FileLayer(Mutex<std::io::LineWriter<std::fs::File>>);

impl<S: Subscriber> Layer<S> for FileLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let text = format_line(&LogLine::from_event(event));
        let mut file = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // A failed write must not take the engine down; the Console still has the line.
        let _ = writeln!(file, "{text}");
    }
}

/// Collects the `message` field plus any other fields as ` name=value`, and
/// tracing-log's `log.*` origin fields separately.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
    target: Option<String>,
    module: Option<String>,
    file: Option<String>,
    line: Option<u32>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        let slot = match field.name() {
            "message" => {
                self.message = value.to_string();
                return;
            }
            "log.target" => &mut self.target,
            "log.module_path" => &mut self.module,
            "log.file" => &mut self.file,
            name => {
                let _ = write!(self.fields, " {name}={value}");
                return;
            }
        };
        *slot = Some(value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "log.line" {
            self.line = u32::try_from(value).ok();
        } else {
            self.record_debug(field, &value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    pub(super) fn log_line(level: Level, target: &str, message: &str) -> LogLine {
        LogLine {
            level,
            target: target.into(),
            message: message.into(),
            time: Duration::ZERO,
            module: None,
            file: None,
            line: None,
        }
    }

    fn line(i: usize) -> LogLine {
        log_line(Level::INFO, "test", &format!("line {i}"))
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
        assert!(lines[0].file.as_deref().unwrap().ends_with("mod.rs"));
    }

    #[test]
    fn layer_moves_tracing_log_fields_out_of_the_message() {
        let log = ConsoleLog::default();
        let subscriber = tracing_subscriber::registry().with(log.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                log.target = "wgpu_core::device",
                log.module_path = "wgpu_core::device::resource",
                log.file = "src/device.rs",
                log.line = 42u64,
                extra = 7,
                "buffer dropped"
            )
        });
        let line = &log.snapshot()[0];
        assert_eq!(line.message, "buffer dropped extra=7");
        assert_eq!(line.target, "wgpu_core::device");
        assert_eq!(line.module.as_deref(), Some("wgpu_core::device::resource"));
        assert_eq!(line.file.as_deref(), Some("src/device.rs"));
        assert_eq!(line.line, Some(42));
    }

    #[test]
    fn format_line_is_time_level_target_message() {
        let mut line = log_line(Level::WARN, "naga", "bad shader");
        line.time = Duration::from_millis(3_723_045);
        assert_eq!(format_line(&line), "01:02:03.045 WARN  naga: bad shader");
        line.message = "a\nb".into();
        assert_eq!(format_line(&line), "01:02:03.045 WARN  naga: a\n    b");
    }

    #[test]
    fn sync_copies_new_lines_and_follows_eviction_and_clear() {
        let log = ConsoleLog::default();
        let mut mirror = Mirror::default();
        log.push(line(0));
        log.push(line(1));
        log.sync(&mut mirror);
        assert_eq!((mirror.start, mirror.end()), (0, 2));
        for i in 2..ConsoleLog::CAPACITY + 5 {
            log.push(line(i));
        }
        log.sync(&mut mirror);
        assert_eq!(mirror.lines.len(), ConsoleLog::CAPACITY);
        assert_eq!(mirror.get(5).unwrap().message, "line 5");
        assert!(mirror.get(4).is_none());
        log.clear();
        log.push(line(9_999));
        log.sync(&mut mirror);
        assert_eq!(mirror.lines.len(), 1);
        assert_eq!(mirror.lines[0].message, "line 9999");
    }

    #[test]
    fn file_layer_truncates_and_writes_one_line_per_event() {
        let dir = std::env::temp_dir().join(format!("melon_console_{}", std::process::id()));
        let path = dir.join("logs/editor.log");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "stale\n").unwrap();
        let subscriber = tracing_subscriber::registry().with(file_layer(&path).unwrap());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("first");
            tracing::warn!(log.target = "naga", log.file = "x.rs", "second");
        });
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].ends_with(" INFO  melon_editor::console::tests: first"));
        assert!(lines[1].ends_with(" WARN  naga: second"));
        assert_eq!(lines[0].as_bytes()[8], b'.', "{}", lines[0]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn capture_filter_drops_vulkan_driver_messages() {
        use tracing_subscriber::Layer as _;
        let log = ConsoleLog::default();
        let subscriber =
            tracing_subscriber::registry().with(log.layer().with_filter(capture_filter()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "wgpu_hal::vulkan::instance", "loader_get_json: Failed to open JSON file");
            tracing::info!("scene loaded");
            tracing::debug!("per-frame detail");
        });
        let lines = log.snapshot();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].message, "scene loaded");
    }

    #[test]
    fn clear_empties_the_log() {
        let log = ConsoleLog::default();
        log.push(line(0));
        log.clear();
        assert!(log.snapshot().is_empty());
    }
}
