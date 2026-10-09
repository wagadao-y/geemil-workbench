//! Camera navigation, independent of the active editing tool.
use super::{Workbench, actions::MAX_PITCH, selection::Tool};
use eframe::egui;
use geemil_core::Camera;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;

const FLIGHT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum CameraMode {
    #[default]
    Orbit,
    Fly,
    /// Horizontal movement, without gravity or collision detection.
    Walk,
}

impl CameraMode {
    pub(super) const ALL: [Self; 3] = [Self::Orbit, Self::Fly, Self::Walk];

    pub(super) fn next(self) -> Self {
        match self {
            Self::Orbit => Self::Fly,
            Self::Fly => Self::Walk,
            Self::Walk => Self::Orbit,
        }
    }

    /// Orbit follows the view scale; Fly/Walk keep their configured m/s.
    pub(super) fn move_speed(self, camera: Camera, configured: f64) -> f64 {
        match self {
            Self::Orbit => camera.distance,
            Self::Fly | Self::Walk => configured,
        }
        .clamp(0.001, 1e6)
    }

    pub(super) fn label(self, t: &crate::i18n::Strings) -> &'static str {
        match self {
            Self::Orbit => t.camera_orbit,
            Self::Fly => t.camera_fly,
            Self::Walk => t.camera_walk,
        }
    }
}

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

    /// Middle drag rotates in every tool; point-picking tools also allow left
    /// drag away from editing handles. Right drag pans. Orbit's wheel zooms;
    /// Fly/Walk's wheel adjusts speed. Finished selections keep their camera.
    pub(super) fn camera_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        if self.dialog.is_some()
            || egui::Popup::is_any_open(ctx)
            || self.crop.dragging()
            || self.gizmo.dragging()
            || self.primary_editing(ctx)
        {
            return;
        }
        self.camera_picking(response);
        if self.panorama_look(ctx, response) {
            return;
        }
        let mut moved = false;
        if self.rotating(response) {
            let delta = ctx.input(|i| i.pointer.delta());
            self.camera = turn_camera(self.camera, self.settings.camera_mode, delta);
            moved = true;
        }
        if response.dragged_by(egui::PointerButton::Secondary) {
            let delta = ctx.input(|i| i.pointer.delta());
            self.camera = pan_camera(self.camera, delta, response.rect.height());
            moved = true;
        }
        if response.hovered() {
            // egui turns Ctrl+wheel into zoom, and leaves no scroll.
            let (scroll, zoom) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            if zoom != 1. && self.settings.camera_mode == CameraMode::Orbit {
                let (.., forward) = view_basis(self.camera);
                self.camera.target = (DVec3::from(self.camera.target)
                    + forward * self.camera.distance * (zoom as f64).ln())
                .to_array();
                moved = true;
            }
            if scroll != 0. {
                if self.settings.camera_mode == CameraMode::Orbit {
                    self.camera.distance =
                        (self.camera.distance * (-scroll as f64 * 0.003).exp()).clamp(0.001, 1e10);
                    moved = true;
                } else {
                    self.settings.move_speed = (self.settings.move_speed
                        * (scroll as f64 * 0.003).exp())
                    .clamp(0.001, 1e6);
                }
            }
        }
        if moved {
            self.flight = None;
            self.dirty = true;
            self.selection.camera_moved();
        }
    }

    fn camera_picking(&mut self, response: &egui::Response) {
        let navigating = self.selection.tool == Tool::Navigate;
        let transforming = self.selection.tool == Tool::Transform && !self.gizmo.on_handle();
        let middle = egui::PointerButton::Middle;
        // Middle click always selects the scan, independent of the tool.
        // Left click off transform handles keeps a selected ancestor folder.
        let middle_clicked = response.clicked_by(middle);
        if selects_scan(
            self.selection.tool,
            self.editing_handle(),
            response.clicked(),
            middle_clicked,
        ) && let Some((scan, _)) = self.pick_shown(response)
        {
            let held = !middle_clicked
                && transforming
                && self
                    .single_tree_item()
                    .zip(self.project.as_ref())
                    .is_some_and(|(item, p)| p.scans_within(item).contains(&scan));
            if !held {
                self.select_tree_item(Some(scan));
                self.reveal = Some(scan);
            }
        }
        if !self.panorama_camera_linked()
            && ((navigating && response.double_clicked()) || response.double_clicked_by(middle))
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
    /// WASD/QE works in every tool while the pointer is on the viewport.
    /// Text input, dialogs, menus and primary-button edits hold it back.
    pub(super) fn fly_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        let allowed = (response.hovered() || response.is_pointer_button_down_on())
            && self.dialog.is_none()
            && !self.crop.dragging()
            && !self.gizmo.dragging()
            && !self.primary_editing(ctx)
            && !self.panorama_camera_linked();
        let step = movement_step(ctx, allowed);
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
        let modifiers = ctx.input(|i| i.modifiers);
        self.camera = fly(
            self.camera,
            self.settings.camera_mode,
            self.settings.move_speed,
            step,
            modifiers,
            dt,
        );
        self.flight = None;
        self.dirty = true;
        self.selection.camera_moved();
        ctx.request_repaint();
    }
    fn primary_editing(&self, ctx: &egui::Context) -> bool {
        primary_edits(
            self.selection.tool,
            self.editing_handle(),
            ctx.input(|i| i.pointer.primary_down()),
        )
    }

    fn editing_handle(&self) -> bool {
        (self.selection.tool == Tool::Box && self.crop.on_handle())
            || (self.selection.tool == Tool::Transform && self.gizmo.on_handle())
    }

    fn rotating(&self, response: &egui::Response) -> bool {
        rotation_drag(
            self.selection.tool,
            self.editing_handle(),
            response.dragged_by(egui::PointerButton::Primary),
            response.dragged_by(egui::PointerButton::Middle),
        )
    }

    /// Whether the pointer drag orbits the camera now.
    fn orbiting(&self, response: &egui::Response) -> bool {
        self.settings.camera_mode == CameraMode::Orbit
            && !self.crop.dragging()
            && !self.gizmo.dragging()
            && self.rotating(response)
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
        let node = self.drawn.get(node)?;
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

fn primary_edits(tool: Tool, on_handle: bool, primary_down: bool) -> bool {
    primary_down && (!tool.camera_on_left_drag() || on_handle)
}

fn rotation_drag(tool: Tool, on_handle: bool, primary: bool, middle: bool) -> bool {
    !primary_edits(tool, on_handle, primary) && (middle || (primary && tool.camera_on_left_drag()))
}

fn selects_scan(tool: Tool, on_handle: bool, primary: bool, middle: bool) -> bool {
    middle || (primary && (tool == Tool::Navigate || (tool == Tool::Transform && !on_handle)))
}

/// A cycle is one fresh press; holding C must not rapidly cycle all modes.
pub(super) fn camera_cycle_key(ctx: &egui::Context, blocked: bool) -> bool {
    if blocked || ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx) {
        return false;
    }
    ctx.input(|i| {
        i.focused
            && i.modifiers.is_none()
            && i.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::C,
                        pressed: true,
                        repeat: false,
                        ..
                    }
                )
            })
    })
}

