//! The bounding box of the scan or folder selected in the tree, and the move
//! and rotate tool: like Potree's transformation tool, dragging an arrow moves
//! the item along a world axis and dragging a ring turns it about one, around
//! the centre of its box. A drag is previewed live and becomes one edit, which
//! can be undone, when released.
use super::{Workbench, selection::Tool};
use eframe::egui;
use geemil_core::{Bounds, Camera, Pose, Project};
use glam::{DMat4, DVec3};
use uuid::Uuid;

const BOX: egui::Color32 = egui::Color32::from_rgb(235, 235, 235);
pub(super) const AXES: [(DVec3, egui::Color32); 3] = [
    (DVec3::X, egui::Color32::from_rgb(235, 75, 75)),
    (DVec3::Y, egui::Color32::from_rgb(95, 205, 95)),
    (DVec3::Z, egui::Color32::from_rgb(85, 145, 255)),
];
const AXIS_NAMES: [&str; 3] = ["X", "Y", "Z"];
/// Arrow length and ring radius on screen, in points.
const ARM: f64 = 90.;
const RING: f64 = 65.;
/// How near a handle the pointer must be to grab it, in points.
const GRAB: f32 = 7.;
const RING_SEGMENTS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Handle {
    Move(usize),
    Turn(usize),
}

#[derive(Default)]
pub(super) struct Gizmo {
    hover: Option<Handle>,
    drag: Option<Drag>,
}
struct Drag {
    item: Uuid,
    handle: Handle,
    /// The state the drag started from; any other state ends it.
    state: Uuid,
    /// The item's own transform when the drag started.
    own: Pose,
    /// The pivot: the centre of the item's box when the drag started.
    center: DVec3,
    start: egui::Pos2,
    /// Moves: where one metre along the axis goes on screen.
    per_metre: egui::Vec2,
    /// Turns: where the pointer's ray met the ring's plane at the start.
    start_hit: Option<DVec3>,
    /// Metres or radians so far, and the transform they give.
    amount: f64,
    preview: Option<Pose>,
}
impl Gizmo {
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }
    pub(super) fn cancel(&mut self) {
        self.drag = None;
    }
}

/// The box of a scan or folder: a frame and bounds in it. A scan's frame is its
/// world matrix and its bounds its own; a folder's frame is its transform with
/// the folders above, and its bounds enclose its scans in that frame, so the
/// box turns with the folder. `pose` replaces the item's own transform.
pub(super) fn item_box(p: &Project, item: Uuid, pose: Option<Pose>) -> Option<(DMat4, Bounds)> {
    if let Some(scan) = p.scans().find(|s| s.id == item) {
        let bounds = scan.nodes.first()?.bounds;
        let world = match pose {
            Some(pose) => p.world_matrix_with(scan, item, pose),
            None => p.world_matrix(scan),
        };
        return Some((world, bounds));
    }
    let frame = p.correction(item);
    let to_frame = frame.inverse();
    let inside = p.scans_within(item);
    let mut bounds: Option<Bounds> = None;
    for scan in p.scans().filter(|s| inside.contains(&s.id)) {
        let Some(root) = scan.nodes.first() else {
            continue;
        };
        let to = to_frame * p.world_matrix(scan);
        for corner in root.bounds.corners() {
            let q = to.transform_point3(corner).to_array();
            match &mut bounds {
                Some(b) => b.include(q),
                None => bounds = Some(Bounds::at(q)),
            }
        }
    }
    let frame = pose.map_or(frame, |pose| p.correction_with(item, item, pose));
    Some((frame, bounds?))
}

/// Projects world points to the screen within `rect`.
pub(super) fn screen(camera: &Camera, rect: egui::Rect) -> impl Fn(DVec3) -> Option<egui::Pos2> {
    let projector = camera.projector();
    move |p| {
        projector.project(p).map(|(uv, _)| {
            rect.left_top() + egui::vec2(uv[0] as f32 * rect.width(), uv[1] as f32 * rect.height())
        })
    }
}

