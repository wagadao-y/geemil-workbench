//! Modal dialogs. At most one is open; `Workbench::dialog` holds its state.
use super::{Settings, Workbench, revisions::RevisionsState};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::Project;
use std::path::PathBuf;
use uuid::Uuid;

pub(super) enum Dialog {
    NewProject {
        name: String,
        location: PathBuf,
        /// Files dropped before a project existed, imported once it does.
        imports: Vec<PathBuf>,
    },
    Revisions(RevisionsState),
    SaveAs {
        name: String,
    },
    /// Confirms throwing away unsaved changes, then does `then`.
    Discard {
        then: AfterDiscard,
    },
    Cleanup,
    RenameGroup {
        id: Uuid,
        name: String,
    },
    Filter(Filter),
    Shortcuts,
    About,
}
/// A point filter and its parameters, as edited in its dialog.
#[derive(Clone, Copy)]
pub(super) enum Filter {
    Subsample { size: f64 },
    Noise { radius: f64, min_neighbours: u32 },
}
#[derive(Clone, Copy)]
pub(super) enum AfterDiscard {
    Nothing,
    Switch(Uuid),
}

impl Dialog {
    pub(super) fn new_project(settings: &Settings, imports: Vec<PathBuf>) -> Self {
        let first = imports.first();
        let name = first
            .and_then(|f| f.file_stem())
            .map_or("point-project".into(), |s| s.to_string_lossy().into_owned());
        let location = settings
            .last_location
            .clone()
            .or_else(|| first.and_then(|f| f.parent()).map(ToOwned::to_owned))
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_default();
        Self::NewProject {
            name,
            location,
            imports,
        }
    }
    pub(super) fn revisions(project: Option<&Project>) -> Self {
        Self::Revisions(RevisionsState::new(project))
    }
}

/// What a dialog asked for once it closes.
enum Outcome {
    Keep,
    Close,
    CreateProject(PathBuf, Vec<PathBuf>),
    Save(String),
    Discard(AfterDiscard),
    Cleanup,
    RenameGroup(Uuid, String),
    Filter(Filter),
}

fn buttons(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
}

