use super::ColorMode;
use super::Workbench;
use crate::render::{DrawOptions, Edl};
use eframe::egui;
use glam::DVec3;

impl Workbench {
    pub(super) fn viewport(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let ctx = &ui.ctx().clone();
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.advance_flight(ctx);
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
            let segments = self.draw_segments();
            let rs = frame.wgpu_render_state().unwrap();
            let settings = &self.settings;
            let id = self.renderer.draw(
                rs,
                &self.camera,
                [(size.x * pixels) as u32, (size.y * pixels) as u32],
                &DrawOptions {
                    point_size: settings.point_size * pixels,
                    edl: Edl {
                        radius: 1.4 * pixels,
                        strength: if settings.edl {
                            settings.edl_strength
                        } else {
                            0.
                        },
                    },
                    segments: segments.as_deref(),
                    clip: self.display_clip().map(|c| c.unit_matrix()),
                    height_ramp: (self.settings.color_mode == ColorMode::Height)
                        .then_some(self.height_range)
                        .flatten(),
                },
            );
            let response =
                ui.add(egui::Image::new((id, size)).sense(egui::Sense::click_and_drag()));
            self.smoke_probes(response.rect);
            self.camera_input(ctx, &response);
            if self.job.is_none() {
                self.selection_input(&response);
                self.measure_input(&response);
                self.align_input(&response);
            }
            self.draw_selection(ui, &response);
            self.draw_measure(ui, response.rect);
            self.draw_align(ui, response.rect);
            self.draw_box(ui, response.rect);
            self.draw_pivot(ui, &response);
        });
    }
}