/// Draws the twelve edges of a box given by its corners (in `Bounds::corners`
/// order). In perspective, edges are cut just in front of the eye so parts
/// behind the camera do not hide the rest.
pub(super) fn draw_box_edges(
    painter: &egui::Painter,
    camera: &Camera,
    rect: egui::Rect,
    corners: &[DVec3; 8],
    color: egui::Color32,
) {
    let to_screen = screen(camera, rect);
    let depth = camera.depth_row();
    let near = if camera.ortho {
        f64::NEG_INFINITY
    } else {
        camera.distance * 1e-3
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
    for a in 0..8usize {
        for bit in [1, 2, 4] {
            let b = a | bit;
            if b == a {
                continue;
            }
            let Some((ea, eb)) = clip_edge(corners[a], corners[b]) else {
                continue;
            };
            if let (Some(pa), Some(pb)) = (to_screen(ea), to_screen(eb)) {
                painter.line_segment([pa, pb], egui::Stroke::new(3., egui::Color32::BLACK));
                painter.line_segment([pa, pb], egui::Stroke::new(1.5, color));
            }
        }
    }
}

/// Metres per screen point at `p`, or none behind the eye.
pub(super) fn metres_per_point(camera: &Camera, rect: egui::Rect, p: DVec3) -> Option<f64> {
    let height = if camera.ortho {
        2. * camera.half_height()
    } else {
        let depth = camera.depth_row().dot(p.extend(1.));
        if depth <= camera.distance * 1e-3 {
            return None;
        }
        2. * depth * (camera.fov * 0.5).tan()
    };
    Some(height / rect.height() as f64)
}

/// Two unit vectors spanning the plane square to `axis`.
fn plane_basis(axis: DVec3) -> (DVec3, DVec3) {
    let u = axis.any_orthonormal_vector();
    (u, axis.cross(u))
}

/// The ray through a screen position: its origin and unit direction.
pub(super) fn ray(camera: &Camera, rect: egui::Rect, pos: egui::Pos2) -> (DVec3, DVec3) {
    let x = 2. * ((pos.x - rect.left()) / rect.width()) as f64 - 1.;
    let y = 1. - 2. * ((pos.y - rect.top()) / rect.height()) as f64;
    let inverse = camera.matrix().inverse();
    let near = inverse.project_point3(DVec3::new(x, y, 0.));
    let far = inverse.project_point3(DVec3::new(x, y, 1.));
    (near, (far - near).normalize())
}

/// Where the ray meets the plane through `center` square to `axis`, unless it
/// runs nearly along the plane.
pub(super) fn plane_hit(origin: DVec3, dir: DVec3, center: DVec3, axis: DVec3) -> Option<DVec3> {
    let along = dir.dot(axis);
    (along.abs() > 0.05).then(|| origin + dir * ((center - origin).dot(axis) / along))
}

fn segment_distance(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0., 1.);
    p.distance(a + ab * t)
}

/// The screen geometry of the handles around `center`.
pub(super) struct Handles {
    pub(super) center: egui::Pos2,
    /// Arrow tips.
    pub(super) tips: [Option<egui::Pos2>; 3],
    /// Ring outlines; points behind the eye are left out.
    rings: [Vec<egui::Pos2>; 3],
    /// Arrow length and ring radius in metres.
    pub(super) arm: f64,
}
impl Handles {
    pub(super) fn new(camera: &Camera, rect: egui::Rect, center: DVec3) -> Option<Self> {
        let to_screen = screen(camera, rect);
        let mpp = metres_per_point(camera, rect, center)?;
        let (arm, radius) = (ARM * mpp, RING * mpp);
        let tips = AXES.map(|(axis, _)| to_screen(center + axis * arm));
        let rings = AXES.map(|(axis, _)| {
            let (u, v) = plane_basis(axis);
            (0..=RING_SEGMENTS)
                .filter_map(|i| {
                    let angle = i as f64 / RING_SEGMENTS as f64 * std::f64::consts::TAU;
                    to_screen(center + (u * angle.cos() + v * angle.sin()) * radius)
                })
                .collect()
        });
        Some(Self {
            center: to_screen(center)?,
            tips,
            rings,
            arm,
        })
    }
    /// The handle under `pos`: arrows before rings.
    pub(super) fn hit(&self, pos: egui::Pos2) -> Option<Handle> {
        let arrows = (0..3).filter_map(|i| {
            let tip = self.tips[i]?;
            Some((segment_distance(pos, self.center, tip), Handle::Move(i)))
        });
        let rings = (0..3).filter_map(|i| {
            let d = self.rings[i]
                .windows(2)
                .map(|w| segment_distance(pos, w[0], w[1]))
                .fold(f32::INFINITY, f32::min);
            d.is_finite().then_some((d + 0.5, Handle::Turn(i)))
        });
        arrows
            .chain(rings)
            .filter(|(d, _)| *d <= GRAB)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, handle)| handle)
    }
}

