//! Camera input: orbit, pan, zoom, and double-click to orbit around a point.
use super::{Workbench, actions::MAX_PITCH, selection::Tool};
use eframe::egui;
use geemil_core::{Camera, Sample};
use glam::DVec3;
use std::time::{Duration, Instant};

const FLIGHT: Duration = Duration::from_millis(250);

/// Turns the camera toward a new orbit centre while the eye stays in place.
pub(super) struct Flight {
    from: Camera,
    to: DVec3,
    start: Instant,
}

impl Workbench {
    /// Orbit (left, in tools that pick by clicking), pan (right), zoom (wheel)
    /// and double-click picking. Any camera change drops the selection, which is
    /// tied to the camera it was drawn with.
    pub(super) fn camera_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        let mut moved = false;
        if self.selection.tool.orbits() && response.dragged_by(egui::PointerButton::Primary) {
            let delta = ctx.input(|i| i.pointer.delta());
            self.camera.yaw -= delta.x as f64 * 0.007;
            self.camera.pitch =
                (self.camera.pitch + delta.y as f64 * 0.007).clamp(-MAX_PITCH, MAX_PITCH);
            moved = true;
        }
        if response.dragged_by(egui::PointerButton::Secondary) {
            let delta = ctx.input(|i| i.pointer.delta());
            let forward = (DVec3::from(self.camera.target) - self.camera.eye()).normalize();
            let right = forward.cross(DVec3::Z).normalize();
            let up = right.cross(forward);
            self.camera.target = (DVec3::from(self.camera.target)
                + (right * (-delta.x as f64) + up * delta.y as f64) * self.camera.distance
                    / response.rect.height() as f64)
                .to_array();
            moved = true;
        }
        if response.hovered() {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0. {
                self.camera.distance =
                    (self.camera.distance * (-scroll as f64 * 0.003).exp()).clamp(0.001, 1e10);
                moved = true;
            }
        }
        if moved {
            self.flight = None;
            self.dirty = true;
            self.selection.clear();
        }
        if self.selection.tool == Tool::Navigate
            && response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let rect = response.rect;
            let click = [
                ((pos.x - rect.left()) / rect.width()) as f64,
                ((pos.y - rect.top()) / rect.height()) as f64,
            ];
            let viewport = [rect.width() as f64, rect.height() as f64];
            // Splat radius in logical pixels, like `viewport`.
            let radius = self.settings.point_size as f64 / 2.;
            if let Some(target) = pick(&self.points, &self.camera, click, viewport, radius) {
                self.flight = Some(Flight {
                    from: self.camera,
                    to: target,
                    start: Instant::now(),
                });
                self.selection.clear();
            }
        }
    }
    /// Advances the retarget animation; call once per frame before drawing.
    pub(super) fn advance_flight(&mut self, ctx: &egui::Context) {
        let Some(flight) = &self.flight else {
            return;
        };
        let s = (flight.start.elapsed().as_secs_f64() / FLIGHT.as_secs_f64()).min(1.);
        let eased = s * s * (3. - 2. * s);
        let from = DVec3::from(flight.from.target);
        self.camera = flight.from.looking_at(from.lerp(flight.to, eased));
        self.dirty = true;
        if s >= 1. {
            self.flight = None;
        } else {
            ctx.request_repaint();
        }
    }
    /// Marks the orbit centre, which is always the viewport centre, while it matters.
    pub(super) fn draw_pivot(&self, ui: &egui::Ui, response: &egui::Response) {
        let orbiting =
            self.selection.tool.orbits() && response.dragged_by(egui::PointerButton::Primary);
        if !(orbiting || self.flight.is_some()) {
            return;
        }
        let center = response.rect.center();
        let painter = ui.painter();
        painter.circle_stroke(center, 6., egui::Stroke::new(3., egui::Color32::BLACK));
        painter.circle_stroke(center, 6., egui::Stroke::new(1.5, egui::Color32::WHITE));
    }
}

/// The displayed point under `click` (normalized viewport coordinates): the
/// nearest to the camera among splats covering the click, else the closest on
/// screen within a few extra pixels. `viewport` and `radius` are in the same
/// pixel units.
pub(super) fn pick(
    points: &[Sample],
    camera: &Camera,
    click: [f64; 2],
    viewport: [f64; 2],
    radius: f64,
) -> Option<DVec3> {
    let near = radius + 6.;
    let projector = camera.projector();
    let mut covering: Option<(f64, DVec3)> = None;
    let mut closest: Option<(f64, DVec3)> = None;
    for sample in points {
        let position = DVec3::from(sample.position);
        let Some((uv, depth)) = projector.project(position) else {
            continue;
        };
        let dx = (uv[0] - click[0]) * viewport[0];
        let dy = (uv[1] - click[1]) * viewport[1];
        let d2 = dx * dx + dy * dy;
        if d2 <= radius * radius {
            if covering.is_none_or(|(best, _)| depth < best) {
                covering = Some((depth, position));
            }
        } else if d2 <= near * near && closest.is_none_or(|(best, _)| d2 < best) {
            closest = Some((d2, position));
        }
    }
    covering.or(closest).map(|(_, position)| position)
}

#[cfg(test)]
mod tests {
    use super::pick;
    use geemil_core::{Camera, Sample};
    use glam::DVec3;

    fn sample(position: [f64; 3]) -> Sample {
        Sample {
            chunk: 0,
            index: 0,
            position,
            color: [255; 4],
        }
    }

    #[test]
    fn picks_the_front_point_under_the_cursor() {
        // Looking along +Y at the origin from 10 m away.
        let camera = Camera {
            yaw: -std::f64::consts::FRAC_PI_2,
            pitch: 0.,
            distance: 10.,
            aspect: 1.,
            ..Camera::default()
        };
        let behind = sample([0., 5., 0.]);
        let front = sample([0., -5., 0.]);
        let aside = sample([3., 0., 0.]);
        let points = [behind, front, aside.clone()];
        let viewport = [1000., 1000.];
        assert_eq!(
            pick(&points, &camera, [0.5, 0.5], viewport, 2.),
            Some(DVec3::new(0., -5., 0.))
        );
        // Nothing covers an empty spot, but a point a few pixels away is taken.
        let (uv, _) = camera.project(DVec3::new(3., 0., 0.)).unwrap();
        assert_eq!(
            pick(
                std::slice::from_ref(&aside),
                &camera,
                [uv[0] + 0.005, uv[1]],
                viewport,
                2.
            ),
            Some(DVec3::new(3., 0., 0.))
        );
        assert_eq!(pick(&[aside], &camera, [0.1, 0.1], viewport, 2.), None);
    }
}
