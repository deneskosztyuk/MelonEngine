//! Watches `assets/` for changes made outside the editor (Explorer, a DCC
//! export, git). Events are drained each frame; once they settle the editor
//! reconciles `.meta` files and refreshes the Content Browser.

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use notify::Watcher;

/// How long the file system must stay quiet before the editor reacts. A
/// move in Explorer arrives as several events (the file, then its `.meta`);
/// reacting in between would split them.
pub const QUIET: Duration = Duration::from_millis(300);

/// Fires once after events stop for `QUIET`.
#[derive(Default)]
pub struct Debounce {
    since: Option<Instant>,
}

impl Debounce {
    /// An event arrived: (re)start the wait.
    pub fn note(&mut self, now: Instant) {
        self.since = Some(now);
    }

    /// True once when the wait is over, then idle until the next event.
    pub fn ready(&mut self, now: Instant) -> bool {
        match self.since {
            Some(since) if now.duration_since(since) >= QUIET => {
                self.since = None;
                true
            }
            _ => false,
        }
    }

    pub fn pending(&self) -> bool {
        self.since.is_some()
    }
}

pub struct AssetWatcher {
    _watcher: notify::RecommendedWatcher,
    events: Receiver<notify::Result<notify::Event>>,
    debounce: Debounce,
}

impl AssetWatcher {
    /// Watch `assets` and everything below it.
    pub fn start(assets: &Path) -> anyhow::Result<Self> {
        let (tx, events) = channel();
        let mut watcher = notify::recommended_watcher(tx)?;
        watcher.watch(assets, notify::RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            events,
            debounce: Debounce::default(),
        })
    }

    /// Drain events without blocking; true once they have settled.
    pub fn poll(&mut self, now: Instant) -> bool {
        while let Ok(event) = self.events.try_recv() {
            if let Err(error) = event {
                tracing::warn!("asset watcher: {error}");
            }
            self.debounce.note(now);
        }
        self.debounce.ready(now)
    }

    /// Events arrived and have not settled yet.
    pub fn pending(&self) -> bool {
        self.debounce.pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn debounce_waits_for_quiet() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        assert!(!d.ready(t0));
        d.note(t0);
        assert!(!d.ready(t0 + Duration::from_millis(299)));
        // A second event restarts the wait.
        d.note(t0 + Duration::from_millis(200));
        assert!(!d.ready(t0 + Duration::from_millis(450)));
        assert!(d.ready(t0 + Duration::from_millis(500)));
        assert!(
            !d.ready(t0 + Duration::from_millis(900)),
            "fires once per settle"
        );
    }

    #[test]
    fn watcher_sees_a_new_file() {
        let dir = std::env::temp_dir().join(format!("melon_watch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut watcher = AssetWatcher::start(&dir).unwrap();
        std::fs::write(dir.join("new.glb"), b"x").unwrap();
        let give_up = Instant::now() + Duration::from_secs(5);
        while !watcher.poll(Instant::now()) {
            assert!(Instant::now() < give_up, "no settled event within 5 s");
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
