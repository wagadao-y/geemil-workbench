use super::Workbench;
use eframe::egui;

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
                } else {
                    ui.label((t.view_stats)(
                        &t.mega_points(self.points.len()),
                        self.view_ms,
                    ));
                }
            });
            let mut dismissed = false;
            if let Some(error) = &self.error {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::LIGHT_RED, &error.message);
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
