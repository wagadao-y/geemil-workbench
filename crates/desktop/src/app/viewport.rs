use super::Workbench;
use crate::render::Edl;
use eframe::egui;
use glam::DVec3;

impl Workbench {
    pub(super) fn viewport(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let ctx = &ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            self.advance_flight(ctx);
            self.selection_tools(ui, ctx);
            self.display_settings(ui);
            ui.small(self.t.controls_hint);
            let size = ui.available_size().max(egui::vec2(1., 1.));
            let aspect = (size.x / size.y) as f64;
            if (self.camera.aspect - aspect).abs() > 1e-6 {
                self.camera.aspect = aspect;
                self.dirty = true;
            }
            self.update_preview(ctx);
            self.renderer.upload(
                &self.points,
                DVec3::from(self.points_origin),
                self.points_generation,
                self.selection.marks(),
            );
            let pixels = ctx.pixels_per_point();
            let rs = frame.wgpu_render_state().unwrap();
            let id = self.renderer.draw(
                rs,
                &self.camera,
                [(size.x * pixels) as u32, (size.y * pixels) as u32],
                self.point_size * pixels,
                Edl {
                    radius: 1.4 * pixels,
                    strength: if self.edl { self.edl_strength } else { 0. },
                },
            );
            let response =
                ui.add(egui::Image::new((id, size)).sense(egui::Sense::click_and_drag()));
            self.smoke_probes(response.rect);
            self.camera_input(ctx, &response);
            self.selection_input(ctx, &response);
            self.draw_selection(ui, &response);
            self.draw_pivot(ui, &response);
        });
    }
    fn display_settings(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        ui.horizontal(|ui| {
            ui.label(t.point_budget);
            if ui
                .add(
                    egui::DragValue::new(&mut self.point_budget)
                        .speed(1000)
                        .range(2048..=2_000_000)
                        .custom_formatter(|n, _| t.count(n as u64))
                        .custom_parser(|s| {
                            s.replace(t.thousands_separator, "").trim().parse().ok()
                        }),
                )
                .changed()
            {
                self.view.invalidate();
                self.dirty = true;
            }
            ui.label(t.point_size);
            ui.add(egui::Slider::new(&mut self.point_size, 1.0..=8.0));
            ui.checkbox(&mut self.edl, t.edl);
            ui.add_enabled(
                self.edl,
                egui::Slider::new(&mut self.edl_strength, 0.1..=5.0).text(t.edl_strength),
            );
        });
    }
}
