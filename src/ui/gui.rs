//! The window: a small panel that shows what Sorta is doing and takes dropped files.
//!
//! **This has never been run.** It compiles on both target platforms (the `gui` CI
//! job proves that), and the fleet already ships egui apps this way (DayDoc uses
//! `eframe` 0.29), but nobody has looked at it on a screen. Treat it as a first cut
//! to try and report back on, not as a finished surface.
//!
//! What it does, in the order the code runs:
//!
//! 1. reads the history tail, the quarantine queue and the pause flag every frame —
//!    the files are small and this avoids a cache to invalidate;
//! 2. offers Pause/Resume, which writes the same flag `sorta pause` writes;
//! 3. accepts files dropped on the window and sorts them on a worker thread, so a
//!    slow PDF cannot freeze the window.
//!
//! What it deliberately does not do yet: the tray icon (that is a shell shortcut in
//! this fleet's pattern, see CLDR), the global hotkey, and the time-travel slider.

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use crate::config::LoadedConfig;
use crate::history::History;
use crate::instance;
use crate::paths;
use crate::pipeline::{Engine, EngineOptions, Outcome};
use crate::quarantine::Quarantine;

/// Opens the window and blocks until it is closed.
pub fn run(loaded: LoadedConfig) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([380.0, 520.0])
            .with_min_inner_size([300.0, 320.0])
            .with_title("Sorta"),
        ..Default::default()
    };

    eframe::run_native(
        "Sorta",
        options,
        Box::new(move |_cc| Ok(Box::new(Window::new(loaded)))),
    )
    .map_err(|error| anyhow::anyhow!("could not open the window: {error}"))
}

/// What a worker thread reports back.
type SortResult = Vec<Outcome>;

struct Window {
    loaded: LoadedConfig,
    lines: Vec<String>,
    pending: Option<Receiver<SortResult>>,
    busy: bool,
}

impl Window {
    fn new(loaded: LoadedConfig) -> Self {
        Self {
            loaded,
            lines: Vec::new(),
            pending: None,
            busy: false,
        }
    }

    fn refresh(&mut self) {
        let history = History::new(&paths::history_path());
        self.lines = history
            .tail(6)
            .iter()
            .map(|entry| {
                let name = entry.dst.rsplit(['/', '\\']).next().unwrap_or(&entry.dst);
                format!("{}  ·  {}", entry.status, name)
            })
            .collect();
    }

    fn sort_files(&mut self, files: Vec<std::path::PathBuf>) {
        if files.is_empty() || self.busy {
            return;
        }
        let loaded = self.loaded.clone();
        let (sender, receiver) = channel();

        std::thread::spawn(move || {
            let options = EngineOptions {
                read_only: false,
                dry_run: false,
                sandbox: None,
            };
            let outcomes = match Engine::new(&loaded, options) {
                Ok(mut engine) => {
                    let outcomes = engine.process_all(&files);
                    let _ = engine.persist();
                    outcomes
                }
                Err(error) => vec![Outcome::new(
                    &files[0],
                    crate::pipeline::Status::Failed,
                    &error.to_string(),
                )],
            };
            let _ = sender.send(outcomes);
        });

        self.pending = Some(receiver);
        self.busy = true;
    }

    fn poll(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        if let Ok(outcomes) = receiver.try_recv() {
            let moved = outcomes
                .iter()
                .filter(|o| o.status == crate::pipeline::Status::Moved)
                .count();
            let other = outcomes.len() - moved;
            self.lines
                .insert(0, format!("{moved} sorted, {other} left alone"));
            self.pending = None;
            self.busy = false;
        }
    }
}

impl eframe::App for Window {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if self.pending.is_some() {
            // Keep repainting while a sort is in flight so the result shows up.
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        let state_dir = paths::state_dir();
        let paused = instance::is_paused(&state_dir);
        let quarantined = Quarantine::new(&self.loaded.quarantine_root())
            .pending()
            .len();

        // Files dropped on the window are sorted immediately.
        let dropped: Vec<std::path::PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.sort_files(dropped);
        }

        eframe::egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Sorta");
            ui.label(if paused { "paused" } else { "watching" });
            ui.separator();

            ui.horizontal(|ui| {
                if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                    let _ = instance::set_paused(&state_dir, !paused);
                }
                if ui.button("Refresh").clicked() {
                    self.refresh();
                }
            });

            if quarantined > 0 {
                ui.label(format!("{quarantined} waiting in quarantine"));
            }
            if self.busy {
                ui.label("sorting…");
            }

            ui.separator();
            ui.label("Drop files here to sort them");
            ui.separator();

            if self.lines.is_empty() {
                ui.label("Nothing sorted yet.");
            } else {
                for line in &self.lines {
                    ui.label(line);
                }
            }

            ui.separator();
            ui.small(format!(
                "destination: {}",
                self.loaded.destination_root().display()
            ));
        });
    }
}
