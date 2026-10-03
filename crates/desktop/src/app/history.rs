use super::{Workbench, jobs::Notice};
use crate::i18n::Strings;
use eframe::egui;
use geemil_core::{Layer, Project, Revision};
use std::sync::Arc;
use uuid::Uuid;

impl Workbench {
    pub(super) fn history(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: &Arc<Project>) {
        let t = self.t;
        ui.heading(t.history);
        egui::ScrollArea::vertical()
            .max_height(230.)
            .show(ui, |ui| {
                for r in &p.manifest.revisions {
                    let depth = revision_depth(p, r.id);
                    let label = format!("{}{}", "  ".repeat(depth.min(8)), revision_label(t, p, r));
                    if ui
                        .add_enabled(
                            self.job.is_none(),
                            egui::Button::new(label).selected(r.id == p.manifest.current),
                        )
                        .clicked()
                    {
                        let mut project = (**p).clone();
                        match project.switch(r.id) {
                            Ok(()) => {
                                let fit = p.current().scans != project.current().scans;
                                self.install(project, fit);
                            }
                            Err(e) => self.error = Some(Notice::new(t, &e)),
                        }
                    }
                }
            });
        ui.text_edit_singleline(&mut self.branch_name);
        if ui
            .add_enabled(self.job.is_none(), egui::Button::new(t.fork))
            .clicked()
        {
            let mut project = (**p).clone();
            let name = self.branch_name.clone();
            self.start(ctx, move |_| {
                project.save_revision(name)?;
                Ok(project)
            });
        }
        ui.small(t.fork_hint);
    }
}

fn revision_depth(p: &Project, id: Uuid) -> usize {
    let mut depth = 0;
    let mut next = p
        .manifest
        .revisions
        .iter()
        .find(|r| r.id == id)
        .and_then(|r| r.parent);
    while let Some(id) = next {
        depth += 1;
        if depth > p.manifest.revisions.len() {
            break;
        }
        next = p
            .manifest
            .revisions
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.parent);
    }
    depth
}

/// The core stores English names; label from the recorded operation instead.
/// User-named revisions (forks, checkpoints) and unknown kinds keep their name.
fn revision_label(t: &Strings, p: &Project, r: &Revision) -> String {
    let op = &r.operation;
    let text = |key| op.get(key).and_then(|v| v.as_str());
    let id = |key| text(key).and_then(|s| Uuid::parse_str(s).ok());
    let layer = |id: Uuid| p.manifest.layers.iter().find(|l| l.id == id);
    let label = match text("kind") {
        Some("create") => Some(t.revision_created.into()),
        Some("import") => text("file").map(t.revision_import),
        // The layer created by a selection is appended to that revision's layers.
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
    label.unwrap_or_else(|| r.name.clone())
}

// Every layer is a manual exclusion until filters add their own layer kinds.
pub(super) fn layer_label(t: &Strings, layer: &Layer) -> String {
    (t.manual_exclusion_layer)(&t.count(layer.excluded))
}
