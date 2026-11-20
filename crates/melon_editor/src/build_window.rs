//! File > Build: package the game. Builds the player (release, no editor)
//! and writes `<output>/<GameName>/` with the executable and `game.pak`,
//! on a background thread; progress lines go to the Console. "Build and
//! Run" starts the packaged game when the build succeeds. Cancel kills the
//! build (cargo and its children); closing the editor does too.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;

use bevy_ecs::world::World;
use melon_core::PlayerSettings;
use melon_core::package::{PackageReport, build_player, package_game};

use crate::StatusKind;

type Status = Option<(StatusKind, String)>;

/// Output folder shown first, relative to the project root.
pub const DEFAULT_OUTPUT: &str = "Builds";

/// Where a project remembers its output folder.
fn output_file(project_root: &Path) -> PathBuf {
    project_root.join(".melon").join("build_output.txt")
}

fn read_output(project_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(output_file(project_root)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn write_output(project_root: &Path, output: &str) {
    let file = output_file(project_root);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&file, output.trim()) {
        tracing::warn!("cannot remember the build output folder: {error}");
    }
}

struct Job {
    done: Receiver<Result<PackageReport, String>>,
    run_after: bool,
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct BuildWindow {
    pub open: bool,
    output: String,
    /// The remembered folder was read for this project.
    loaded: bool,
    job: Option<Job>,
    /// The last successful build, for Open Folder / Run.
    last: Option<PackageReport>,
}

impl Default for BuildWindow {
    fn default() -> Self {
        Self {
            open: false,
            output: DEFAULT_OUTPUT.to_string(),
            loaded: false,
            job: None,
            last: None,
        }
    }
}

impl Drop for BuildWindow {
    /// Closing the editor mid-build ends cargo too (it waits for the thread,
    /// which polls the flag every 100 ms).
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            let _ = job.thread.join();
        }
    }
}

/// `output` as a path: relative ones are under the project root.
fn output_dir(project_root: &Path, output: &str) -> PathBuf {
    project_root.join(output.trim())
}

/// Show `dir` in the file manager.
fn open_folder(dir: &Path) -> std::io::Result<()> {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map(|_| ())
}

fn run_game(report: &PackageReport) -> std::io::Result<()> {
    std::process::Command::new(&report.exe)
        .current_dir(&report.dir)
        .spawn()
        .map(|_| ())
}

impl BuildWindow {
    pub fn building(&self) -> bool {
        self.job.is_some()
    }

    /// Run `work` on a thread; `poll` reports its result. `work` gets the
    /// cancel flag.
    fn start(
        &mut self,
        run_after: bool,
        work: impl FnOnce(&AtomicBool) -> Result<PackageReport, String> + Send + 'static,
    ) {
        let (tx, done) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let thread = std::thread::spawn(move || {
            let _ = tx.send(work(&flag));
        });
        self.job = Some(Job {
            done,
            run_after,
            cancel,
            thread,
        });
    }

    /// The real job: build the player, then package the project.
    fn start_build(
        &mut self,
        run_after: bool,
        project_root: &Path,
        output: &str,
        settings: PlayerSettings,
    ) {
        let root = project_root.to_path_buf();
        let out = output_dir(project_root, output);
        self.start(run_after, move |cancel| {
            tracing::info!("build: building the player (the first build takes minutes)");
            let mut compiled = 0u32;
            let exe = build_player(
                &root,
                &mut |line| {
                    if line.trim_start().starts_with("Compiling") {
                        compiled += 1;
                    } else {
                        tracing::info!("build: {line}");
                    }
                },
                cancel,
            )
            .map_err(|e| format!("{e:#}"))?;
            tracing::info!("build: compiled {compiled} crates; writing the pack");
            let report =
                package_game(&root, &exe, &out, &settings).map_err(|e| format!("{e:#}"))?;
            tracing::info!(
                "build: wrote {} ({} files in game.pak)",
                report.dir.display(),
                report.files
            );
            Ok(report)
        });
    }

