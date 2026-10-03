//! The 3D box tool: a box turned about the vertical axis that can limit the
//! display to its inside (a live section) and move the points inside or
//! outside it to another layer, judged on original points like selections.
use super::{
    Workbench,
    layers::{destination, destination_combo},
    selection::Tool,
};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::CropBox;
use glam::DVec3;

const EDGE: egui::Color32 = egui::Color32::from_rgb(255, 220, 80);

#[derive(Default)]
pub(super) struct Crop {
    /// None until the tool is first used; then placed at the view centre.
    pub(super) region: Option<CropBox>,
    /// Draw only what is inside the box.
    pub(super) clip: bool,
    /// The layer moves go to; none for the "deleted" layer.
    destination: Option<u8>,
}

impl Workbench {
    /// The box the display is limited to, if any.
    pub(super) fn display_clip(&self) -> Option<CropBox> {
        self.crop
            .region
            .filter(|_| self.crop.clip && self.project.is_some())
    }
    /// A box around the view centre, sized to the view.
    fn place_box(&mut self) {
        let h = self.camera.half_height();
        self.crop.region = Some(CropBox {
            center: self.camera.target,
            size: [h, h, h],
            yaw: 0.,
        });
    }
    pub(super) fn crop_panel(&mut self, ui: &mut egui::Ui) {
        if self.selection.tool != Tool::Box || self.project.is_none() {
            return;
        }
        if self.crop.region.is_none() {
            self.place_box();
        }
        let t = self.t;
        let ctx = ui.ctx().clone();
        let idle = self.job.is_none();
        egui::Panel::right("crop")
            .resizable(true)
            .default_size(280.)
            .show(ui, |ui| {
                ui.strong(format!("{} {}", icon::CUBE_FOCUS, t.box_title));
                ui.small(t.box_hint);
                ui.add_space(4.);
                let Some(region) = &mut self.crop.region else {
                    return;
                };
                let mut yaw = region.yaw.to_degrees();
                egui::Grid::new("box").num_columns(4).show(ui, |ui| {
                    ui.label(t.box_center);
                    for v in &mut region.center {
                        ui.add(egui::DragValue::new(v).speed(0.01).max_decimals(3));
                    }
                    ui.end_row();
                    ui.label(t.box_size);
                    for v in &mut region.size {
                        ui.add(
                            egui::DragValue::new(v)
                                .speed(0.01)
                                .range(0.001..=1e6)
                                .max_decimals(3),
                        );
                    }
                    ui.end_row();
                    ui.label(t.box_yaw);
                    ui.add(egui::DragValue::new(&mut yaw).speed(0.2).max_decimals(2));
                    ui.end_row();
                });
                region.yaw = yaw.to_radians();
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("{} {}", icon::CROSSHAIR_SIMPLE, t.box_to_view))
                        .on_hover_text(t.box_to_view_hint)
                        .clicked()
                    {
                        let target = self.camera.target;
                        if let Some(region) = &mut self.crop.region {
                            region.center = target;
                        }
                    }
                    if ui.button(t.box_reset).clicked() {
                        self.place_box();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(t.box_thickness);
                    for preset in [0.05, 0.1, 0.5, 1.0] {
                        if ui.small_button(format!("{preset} m")).clicked()
                            && let Some(region) = &mut self.crop.region
                        {
                            region.size[2] = preset;
                        }
                    }
                });
                if ui.checkbox(&mut self.crop.clip, t.box_clip).changed() {
                    self.dirty = true;
                }
                ui.separator();
                ui.small((t.filter_targets)(self.visible.len()));
                if let Some(p) = &self.project {
                    ui.horizontal(|ui| {
                        destination_combo(
                            ui,
                            t,
                            p,
                            "box destination",
                            t.layer_deleted,
                            &mut self.crop.destination,
                        );
                    });
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            idle,
                            egui::Button::new(format!(
                                "{} {}",
                                icon::SCISSORS,
                                t.box_exclude_outside
                            )),
                        )
                        .clicked()
                    {
                        self.exclude_box(&ctx, false);
                    }
                    if ui
                        .add_enabled(
                            idle,
                            egui::Button::new(format!(
                                "{} {}",
                                icon::ARROW_BEND_DOWN_RIGHT,
                                t.box_exclude_inside
                            )),
                        )
                        .clicked()
                    {
                        self.exclude_box(&ctx, true);
                    }
                });
            });
    }
    pub(super) fn exclude_box(&mut self, ctx: &egui::Context, inside: bool) {
        let (Some(region), Some(p)) = (self.crop.region, &self.project) else {
            return;
        };
        let target = destination(p, self.crop.destination, self.t.layer_deleted);
        let mut project = (**p).clone();
        let ids: Vec<_> = self.visible.iter().copied().collect();
        self.start(ctx, true, move |job| {
            project.move_box(&region, inside, &ids, &target, &job)?;
            Ok(project)
        });
    }
    /// The box's edges, while the tool is active or the display is clipped.
    pub(super) fn draw_box(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some(region) = self.crop.region else {
            return;
        };
        if self.selection.tool != Tool::Box && !self.crop.clip {
            return;
        }
        let corners = region.corners();
        let projector = self.camera.projector();
        let screen = |p: DVec3| {
            projector.project(p).map(|(uv, _)| {
                rect.left_top()
                    + egui::vec2(uv[0] as f32 * rect.width(), uv[1] as f32 * rect.height())
            })
        };
        // In perspective, cut edges at a plane just in front of the eye so
        // parts behind the camera do not hide the rest of the edge.
        let depth = self.camera.depth_row();
        let near = if self.camera.ortho {
            f64::NEG_INFINITY
        } else {
            self.camera.distance * 1e-3
        };
        let clip_edge = |a: DVec3, b: DVec3| -> Option<(DVec3, DVec3)> {
            let (da, db) = (depth.dot(a.extend(1.)), depth.dot(b.extend(1.)));
            match (da >= near, db >= near) {
                (true, true) => Some((a, b)),
                (false, false) => None,
                (true, false) => Some((a, a + (b - a) * ((da - near) / (da - db)))),
                (false, true) => Some((b + (a - b) * ((db - near) / (db - da)), b)),
            }
        };
        let painter = ui.painter_at(rect);
        for a in 0..8usize {
            for bit in [1, 2, 4] {
                let b = a | bit;
                if b == a {
                    continue;
                }
                let Some((ea, eb)) = clip_edge(corners[a], corners[b]) else {
                    continue;
                };
                if let (Some(pa), Some(pb)) = (screen(ea), screen(eb)) {
                    painter.line_segment([pa, pb], egui::Stroke::new(3., egui::Color32::BLACK));
                    painter.line_segment([pa, pb], egui::Stroke::new(1.5, EDGE));
                }
            }
        }
    }
    /// For smoke tests: a 2 m slice at the median height of the displayed points.
    pub(super) fn smoke_box(&mut self) {
        self.selection.tool = Tool::Box;
        self.place_box();
        let mut heights: Vec<f64> = self.points.iter().map(|p| p.position[2]).collect();
        heights.sort_by(f64::total_cmp);
        let median = heights.get(heights.len() / 2).copied();
        if let Some(region) = &mut self.crop.region {
            region.size = [region.size[0] * 3., region.size[1] * 3., 2.];
            if let Some(z) = median {
                region.center[2] = z;
            }
        }
        self.crop.clip = true;
        self.dirty = true;
    }
}
