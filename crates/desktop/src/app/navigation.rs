//! Camera input: orbit, pan, zoom, double-click to orbit around a point, and
//! click to select a point's scan.
use super::{Workbench, actions::MAX_PITCH, selection::Tool};
use eframe::egui;
use geemil_core::Camera;
use glam::DVec3;
use std::time::{Duration, Instant};

const FLIGHT: Duration = Duration::from_millis(250);

/// Retargets the orbit centre, optionally moving closer to frame a tree item.
pub(super) struct Flight {
    from: Camera,
    to: DVec3,
    fit_distance: Option<f64>,
    start: Instant,
}

impl Workbench {
    pub(super) fn focus_tree_item(&mut self, id: uuid::Uuid) {
        let Some((frame, bounds)) = self
            .project
            .as_deref()
            .and_then(|p| super::gizmo::item_box(p, id, None))
        else {
            return;
        };
        let to = framed_camera(
            self.camera,
            &bounds
                .corners()
                .map(|p| frame.transform_point3(p))
                .collect::<Vec<_>>(),
        );
        self.flight = Some(Flight {
            from: self.camera,
            to: DVec3::from(to.target),
            fit_distance: Some(to.distance),
            start: Instant::now(),
        });
        self.selection.clear();
        self.dirty = true;
    }

    /// Orbit (middle in every tool, left in tools that pick by clicking), pan
    /// (right), zoom (wheel), double-click picking of the orbit centre (middle
    /// in every tool, left in camera mode), and clicking a point to select its
    /// scan (camera mode). Any camera change drops the selection, which is
    /// tied to the camera it was drawn with.
    pub(super) fn camera_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        if self.crop.dragging() {
            return;
        }
        let mut moved = false;
        if self.orbiting(response) {
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
        let navigating = self.selection.tool == Tool::Navigate;
        let transforming = self.selection.tool == Tool::Transform && !self.gizmo.on_handle();
        let middle = egui::PointerButton::Middle;
        // Selecting a scan elsewhere would change what the tool works on, so
        // only camera mode and the transform tool, off its handles, select by
        // clicking. The transform tool keeps a selected folder holding the
        // scan, so a double-click can still put the folder's handles on it.
        if (navigating || transforming)
            && (response.clicked() || response.clicked_by(middle))
            && let Some((scan, _)) = self.pick_shown(response)
        {
            let held = transforming
                && self
                    .single_tree_item()
                    .zip(self.project.as_ref())
                    .is_some_and(|(item, p)| p.scans_within(item).contains(&scan));
            if !held {
                self.select_tree_item(Some(scan));
                self.reveal = Some(scan);
            }
        }
        if ((navigating && response.double_clicked()) || response.double_clicked_by(middle))
            && let Some((_, target)) = self.pick_shown(response)
        {
            self.flight = Some(Flight {
                from: self.camera,
                to: target,
                fit_distance: None,
                start: Instant::now(),
            });
            self.selection.clear();
        }
    }
    /// Whether the pointer drag orbits the camera now.
    fn orbiting(&self, response: &egui::Response) -> bool {
        !self.crop.dragging()
            && (response.dragged_by(egui::PointerButton::Middle)
                || (self.selection.tool.orbits()
                    && !self.gizmo.dragging()
                    && response.dragged_by(egui::PointerButton::Primary)))
    }
    /// The shown point under the pointer, with its scan.
    fn pick_shown(&self, response: &egui::Response) -> Option<(uuid::Uuid, DVec3)> {
        let pos = response.interact_pointer_pos()?;
        let rect = response.rect;
        let click = [
            ((pos.x - rect.left()) / rect.width()) as f64,
            ((pos.y - rect.top()) / rect.height()) as f64,
        ];
        let viewport = [rect.width() as f64, rect.height() as f64];
        // Splat radius in logical pixels, like `viewport`.
        let radius = self.settings.point_size as f64 / 2.;
        let points = self.shown_points(true).map(|(scan, _, p)| (scan, p));
        pick_tagged(points, &self.camera, click, viewport, radius)
    }
    /// Advances the retarget animation; call once per frame before drawing.
    pub(super) fn advance_flight(&mut self, ctx: &egui::Context) {
        let Some(flight) = &self.flight else {
            return;
        };
        let s = (flight.start.elapsed().as_secs_f64() / FLIGHT.as_secs_f64()).min(1.);
        let eased = s * s * (3. - 2. * s);
        let from = DVec3::from(flight.from.target);
        let target = from.lerp(flight.to, eased);
        self.camera = if let Some(distance) = flight.fit_distance {
            Camera {
                target: target.to_array(),
                distance: flight.from.distance + (distance - flight.from.distance) * eased,
                ..flight.from
            }
        } else {
            flight.from.looking_at(target)
        };
        self.dirty = true;
        if s >= 1. {
            self.flight = None;
        } else {
            ctx.request_repaint();
        }
    }
    /// Marks the orbit centre, which is always the viewport centre, while it matters.
    pub(super) fn draw_pivot(&self, ui: &egui::Ui, response: &egui::Response) {
        if !(self.orbiting(response) || self.flight.is_some()) {
            return;
        }
        let center = response.rect.center();
        let painter = ui.painter();
        painter.circle_stroke(center, 6., egui::Stroke::new(3., egui::Color32::BLACK));
        painter.circle_stroke(center, 6., egui::Stroke::new(1.5, egui::Color32::WHITE));
    }

