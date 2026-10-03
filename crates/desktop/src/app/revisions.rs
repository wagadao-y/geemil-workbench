//! The revision list: a tree of saved revisions to open, rename or delete.
use super::{
    Workbench,
    dialogs::{AfterDiscard, Dialog},
    jobs::Notice,
};
use crate::i18n::Strings;
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{Layer, Project, Revision};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Default)]
pub(super) struct RevisionsState {
    selected: Option<Uuid>,
    renaming: Option<(Uuid, String)>,
    /// Delete needs a second click.
    confirm_delete: Option<Uuid>,
}
impl RevisionsState {
    pub(super) fn new(project: Option<&Project>) -> Self {
        Self {
            selected: project.map(|p| p.manifest.current),
            ..Default::default()
        }
    }
}

/// Revisions depth first, each after its parent, oldest first among siblings.
fn tree_order(revisions: &[Revision]) -> Vec<(usize, &Revision)> {
    let ids: Vec<_> = revisions.iter().map(|r| r.id).collect();
    let mut children: BTreeMap<Option<Uuid>, Vec<&Revision>> = BTreeMap::new();
    for r in revisions {
        let parent = r.parent.filter(|p| ids.contains(p));
        children.entry(parent).or_default().push(r);
    }
    let mut out = vec![];
    let mut stack: Vec<_> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|r| (0, *r))
        .collect();
    while let Some((depth, r)) = stack.pop() {
        out.push((depth, r));
        if out.len() > revisions.len() {
            break;
        }
        for child in children.get(&Some(r.id)).into_iter().flatten().rev() {
            stack.push((depth + 1, child));
        }
    }
    out
}

/// A revision's title: the user's name, or for revisions from versions that
/// committed every edit, a label from the recorded operation.
pub(super) fn revision_title(t: &Strings, p: &Project, r: &Revision) -> String {
    let op = &r.operation;
    let text = |key| op.get(key).and_then(|v| v.as_str());
    let id = |key| text(key).and_then(|s| Uuid::parse_str(s).ok());
    let layer = |id: Uuid| p.manifest.layers.iter().find(|l| l.id == id);
    let legacy = match text("kind") {
        Some("edits") => None,
        Some("create") => Some(t.revision_created.into()),
        Some("import") => text("file").map(t.revision_import),
        Some("selection") => r.layers.last().and_then(|id| layer(*id)).map(|l| {
            let crop = op["selection"]["mode"] == "exclude_outside";
            let label = if crop {
                t.revision_crop
            } else {
                t.revision_exclude
            };
            label(&t.count(l.excluded))
        }),
        Some("layer") => id("id").and_then(layer).map(|l| {
            let enabled = op.get("enabled").and_then(|v| v.as_bool());
            (t.revision_layer)(&layer_label(t, l), enabled.unwrap_or(true))
        }),
        Some("transform") => id("scan")
            .and_then(|id| p.manifest.scans.iter().find(|s| s.id == id))
            .map(|s| (t.revision_transform)(&s.name)),
        _ => None,
    };
    legacy.unwrap_or_else(|| r.name.clone())
}

