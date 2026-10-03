use super::{Workbench, revisions::revision_title};
use eframe::egui;
use egui_phosphor::regular as icon;

impl Workbench {
    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if let Some(job) = &self.job {
                    ui.add(egui::ProgressBar::new(self.progress).desired_width(160.));
                    if ui.button(t.cancel).clicked() {
                        job.cancel();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.project.is_none() {
                        return;
                    }
                    ui.label((t.view_stats)(
                        &t.mega_points(self.shown_count()),
                        self.view_ms,
                    ));
                    ui.separator();
                    if let Some(p) = &self.project {
                        if p.has_unsaved_changes() {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                format!("{} {}", icon::PENCIL_SIMPLE, t.status_unsaved),
                            );
                        }
                        let name = revision_title(t, p.base());
                        ui.label(format!(
                            "{} {}",
                            icon::GIT_BRANCH,
                            (t.status_revision)(&name)
                        ));
                    }
                });
            });
            let mut dismissed = false;
            if let Some(error) = &self.error {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        format!("{} {}", icon::WARNING, error.message),
                    );
                    dismissed = ui.small_button(t.dismiss).clicked();
                });
                if let Some(detail) = &error.detail {
                    ui.collapsing(t.details, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(detail).small()).selectable(true),
                        );
                    });
                }
            }
            if dismissed {
                self.error = None;
            }
        });
    }
}
