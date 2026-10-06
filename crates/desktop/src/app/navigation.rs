//! Camera input: orbit, pan, zoom, double-click to orbit around a point, and
//! click to select a point's scan.
use super::{Workbench, actions::MAX_PITCH, selection::Tool};
use eframe::egui;
use geemil_core::Camera;
use glam::DVec3;
use std::time::{Duration, Instant};
use uuid::Uuid;

const FLIGHT: Duration = Duration::from_millis(250);

/// A shown point under the pointer.
pub(super) struct Picked {
    pub(super) scan: Uuid,
    /// In scan coordinates.
    pub(super) local: DVec3,
    /// In the project frame, as drawn.
    pub(super) world: DVec3,
}

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
        self.selection.camera_moved();
        self.dirty = true;
    }

    /// Orbit (middle in every tool, left in tools that pick by clicking), pan
    /// (right), zoom (wheel), double-click picking of the orbit centre (middle
    /// in every tool, left in camera mode), and clicking a point to select its
    /// scan (camera mode). Ctrl+wheel moves forward and back, carrying the
    /// orbit centre along. A finished selection keeps the camera it was drawn
    /// with; one being drawn is dropped.
    pub(super) fn camera_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        self.flying =
            response.is_pointer_button_down_on() && ctx.input(|i| i.pointer.secondary_down());
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
            // egui turns Ctrl+wheel into zoom, and leaves no scroll.
            let (scroll, zoom) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            if zoom != 1. {
                let (.., forward) = view_basis(self.camera);
                self.camera.target = (DVec3::from(self.camera.target)
                    + forward * self.camera.distance * (zoom as f64).ln())
                .to_array();
                moved = true;
            }
            if scroll != 0. {
                self.camera.distance =
                    (self.camera.distance * (-scroll as f64 * 0.003).exp()).clamp(0.001, 1e10);
                moved = true;
            }
        }
        if moved {
            self.flight = None;
            self.dirty = true;
            self.selection.camera_moved();
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
            self.selection.camera_moved();
        }
    }
    /// Flying with the keys in every tool while the right button is held on
    /// the viewport: W/S along the view, A/D sideways and E/Q up and down.
    /// The orbit centre moves along at the orbit distance a second; Shift is
    /// four times faster and Ctrl four times slower.
    pub(super) fn fly_input(&mut self, ctx: &egui::Context) {
        if !self.flying || ctx.egui_wants_keyboard_input() || self.dialog.is_some() {
            self.fly_last = None;
            return;
        }
        let (step, modifiers) = ctx.input(|i| (fly_step(|key| i.key_down(key)), i.modifiers));
        if step == [0; 3] {
            self.fly_last = None;
            return;
        }
        // Timed from the previous moving frame; the first moves one frame's
        // worth rather than however long the app idled.
        let now = Instant::now();
        let dt = self
            .fly_last
            .map_or(1. / 60., |last| now.duration_since(last).as_secs_f64())
            .min(0.1);
        self.fly_last = Some(now);
        self.camera = fly(self.camera, step, modifiers, dt);
        self.flight = None;
        self.dirty = true;
        self.selection.camera_moved();
        ctx.request_repaint();
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
    fn pick_shown(&self, response: &egui::Response) -> Option<(Uuid, DVec3)> {
        self.pick_at(response).map(|p| (p.scan, p.world))
    }
    /// The shown point under the pointer in the viewport.
    pub(super) fn pick_at(&self, response: &egui::Response) -> Option<Picked> {
        let pos = response.interact_pointer_pos()?;
        let rect = response.rect;
        self.pick_point([
            ((pos.x - rect.left()) / rect.width()) as f64,
            ((pos.y - rect.top()) / rect.height()) as f64,
        ])
    }
    /// The shown point at `click` (normalized viewport coordinates) as last
    /// drawn: the frontmost one covering it, else the closest on screen
    /// within a few pixels of its splat.
    pub(super) fn pick_point(&self, click: [f64; 2]) -> Option<Picked> {
        let reach = (self.settings.point_size / 2. + 6.) * self.pixels_per_point;
        let (node, index) = self.renderer.as_ref()?.pick(click, reach.ceil() as u32)?;
        let node = self.nodes.get(node)?;
        let local = (index < node.points.len()).then(|| node.points.position(index))?;
        let world = self
            .scan_worlds(true)
            .get(&node.scan)?
            .transform_point3(local);
        Some(Picked {
            scan: node.scan,
            local,
            world,
        })
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

/// The step [`fly`] takes for the keys `held`: W/S, A/D and E/Q. Opposite
/// keys cancel out.
fn fly_step(held: impl Fn(egui::Key) -> bool) -> [i32; 3] {
    use egui::Key;
    let axis = |plus, minus| held(plus) as i32 - held(minus) as i32;
    [
        axis(Key::D, Key::A),
        axis(Key::W, Key::S),
        axis(Key::E, Key::Q),
    ]
}

/// `camera` moved for `dt` seconds by `step` (right, forward, up; each -1,
/// 0 or 1) at its orbit distance a second, four times faster with Shift and
/// slower with Ctrl. The orbit centre moves along.
fn fly(mut camera: Camera, step: [i32; 3], modifiers: egui::Modifiers, dt: f64) -> Camera {
    let (right, _, forward) = view_basis(camera);
    let speed = if modifiers.shift {
        4.
    } else if modifiers.command {
        0.25
    } else {
        1.
    } * camera.distance;
    let direction = right * step[0] as f64 + forward * step[1] as f64 + DVec3::Z * step[2] as f64;
    camera.target = (DVec3::from(camera.target) + direction * speed * dt).to_array();
    camera
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

#[cfg(test)]
mod tests {
    use super::{fly, fly_step, framed_camera, view_basis};
    use geemil_core::{Bounds, Camera};
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

    #[test]
    fn flying_moves_eye_and_centre_together_at_the_orbit_distance_a_second() {
        let camera = Camera {
            target: [1., 2., 3.],
            distance: 10.,
            yaw: 0.3,
            pitch: 0.4,
            ..Camera::default()
        };
        let (right, _, forward) = view_basis(camera);
        let none = eframe::egui::Modifiers::NONE;
        let ahead = fly(camera, [0, 1, 0], none, 0.5);
        let moved = DVec3::from(ahead.target) - DVec3::from(camera.target);
        assert!(moved.abs_diff_eq(forward * 5., 1e-9));
        assert!((ahead.eye() - camera.eye()).abs_diff_eq(moved, 1e-9));
        assert_eq!(ahead.distance, camera.distance);
        let fast = fly(camera, [-1, 0, 1], eframe::egui::Modifiers::SHIFT, 0.5);
        let moved = DVec3::from(fast.target) - DVec3::from(camera.target);
        assert!(moved.abs_diff_eq((DVec3::Z - right) * 20., 1e-9));
        let slow = fly(camera, [0, 0, -1], eframe::egui::Modifiers::COMMAND, 1.);
        assert!((slow.target[2] - (camera.target[2] - 2.5)).abs() < 1e-9);
    }

    #[test]
    fn letters_fly_and_opposites_cancel() {
        use eframe::egui::Key;
        let held = |keys: &'static [Key]| move |key| keys.contains(&key);
        assert_eq!(fly_step(held(&[Key::W, Key::D, Key::E])), [1, 1, 1]);
        assert_eq!(fly_step(held(&[Key::S, Key::A, Key::Q])), [-1, -1, -1]);
        assert_eq!(fly_step(held(&[Key::W, Key::S, Key::D])), [1, 0, 0]);
        // Arrows and PageUp/PageDown do not fly.
        assert_eq!(fly_step(held(&[Key::ArrowUp, Key::PageUp])), [0, 0, 0]);
    }
}
