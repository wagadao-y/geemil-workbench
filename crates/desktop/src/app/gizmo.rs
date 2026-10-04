//! The bounding box of the scan or folder selected in the tree, and the move
//! and rotate tool: like Potree's transformation tool, dragging an arrow moves
//! the item along a world axis, dragging the square between X and Y moves it
//! in the horizontal plane, and dragging a ring turns it about an axis, around
//! the scanner position, or the centre of the box where there is none.
//! Double-clicking a displayed point puts the handles there instead, to turn
//! about a feature that already matches. A drag is previewed live and becomes
//! one edit, which can be undone, when released.
use super::{Workbench, selection::Tool};
use eframe::egui;
use geemil_core::{Bounds, Camera, Pose, Project};
use glam::{DMat4, DVec3};
use std::collections::HashSet;
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
/// The XY square's extent along X and Y, as fractions of the arrow length:
/// clear of the arrows and inside the Z ring.
const PLANE: (f64, f64) = (0.2, 0.45);
/// Below this on-screen area (square points) the XY square is seen edge on
/// and cannot be dragged.
const PLANE_MIN_AREA: f32 = 80.;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Handle {
    Move(usize),
    Turn(usize),
    /// Moves in the horizontal (XY) plane.
    Plane,
}
impl Handle {
    /// The world axis moved along or turned about; the plane's normal (Z).
    pub(super) fn axis(self) -> usize {
        match self {
            Handle::Move(i) | Handle::Turn(i) => i,
            Handle::Plane => 2,
        }
    }
    /// What a drag has done so far: metres, radians, or for the plane the
    /// offset; as shown next to the pointer.
    pub(super) fn describe(self, amount: f64, offset: DVec3) -> String {
        match self {
            Handle::Move(i) => format!("{} {:+.3} m", AXIS_NAMES[i], amount),
            Handle::Turn(i) => format!("{} {:+.2}°", AXIS_NAMES[i], amount.to_degrees()),
            Handle::Plane => format!("X {:+.3} m  Y {:+.3} m", offset.x, offset.y),
        }
    }
}
/// How far a drag on the XY square has moved: from where the pointer's ray met
/// the horizontal plane through the pivot at the start to where it meets it now.
pub(super) fn plane_offset(start: Option<DVec3>, now: Option<DVec3>) -> Option<DVec3> {
    let offset = now? - start?;
    Some(DVec3::new(offset.x, offset.y, 0.))
}

