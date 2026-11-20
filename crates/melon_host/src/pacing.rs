//! Frame pacing: raw device events only while the game has input, the frame
//! cap used with a non-blocking present mode, and the `MELON_FRAME_STATS`
//! line.

use std::time::{Duration, Instant};

use winit::event_loop::DeviceEvents;

/// Raw device events (mouse motion) feed only the game's mouse look. While the
/// editor has input they are thousands of wasted wake-ups per second.
pub fn device_events(game_input: bool) -> DeviceEvents {
    if game_input {
        DeviceEvents::WhenFocused
    } else {
        DeviceEvents::Never
    }
}

/// Frame period at a monitor's refresh rate, 60 Hz when it is unknown.
pub fn frame_period(refresh_millihertz: Option<u32>) -> Duration {
    let millihertz = refresh_millihertz.filter(|&m| m > 0).unwrap_or(60_000);
    Duration::from_secs_f64(1000.0 / f64::from(millihertz))
}

/// When the next frame may start. Each deadline is the previous one plus the
/// period, so wake-up jitter does not drift the frame rate.
pub struct FrameDeadline {
    period: Duration,
    next: Instant,
}

impl FrameDeadline {
    pub fn new(period: Duration, now: Instant) -> Self {
        Self { period, next: now }
    }

    pub fn next(&self) -> Instant {
        self.next
    }

    /// A frame starts at `now`: move to the following deadline. More than a
    /// period late (a hitch), the next frame may start at once and the grid
    /// restarts there, instead of a burst of frames catching up.
    pub fn advance(&mut self, now: Instant) {
        self.next += self.period;
        if self.next < now {
            self.next = now;
        }
    }
}

/// Host frame stages, in frame order.
#[derive(Clone, Copy)]
pub enum Stage {
    /// `core.update`.
    Update,
    /// Surface acquire (`begin_frame`; the vsync wait with FIFO).
    Acquire,
    /// The editor UI (egui).
    Editor,
    /// `render_scene` and the UI pass, recording only.
    Render,
    /// `end_frame`: submit, poll and present.
    Submit,
}

const STAGE_NAMES: [&str; 5] = ["update", "acquire", "editor", "render", "submit"];

/// Seconds between `MELON_FRAME_STATS` lines.
const REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// One report window of frame timings. Off (`MELON_FRAME_STATS` unset) every
/// method returns at once: no clock reads, no output.
#[derive(Default)]
pub struct FrameStats {
    on: bool,
    window_start: Option<Instant>,
    frame_start: Option<Instant>,
    mark: Option<Instant>,
    /// Stage times of the frame in flight, in seconds.
    stages: [f32; 5],
    frames: u32,
    total: f32,
    worst: f32,
    worst_stages: [f32; 5],
    window_events: u32,
    device_events: u32,
}

impl FrameStats {
    pub fn from_env() -> Self {
        Self {
            on: std::env::var("MELON_FRAME_STATS").is_ok_and(|v| v == "1"),
            ..Self::default()
        }
    }

    pub fn window_event(&mut self) {
        self.window_events += u32::from(self.on);
    }

    pub fn device_event(&mut self) {
        self.device_events += u32::from(self.on);
    }

    /// A frame starts: the previous frame's interval ends here.
    pub fn frame_start(&mut self) {
        if !self.on {
            return;
        }
        let now = Instant::now();
        if let Some(line) = self.frame_start_at(now) {
            eprintln!("{line}");
        }
    }

    /// The stage that ran since the last mark ends now.
    pub fn lap(&mut self, stage: Stage) {
        if !self.on {
            return;
        }
        let now = Instant::now();
        if let Some(mark) = self.mark {
            self.stages[stage as usize] += (now - mark).as_secs_f32();
        }
        self.mark = Some(now);
    }

    /// Close the previous frame's interval at `now`; returns the report line
    /// when the window is over.
    fn frame_start_at(&mut self, now: Instant) -> Option<String> {
        if let Some(start) = self.frame_start {
            let interval = (now - start).as_secs_f32();
            self.frames += 1;
            self.total += interval;
            if interval > self.worst {
                self.worst = interval;
                self.worst_stages = self.stages;
            }
        }
        self.frame_start = Some(now);
        self.mark = Some(now);
        self.stages = [0.0; 5];
        let window_start = *self.window_start.get_or_insert(now);
        if now - window_start < REPORT_INTERVAL || self.frames == 0 {
            return None;
        }
        let line = self.report((now - window_start).as_secs_f32());
        *self = Self {
            on: self.on,
            window_start: Some(now),
            frame_start: Some(now),
            mark: Some(now),
            ..Self::default()
        };
        Some(line)
    }

