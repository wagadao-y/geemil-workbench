//! Which scans are aligned and how well: a mark in the tree and a list of
//! every scan and folder with its last alignment, the last global adjustment
//! pair by pair, and both as CSV.
use super::Workbench;
use crate::i18n::Strings;
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{Project, RegistrationMethod, RegistrationState};
use std::fmt::Write as _;
use uuid::Uuid;

const GOOD: egui::Color32 = egui::Color32::from_rgb(110, 200, 120);
const CHECK: egui::Color32 = egui::Color32::from_rgb(255, 190, 80);

/// An item's alignment as the list and the tree show it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Done,
    /// Moved since, stopped short, or too little overlap to trust.
    Check,
    None,
}
fn status(state: Option<&RegistrationState>) -> Status {
    let Some(state) = state else {
        return Status::None;
    };
    let fit = state.registration.fit;
    // A global adjustment's overlap is that of the whole scan with each
    // other one, small by nature; only ICP's says whether it held on.
    let thin = fit.method == RegistrationMethod::Icp && fit.overlap.is_some_and(|o| o < 0.1);
    if state.moved || fit.stopped || thin {
        Status::Check
    } else {
        Status::Done
    }
}
fn status_icon(status: Status) -> egui::RichText {
    match status {
        Status::Done => egui::RichText::new(icon::CHECK_CIRCLE).color(GOOD),
        Status::Check => egui::RichText::new(icon::WARNING).color(CHECK),
        Status::None => egui::RichText::new(icon::CIRCLE_DASHED).weak(),
    }
}
fn method_name(t: &Strings, method: RegistrationMethod) -> &'static str {
    match method {
        RegistrationMethod::Pairs => t.registration_methods[0],
        RegistrationMethod::Icp => t.registration_methods[1],
        RegistrationMethod::Global => t.registration_methods[2],
    }
}
fn time(at: u64) -> String {
    chrono::DateTime::from_timestamp(at as i64, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}
/// Several lines on an alignment, for a tooltip.
fn describe(t: &Strings, p: &Project, state: &RegistrationState, item: Uuid) -> String {
    let fit = state.registration.fit;
    let mut text = (t.registration_summary)(
        method_name(t, fit.method),
        fit.rms,
        fit.distance,
        fit.overlap.map(|o| o * 100.),
    );
    let _ = write!(
        text,
        "\n{}・{}",
        (t.registration_references)(fit.references),
        time(state.registration.at)
    );
    if state.item != item
        && let Some(folder) = p.groups().iter().find(|g| g.id == state.item)
    {
        let _ = write!(text, "\n{}", (t.registration_by_folder)(&folder.name));
    }
    for (problem, note) in [
        (state.moved, t.registration_moved),
        (fit.stopped, t.registration_stopped),
        (
            status(Some(state)) == Status::Check && !state.moved && !fit.stopped,
            t.registration_thin,
        ),
    ] {
        if problem {
            let _ = write!(text, "\n{note}");
        }
    }
    text
}

/// The alignment a row stands for: a folder's own; a scan's own or its
/// folder's, as [`Project::scan_registration`] picks.
fn item_registration(p: &Project, id: Uuid, folder: bool) -> Option<RegistrationState> {
    if folder {
        p.registration(id)
    } else {
        p.scan_registration(id)
    }
}

/// Metres to the millimetre and a tenth, or a dash for none.
fn metres(value: Option<f64>) -> String {
    value.map_or("—".into(), |v| format!("{v:.4} m"))
}
/// A pair's RMS for sorting worst first: one with no correspondence near
/// enough to measure is worse than any.
fn worst(pair: &geemil_core::PairFit) -> f64 {
    pair.rms.unwrap_or(f64::INFINITY)
}

/// The tree's folders and scans in display order, with their depth.
fn tree_rows(p: &Project) -> Vec<(usize, Uuid, String, bool)> {
    fn walk(
        p: &Project,
        parent: Option<Uuid>,
        depth: usize,
        rows: &mut Vec<(usize, Uuid, String, bool)>,
    ) {
        let (groups, scans) = p.children(parent);
        for g in groups {
            rows.push((depth, g.id, g.name.clone(), true));
            walk(p, Some(g.id), depth + 1, rows);
        }
        for s in scans {
            rows.push((depth, s.id, s.name.clone(), false));
        }
    }
    let mut rows = vec![];
    walk(p, None, 0, &mut rows);
    rows
}

/// A CSV field, quoted when it needs to be.
fn field(text: &str) -> String {
    if text.contains([',', '"', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.into()
    }
}

impl Workbench {
    /// For smoke tests: how many scans are aligned, and the last global
    /// adjustment's pairs.
    pub(super) fn registration_counts(&self) -> String {
        let Some(p) = &self.project else {
            return String::new();
        };
        let mut counts = [0; 3];
        for s in p.scans() {
            counts[status(p.scan_registration(s.id).as_ref()) as usize] += 1;
        }
        let global = self.align.last_global.as_ref().map_or(String::new(), |g| {
            let mean = |fits: &[geemil_core::PairFit]| {
                let rms: Vec<f64> = fits.iter().filter_map(|f| f.rms).collect();
                rms.iter().sum::<f64>() / rms.len().max(1) as f64
            };
            format!(
                ", global: {} moved, {} pairs, mean pair rms {:.4} -> {:.4} m",
                g.poses.len(),
                g.after.len(),
                mean(&g.before),
                mean(&g.after)
            )
        });
        format!(
            "done {}, check {}, none {}{global}",
            counts[0], counts[1], counts[2]
        )
    }
    /// A scan's or folder's mark after its name in the tree, if it has an
    /// alignment of its own.
    pub(super) fn registration_mark(&self, ui: &mut egui::Ui, p: &Project, id: Uuid) {
        let Some(state) = p.registration(id) else {
            return;
        };
        ui.label(status_icon(status(Some(&state))))
            .on_hover_text(describe(self.t, p, &state, id));
    }
    pub(super) fn registrations_window(&mut self, ctx: &egui::Context) {
        if !self.align.list_open {
            return;
        }
        let Some(p) = self.project.clone() else {
            return;
        };
        let t = self.t;
        let rows = tree_rows(&p);
        let mut open = true;
        let mut select = None;
        let mut export = false;
        egui::Window::new(format!("{} {}", icon::LIST_CHECKS, t.align_list))
            .open(&mut open)
            .default_size([680., 520.])
            // Clear of the tree, whose marks it lists.
            .default_pos([320., 120.])
            .show(ctx, |ui| {
                let (mut done, mut check, mut none) = (0, 0, 0);
                for s in p.scans() {
                    match status(p.scan_registration(s.id).as_ref()) {
                        Status::Done => done += 1,
                        Status::Check => check += 1,
                        Status::None => none += 1,
                    }
                }
                ui.label((t.align_list_summary)(done, check, none));
                if let Some(g) = &self.align.last_global {
                    egui::CollapsingHeader::new(t.align_global_last)
                        .default_open(true)
                        .show(ui, |ui| {
                            let name =
                                |id: &Uuid| p.scan(*id).map_or("?".into(), |s| s.name.clone());
                            let names =
                                |ids: &[Uuid]| ids.iter().map(name).collect::<Vec<_>>().join("、");
                            if let Some(last) = g.steps.last() {
                                let iterations = g.steps.iter().map(|s| s.iterations).sum();
                                ui.label((t.align_global_summary)(
                                    last.rms,
                                    last.distance,
                                    g.steps.len(),
                                    iterations,
                                ));
                            }
                            ui.label((t.align_global_fixed_list)(&names(&g.fixed)));
                            if !g.isolated.is_empty() {
                                ui.colored_label(
                                    CHECK,
                                    (t.align_global_isolated)(&names(&g.isolated)),
                                );
                            }
                            if let Some(at) = g.stopped_at {
                                let kept = g.steps.last().map_or(0., |s| s.distance);
                                ui.colored_label(CHECK, (t.align_icp_stopped)(at, kept));
                            }
                            // Worst first: the pairs to look at.
                            let mut pairs: Vec<_> = g
                                .after
                                .iter()
                                .map(|a| {
                                    let before = g.before.iter().find(|b| b.a == a.a && b.b == a.b);
                                    (a, before)
                                })
                                .collect();
                            pairs.sort_by(|x, y| worst(y.0).total_cmp(&worst(x.0)));
                            egui::ScrollArea::vertical()
                                .id_salt("global pairs")
                                .max_height(180.)
                                .show(ui, |ui| {
                                    egui::Grid::new("global pairs grid")
                                        .num_columns(5)
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for heading in t.align_global_pair_columns {
                                                ui.strong(*heading);
                                            }
                                            ui.end_row();
                                            for (after, before) in pairs {
                                                ui.label(name(&after.a));
                                                ui.label(name(&after.b));
                                                ui.label(format!("{:.0}%", after.overlap * 100.));
                                                ui.label(
                                                    before.map_or("—".into(), |b| metres(b.rms)),
                                                );
                                                ui.label(metres(after.rms));
                                                ui.end_row();
                                            }
                                        });
                                });
                        });
                    ui.separator();
                }
                egui::ScrollArea::vertical()
                    .id_salt("registrations")
                    .max_height(ui.available_height() - 36.)
                    .show(ui, |ui| {
                        egui::Grid::new("registrations grid")
                            .num_columns(8)
                            .striped(true)
                            .show(ui, |ui| {
                                for heading in t.align_list_columns {
                                    ui.strong(*heading);
                                }
                                ui.end_row();
                                for (depth, id, name, folder) in &rows {
                                    let shown = item_registration(&p, *id, *folder);
                                    let state = shown.as_ref();
                                    let mark = ui.label(status_icon(status(state)));
                                    if let Some(state) = state {
                                        mark.on_hover_text(describe(t, &p, state, *id));
                                    }
                                    let label = format!(
                                        "{}{} {}",
                                        "    ".repeat(*depth),
                                        if *folder {
                                            icon::FOLDER
                                        } else {
                                            icon::CUBE_TRANSPARENT
                                        },
                                        name
                                    );
                                    if ui
                                        .selectable_label(self.selected == Some(*id), label)
                                        .clicked()
                                    {
                                        select = Some(*id);
                                    }
                                    match state {
                                        Some(state) => {
                                            let fit = state.registration.fit;
                                            let inherited = state.item != *id;
                                            let method = method_name(t, fit.method);
                                            if inherited {
                                                ui.weak((t.registration_inherited)(method));
                                            } else {
                                                ui.label(method);
                                            }
                                            ui.label(format!("{:.4} m", fit.rms));
                                            ui.label(fit.overlap.map_or("—".into(), |o| {
                                                format!("{:.0}%", o * 100.)
                                            }));
                                            ui.label(
                                                fit.distance
                                                    .map_or("—".into(), |d| format!("{d:.3} m")),
                                            );
                                            ui.label(fit.references.to_string());
                                            ui.label(time(state.registration.at));
                                        }
                                        None => {
                                            for _ in 0..6 {
                                                ui.label("");
                                            }
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.separator();
                export = ui
                    .button(format!("{} {}", icon::FILE_CSV, t.align_list_export))
                    .clicked();
            });
        if !open {
            self.align.list_open = false;
        }
        if let Some(id) = select {
            self.select_tree_item(Some(id));
            self.reveal = Some(id);
        }
        if export {
            self.export_registrations(&p, &rows);
        }
    }
    fn export_registrations(&mut self, p: &Project, rows: &[(usize, Uuid, String, bool)]) {
        let t = self.t;
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("registration.csv")
            .save_file()
        else {
            return;
        };
        // A byte order mark so spreadsheet programs read the names as UTF-8.
        let mut csv = String::from("\u{feff}");
        let _ = writeln!(csv, "{}", t.align_csv_columns.join(","));
        for (_, id, name, folder) in rows {
            let state = item_registration(p, *id, *folder);
            let kind = t.align_csv_kinds[usize::from(!folder)];
            let status_name = t.align_csv_statuses[match status(state.as_ref()) {
                Status::Done => 0,
                Status::Check => 1,
                Status::None => 2,
            }];
            let _ = match state {
                Some(state) => {
                    let fit = state.registration.fit;
                    let source = p
                        .groups()
                        .iter()
                        .find(|g| g.id == state.item && state.item != *id)
                        .map_or(String::new(), |g| g.name.clone());
                    writeln!(
                        csv,
                        "{},{kind},{status_name},{},{:.5},{},{},{},{},{},{},{}",
                        field(name),
                        method_name(t, fit.method),
                        fit.rms,
                        fit.overlap
                            .map_or(String::new(), |o| format!("{:.1}", o * 100.)),
                        fit.distance.map_or(String::new(), |d| format!("{d:.4}")),
                        fit.references,
                        u8::from(state.moved),
                        u8::from(fit.stopped),
                        time(state.registration.at),
                        field(&source),
                    )
                }
                None => writeln!(csv, "{},{kind},{status_name}", field(name)),
            };
        }
        if let Some(g) = &self.align.last_global {
            let name = |id: &Uuid| p.scan(*id).map_or(String::new(), |s| s.name.clone());
            let _ = writeln!(csv);
            let _ = writeln!(csv, "{}", t.align_csv_pair_columns.join(","));
            for after in &g.after {
                let before = g.before.iter().find(|b| b.a == after.a && b.b == after.b);
                let _ = writeln!(
                    csv,
                    "{},{},{:.1},{},{}",
                    field(&name(&after.a)),
                    field(&name(&after.b)),
                    after.overlap * 100.,
                    before
                        .and_then(|b| b.rms)
                        .map_or(String::new(), |r| format!("{r:.5}")),
                    after.rms.map_or(String::new(), |r| format!("{r:.5}")),
                );
            }
        }
        match std::fs::write(&path, csv) {
            Ok(()) => self.status = (t.align_list_saved)(&path.display().to_string()),
            Err(e) => self.error = Some(super::jobs::Notice::new(t, &anyhow::Error::from(e))),
        }
    }
}
