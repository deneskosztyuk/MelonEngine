//! File > Build: package the game. Builds the player (release, no editor)
//! and writes `<output>/<GameName>/` with the executable and `game.pak`,
//! on a background thread; progress lines go to the Console. "Build and
//! Run" starts the packaged game when the build succeeds.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use bevy_ecs::world::World;
use melon_core::PlayerSettings;
use melon_core::package::{PackageReport, build_player, package_game};

use crate::StatusKind;

type Status = Option<(StatusKind, String)>;

/// Output folder shown first, relative to the project root.
pub const DEFAULT_OUTPUT: &str = "Builds";

struct Job {
    done: Receiver<Result<PackageReport, String>>,
    run_after: bool,
}

pub struct BuildWindow {
    pub open: bool,
    output: String,
    job: Option<Job>,
}

impl Default for BuildWindow {
    fn default() -> Self {
        Self {
            open: false,
            output: DEFAULT_OUTPUT.to_string(),
            job: None,
        }
    }
}

/// `output` as a path: relative ones are under the project root.
fn output_dir(project_root: &Path, output: &str) -> PathBuf {
    project_root.join(output.trim())
}

impl BuildWindow {
    pub fn building(&self) -> bool {
        self.job.is_some()
    }

    /// Run `work` on a thread; `poll` reports its result.
    fn start(
        &mut self,
        run_after: bool,
        work: impl FnOnce() -> Result<PackageReport, String> + Send + 'static,
    ) {
        let (tx, done) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        self.job = Some(Job { done, run_after });
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
        self.start(run_after, move || {
            tracing::info!("build: building the player (the first build takes minutes)");
            let mut compiled = 0u32;
            let exe = build_player(&root, &mut |line| {
                if line.trim_start().starts_with("Compiling") {
                    compiled += 1;
                } else {
                    tracing::info!("build: {line}");
                }
            })
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
        self.job = None;
        Some(match result {
            Ok(report) => {
                if run_after {
                    match std::process::Command::new(&report.exe)
                        .current_dir(&report.dir)
                        .spawn()
                    {
                        Ok(_) => tracing::info!("build: started {}", report.exe.display()),
                        Err(error) => {
                            tracing::error!(
                                "build: cannot start {}: {error}",
                                report.exe.display()
                            );
                            return Some((
                                StatusKind::Error,
                                format!("Built, but cannot start it: {error}"),
                            ));
                        }
                    }
                }
                (
                    StatusKind::Success,
                    format!("Built {}", report.dir.display()),
                )
            }
            Err(message) => {
                tracing::error!("build failed: {message}");
                (StatusKind::Error, format!("Build failed: {message}"))
            }
        })
    }

    /// Draw the window (when open). Returns a status message for the status bar.
    pub fn ui(&mut self, ctx: &egui::Context, world: &mut World, project_root: &Path) -> Status {
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
                        ui.spinner();
                        ui.label("Building… progress is in the Console");
                    }
                });
            });
        self.open = open;
        let mut status = finished;
        if let Some(run_after) = request {
            if let Err(message) = settings.validate() {
                status = Some((StatusKind::Warning, format!("Player settings: {message}")));
            } else {
                self.start_build(run_after, project_root, &self.output.clone(), settings);
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
        window.start(false, || Ok(report(Path::new("out/Game"))));
        assert!(window.building());
        let (kind, message) = wait(&mut window).expect("a result");
        assert_eq!(kind, StatusKind::Success);
        assert!(message.contains("Game"), "{message}");
        assert!(!window.building());
        assert!(window.poll().is_none(), "reported once");
    }

    #[test]
    fn a_failed_build_reports_the_error() {
        let mut window = BuildWindow::default();
        window.start(true, || Err("cargo build of the player failed".into()));
        let (kind, message) = wait(&mut window).expect("a result");
        assert_eq!(kind, StatusKind::Error);
        assert!(message.contains("cargo build"), "{message}");
        assert!(!window.building());
    }

    #[test]
    fn window_draws_and_refuses_an_invalid_game_name() {
        let mut world = melon_core::Core::new().world;
        world.insert_resource(PlayerSettings {
            game_name: "a/b".into(),
            ..Default::default()
        });
        let mut window = BuildWindow {
            open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(window.ui(ui.ctx(), &mut world, Path::new(".")).is_none());
        });
        assert!(!window.building());
    }
}