impl Workbench {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        if let Dialog::Revisions(state) = dialog {
            let mut state = std::mem::take(state);
            let keep = self.revisions_dialog(ctx, &mut state);
            if keep && let Some(Dialog::Revisions(slot)) = &mut self.dialog {
                *slot = state;
            }
            return;
        }
        let id = egui::Id::new("dialog");
        let mut outcome = Outcome::Keep;
        let visible = self.visible.len();
        let response = egui::Modal::new(id).show(ctx, |ui| {
            ui.set_max_width(520.);
            match dialog {
                Dialog::NewProject {
                    name,
                    location,
                    imports,
                } => {
                    ui.heading(format!("{} {}", icon::FOLDER_PLUS, t.new_project_title));
                    egui::Grid::new("new project")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label(t.project_name);
                            ui.add(egui::TextEdit::singleline(name).desired_width(300.));
                            ui.end_row();
                            ui.label(t.location);
                            ui.horizontal(|ui| {
                                ui.label(location.display().to_string());
                                if ui.button(t.browse).clicked()
                                    && let Some(dir) = rfd::FileDialog::new()
                                        .set_directory(&*location)
                                        .pick_folder()
                                {
                                    *location = dir;
                                }
                            });
                            ui.end_row();
                        });
                    let path = location.join(name.trim());
                    ui.small((t.folder_will_be)(&path.display().to_string()));
                    if !imports.is_empty() {
                        ui.small((t.import_after)(imports.len()));
                    }
                    let valid = !name.trim().is_empty() && !path.exists();
                    buttons(ui, |ui| {
                        if ui.add_enabled(valid, egui::Button::new(t.create)).clicked() {
                            outcome = Outcome::CreateProject(path, std::mem::take(imports));
                        }
                        if ui.button(t.cancel).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::SaveAs { name } => {
                    ui.heading(format!("{} {}", icon::FLOPPY_DISK, t.save_title));
                    ui.horizontal(|ui| {
                        ui.label(t.name);
                        let edit = ui.add(egui::TextEdit::singleline(name).desired_width(320.));
                        edit.request_focus();
                        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            outcome = Outcome::Save(name.clone());
                        }
                    });
                    buttons(ui, |ui| {
                        if ui.button(t.save_button).clicked() {
                            outcome = Outcome::Save(name.clone());
                        }
                        if ui.button(t.cancel).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::Discard { then } => {
                    ui.heading(format!("{} {}", icon::WARNING, t.discard_title));
                    ui.label(t.discard_message);
                    buttons(ui, |ui| {
                        if ui.button(t.discard_and_continue).clicked() {
                            outcome = Outcome::Discard(*then);
                        }
                        if ui.button(t.cancel).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::Cleanup => {
                    ui.heading(format!("{} {}", icon::BROOM, t.cleanup_title));
                    ui.label(t.cleanup_message);
                    buttons(ui, |ui| {
                        if ui.button(t.cleanup_run).clicked() {
                            outcome = Outcome::Cleanup;
                        }
                        if ui.button(t.cancel).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::RenameGroup { id, name } => {
                    ui.heading(format!("{} {}", icon::PENCIL_SIMPLE, t.rename));
                    let edit = ui.add(egui::TextEdit::singleline(name).desired_width(320.));
                    edit.request_focus();
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    buttons(ui, |ui| {
                        if ui.button(t.apply).clicked() || enter {
                            outcome = Outcome::RenameGroup(*id, name.clone());
                        }
                        if ui.button(t.cancel).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::Filter(filter) => {
                    let (heading, message) = match filter {
                        Filter::Subsample { .. } => (
                            format!("{} {}", icon::DOTS_NINE, t.subsample),
                            t.subsample_message,
                        ),
                        Filter::Noise { .. } => (
                            format!("{} {}", icon::FUNNEL, t.remove_noise),
                            t.noise_message,
                        ),
                    };
                    ui.heading(heading.trim_end_matches('…'));
                    ui.label(message);
                    ui.add_space(6.);
                    egui::Grid::new("filter")
                        .num_columns(2)
                        .show(ui, |ui| match filter {
                            Filter::Subsample { size } => {
                                ui.label(t.voxel_size);
                                ui.add(metres(size));
                                ui.end_row();
                            }
                            Filter::Noise {
                                radius,
                                min_neighbours,
                            } => {
                                ui.label(t.search_radius);
                                ui.add(metres(radius));
                                ui.end_row();
                                ui.label(t.min_neighbours);
                                ui.add(egui::DragValue::new(min_neighbours).range(1..=100));
                                ui.end_row();
                            }
                        });
                    ui.small((t.filter_targets)(visible));
                    buttons(ui, |ui| {
                        if ui.button(t.run).clicked() {
                            outcome = Outcome::Filter(*filter);
                        }
                        if ui.button(t.cancel).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::Shortcuts => {
                    ui.heading(format!("{} {}", icon::KEYBOARD, t.shortcuts));
                    egui::Grid::new("shortcuts")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            for (key, what) in t.shortcut_rows {
                                ui.strong(*key);
                                ui.label(*what);
                                ui.end_row();
                            }
                        });
                    buttons(ui, |ui| {
                        if ui.button(t.close).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::About => {
                    ui.heading(format!(
                        "{} Geemil Workbench {}",
                        icon::INFO,
                        env!("CARGO_PKG_VERSION")
                    ));
                    ui.label(t.about_text);
                    buttons(ui, |ui| {
                        if ui.button(t.close).clicked() {
                            outcome = Outcome::Close;
                        }
                    });
                }
                Dialog::Revisions(_) => unreachable!(),
            }
        });
        if matches!(outcome, Outcome::Keep) && response.should_close() {
            outcome = Outcome::Close;
        }
        match outcome {
            Outcome::Keep => {}
            Outcome::Close => self.dialog = None,
            Outcome::CreateProject(path, imports) => {
                self.dialog = None;
                self.create_project(ctx, path, imports);
            }
            Outcome::Save(name) => {
                self.dialog = None;
                self.save_revision(name);
            }
            Outcome::Discard(then) => {
                self.dialog = None;
                match then {
                    AfterDiscard::Nothing => {
                        let current = self.project.as_ref().map(|p| p.manifest.current);
                        if let Some(id) = current {
                            self.switch_revision(id);
                        }
                    }
                    AfterDiscard::Switch(id) => self.switch_revision(id),
                }
            }
            Outcome::Cleanup => {
                self.dialog = None;
                self.cleanup(ctx);
            }
            Outcome::RenameGroup(id, name) => {
                self.dialog = None;
                self.apply_edit(|p| p.rename_group(id, name));
            }
            Outcome::Filter(filter) => {
                self.dialog = None;
                self.run_filter(ctx, filter);
            }
        }
    }
    fn cleanup(&mut self, ctx: &egui::Context) {
        let Some(project) = &self.project else { return };
        let mut project = (**project).clone();
        self.undo.clear();
        let (tx, rx) = std::sync::mpsc::channel();
        self.cleanup_report = Some(rx);
        self.start(ctx, false, move |_| {
            let report = project.cleanup()?;
            let _ = tx.send(report);
            Ok(project)
        });
    }
}

/// A length input in metres for filter parameters.
fn metres(v: &mut f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v)
        .range(0.001..=10.)
        .speed(0.001)
        .max_decimals(3)
}