    /// The window's line: FPS, average and worst interval, the worst frame's
    /// stages (`other` is the rest of its interval: events and waiting), and
    /// events per frame.
    fn report(&self, seconds: f32) -> String {
        let frames = self.frames as f32;
        let ms = |s: f32| s * 1000.0;
        let stages: Vec<String> = STAGE_NAMES
            .iter()
            .zip(self.worst_stages)
            .map(|(name, s)| format!("{name} {:.2}", ms(s)))
            .collect();
        let other = self.worst - self.worst_stages.iter().sum::<f32>();
        format!(
            "frame stats: {:.1} fps, avg {:.2} ms, worst {:.2} ms ({}, other {:.2}), events/frame: window {:.1}, device {:.1}",
            frames / seconds,
            ms(self.total / frames),
            ms(self.worst),
            stages.join(", "),
            ms(other),
            self.window_events as f32 / frames,
            self.device_events as f32 / frames,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn device_events_only_while_the_game_has_input() {
        assert_eq!(device_events(true), DeviceEvents::WhenFocused);
        assert_eq!(device_events(false), DeviceEvents::Never);
    }

    #[test]
    fn frame_period_follows_the_refresh_rate() {
        let period = frame_period(Some(120_000));
        assert!((period.as_secs_f64() - 1.0 / 120.0).abs() < 1e-9);
        let fallback = frame_period(None);
        assert!((fallback.as_secs_f64() - 1.0 / 60.0).abs() < 1e-9);
        assert_eq!(frame_period(Some(0)), fallback, "0 mHz is unknown");
    }

    #[test]
    fn deadlines_do_not_drift_with_late_wake_ups() {
        let t0 = Instant::now();
        let period = 8 * MS;
        let mut deadline = FrameDeadline::new(period, t0);
        assert_eq!(deadline.next(), t0, "the first frame starts at once");
        // Every wake-up is 0.7 ms late; the grid stays on t0 + k * period.
        for k in 0..1000u32 {
            let now = deadline.next() + Duration::from_micros(700);
            deadline.advance(now);
            assert_eq!(deadline.next(), t0 + period * (k + 1));
        }
    }

    #[test]
    fn late_frames_catch_up_once_then_restart_the_grid() {
        let t0 = Instant::now();
        let period = 8 * MS;
        let mut deadline = FrameDeadline::new(period, t0);
        // A 30 ms hitch: the next frame may start at once, no burst after it.
        let late = t0 + 30 * MS;
        deadline.advance(late);
        assert_eq!(deadline.next(), late);
        deadline.advance(late + MS);
        assert_eq!(deadline.next(), late + period);
    }

    #[test]
    fn stats_off_counts_nothing() {
        let mut stats = FrameStats::default();
        stats.window_event();
        stats.device_event();
        stats.frame_start();
        stats.lap(Stage::Update);
        assert_eq!((stats.window_events, stats.device_events), (0, 0));
        assert!(stats.frame_start.is_none() && stats.mark.is_none());
    }

    #[test]
    fn stats_report_every_two_seconds_with_the_worst_frame() {
        let t0 = Instant::now();
        let mut stats = FrameStats {
            on: true,
            ..FrameStats::default()
        };
        assert!(stats.frame_start_at(t0).is_none());
        // 199 frames of 10 ms, then one of 20 ms whose acquire took 15 ms.
        let mut now = t0;
        for _ in 0..199 {
            stats.window_event();
            stats.device_event();
            stats.device_event();
            now += 10 * MS;
            assert!(stats.frame_start_at(now).is_none());
        }
        stats.stages[Stage::Acquire as usize] = 0.015;
        stats.stages[Stage::Submit as usize] = 0.001;
        now += 20 * MS;
        let line = stats.frame_start_at(now).expect("2 s are up");
        assert_eq!(
            line,
            "frame stats: 99.5 fps, avg 10.05 ms, worst 20.00 ms (update 0.00, acquire 15.00, \
             editor 0.00, render 0.00, submit 1.00, other 4.00), events/frame: window 1.0, device 2.0"
        );
        assert_eq!(stats.frames, 0, "a new window starts");
        assert_eq!(stats.window_start, Some(now));
        assert!(stats.on);
    }
}
