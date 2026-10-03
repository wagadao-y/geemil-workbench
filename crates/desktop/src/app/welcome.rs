//! The start screen shown while no project is open.
use super::{Workbench, actions::Action};
use eframe::egui;
use egui_phosphor::regular as icon;

impl Workbench {
    pub(super) fn welcome(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let mut chosen = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.15).max(16.));
                ui.label(
                    egui::RichText::new(format!("{} Geemil Workbench", icon::CUBE_TRANSPARENT))
                        .size(30.)
                        .strong(),
                );
                ui.label(egui::RichText::new(t.welcome_lead).size(15.).weak());
                ui.add_space(20.);
                ui.horizontal(|ui| {
                    // Centre the two buttons.
                    let width = 2. * 220. + ui.spacing().item_spacing.x;
                    ui.add_space(((ui.available_width() - width) / 2.).max(0.));
                    for action in [Action::NewProject, Action::Open] {
                        let text =
                            egui::RichText::new(format!("{}  {}", action.icon(), action.label(t)))
                                .size(16.);
                        if ui
                            .add_enabled(
                                self.enabled(&action),
                                egui::Button::new(text).min_size(egui::vec2(220., 44.)),
                            )
                            .clicked()
                        {
                            chosen = Some(action);
                        }
                    }
                });
                ui.add_space(24.);
                if !self.settings.recent.is_empty() {
                    ui.label(egui::RichText::new(t.recent_projects).strong());
                    ui.add_space(4.);
                    for path in self.settings.recent.clone() {
                        let exists = path.join("project.json").is_file();
                        let name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let button = egui::Button::new(format!("{}  {name}", icon::FOLDER))
                            .min_size(egui::vec2(460., 0.))
                            .right_text(
                                egui::RichText::new(path.display().to_string())
                                    .small()
                                    .weak(),
                            );
                        if ui.add_enabled(exists, button).clicked() {
                            chosen = Some(Action::OpenRecent(path));
                        }
                    }
                }
                ui.add_space(24.);
                ui.weak(format!("{}  {}", icon::FILE_ARROW_DOWN, t.drop_hint));
            });
        });
        if let Some(action) = chosen {
            let ctx = ui.ctx().clone();
            self.perform(&ctx, action);
        }
    }
}