    /// A passive world-axis triad, rotated with the view, in logical pixels.
    pub(super) fn draw_orientation(&self, ui: &egui::Ui, rect: egui::Rect) {
        if rect.width() < 120. || rect.height() < 120. {
            return;
        }
        let painter = ui.painter().with_clip_rect(rect);
        let origin = rect.right_bottom() - egui::vec2(58., 58.);
        painter.circle_filled(origin, 48., egui::Color32::from_black_alpha(100));
        let (right, up, forward) = view_basis(self.camera);
        let mut axes = super::gizmo::AXES.map(|(axis, color)| (axis, color, axis.dot(forward)));
        // Draw the far axes first so the near ones remain readable.
        axes.sort_by(|a, b| b.2.total_cmp(&a.2));
        for (axis, color, depth) in axes {
            let offset = egui::vec2(axis.dot(right) as f32, -axis.dot(up) as f32) * 32.;
            let end = origin + offset;
            painter.line_segment([origin, end], egui::Stroke::new(4., egui::Color32::BLACK));
            painter.line_segment([origin, end], egui::Stroke::new(2., color));
            painter.circle_filled(end, 9., egui::Color32::BLACK);
            painter.circle_stroke(end, 9., egui::Stroke::new(1., color));
            let name = if axis == DVec3::X {
                "X"
            } else if axis == DVec3::Y {
                "Y"
            } else {
                "Z"
            };
            painter.text(
                end,
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(12.),
                if depth > 0. {
                    color.gamma_multiply(0.7)
                } else {
                    color
                },
            );
        }
    }
}

fn view_basis(camera: Camera) -> (DVec3, DVec3, DVec3) {
    let forward = -DVec3::new(
        camera.yaw.cos() * camera.pitch.cos(),
        camera.yaw.sin() * camera.pitch.cos(),
        camera.pitch.sin(),
    );
    let right = DVec3::new(-camera.yaw.sin(), camera.yaw.cos(), 0.);
    let up = right.cross(forward);
    (right, up, forward)
}

/// Fit transformed bounds with a small margin while preserving the view direction.
fn framed_camera(camera: Camera, corners: &[DVec3]) -> Camera {
    let target = corners.iter().copied().sum::<DVec3>() / corners.len() as f64;
    let (right, up, forward) = view_basis(camera);
    let vertical = (camera.fov * 0.5).tan();
    let horizontal = vertical * camera.aspect;
    let mut distance: f64 = 0.001;
    for corner in corners {
        let q = corner - target;
        let needed = (q.dot(right).abs() / horizontal).max(q.dot(up).abs() / vertical) * 1.08;
        distance = distance.max(if camera.ortho {
            needed
        } else {
            (needed - q.dot(forward)).max(0.001 - q.dot(forward))
        });
    }
    Camera {
        target: target.to_array(),
        distance,
        ..camera
    }
}

/// The displayed point under `click` (normalized viewport coordinates): the
/// nearest to the camera among splats covering the click, else the closest on
/// screen within a few extra pixels. `viewport` and `radius` are in the same
/// pixel units.
pub(super) fn pick(
    points: impl IntoIterator<Item = DVec3>,
    camera: &Camera,
    click: [f64; 2],
    viewport: [f64; 2],
    radius: f64,
) -> Option<DVec3> {
    let points = points.into_iter().map(|p| ((), p));
    pick_tagged(points, camera, click, viewport, radius).map(|(_, p)| p)
}