fn pan_camera(mut camera: Camera, delta: egui::Vec2, height: f32) -> Camera {
    let (right, up, _) = view_basis(camera);
    camera.target = (DVec3::from(camera.target)
        + (right * (-delta.x as f64) + up * delta.y as f64) * camera.distance
            / height.max(1.) as f64)
        .to_array();
    camera
}

/// Read held keys rather than key events, but never interpret a command such
/// as Ctrl+S as movement, even after the shortcut consumed its event.
fn movement_step(ctx: &egui::Context, allowed: bool) -> [i32; 3] {
    if !allowed || ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx) {
        return [0; 3];
    }
    ctx.input(|i| {
        if !i.focused || i.modifiers.ctrl || i.modifiers.command || i.modifiers.mac_cmd {
            [0; 3]
        } else {
            fly_step(|key| i.key_down(key))
        }
    })
}

fn turn_camera(mut camera: Camera, mode: CameraMode, delta: egui::Vec2) -> Camera {
    let eye = camera.eye();
    camera.yaw -= delta.x as f64 * 0.007;
    camera.pitch = (camera.pitch + delta.y as f64 * 0.007).clamp(-MAX_PITCH, MAX_PITCH);
    if mode != CameraMode::Orbit {
        let (.., forward) = view_basis(camera);
        camera.target = (eye + forward * camera.distance).to_array();
    }
    camera
}

