use super::Workbench;
use eframe::egui;
use geemil_core::Selection;
use glam::DVec3;

impl Workbench {
    pub(super) fn viewport(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.edit_tools(ui, ctx);
            self.display_settings(ui);
            ui.small(self.t.controls_hint);
            let size = ui.available_size().max(egui::vec2(1., 1.));
            let aspect = (size.x / size.y) as f64;
            if (self.camera.aspect - aspect).abs() > 1e-6 {
                self.camera.aspect = aspect;
                self.dirty = true;
            }
            self.renderer.upload(
                &self.points,
                DVec3::from(self.points_origin),
                self.points_generation,
            );
            let pixels = ctx.pixels_per_point();
            let rs = frame.wgpu_render_state().unwrap();
            let id = self.renderer.draw(
                rs,
                &self.camera,
                [(size.x * pixels) as u32, (size.y * pixels) as u32],
                self.point_size * pixels,
            );
            let response =
                ui.add(egui::Image::new((id, size)).sense(egui::Sense::click_and_drag()));
            self.smoke_probes(response.rect);
            self.camera_input(ctx, &response);
            self.selection_input(&response);
            self.draw_selection(ui, response.rect);
        });
    }
    fn edit_tools(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let t = self.t;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.select_mode, false, t.navigate);
            ui.selectable_value(&mut self.select_mode, true, t.select);
            ui.checkbox(&mut self.lasso, t.polygon);
            ui.label(t.depth);
            ui.add(
                egui::DragValue::new(&mut self.depth)
                    .speed(0.05)
                    .range(0.001..=1_000_000.)
                    .suffix(" m"),
            );
            if ui
                .add_enabled(
                    self.job.is_none() && self.polygon.len() >= 3,
                    egui::Button::new(t.exclude_selection),
                )
                .clicked()
            {
                self.delete(ctx);
            }
            if ui.button(t.clear_selection).clicked() {
                self.polygon.clear();
            }
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
        });
    }
    /// Orbit (left, outside selection mode), pan (right/middle) and zoom (wheel).
    /// Any camera change drops the selection, which is tied to the camera.
    fn camera_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        if !self.select_mode && response.dragged_by(egui::PointerButton::Primary) {
            let delta = ctx.input(|i| i.pointer.delta());
            self.camera.yaw -= delta.x as f64 * 0.007;
            self.camera.pitch = (self.camera.pitch + delta.y as f64 * 0.007).clamp(-1.5, 1.5);
            self.dirty = true;
            self.polygon.clear();
        }
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            let delta = ctx.input(|i| i.pointer.delta());
            let forward = (DVec3::from(self.camera.target) - self.camera.eye()).normalize();
            let right = forward.cross(DVec3::Z).normalize();
            let up = right.cross(forward);
            self.camera.target = (DVec3::from(self.camera.target)
                + (right * (-delta.x as f64) + up * delta.y as f64) * self.camera.distance
                    / response.rect.height() as f64)
                .to_array();
            self.dirty = true;
            self.polygon.clear();
        }
        if response.hovered() {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0. {
                self.camera.distance =
                    (self.camera.distance * (-scroll as f64 * 0.003).exp()).clamp(0.001, 1e10);
                self.dirty = true;
                self.polygon.clear();
            }
        }
    }
    /// Rectangle drag or polygon clicks, stored in normalized viewport coordinates.
    fn selection_input(&mut self, response: &egui::Response) {
        if !self.select_mode || self.job.is_some() {
            return;
        }
        let rect = response.rect;
        let normalize = |p: egui::Pos2| {
            egui::pos2(
                ((p.x - rect.left()) / rect.width()).clamp(0., 1.),
                ((p.y - rect.top()) / rect.height()).clamp(0., 1.),
            )
        };
        if self.lasso {
            if response.clicked_by(egui::PointerButton::Primary)
                && let Some(pos) = response.interact_pointer_pos()
            {
                if self.polygon.is_empty() {
                    self.selection_camera = Some(self.camera);
                }
                self.polygon.push(normalize(pos));
            }
            return;
        }
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.drag_start = Some(normalize(pos));
            self.selection_camera = Some(self.camera);
            self.polygon.clear();
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && let (Some(start), Some(pos)) = (self.drag_start, response.interact_pointer_pos())
        {
            let end = normalize(pos);
            self.polygon = vec![
                start,
                egui::pos2(end.x, start.y),
                end,
                egui::pos2(start.x, end.y),
            ];
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            self.drag_start = None;
        }
    }
    fn draw_selection(&self, ui: &egui::Ui, rect: egui::Rect) {
        if self.polygon.len() < 2 {
            return;
        }
        let points: Vec<_> = self
            .polygon
            .iter()
            .map(|p| {
                egui::pos2(
                    rect.left() + p.x * rect.width(),
                    rect.top() + p.y * rect.height(),
                )
            })
            .collect();
        ui.painter().add(egui::Shape::closed_line(
            points,
            egui::Stroke::new(2., egui::Color32::from_rgb(80, 220, 190)),
        ));
    }
    /// Excludes the selection from the visible scans, judged on original points
    /// with the camera captured when the selection started.
    fn delete(&mut self, ctx: &egui::Context) {
        if self.polygon.len() < 3 || self.job.is_some() {
            return;
        }
        if let Some(p) = &self.project {
            let mut project = (**p).clone();
            let selection = Selection {
                camera: self.selection_camera.unwrap_or(self.camera),
                polygon: self
                    .polygon
                    .iter()
                    .map(|p| [p.x as f64, p.y as f64])
                    .collect(),
                depth_meters: self.depth,
            };
            let ids = self.visible.iter().copied().collect::<Vec<_>>();
            self.start(ctx, move |job| {
                project.delete_selection(&selection, &ids, &job)?;
                Ok(project)
            });
        }
    }
}