impl Handles {
    pub(super) fn draw(&self, painter: &egui::Painter, active: Option<Handle>) {
        let width = |handle| if active == Some(handle) { 4. } else { 2.5 };
        let color = |i: usize, handle| {
            if active == Some(handle) {
                egui::Color32::from_rgb(255, 230, 90)
            } else {
                AXES[i].1
            }
        };
        let outline = |w: f32| egui::Stroke::new(w + 2., egui::Color32::from_black_alpha(160));
        for (i, ring) in self.rings.iter().enumerate() {
            let handle = Handle::Turn(i);
            let w = width(handle);
            painter.add(egui::Shape::line(ring.clone(), outline(w)));
            painter.add(egui::Shape::line(
                ring.clone(),
                egui::Stroke::new(w, color(i, handle)),
            ));
        }
        for (i, tip) in self.tips.iter().enumerate() {
            let Some(tip) = *tip else { continue };
            let handle = Handle::Move(i);
            let w = width(handle);
            let c = color(i, handle);
            painter.line_segment([self.center, tip], outline(w));
            painter.line_segment([self.center, tip], egui::Stroke::new(w, c));
            let dir = (tip - self.center).normalized();
            let side = egui::vec2(-dir.y, dir.x);
            let head = vec![tip + dir * 10., tip + side * 5., tip - side * 5.];
            painter.add(egui::Shape::convex_polygon(head, c, outline(0.)));
        }
        painter.circle(
            self.center,
            3.5,
            egui::Color32::WHITE,
            egui::Stroke::new(1., egui::Color32::BLACK),
        );
    }
}