/// Translate eye and target together, using distance-scaled speed in Orbit
/// and configured m/s in Fly/Walk. Shift is four times faster, Alt four times
/// slower; diagonal motion has the same speed.
fn fly(
    mut camera: Camera,
    mode: CameraMode,
    speed: f64,
    step: [i32; 3],
    modifiers: egui::Modifiers,
    dt: f64,
) -> Camera {
    let (right, _, mut forward) = view_basis(camera);
    if mode == CameraMode::Walk {
        forward = DVec3::new(-camera.yaw.cos(), -camera.yaw.sin(), 0.);
    }
    let speed = if modifiers.shift {
        4.
    } else if modifiers.alt {
        0.25
    } else {
        1.
    } * mode.move_speed(camera, speed);
    let direction = right * step[0] as f64 + forward * step[1] as f64 + DVec3::Z * step[2] as f64;
    camera.target =
        (DVec3::from(camera.target) + direction.normalize_or_zero() * speed * dt).to_array();
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
    use super::{
        CameraMode, Tool, camera_cycle_key, fly, fly_step, framed_camera, movement_step,
        pan_camera, primary_edits, rotation_drag, selects_scan, turn_camera, view_basis,
    };
    use eframe::egui::{self, Key, Modifiers};
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
    fn flying_moves_eye_and_centre_together_at_the_selected_speed() {
        let camera = Camera {
            target: [1., 2., 3.],
            distance: 10.,
            yaw: 0.3,
            pitch: 0.4,
            ..Camera::default()
        };
        let (right, _, forward) = view_basis(camera);
        let none = eframe::egui::Modifiers::NONE;
        let ahead = fly(camera, CameraMode::Fly, 10., [0, 1, 0], none, 0.5);
        let moved = DVec3::from(ahead.target) - DVec3::from(camera.target);
        assert!(moved.abs_diff_eq(forward * 5., 1e-9));
        assert!((ahead.eye() - camera.eye()).abs_diff_eq(moved, 1e-9));
        assert_eq!(ahead.distance, camera.distance);
        let fast = fly(
            camera,
            CameraMode::Fly,
            10.,
            [-1, 0, 1],
            Modifiers::SHIFT,
            0.5,
        );
        let moved = DVec3::from(fast.target) - DVec3::from(camera.target);
        assert!(moved.abs_diff_eq((DVec3::Z - right).normalize() * 20., 1e-9));
        let slow = fly(camera, CameraMode::Fly, 10., [0, 0, -1], Modifiers::ALT, 1.);
        assert!((slow.target[2] - (camera.target[2] - 2.5)).abs() < 1e-9);
    }

    #[test]
    fn orbit_movement_scales_with_distance_and_fly_walk_keep_configured_speed() {
        for ortho in [false, true] {
            for distance in [0.01, 1., 1000.] {
                let camera = Camera {
                    distance,
                    ortho,
                    ..Camera::default()
                };
                for mode in CameraMode::ALL {
                    for configured in [2., 20.] {
                        let speed = if mode == CameraMode::Orbit {
                            distance
                        } else {
                            configured
                        };
                        for step in [[1, 0, 0], [0, 1, 0], [0, 0, -1], [1, 1, 1]] {
                            for (modifiers, factor) in [
                                (Modifiers::NONE, 1.),
                                (Modifiers::SHIFT, 4.),
                                (Modifiers::ALT, 0.25),
                            ] {
                                let moved = fly(camera, mode, configured, step, modifiers, 0.1);
                                let offset = DVec3::from(moved.target) - DVec3::from(camera.target);
                                assert!((offset.length() - speed * factor * 0.1).abs() < 1e-9);
                                assert!((moved.eye() - camera.eye()).abs_diff_eq(offset, 1e-9));
                                assert_eq!(moved.distance, distance);
                            }
                        }
                    }
                }
            }
        }
        for (distance, speed) in [(0., 0.001), (1e10, 1e6)] {
            let camera = Camera {
                distance,
                ..Camera::default()
            };
            let moved = fly(
                camera,
                CameraMode::Orbit,
                10.,
                [0, 1, 0],
                Modifiers::NONE,
                0.1,
            );
            let offset = DVec3::from(moved.target) - DVec3::from(camera.target);
            assert!((offset.length() - speed * 0.1).abs() < 1e-9);
        }
    }

    #[test]
    fn orbit_keeps_its_target_and_mouse_look_keeps_its_eye() {
        for ortho in [false, true] {
            for mode in CameraMode::ALL {
                let camera = Camera {
                    target: [100_000., -200_000., 30.],
                    ortho,
                    ..Camera::default()
                };
                let turned = turn_camera(camera, mode, egui::vec2(40., -30.));
                assert_ne!(turned.yaw, camera.yaw);
                assert_ne!(turned.pitch, camera.pitch);
                assert_eq!(turned.distance, camera.distance);
                if mode == CameraMode::Orbit {
                    assert_eq!(turned.target, camera.target);
                    assert!(!turned.eye().abs_diff_eq(camera.eye(), 1e-9));
                } else {
                    assert!(turned.eye().abs_diff_eq(camera.eye(), 1e-9));
                }
                let vertical = turn_camera(turned, mode, egui::vec2(0., 1e6));
                assert!(vertical.matrix().is_finite());
            }
        }
    }

    #[test]
    fn walk_moves_horizontally_even_when_looking_straight_up() {
        for pitch in [-super::MAX_PITCH, -0.6, 0., 0.6, super::MAX_PITCH] {
            let camera = Camera {
                pitch,
                ..Camera::default()
            };
            for step in [[0, 1, 0], [1, 1, 0], [-1, -1, 0]] {
                let walk = fly(camera, CameraMode::Walk, 2., step, Modifiers::NONE, 0.5);
                let offset = DVec3::from(walk.target) - DVec3::from(camera.target);
                assert_eq!(walk.target[2], camera.target[2]);
                assert!((offset.length() - 1.).abs() < 1e-9);
            }
            let fly = fly(camera, CameraMode::Fly, 2., [0, 1, 0], Modifiers::NONE, 0.5);
            assert!((fly.target[2] - camera.target[2] + pitch.sin()).abs() < 1e-9);
            let up = super::fly(
                camera,
                CameraMode::Walk,
                2.,
                [0, 0, 1],
                Modifiers::NONE,
                0.5,
            );
            assert_eq!(up.target[2], camera.target[2] + 1.);
        }
    }

    fn key_input(key: Key, modifiers: Modifiers) -> egui::RawInput {
        egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ],
            ..Default::default()
        }
    }

    fn input_frame(ctx: &egui::Context, raw: egui::RawInput, test: impl FnOnce(&egui::Context)) {
        ctx.begin_pass(raw);
        test(ctx);
        // This headless input test intentionally has no texture renderer.
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn held_keys_move_without_mouse_buttons_and_commands_never_move() {
        let ctx = egui::Context::default();
        input_frame(&ctx, key_input(Key::W, Modifiers::NONE), |ctx| {
            assert_eq!(movement_step(ctx, true), [0, 1, 0]);
            assert_eq!(movement_step(ctx, false), [0; 3]);
        });
        input_frame(&ctx, egui::RawInput::default(), |ctx| {
            assert_eq!(movement_step(ctx, true), [0, 1, 0]);
        });
        for modifiers in [Modifiers::CTRL, Modifiers::COMMAND, Modifiers::MAC_CMD] {
            let ctx = egui::Context::default();
            input_frame(&ctx, key_input(Key::S, modifiers), |ctx| {
                ctx.input_mut(|i| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(modifiers, Key::S));
                });
                assert_eq!(movement_step(ctx, true), [0; 3]);
            });
        }
    }

    #[test]
    fn typing_menus_primary_edits_and_lost_focus_suspend_movement() {
        let ctx = egui::Context::default();
        input_frame(&ctx, key_input(Key::W, Modifiers::NONE), |ctx| {
            let id = egui::Id::new("typing");
            ctx.memory_mut(|m| m.request_focus(id));
            assert_eq!(movement_step(ctx, true), [0; 3]);
            ctx.memory_mut(|m| m.surrender_focus(id));
            egui::Popup::open_id(ctx, egui::Id::new("menu"));
            assert_eq!(movement_step(ctx, true), [0; 3]);
        });
        let ctx = egui::Context::default();
        let mut raw = key_input(Key::W, Modifiers::NONE);
        raw.events.push(egui::Event::PointerButton {
            pos: egui::pos2(100., 100.),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        input_frame(&ctx, raw, |ctx| {
            for tool in super::super::selection::TOOLS {
                let editing = primary_edits(tool, false, ctx.input(|i| i.pointer.primary_down()));
                assert_eq!(
                    movement_step(ctx, !editing),
                    if tool.camera_on_left_drag() {
                        [0, 1, 0]
                    } else {
                        [0; 3]
                    },
                );
            }
            let editing = primary_edits(Tool::Transform, true, true);
            assert_eq!(movement_step(ctx, !editing), [0; 3]);
        });
        let ctx = egui::Context::default();
        let mut raw = key_input(Key::W, Modifiers::NONE);
        raw.focused = false;
        input_frame(&ctx, raw, |ctx| {
            assert_eq!(movement_step(ctx, true), [0; 3])
        });
    }

    #[test]
    fn left_camera_drag_leaves_selection_and_handles_to_the_tool() {
        for tool in super::super::selection::TOOLS {
            let left_camera = !matches!(tool, Tool::Rect | Tool::Polygon);
            assert_eq!(rotation_drag(tool, false, true, false), left_camera);
            assert!(rotation_drag(tool, false, false, true));
            assert!(!rotation_drag(tool, false, false, false));
            assert_eq!(rotation_drag(tool, false, true, true), left_camera);
        }
        for tool in [Tool::Box, Tool::Transform] {
            assert!(!rotation_drag(tool, true, true, false));
            assert!(!rotation_drag(tool, true, true, true));
            // Hovering a handle leaves middle drag available to the camera.
            assert!(rotation_drag(tool, true, false, true));
        }
    }

    #[test]
    fn middle_click_selects_scans_in_every_tool_without_taking_left_picks() {
        for tool in super::super::selection::TOOLS {
            for on_handle in [false, true] {
                assert!(selects_scan(tool, on_handle, false, true));
                assert!(!selects_scan(tool, on_handle, false, false));
                assert_eq!(
                    selects_scan(tool, on_handle, true, false),
                    tool == Tool::Navigate || (tool == Tool::Transform && !on_handle),
                );
            }
        }
    }

    #[test]
    fn panning_preserves_orientation_and_moves_eye_and_target_together() {
        for mode in CameraMode::ALL {
            let camera = turn_camera(Camera::default(), mode, egui::vec2(20., 10.));
            let panned = pan_camera(camera, egui::vec2(40., -30.), 800.);
            let offset = DVec3::from(panned.target) - DVec3::from(camera.target);
            let (right, up, forward) = view_basis(camera);
            assert!(offset.abs_diff_eq((-right * 40. - up * 30.) * camera.distance / 800., 1e-9));
            assert!((panned.eye() - camera.eye()).abs_diff_eq(offset, 1e-9));
            assert!(offset.dot(forward).abs() < 1e-9);
            assert_eq!(
                (panned.yaw, panned.pitch, panned.distance),
                (camera.yaw, camera.pitch, camera.distance)
            );
        }
    }

    #[test]
    fn c_cycles_once_per_press_and_stays_inactive_while_editing_text_or_blocked() {
        assert_eq!(CameraMode::Orbit.next(), CameraMode::Fly);
        assert_eq!(CameraMode::Fly.next(), CameraMode::Walk);
        assert_eq!(CameraMode::Walk.next(), CameraMode::Orbit);
        let ctx = egui::Context::default();
        input_frame(&ctx, key_input(Key::C, Modifiers::NONE), |ctx| {
            assert!(camera_cycle_key(ctx, false));
            assert!(!camera_cycle_key(ctx, true));
            let text = egui::Id::new("text input");
            ctx.memory_mut(|m| m.request_focus(text));
            assert!(!camera_cycle_key(ctx, false));
            ctx.memory_mut(|m| m.surrender_focus(text));
            egui::Popup::open_id(ctx, egui::Id::new("menu"));
            assert!(!camera_cycle_key(ctx, false));
        });
        for modifiers in [Modifiers::SHIFT, Modifiers::CTRL, Modifiers::ALT] {
            let ctx = egui::Context::default();
            input_frame(&ctx, key_input(Key::C, modifiers), |ctx| {
                assert!(!camera_cycle_key(ctx, false));
            });
        }
        let ctx = egui::Context::default();
        input_frame(&ctx, key_input(Key::C, Modifiers::NONE), |ctx| {
            assert!(camera_cycle_key(ctx, false));
        });
        let mut raw = key_input(Key::C, Modifiers::NONE);
        if let egui::Event::Key { repeat, .. } = &mut raw.events[1] {
            *repeat = true;
        }
        input_frame(&ctx, raw, |ctx| assert!(!camera_cycle_key(ctx, false)));
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