/// Counts of the edits a revision recorded, e.g. "取り込み ×1・除外 ×3".
fn revision_summary(t: &Strings, r: &Revision) -> String {
    let ops = match r.operation.get("operations").and_then(|o| o.as_array()) {
        Some(ops) => ops.iter().collect(),
        None if r.operation.get("kind").and_then(|k| k.as_str()) == Some("create") => vec![],
        None => vec![&r.operation],
    };
    let mut counts: Vec<(&str, usize)> = vec![];
    for op in ops {
        let kind = (t.operation_kind)(op.get("kind").and_then(|k| k.as_str()).unwrap_or(""));
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
        .iter()
        .map(|(kind, n)| format!("{kind} ×{n}"))
        .collect::<Vec<_>>()
        .join("・")
}

pub(super) fn saved_time(r: &Revision) -> String {
    r.saved_at
        .and_then(|s| chrono::DateTime::from_timestamp(s as i64, 0))
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub(super) fn layer_label(t: &Strings, layer: &Layer) -> String {
    (t.exclusion_layer)(&t.count(layer.excluded))
}

impl Workbench {
    /// Shows the revision list. Returns whether it stays open.
    pub(super) fn revisions_dialog(
        &mut self,
        ctx: &egui::Context,
        state: &mut RevisionsState,
    ) -> bool {
        let t = self.t;
        let Some(project) = self.project.clone() else {
            return false;
        };
        let current = project.manifest.current;
        let draft = project.manifest.draft.as_ref();
        let mut keep = true;
        let mut open = None;
        let mut delete = None;
        let mut rename = None;
        let modal = egui::Modal::new(egui::Id::new("revisions")).show(ctx, |ui| {
            ui.set_width(640.);
            ui.heading(format!("{} {}", icon::GIT_BRANCH, t.revisions_title));
            ui.label(t.revisions_lead);
            ui.add_space(4.);
            egui::ScrollArea::vertical()
                .max_height(380.)
                .show(ui, |ui| {
                    for (depth, r) in tree_order(&project.manifest.revisions) {
                        ui.horizontal(|ui| {
                            ui.add_space(depth as f32 * 18.);
                            let selected = state.selected == Some(r.id);
                            if let Some((id, name)) = &mut state.renaming
                                && *id == r.id
                            {
                                let edit = ui.text_edit_singleline(name);
                                edit.request_focus();
                                if edit.lost_focus() {
                                    rename = Some((r.id, name.clone()));
                                }
                                return;
                            }
                            let marker = if r.id == current {
                                icon::EYE
                            } else {
                                icon::GIT_COMMIT
                            };
                            let title = format!("{marker} {}", revision_title(t, &project, r));
                            let row = ui.selectable_label(selected, title);
                            if row.clicked() {
                                state.selected = Some(r.id);
                                state.confirm_delete = None;
                            }
                            if row.double_clicked() {
                                open = Some(r.id);
                            }
                            if r.id == current {
                                ui.small(egui::RichText::new(t.shown_revision).strong());
                            }
                            ui.small(saved_time(r));
                            ui.small(revision_summary(t, r));
                        });
                        if let Some(draft) = draft
                            && r.id == current
                        {
                            ui.horizontal(|ui| {
                                ui.add_space((depth + 1) as f32 * 18.);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} {}",
                                        icon::PENCIL_SIMPLE,
                                        t.unsaved_state
                                    ))
                                    .italics(),
                                );
                                ui.small(revision_summary(t, draft));
                            });
                        }
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                let selected = state.selected;
                let can_open = selected.is_some_and(|id| id != current || draft.is_some());
                if ui
                    .add_enabled(
                        can_open,
                        egui::Button::new(format!("{} {}", icon::FOLDER_OPEN, t.open_revision)),
                    )
                    .clicked()
                {
                    open = selected;
                }
                if ui
                    .add_enabled(
                        selected.is_some(),
                        egui::Button::new(format!("{} {}", icon::PENCIL_SIMPLE, t.rename)),
                    )
                    .clicked()
                    && let Some(id) = selected
                {
                    let r = project.manifest.revisions.iter().find(|r| r.id == id);
                    let name = r
                        .map(|r| revision_title(t, &project, r))
                        .unwrap_or_default();
                    state.renaming = Some((id, name));
                }
                let confirming = state.confirm_delete.is_some() && state.confirm_delete == selected;
                let label = if confirming {
                    format!("{} {}?", icon::TRASH, t.delete_revision)
                } else {
                    format!("{} {}", icon::TRASH, t.delete_revision)
                };
                if ui
                    .add_enabled(
                        selected.is_some_and(|id| id != current),
                        egui::Button::new(label),
                    )
                    .clicked()
                {
                    if confirming {
                        delete = selected;
                        state.confirm_delete = None;
                    } else {
                        state.confirm_delete = selected;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let close = if draft.is_some() {
                        t.continue_unsaved
                    } else {
                        t.close
                    };
                    if ui.button(close).clicked() {
                        keep = false;
                    }
                });
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if let Some((id, name)) = rename {
            state.renaming = None;
            self.update_project(|p| p.rename_revision(id, name));
        }
        if let Some(id) = delete {
            self.update_project(|p| p.delete_revision(id));
            state.selected = Some(current);
        }
        if let Some(id) = open {
            if project.has_unsaved_changes() {
                self.dialog = Some(Dialog::Discard {
                    then: AfterDiscard::Switch(id),
                });
                return false;
            }
            self.switch_revision(id);
            keep = false;
        }
        if !keep {
            self.dialog = None;
        }
        keep
    }
    /// Applies a change that is not an edit of the working state (no undo).
    fn update_project(&mut self, change: impl FnOnce(&mut Project) -> anyhow::Result<()>) {
        let Some(project) = &self.project else { return };
        let mut project = (**project).clone();
        match change(&mut project) {
            Ok(()) => self.install(project, false),
            Err(e) => self.error = Some(Notice::new(self.t, &e)),
        }
    }
}
