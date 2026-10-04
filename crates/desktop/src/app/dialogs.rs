//! Modal dialogs. At most one is open; `Workbench::dialog` holds its state.
use super::{Settings, Workbench, layers::destination_combo, revisions::RevisionsState};
use crate::i18n::Strings;
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{LasExportCompatibility, LasExportPolicy, Project};
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
    Properties {
        id: Uuid,
    },
    SaveAs {
        name: String,
    },
    /// Confirms throwing away unsaved changes, then does `then`.
    Discard {
        then: AfterDiscard,
        /// Restore the revision picker, including its selection, on cancellation.
        return_to_revisions: Option<RevisionsState>,
    },
    Cleanup,
    /// Confirms scattering the scans for registration practice.
    Scatter,
    /// Asks what to do with an unapplied alignment result before the tool
    /// moves on to `next`, which the tree selected.
    AlignPending {
        next: Option<Uuid>,
    },
    RenameGroup {
        id: Uuid,
        name: String,
    },
    /// Names a new layer (`code` none) or renames one.
    RenameLayer {
        code: Option<u8>,
        name: String,
    },
    /// A filter, and the layer it moves points to; none for its default layer.
    Filter(Filter, Option<u8>),
    Export {
        format: ExportFormat,
        per_scan: bool,
        policy: LasExportPolicy,
        compatibility: Option<Result<LasExportCompatibility, String>>,
    },
    Shortcuts,
    About,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportFormat {
    E57,
    Las,
    Laz,
}
impl ExportFormat {
    pub(super) fn extension(self) -> &'static str {
        match self {
            Self::E57 => "e57",
            Self::Las => "las",
            Self::Laz => "laz",
        }
    }
}
/// A point filter and its parameters, as edited in its dialog.
#[derive(Clone, Copy)]
pub(super) enum Filter {
    Subsample {
        size: f64,
        merged: bool,
    },
    /// Overlap reduction over the visible scans together.
    Overlap {
        size: f64,
    },
    Noise {
        radius: f64,
        min_neighbours: u32,
    },
    Statistical {
        neighbours: u32,
        deviations: f64,
        reach: f64,
    },
}
impl Filter {
    /// The layer the filter moves points to unless another is chosen.
    pub(super) fn default_layer(&self, t: &'static Strings) -> &'static str {
        match self {
            Filter::Subsample { .. } => t.layer_subsampled,
            Filter::Overlap { .. } => t.layer_overlap,
            Filter::Noise { .. } | Filter::Statistical { .. } => t.layer_noise,
        }
    }
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
    /// Leave the alignment result for `next`, applying it first if asked.
    AlignSwitch {
        apply: bool,
        next: Option<Uuid>,
    },
    Cleanup,
    Scatter,
    RenameGroup(Uuid, String),
    RenameLayer(Option<u8>, String),
    Filter(Filter, Option<u8>),
    Export {
        format: ExportFormat,
        per_scan: bool,
        policy: LasExportPolicy,
    },
}

/// Dialog buttons at the bottom right in Windows order: the actions `add`
/// puts, then `cancel` at the far right. Returns whether `cancel` was clicked.
fn buttons(ui: &mut egui::Ui, cancel: &str, add: impl FnOnce(&mut egui::Ui)) -> bool {
    ui.add_space(8.);
    // Right to left: the first button is the rightmost.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let cancelled = ui.button(cancel).clicked();
        add(ui);
        cancelled
    })
    .inner
}