    /// The finished job's result, once (starts the game after "Build and Run").
    fn poll(&mut self) -> Status {
        let job = self.job.as_ref()?;
        let result = match job.done.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the build thread stopped".to_string()),
        };
        let run_after = job.run_after;
        let job = self.job.take()?;
        let _ = job.thread.join();
        Some(match result {
            Ok(report) => {
                let dir = report.dir.display().to_string();
                if run_after && let Err(error) = run_game(&report) {
                    tracing::error!("build: cannot start {}: {error}", report.exe.display());
                    self.last = Some(report);
                    return Some((
                        StatusKind::Error,
                        format!("Built, but cannot start it: {error}"),
                    ));
                }
                self.last = Some(report);
                (StatusKind::Success, format!("Built {dir}"))
            }
            Err(message) if message.contains("cancelled") => {
                tracing::info!("build: cancelled");
                (StatusKind::Warning, "Build cancelled".to_string())
            }
            Err(message) => {
                tracing::error!("build failed: {message}");
                (StatusKind::Error, format!("Build failed: {message}"))
            }
        })
    }

    /// Draw the window (when open). Returns a status message for the status bar.
    pub fn ui(&mut self, ctx: &egui::Context, world: &mut World, project_root: &Path) -> Status {
        if !self.loaded {
            self.loaded = true;
            if let Some(output) = read_output(project_root) {
                self.output = output;
            }
        }
        let finished = self.poll();
        if self.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        if !self.open {
            return finished;
        }
        let settings = world
            .get_resource::<PlayerSettings>()
            .cloned()
            .unwrap_or_default();
        let mut open = true;
        let mut request = None;
        let mut cancel = false;
        let mut open_dir = None;
        let mut run = false;
        egui::Window::new("Build")
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Game: {} {} (Project Settings > Player)",
                    settings.game_name, settings.version
                ));
                ui.horizontal(|ui| {
                    ui.label("Output folder");
                    ui.text_edit_singleline(&mut self.output);
                });
                ui.weak(format!(
                    "Writes {}",
                    output_dir(project_root, &self.output)
                        .join(&settings.game_name)
                        .display()
                ));
                ui.horizontal(|ui| {
                    let idle = self.job.is_none();
                    if ui.add_enabled(idle, egui::Button::new("Build")).clicked() {
                        request = Some(false);
                    }
                    if ui
                        .add_enabled(idle, egui::Button::new("Build and Run"))
                        .clicked()
                    {
                        request = Some(true);
                    }
                    if !idle {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        ui.spinner();
                        ui.label("Building… progress is in the Console");
                    }
                });
                if let Some(last) = &self.last
                    && self.job.is_none()
                {
                    ui.separator();
                    ui.label(format!("Last build: {}", last.dir.display()));
                    ui.horizontal(|ui| {
                        if ui.button("Open Folder").clicked() {
                            open_dir = Some(last.dir.clone());
                        }
                        if ui.button("Run").clicked() {
                            run = true;
                        }
                    });
                }
            });
        self.open = open;
        let mut status = finished;
        if cancel && let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(dir) = open_dir
            && let Err(error) = open_folder(&dir)
        {
            status = Some((
                StatusKind::Error,
                format!("Cannot open the folder: {error}"),
            ));
        }
        if run
            && let Some(last) = &self.last
            && let Err(error) = run_game(last)
        {
            status = Some((StatusKind::Error, format!("Cannot start the game: {error}")));
        }
        if let Some(run_after) = request {
            if let Err(message) = settings.validate() {
                status = Some((StatusKind::Warning, format!("Player settings: {message}")));
            } else {
                let output = self.output.clone();
                write_output(project_root, &output);
                self.start_build(run_after, project_root, &output, settings);
                status = Some((StatusKind::Success, "Build started".into()));
            }
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn report(dir: &Path) -> PackageReport {
        PackageReport {
            dir: dir.to_path_buf(),
            exe: dir.join("game.exe"),
            files: 3,
        }
    }

    fn wait(window: &mut BuildWindow) -> Status {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Some(status) = window.poll() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn output_is_under_the_project_root() {
        assert_eq!(
            output_dir(Path::new("proj"), " Builds "),
            Path::new("proj").join("Builds")
        );
    }

    #[test]
    fn a_finished_build_reports_success_once_and_is_idle_again() {
        let mut window = BuildWindow::default();
        window.start(false, |_| Ok(report(Path::new("out/Game"))));
        assert!(window.building());
        let (kind, message) = wait(&mut window).expect("a result");
        assert_eq!(kind, StatusKind::Success);
        assert!(message.contains("Game"), "{message}");
        assert!(!window.building());
        assert!(window.poll().is_none(), "reported once");
        assert!(window.last.is_some(), "kept for Open Folder / Run");
    }

    #[test]
    fn a_failed_build_reports_the_error() {
        let mut window = BuildWindow::default();
        window.start(true, |_| Err("cargo build of the player failed".into()));
        let (kind, message) = wait(&mut window).expect("a result");
        assert_eq!(kind, StatusKind::Error);
        assert!(message.contains("cargo build"), "{message}");
        assert!(!window.building());
    }

    #[test]
    fn cancel_reaches_the_job_and_reads_as_a_warning() {
        let mut window = BuildWindow::default();
        window.start(false, |cancel| {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err("cancelled".into())
        });
        window
            .job
            .as_ref()
            .unwrap()
            .cancel
            .store(true, Ordering::Relaxed);
        let (kind, _) = wait(&mut window).expect("a result");
        assert_eq!(kind, StatusKind::Warning);
        assert!(window.last.is_none());
    }

    #[test]
    fn dropping_the_window_cancels_a_running_build() {
        let seen = Arc::new(AtomicBool::new(false));
        let flag = seen.clone();
        let mut window = BuildWindow::default();
        window.start(false, move |cancel| {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            flag.store(true, Ordering::Relaxed);
            Err("cancelled".into())
        });
        drop(window);
        assert!(seen.load(Ordering::Relaxed), "the job saw the cancel flag");
    }

    #[test]
    fn the_output_folder_is_remembered_per_project() {
        let root = std::env::temp_dir().join(format!("melon_build_out_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(read_output(&root), None);
        write_output(&root, " D:/Games ");
        assert_eq!(read_output(&root).as_deref(), Some("D:/Games"));
        let mut world = melon_core::Core::new().world;
        let mut window = BuildWindow::default();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            window.ui(ui.ctx(), &mut world, &root);
        });
        assert_eq!(window.output, "D:/Games");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)] // `BuildWindow` has a Drop impl
    fn window_draws_and_refuses_an_invalid_game_name() {
        let mut world = melon_core::Core::new().world;
        world.insert_resource(PlayerSettings {
            game_name: "a/b".into(),
            ..Default::default()
        });
        let mut window = BuildWindow::default();
        window.open = true;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(window.ui(ui.ctx(), &mut world, Path::new(".")).is_none());
        });
        assert!(!window.building());
    }
}