impl Workbench {
    /// The tree selection, if it holds any scans.
    fn boxed_item(&self) -> Option<(std::sync::Arc<Project>, Uuid)> {
        let p = self.project.clone()?;
        let item = self
            .single_tree_item()
            .filter(|id| !p.scans_within(*id).is_empty())?;
        Some((p, item))
    }
    fn transforming(&self) -> bool {
        self.selection.tool == Tool::Transform && self.job.is_none()
    }
    /// The transform a drag shows in place of the applied one.
    pub(super) fn gizmo_preview(&self) -> Option<(Uuid, Pose)> {
        let drag = self.gizmo.drag.as_ref()?;
        Some((drag.item, drag.preview?))
    }
    /// Grabs, drags and releases handles. Call before the camera input, which
    /// leaves the left drag alone while a handle is held.
    pub(super) fn gizmo_input(&mut self, response: &egui::Response) {
        let selected = self.boxed_item().filter(|_| self.transforming());
        let Some((p, item)) = selected else {
            self.gizmo = Gizmo::default();
            return;
        };
        let state = p.current().id;
        if self
            .gizmo
            .drag
            .as_ref()
            .is_some_and(|d| d.item != item || d.state != state)
        {
            self.gizmo.drag = None;
        }
        let rect = response.rect;
        let camera = self.camera;
        if let Some(drag) = &mut self.gizmo.drag {
            if response.dragged_by(egui::PointerButton::Primary)
                && let Some(pos) = response.interact_pointer_pos()
            {
                let (axis, _) = AXES[match drag.handle {
                    Handle::Move(i) | Handle::Turn(i) => i,
                }];
                let motion = match drag.handle {
                    Handle::Move(_) => {
                        let s = drag.per_metre;
                        let along = (pos - drag.start).dot(s) / s.length_sq().max(1e-9);
                        drag.amount = along as f64;
                        DMat4::from_translation(axis * drag.amount)
                    }
                    Handle::Turn(_) => {
                        let (origin, dir) = ray(&camera, rect, pos);
                        let hit = plane_hit(origin, dir, drag.center, axis);
                        if let (Some(a), Some(b)) = (drag.start_hit, hit) {
                            let (a, b) = (a - drag.center, b - drag.center);
                            drag.amount = axis.dot(a.cross(b)).atan2(a.dot(b));
                        }
                        DMat4::from_translation(drag.center)
                            * DMat4::from_axis_angle(axis, drag.amount)
                            * DMat4::from_translation(-drag.center)
                    }
                };
                drag.preview = Some(p.moved_pose(item, drag.own, motion));
            }
            if response.drag_stopped() {
                let pose = drag.preview;
                self.gizmo.drag = None;
                if let Some(pose) = pose {
                    self.apply_edit(|p| p.set_transform(item, pose));
                }
            }
            return;
        }
        let Some((frame, bounds)) = item_box(&p, item, None) else {
            return;
        };
        let center = frame.transform_point3(bounds.center());
        let handles = Handles::new(&camera, rect, center);
        let pointer = response.hover_pos();
        self.gizmo.hover = handles
            .as_ref()
            .zip(pointer)
            .and_then(|(h, pos)| h.hit(pos));
        if !response.drag_started_by(egui::PointerButton::Primary) {
            return;
        }
        // Judge the grab where the button went down, not where the drag began.
        let origin = response.ctx.input(|i| i.pointer.press_origin());
        let (Some(handles), Some(start)) = (handles, origin.or(pointer)) else {
            return;
        };
        let Some(handle) = handles.hit(start) else {
            return;
        };
        let (axis, _) = AXES[match handle {
            Handle::Move(i) | Handle::Turn(i) => i,
        }];
        let per_metre = match handle {
            Handle::Move(i) => handles.tips[i].map_or(egui::Vec2::ZERO, |tip| {
                (tip - handles.center) / handles.arm as f32
            }),
            Handle::Turn(_) => egui::Vec2::ZERO,
        };
        let (origin, dir) = ray(&camera, rect, start);
        self.gizmo.drag = Some(Drag {
            item,
            handle,
            state,
            own: p
                .current()
                .transforms
                .get(&item)
                .copied()
                .unwrap_or_default(),
            center,
            start,
            per_metre,
            start_hit: plane_hit(origin, dir, center, axis),
            amount: 0.,
            preview: None,
        });
    }
    /// The selected item's box, and the handles while the tool is active.
    pub(super) fn draw_gizmo(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some((p, item)) = self.boxed_item() else {
            return;
        };
        let pose = self
            .transform_preview()
            .filter(|(id, _)| *id == item)
            .map(|(_, pose)| pose);
        let Some((frame, bounds)) = item_box(&p, item, pose) else {
            return;
        };
        let painter = ui.painter_at(rect);
        let corners: [DVec3; 8] = std::array::from_fn(|i| {
            let c = bounds.corners().nth(i).unwrap();
            frame.transform_point3(c)
        });
        draw_box_edges(&painter, &self.camera, rect, &corners, BOX);
        if !self.transforming() {
            return;
        }
        let center = frame.transform_point3(bounds.center());
        let Some(handles) = Handles::new(&self.camera, rect, center) else {
            return;
        };
        let active = self
            .gizmo
            .drag
            .as_ref()
            .map(|d| d.handle)
            .or(self.gizmo.hover);
        handles.draw(&painter, active);
        if let Some(drag) = &self.gizmo.drag
            && let Some(pos) = ui.ctx().pointer_hover_pos()
        {
            let text = match drag.handle {
                Handle::Move(i) => format!("{} {:+.3} m", AXIS_NAMES[i], drag.amount),
                Handle::Turn(i) => format!("{} {:+.2}°", AXIS_NAMES[i], drag.amount.to_degrees()),
            };
            painter.text(
                pos + egui::vec2(14., -14.),
                egui::Align2::LEFT_BOTTOM,
                text,
                egui::FontId::proportional(14.),
                egui::Color32::WHITE,
            );
        }
    }
}