impl Workbench {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let t = self.t;
        let align_name = match self.dialog {
            Some(Dialog::AlignPending { .. }) => self.align_item_name(),
            _ => String::new(),
        };
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        if let Dialog::Properties { id } = dialog {
            let id = *id;
            self.properties_dialog(ctx, id);
            return;
        }
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
        let project = self.project.clone();
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
                    if buttons(ui, t.cancel, |ui| {
                        if ui.add_enabled(valid, egui::Button::new(t.create)).clicked() {
                            outcome = Outcome::CreateProject(path, std::mem::take(imports));
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::SaveAs { name } => {
                    ui.heading(format!("{} {}", icon::FLOPPY_DISK, t.save_title));
                    ui.horizontal(|ui| {
                        ui.label(t.name);
                        let edit = ui.add(egui::TextEdit::singleline(name).desired_width(320.));
                        // Requesting focus interrupts IME composition, so only take it
                        // when it is elsewhere; otherwise Japanese input never lands.
                        if !edit.has_focus() {
                            edit.request_focus();
                        }
                        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            outcome = Outcome::Save(name.clone());
                        }
                    });
                    if buttons(ui, t.cancel, |ui| {
                        if ui.button(t.save_button).clicked() {
                            outcome = Outcome::Save(name.clone());
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Discard { then, .. } => {
                    ui.heading(format!("{} {}", icon::WARNING, t.discard_title));
                    ui.label(t.discard_message);
                    if buttons(ui, t.cancel, |ui| {
                        if ui.button(t.discard_and_continue).clicked() {
                            outcome = Outcome::Discard(*then);
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::AlignPending { next } => {
                    ui.heading(format!("{} {}", icon::WARNING, t.align_pending_title));
                    ui.label((t.align_pending_message)(&align_name));
                    if buttons(ui, t.cancel, |ui| {
                        if ui.button(t.align_pending_apply).clicked() {
                            outcome = Outcome::AlignSwitch {
                                apply: true,
                                next: *next,
                            };
                        }
                        if ui.button(t.align_pending_discard).clicked() {
                            outcome = Outcome::AlignSwitch {
                                apply: false,
                                next: *next,
                            };
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Cleanup => {
                    ui.heading(format!("{} {}", icon::BROOM, t.cleanup_title));
                    ui.label(t.cleanup_message);
                    if buttons(ui, t.cancel, |ui| {
                        if ui.button(t.cleanup_run).clicked() {
                            outcome = Outcome::Cleanup;
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Scatter => {
                    ui.heading(format!("{} {}", icon::SHUFFLE, t.scatter_title));
                    ui.label(t.scatter_message);
                    if buttons(ui, t.cancel, |ui| {
                        if ui.button(t.scatter_run).clicked() {
                            outcome = Outcome::Scatter;
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::RenameGroup { id, name } => {
                    ui.heading(format!("{} {}", icon::PENCIL_SIMPLE, t.rename));
                    let edit = ui.add(egui::TextEdit::singleline(name).desired_width(320.));
                    // Requesting focus interrupts IME composition, so only take it
                    // when it is elsewhere; otherwise Japanese input never lands.
                    if !edit.has_focus() {
                        edit.request_focus();
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if buttons(ui, t.cancel, |ui| {
                        if ui.button(t.apply).clicked() || enter {
                            outcome = Outcome::RenameGroup(*id, name.clone());
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::RenameLayer { code, name } => {
                    let heading = match code {
                        Some(_) => format!("{} {}", icon::PENCIL_SIMPLE, t.rename),
                        None => format!("{} {}", icon::STACK, t.new_layer_title),
                    };
                    ui.heading(heading);
                    let edit = ui.add(egui::TextEdit::singleline(name).desired_width(320.));
                    // Requesting focus interrupts IME composition, so only take it
                    // when it is elsewhere; otherwise Japanese input never lands.
                    if !edit.has_focus() {
                        edit.request_focus();
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let valid = !name.trim().is_empty();
                    let label = if code.is_some() { t.apply } else { t.create };
                    if buttons(ui, t.cancel, |ui| {
                        if ui.add_enabled(valid, egui::Button::new(label)).clicked()
                            || (enter && valid)
                        {
                            outcome = Outcome::RenameLayer(*code, name.trim().to_owned());
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Filter(filter, destination) => {
                    let (heading, message) = match filter {
                        Filter::Subsample { .. } => (
                            format!("{} {}", icon::DOTS_NINE, t.subsample),
                            t.subsample_message,
                        ),
                        Filter::Overlap { .. } => (
                            format!("{} {}", icon::INTERSECT, t.reduce_overlap),
                            t.overlap_message,
                        ),
                        Filter::Noise { .. } => (
                            format!("{} {}", icon::FUNNEL, t.remove_noise),
                            t.noise_message,
                        ),
                        Filter::Statistical { .. } => (
                            format!("{} {}", icon::CHART_SCATTER, t.remove_outliers),
                            t.outliers_message,
                        ),
                    };
                    ui.heading(heading.trim_end_matches('…'));
                    ui.label(message);
                    ui.add_space(6.);
                    egui::Grid::new("filter")
                        .num_columns(2)
                        .show(ui, |ui| match filter {
                            Filter::Subsample { size, merged } => {
                                ui.label(t.voxel_size);
                                ui.add(metres(size));
                                ui.end_row();
                                ui.label("");
                                ui.checkbox(merged, t.subsample_merged)
                                    .on_hover_text(t.subsample_merged_hint);
                                ui.end_row();
                            }
                            Filter::Overlap { size } => {
                                ui.label(t.overlap_cell);
                                ui.add(metres(size)).on_hover_text(t.overlap_cell_hint);
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
                            Filter::Statistical {
                                neighbours,
                                deviations,
                                reach,
                            } => {
                                ui.label(t.outlier_neighbours);
                                ui.add(egui::DragValue::new(neighbours).range(1..=64));
                                ui.end_row();
                                ui.label(t.outlier_deviations);
                                ui.add(
                                    egui::DragValue::new(deviations)
                                        .range(0.0..=10.)
                                        .speed(0.05)
                                        .max_decimals(2),
                                );
                                ui.end_row();
                                ui.label(t.outlier_reach);
                                ui.add(metres(reach));
                                ui.end_row();
                            }
                        });
                    if let Some(p) = &project {
                        ui.horizontal(|ui| {
                            let default = filter.default_layer(t);
                            destination_combo(ui, t, p, "filter destination", default, destination);
                        });
                    }
                    ui.small((t.filter_targets)(visible));
                    ui.horizontal(|ui| {
                        ui.label(t.filter_memory);
                        ui.add(
                            egui::DragValue::new(&mut self.settings.filter_memory_mib)
                                .range(128..=4096)
                                .speed(64)
                                .suffix(" MiB"),
                        )
                        .on_hover_text(t.filter_memory_hint);
                    });
                    if buttons(ui, t.cancel, |ui| {
                        if ui.button(t.run).clicked() {
                            outcome = Outcome::Filter(*filter, *destination);
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Export {
                    format,
                    per_scan,
                    policy,
                    compatibility,
                } => {
                    ui.heading(format!(
                        "{} {}",
                        icon::EXPORT,
                        t.export.trim_end_matches('…')
                    ));
                    ui.label(t.export_message);
                    ui.add_space(6.);
                    ui.horizontal(|ui| {
                        ui.label(t.export_format);
                        ui.radio_value(format, ExportFormat::E57, "E57");
                        ui.radio_value(format, ExportFormat::Las, "LAS");
                        ui.radio_value(format, ExportFormat::Laz, "LAZ");
                    });
                    ui.add_space(6.);
                    if *format == ExportFormat::E57 {
                        ui.label(t.export_e57_message);
                        if project
                            .as_ref()
                            .is_some_and(|p| p.scans().any(|s| s.las.is_some()))
                        {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                t.export_las_to_e57_notice,
                            );
                        }
                    } else {
                        ui.label(t.export_las_message);
                        ui.radio_value(per_scan, false, t.export_merged);
                        ui.radio_value(per_scan, true, t.export_per_scan);
                        if !*per_scan {
                            let report = compatibility.get_or_insert_with(|| {
                                project.as_ref().map_or_else(
                                    || Ok(LasExportCompatibility::default()),
                                    |p| p.las_export_compatibility().map_err(|e| format!("{e:#}")),
                                )
                            });
                            ui.radio_value(
                                policy,
                                LasExportPolicy::Preserve,
                                t.export_preserve_attributes,
                            );
                            ui.radio_value(
                                policy,
                                LasExportPolicy::OmitIncompatible,
                                t.export_omit_incompatible,
                            );
                            match report {
                                Ok(report) if report.has_conflicts() => {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        t.export_conflicts,
                                    );
                                    egui::ScrollArea::vertical()
                                        .max_height(150.)
                                        .show(ui, |ui| {
                                            if report.omit_extra_bytes {
                                                ui.label(t.export_omit_extra);
                                            }
                                            if report.omit_gps_time {
                                                ui.label(t.export_omit_gps);
                                            }
                                            if report.omit_crs {
                                                ui.label(t.export_omit_crs);
                                            }
                                            for (user, id) in &report.omitted_metadata {
                                                ui.label(format!("VLR/EVLR: {user} / {id}"));
                                            }
                                        });
                                }
                                Ok(_) => {
                                    ui.small(t.export_no_conflicts);
                                }
                                Err(error) => {
                                    ui.colored_label(ui.visuals().error_fg_color, error.as_str());
                                }
                            }
                        }
                    }
                    let allowed = *format == ExportFormat::E57
                        || *per_scan
                        || compatibility.as_ref().is_some_and(|r| {
                            r.as_ref().is_ok_and(|r| {
                                *policy == LasExportPolicy::OmitIncompatible || !r.has_conflicts()
                            })
                        });
                    if buttons(ui, t.cancel, |ui| {
                        if ui
                            .add_enabled(allowed, egui::Button::new(t.export_button))
                            .clicked()
                        {
                            outcome = Outcome::Export {
                                format: *format,
                                per_scan: *per_scan,
                                policy: *policy,
                            };
                        }
                    }) {
                        outcome = Outcome::Close;
                    }
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
                    if buttons(ui, t.close, |_| {}) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::About => {
                    ui.heading(format!(
                        "{} Geemil Workbench {}",
                        icon::INFO,
                        env!("CARGO_PKG_VERSION")
                    ));
                    ui.label(t.about_text);
                    if buttons(ui, t.close, |_| {}) {
                        outcome = Outcome::Close;
                    }
                }
                Dialog::Revisions(_) | Dialog::Properties { .. } => unreachable!(),
            }
        });
        if matches!(outcome, Outcome::Keep) && response.should_close() {
            outcome = Outcome::Close;
        }
        match outcome {
            Outcome::Keep => {}
            Outcome::Close => {
                self.dialog = match self.dialog.take() {
                    Some(Dialog::Discard {
                        return_to_revisions: Some(state),
                        ..
                    }) => Some(Dialog::Revisions(state)),
                    _ => None,
                };
            }
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
            Outcome::AlignSwitch { apply, next } => {
                self.dialog = None;
                if apply {
                    self.align_apply();
                }
                self.select_tree_item(next);
                self.align_switch(next);
            }
            Outcome::Cleanup => {
                self.dialog = None;
                self.cleanup(ctx);
            }
            Outcome::Scatter => {
                self.dialog = None;
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64);
                if self.apply_edit(|p| p.scatter_scans(seed)).is_some() {
                    self.status = t.scatter_done.into();
                }
            }
            Outcome::RenameGroup(id, name) => {
                self.dialog = None;
                self.apply_edit(|p| p.rename_group(id, name));
            }
            Outcome::RenameLayer(code, name) => {
                self.dialog = None;
                match code {
                    Some(code) => self.apply_edit(|p| p.rename_layer(code, name)),
                    None => self.apply_edit(|p| p.create_layer(name).map(|_| ())),
                };
            }
            Outcome::Filter(filter, destination) => {
                self.dialog = None;
                self.run_filter(ctx, filter, destination);
            }
            Outcome::Export {
                format,
                per_scan,
                policy,
            } => {
                self.dialog = None;
                self.export(ctx, format, per_scan, policy);
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