#[derive(Default)]
pub(super) struct Gizmo {
    hover: Option<Handle>,
    drag: Option<Drag>,
    /// Where the user put the handles of an item, in its own frame
    /// ([`item_frame`]), so they move with it through edits and undo.
    placed: Option<(Uuid, DVec3)>,
}
struct Drag {
    item: Uuid,
    handle: Handle,
    /// The state the drag started from; any other state ends it.
    state: Uuid,
    /// The item's own transform when the drag started.
    own: Pose,
    /// The pivot ([`item_pivot`]) when the drag started.
    center: DVec3,
    start: egui::Pos2,
    /// Moves: where one metre along the axis goes on screen.
    per_metre: egui::Vec2,
    /// Turns, and drags on the XY square: where the pointer's ray met the
    /// ring's or square's plane at the start.
    start_hit: Option<DVec3>,
    /// Metres or radians so far, the XY square's offset, and the transform
    /// they give.
    amount: f64,
    offset: DVec3,
    preview: Option<Pose>,
}
impl Gizmo {
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }
    /// Where the user put `item`'s handles, in its own frame.
    fn placed(&self, item: Uuid) -> Option<DVec3> {
        self.placed.filter(|(id, _)| *id == item).map(|(_, p)| p)
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
    let inside: HashSet<Uuid> = p.scans_within(item).into_iter().collect();
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

/// Where the handles sit and the item turns about: a scan's scanner position
/// ([`Project::scanner_position`]), so a levelled scan turns about its
/// instrument; else the centre of the item's box, for scans without one
/// (LAS/LAZ keep no scanner position) and for folders.
/// `placed`, in the item's own frame, overrides both.
pub(super) fn item_pivot(
    p: &Project,
    item: Uuid,
    pose: Option<Pose>,
    placed: Option<DVec3>,
) -> Option<DVec3> {
    if let Some(local) = placed {
        return Some(item_frame(p, item, pose)?.transform_point3(local));
    }
    if let Some(scan) = p.scan(item)
        && let Some(position) = p.scanner_position(scan)
    {
        let world = match pose {
            Some(pose) => p.world_matrix_with(scan, item, pose),
            None => p.world_matrix(scan),
        };
        return Some(world.transform_point3(position));
    }
    let (frame, bounds) = item_box(p, item, pose)?;
    Some(frame.transform_point3(bounds.center()))
}

/// An item's own frame in the project: a scan's world matrix, a folder's
/// transform with the folders above. `pose` replaces the item's own transform.
fn item_frame(p: &Project, item: Uuid, pose: Option<Pose>) -> Option<DMat4> {
    if let Some(scan) = p.scan(item) {
        return Some(match pose {
            Some(pose) => p.world_matrix_with(scan, item, pose),
            None => p.world_matrix(scan),
        });
    }
    p.groups().iter().any(|g| g.id == item).then(|| match pose {
        Some(pose) => p.correction_with(item, item, pose),
        None => p.correction(item),
    })
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

/// The area of a simple polygon on screen.
fn polygon_area(points: &[egui::Pos2]) -> f32 {
    let n = points.len();
    let twice: f32 = (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    twice.abs() / 2.
}

/// Whether `p` is inside a convex polygon, whichever way it winds.
fn inside(polygon: &[egui::Pos2], p: egui::Pos2) -> bool {
    let n = polygon.len();
    let sides: Vec<f32> = (0..n)
        .map(|i| {
            let (edge, to) = (polygon[(i + 1) % n] - polygon[i], p - polygon[i]);
            edge.x * to.y - edge.y * to.x
        })
        .collect();
    sides.iter().all(|s| *s >= 0.) || sides.iter().all(|s| *s <= 0.)
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
    /// The XY square's corners, unless it is seen edge on or behind the eye.
    plane: Option<[egui::Pos2; 4]>,
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
        let (a, b) = (PLANE.0 * arm, PLANE.1 * arm);
        let corners =
            [(a, a), (b, a), (b, b), (a, b)].map(|(x, y)| to_screen(center + DVec3::new(x, y, 0.)));
        let plane = corners
            .iter()
            .all(Option::is_some)
            .then(|| corners.map(Option::unwrap))
            .filter(|q| polygon_area(q) >= PLANE_MIN_AREA);
        Some(Self {
            center: to_screen(center)?,
            tips,
            rings,
            plane,
            arm,
        })
    }
    /// The handle under `pos`: arrows before the XY square before rings.
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
        // Inside the square scores as a near miss, so an arrow right under the
        // pointer still wins where they overlap.
        let plane = self
            .plane
            .filter(|q| inside(q, pos))
            .map(|_| (2., Handle::Plane));
        arrows
            .chain(plane)
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
        if let Some(square) = self.plane {
            let active = active == Some(Handle::Plane);
            let [r, g, b, _] = AXES[2].1.to_array();
            let (fill, stroke) = if active {
                (
                    egui::Color32::from_rgba_unmultiplied(255, 230, 90, 150),
                    egui::Color32::from_rgb(255, 230, 90),
                )
            } else {
                (
                    egui::Color32::from_rgba_unmultiplied(r, g, b, 90),
                    AXES[2].1,
                )
            };
            painter.add(egui::Shape::convex_polygon(
                square.to_vec(),
                fill,
                egui::Stroke::new(if active { 2.5 } else { 1.5 }, stroke),
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
        // Handles put elsewhere belong to the item they were put for.
        if self.gizmo.placed.is_some_and(|(id, _)| id != item) {
            self.gizmo.placed = None;
        }
        let rect = response.rect;
        let camera = self.camera;
        if let Some(drag) = &mut self.gizmo.drag {
            if response.dragged_by(egui::PointerButton::Primary)
                && let Some(pos) = response.interact_pointer_pos()
            {
                let (axis, _) = AXES[drag.handle.axis()];
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
                    Handle::Plane => {
                        let (origin, dir) = ray(&camera, rect, pos);
                        let hit = plane_hit(origin, dir, drag.center, axis);
                        if let Some(offset) = plane_offset(drag.start_hit, hit) {
                            drag.offset = offset;
                        }
                        DMat4::from_translation(drag.offset)
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
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let click = [
                ((pos.x - rect.left()) / rect.width()) as f64,
                ((pos.y - rect.top()) / rect.height()) as f64,
            ];
            self.place_handles(&p, item, click, [rect.width() as f64, rect.height() as f64]);
            return;
        }
        let Some(center) = item_pivot(&p, item, None, self.gizmo.placed(item)) else {
            return;
        };
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
        let (axis, _) = AXES[handle.axis()];
        let per_metre = match handle {
            Handle::Move(i) => handles.tips[i].map_or(egui::Vec2::ZERO, |tip| {
                (tip - handles.center) / handles.arm as f32
            }),
            Handle::Turn(_) | Handle::Plane => egui::Vec2::ZERO,
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
            offset: DVec3::ZERO,
            preview: None,
        });
    }
    /// Puts the handles on the displayed point of any scan double-clicked at
    /// `click` (normalized viewport coordinates); a double-click on no point
    /// returns them to their default. `viewport` is in screen points.
    pub(super) fn place_handles(
        &mut self,
        p: &Project,
        item: Uuid,
        click: [f64; 2],
        viewport: [f64; 2],
    ) {
        let radius = self.settings.point_size as f64 / 2.;
        let points = self.shown_points(true).map(|(.., p)| p);
        let picked = super::navigation::pick(points, &self.camera, click, viewport, radius);
        self.gizmo.placed = picked
            .zip(item_frame(p, item, None))
            .map(|(world, frame)| (item, frame.inverse().transform_point3(world)));
        self.gizmo.hover = None;
    }
    /// For smoke tests: the handles of the selected item, in the project frame.
    pub(super) fn smoke_handles(&self) -> Option<DVec3> {
        let (p, item) = self.boxed_item()?;
        item_pivot(&p, item, None, self.gizmo.placed(item))
    }
    /// The edges of the selected item's box, which the renderer draws behind
    /// points in front of them, with a dark outline, in physical pixels.
    pub(super) fn box_lines(&self, pixels: f32) -> Vec<crate::render::Line> {
        let Some((p, item)) = self.boxed_item() else {
            return vec![];
        };
        let pose = self
            .transform_preview()
            .filter(|(id, _)| *id == item)
            .map(|(_, pose)| pose);
        let Some((frame, bounds)) = item_box(&p, item, pose) else {
            return vec![];
        };
        let corners: Vec<DVec3> = bounds
            .corners()
            .map(|c| frame.transform_point3(c))
            .collect();
        let mut lines = vec![];
        for (color, width) in [([0, 0, 0, 255], 3.), (BOX.to_array(), 1.5)] {
            for a in 0..8usize {
                for bit in [1, 2, 4] {
                    if a & bit == 0 {
                        lines.push(crate::render::Line {
                            a: corners[a],
                            b: corners[a | bit],
                            color,
                            width: width * pixels,
                        });
                    }
                }
            }
        }
        lines
    }
    /// The handles while the tool is active; the selected item's box is
    /// drawn by the renderer (see `box_lines`).
    pub(super) fn draw_gizmo(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some((p, item)) = self.boxed_item() else {
            return;
        };
        let pose = self
            .transform_preview()
            .filter(|(id, _)| *id == item)
            .map(|(_, pose)| pose);
        let painter = ui.painter_at(rect);
        if !self.transforming() {
            return;
        }
        let Some(handles) = item_pivot(&p, item, pose, self.gizmo.placed(item))
            .and_then(|center| Handles::new(&self.camera, rect, center))
        else {
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
            let text = drag.handle.describe(drag.amount, drag.offset);
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

#[cfg(test)]
mod tests {
    use super::{
        Camera, Handle, Handles, item_frame, item_pivot, plane_hit, plane_offset, ray, screen,
    };
    use eframe::egui;
    use geemil_core::{ImportOptions, JobControl, Pose, Project, interchange};
    use glam::{DMat4, DVec3};

    #[test]
    fn turning_about_placed_handles_keeps_that_point_and_they_follow_moves() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("demo.e57");
        interchange::create_demo(&source).unwrap();
        let mut p = Project::create(&dir.path().join("p"), "Test").unwrap();
        p.import_file(&source, ImportOptions::default(), &JobControl::default())
            .unwrap();
        let scan = p.scans().next().unwrap().id;
        let folder = p.groups()[0].id;
        for item in [scan, folder] {
            // A point away from the default pivot, kept in the item's frame.
            let point = DVec3::new(1.5, -0.8, 0.2);
            let local = item_frame(&p, item, None)
                .unwrap()
                .inverse()
                .transform_point3(point);
            let own = p
                .current()
                .transforms
                .get(&item)
                .copied()
                .unwrap_or_default();
            let turn = DMat4::from_translation(point)
                * DMat4::from_rotation_z(0.4)
                * DMat4::from_translation(-point);
            let turned = p.moved_pose(item, own, turn);
            let pivot = item_pivot(&p, item, Some(turned), Some(local)).unwrap();
            assert!(pivot.distance(point) < 1e-9, "{pivot:?}");
            assert!(
                item_pivot(&p, item, Some(turned), None)
                    .unwrap()
                    .distance(point)
                    > 0.1
            );

            let shift = DVec3::new(2., 1., 0.);
            let moved = p.moved_pose(item, own, DMat4::from_translation(shift));
            let pivot = item_pivot(&p, item, Some(moved), Some(local)).unwrap();
            assert!(pivot.distance(point + shift) < 1e-9);
            p.set_transform(item, moved).unwrap();
            let pivot = item_pivot(&p, item, None, Some(local)).unwrap();
            assert!(pivot.distance(point + shift) < 1e-9);
            p.set_transform(item, Pose::default()).unwrap();
        }
    }

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(40., 30.), egui::vec2(1200., 800.))
    }

    #[test]
    fn the_xy_square_moves_with_the_ground_under_the_pointer() {
        let camera = Camera {
            target: [10., 20., 5.],
            yaw: 0.6,
            pitch: 0.9,
            distance: 30.,
            aspect: 1.5,
            ..Camera::default()
        };
        let center = DVec3::from(camera.target);
        let handles = Handles::new(&camera, rect(), center).unwrap();
        let square = handles.plane.expect("seen from above");
        let middle = (square.iter().fold(egui::Vec2::ZERO, |a, p| a + p.to_vec2()) / 4.).to_pos2();
        assert_eq!(handles.hit(middle), Some(Handle::Plane));
        // The arrows' tips still grab their own handles.
        assert_eq!(handles.hit(handles.tips[0].unwrap()), Some(Handle::Move(0)));

        let hit = |pos| {
            let (origin, dir) = ray(&camera, rect(), pos);
            plane_hit(origin, dir, center, DVec3::Z)
        };
        let moved = DVec3::new(1.5, -0.7, 0.);
        let pos = screen(&camera, rect())(hit(middle).unwrap() + moved).unwrap();
        let offset = plane_offset(hit(middle), hit(pos)).unwrap();
        assert!((offset - moved).length() < 1e-2, "{offset:?}");
        assert_eq!(offset.z, 0.);
    }

    #[test]
    fn the_xy_square_is_left_out_edge_on() {
        let camera = Camera {
            target: [0., 0., 0.],
            pitch: 0.,
            distance: 30.,
            aspect: 1.5,
            ..Camera::default()
        };
        let handles = Handles::new(&camera, rect(), DVec3::ZERO).unwrap();
        assert!(handles.plane.is_none());
    }
}
