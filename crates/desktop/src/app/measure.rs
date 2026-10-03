//! Distance measurement between two picked points.
use super::{Workbench, navigation::pick, selection::Tool};
use eframe::egui;
use glam::DVec3;

#[derive(Default)]
pub(super) struct Measure {
    points: Vec<DVec3>,
}
impl Measure {
    pub(super) fn clear(&mut self) {
        self.points.clear();
    }
    /// 3D distance, horizontal distance and height difference (second minus first).
    pub(super) fn distance(&self) -> Option<(f64, f64, f64)> {
        let [a, b] = self.points[..] else {
            return None;
        };
        let d = b - a;
        Some((d.length(), d.truncate().length(), d.z))
    }
}

impl Workbench {
    pub(super) fn measure_points(&mut self, a: DVec3, b: DVec3) {
        self.measure.points = vec![a, b];
    }
    pub(super) fn measure_distance(&self) -> Option<(f64, f64, f64)> {
        self.measure.distance()
    }
    pub(super) fn measure_input(&mut self, response: &egui::Response) {
        if self.selection.tool != Tool::Measure || !response.clicked() {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        let rect = response.rect;
        let click = [
            ((pos.x - rect.left()) / rect.width()) as f64,
            ((pos.y - rect.top()) / rect.height()) as f64,
        ];
        let viewport = [rect.width() as f64, rect.height() as f64];
        let radius = self.settings.point_size as f64 / 2.;
        if let Some(point) = pick(&self.points, &self.camera, click, viewport, radius) {
            if self.measure.points.len() >= 2 {
                self.measure.points.clear();
            }
            self.measure.points.push(point);
        }
    }
    pub(super) fn draw_measure(&self, ui: &egui::Ui, rect: egui::Rect) {
        if self.measure.points.is_empty() {
            return;
        }
        let projector = self.camera.projector();
        let screen: Vec<_> = self
            .measure
            .points
            .iter()
            .map(|p| {
                projector.project(*p).map(|(uv, _)| {
                    rect.left_top()
                        + egui::vec2(uv[0] as f32 * rect.width(), uv[1] as f32 * rect.height())
                })
            })
            .collect();
        let painter = ui.painter_at(rect);
        let color = egui::Color32::from_rgb(255, 210, 60);
        if let [Some(a), Some(b)] = screen[..] {
            painter.line_segment([a, b], egui::Stroke::new(4., egui::Color32::BLACK));
            painter.line_segment([a, b], egui::Stroke::new(2., color));
            if let Some((d, _, _)) = self.measure.distance() {
                let text = format!("{d:.3} m");
                let at = a + (b - a) * 0.5 + egui::vec2(0., -14.);
                let galley = painter.layout_no_wrap(text, egui::FontId::proportional(15.), color);
                let bg = egui::Rect::from_center_size(at, galley.size() + egui::vec2(10., 4.));
                painter.rect_filled(bg, 3., egui::Color32::from_black_alpha(180));
                painter.galley(bg.center() - galley.size() / 2., galley, color);
            }
        }
        for p in screen.into_iter().flatten() {
            painter.circle(p, 5., color, egui::Stroke::new(1.5, egui::Color32::BLACK));
        }
    }
}