/// [`pick`] for points that carry a tag, such as their scan.
fn pick_tagged<T>(
    points: impl IntoIterator<Item = (T, DVec3)>,
    camera: &Camera,
    click: [f64; 2],
    viewport: [f64; 2],
    radius: f64,
) -> Option<(T, DVec3)> {
    let near = radius + 6.;
    let projector = camera.projector();
    let mut covering: Option<(f64, (T, DVec3))> = None;
    let mut closest: Option<(f64, (T, DVec3))> = None;
    for (tag, position) in points {
        let Some((uv, depth)) = projector.project(position) else {
            continue;
        };
        let dx = (uv[0] - click[0]) * viewport[0];
        let dy = (uv[1] - click[1]) * viewport[1];
        let d2 = dx * dx + dy * dy;
        if d2 <= radius * radius {
            if covering.as_ref().is_none_or(|(best, _)| depth < *best) {
                covering = Some((depth, (tag, position)));
            }
        } else if d2 <= near * near && closest.as_ref().is_none_or(|(best, _)| d2 < *best) {
            closest = Some((d2, (tag, position)));
        }
    }
    covering.or(closest).map(|(_, picked)| picked)
}

#[cfg(test)]
mod tests {
    use super::{framed_camera, pick, pick_tagged, view_basis};
    use geemil_core::{Bounds, Camera, Sample};
    use glam::{DMat4, DVec3};

    #[test]
    fn frames_transformed_bounds_in_portrait_and_landscape_in_both_projections() {
        let bounds = Bounds {
            min: [-12., -3., -1.],
            max: [12., 3., 1.],
        };
        let world = DMat4::from_translation(DVec3::new(100_000., -200_000., 150.))
            * DMat4::from_rotation_z(0.7);
        let corners: Vec<_> = bounds
            .corners()
            .map(|p| world.transform_point3(p))
            .collect();
        for aspect in [0.4, 1., 2.5] {
            for ortho in [false, true] {
                let from = Camera {
                    aspect,
                    ortho,
                    ..Camera::default()
                };
                let camera = framed_camera(from, &corners);
                assert_eq!((camera.yaw, camera.pitch), (from.yaw, from.pitch));
                let mut edge: f64 = 0.;
                for corner in &corners {
                    let (uv, depth) = camera.project(*corner).unwrap();
                    assert!(depth > 0. || ortho);
                    for coordinate in uv {
                        assert!((0.03..=0.97).contains(&coordinate), "{camera:?}: {uv:?}");
                        edge = edge.max((coordinate - 0.5).abs());
                    }
                }
                // The item fills the limiting dimension without clipping.
                assert!(edge > 0.45);
            }
        }
    }

    #[test]
    fn orientation_axes_match_projection_and_ignore_camera_translation() {
        for yaw in [-1.2, 0., 1.8] {
            for pitch in [-1.5, 0., 1.5] {
                let camera = Camera {
                    yaw,
                    pitch,
                    ortho: true,
                    aspect: 1.,
                    target: [1000., 2000., -3000.],
                    ..Camera::default()
                };
                let (right, up, forward) = view_basis(camera);
                assert!((right.cross(up) + forward).length() < 1e-12);
                let target = DVec3::from(camera.target);
                for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
                    let (uv, _) = camera.project(target + axis).unwrap();
                    assert!(
                        ((uv[0] - 0.5) * 2. * camera.half_height() - axis.dot(right)).abs() < 1e-9
                    );
                    assert!(
                        ((0.5 - uv[1]) * 2. * camera.half_height() - axis.dot(up)).abs() < 1e-9
                    );
                }
            }
        }
    }

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
        let (behind_at, front_at) = (DVec3::from(behind.position), DVec3::from(front.position));
        let points = [behind, front, aside.clone()].map(|s| DVec3::from(s.position));
        let viewport = [1000., 1000.];
        assert_eq!(
            pick(points, &camera, [0.5, 0.5], viewport, 2.),
            Some(DVec3::new(0., -5., 0.))
        );
        // Nothing covers an empty spot, but a point a few pixels away is taken.
        let (uv, _) = camera.project(DVec3::new(3., 0., 0.)).unwrap();
        assert_eq!(
            pick(
                [DVec3::from(aside.position)],
                &camera,
                [uv[0] + 0.005, uv[1]],
                viewport,
                2.
            ),
            Some(DVec3::new(3., 0., 0.))
        );
        let aside = [DVec3::from(aside.position)];
        assert_eq!(pick(aside, &camera, [0.1, 0.1], viewport, 2.), None);
        // The front point's tag, such as its scan, comes with it.
        let tagged = [("behind", behind_at), ("front", front_at)];
        assert_eq!(
            pick_tagged(tagged, &camera, [0.5, 0.5], viewport, 2.),
            Some(("front", front_at))
        );
    }
}
