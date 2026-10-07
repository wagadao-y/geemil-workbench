use super::ColorMode;
use super::Workbench;
use super::actions::Action;
use crate::render::{DrawOptions, Edl};
use eframe::egui;

impl Workbench {
    /// Handles the viewport's input first and then draws it, so a camera
    /// move shows in the frame it was made in rather than the next.
    pub(super) fn viewport(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let ctx = &ui.ctx().clone();
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.advance_flight(ctx);
            let size = ui.available_size().max(egui::vec2(1., 1.));
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
            // Picks read the last frame drawn, which is what was clicked.
            if self.job.is_none() {
                self.gizmo_input(&response);
                self.crop_input(&response);
            }
            self.camera_input(ctx, &response);
            if self.job.is_none() {
                self.selection_input(&response);
                self.measure_input(&response);
                self.align_input(&response);
                self.panorama_input(&response);
            }
            let aspect = (size.x / size.y) as f64;
            if (self.camera.aspect - aspect).abs() > 1e-6 {
                self.camera.aspect = aspect;
                self.dirty = true;
            }
            self.update_preview(ctx);
            let pixels = ctx.pixels_per_point();
            self.pixels_per_point = pixels;
            let mut renderer = self.renderer.take().expect("renderer");
            renderer.set_point_limit(self.settings.point_budget.saturating_mul(2));
            let nodes = self.draw_nodes();
            let lines = self.box_lines(pixels);
            let rs = frame.wgpu_render_state().unwrap();
            let settings = &self.settings;
            let id = renderer.draw(
                rs,
                &self.camera,
                [(size.x * pixels) as u32, (size.y * pixels) as u32],
                &DrawOptions {
                    point_size: settings.point_size * pixels,
                    // Potree's size 1 matches the default fixed size of 2.
                    adaptive: settings.adaptive_size.then_some(settings.point_size / 2.),
                    min_size: pixels,
                    edl: Edl {
                        radius: 1.4 * pixels,
                        strength: if settings.edl {
                            settings.edl_strength
                        } else {
                            0.
                        },
                    },
                    nodes: &nodes,
                    lines: &lines,
                    highlight_box: self.display_highlight_box().map(|c| c.unit_matrix()),
                    height_ramp: (self.settings.color_mode == ColorMode::Height)
                        .then_some(self.height_range)
                        .flatten(),
                },
            );
            let points = nodes.iter().map(|n| n.points.len()).sum();
            drop(nodes);
            self.drawn = self.nodes.clone();
            self.smoke.view_rendered(renderer.pending(), points);
            self.renderer = Some(renderer);
            let uv = egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.));
            ui.painter().image(id, rect, uv, egui::Color32::WHITE);
            self.smoke_probes(rect);
            self.draw_selection(ui, &response);
            self.draw_measure(ui, rect);
            self.draw_align(ui, rect);
            self.draw_panorama(ui, rect);
            self.draw_box(ui, rect);
            self.draw_gizmo(ui, rect);
            self.draw_pivot(ui, &response);
            self.draw_orientation(ui, rect);
            self.empty_hint(ui, rect);
        });
    }
    /// Tells how to add points while the project has none.
    fn empty_hint(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let empty = self
            .project
            .as_ref()
            .is_some_and(|p| p.scans().next().is_none());
        if !empty || self.job.is_some() {
            return;
        }
        let t = self.t;
        let action = Action::Import;
        let mut clicked = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((rect.height() * 0.35).max(16.));
                ui.label(egui::RichText::new(t.empty_project).size(15.));
                ui.add_space(12.);
                let text = egui::RichText::new(format!("{}  {}", action.icon(), action.label(t)))
                    .size(16.);
                clicked = ui
                    .add(egui::Button::new(text).min_size(egui::vec2(220., 44.)))
                    .clicked();
            });
        });
        if clicked {
            let ctx = ui.ctx().clone();
            self.perform(&ctx, action);
        }
    }
}
