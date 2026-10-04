//! The 3D box tool: an oriented box with viewport handles that highlights
//! the points inside while keeping the surroundings visible, and moves points
//! inside or outside to another layer, judged on original points like selections.
use super::gizmo::{self, AXES, Handle, Handles};
use super::{
    Workbench,
    layers::{destination, destination_combo},
    selection::Tool,
};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::CropBox;
use glam::{DQuat, DVec3};

const EDGE: egui::Color32 = egui::Color32::from_rgb(255, 220, 80);

pub(super) struct Crop {
    /// None until the tool is first used; then placed at the view centre.
    pub(super) region: Option<CropBox>,
    /// Highlight points inside the box, keeping outside points visible.
    pub(super) highlight: bool,
    /// The layer moves go to; none for the "deleted" layer.
    destination: Option<u8>,
    hover: Option<BoxHandle>,
    drag: Option<BoxDrag>,
}

impl Default for Crop {
    fn default() -> Self {
        Self {
            region: None,
            highlight: true,
            destination: None,
            hover: None,
            drag: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoxHandle {
    Transform(Handle),
    Face(usize, bool),
}
struct BoxDrag {
    handle: BoxHandle,
    original: CropBox,
    start: egui::Pos2,
    axis: DVec3,
    per_metre: egui::Vec2,
    start_hit: Option<DVec3>,
    amount: f64,
    /// Drags on the XY square: how far the box has moved.
    offset: DVec3,
}
impl Crop {
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }
    pub(super) fn cancel(&mut self) {
        if let Some(drag) = self.drag.take() {
            self.region = Some(drag.original);
        }
        self.hover = None;
    }
}
/// The six face centres and outward local axes, projected for resize handles.
fn faces(
    camera: &geemil_core::Camera,
    rect: egui::Rect,
    region: CropBox,
) -> Vec<(BoxHandle, egui::Pos2, DVec3, egui::Vec2)> {
    let rotation = DQuat::from_array(region.rotation);
    let center = DVec3::from(region.center);
    let screen = gizmo::screen(camera, rect);
    let mut faces = Vec::new();
    for (i, (axis, _)) in AXES.iter().enumerate() {
        for positive in [false, true] {
            let axis = rotation * *axis * if positive { 1. } else { -1. };
            let face = center + axis * region.size[i] * 0.5;
            if let (Some(pos), Some(mpp)) =
                (screen(face), gizmo::metres_per_point(camera, rect, face))
                && let Some(tip) = screen(face + axis * mpp * 40.)
            {
                let per_metre = (tip - pos) / (mpp * 40.) as f32;
                // An axis viewed end-on cannot be resized by a screen drag.
                if (tip - pos).length() > 4. {
                    faces.push((BoxHandle::Face(i, positive), pos, axis, per_metre));
                }
            }
        }
    }
    faces
}
/// Resize one face, keeping the opposite face fixed, even for a rotated box.
fn resize_face(original: CropBox, i: usize, axis: DVec3, amount: f64) -> CropBox {
    let mut region = original;
    region.size[i] = (original.size[i] + amount).clamp(0.001, 1e6);
    region.center = (DVec3::from(original.center)
        + axis * (region.size[i] - original.size[i]) * 0.5)
        .to_array();
    region
}

impl Workbench {
    /// The box whose interior points are highlighted while the box tool is active.
    pub(super) fn display_highlight_box(&self) -> Option<CropBox> {
        self.crop.region.filter(|_| {
            self.selection.tool == Tool::Box && self.crop.highlight && self.project.is_some()
        })
    }
    /// A box around the view centre, sized to the view.
    fn place_box(&mut self) {
        let h = self.camera.half_height();
        self.crop.region = Some(CropBox {
            center: self.camera.target,
            size: [h, h, h],
            rotation: DQuat::IDENTITY.to_array(),
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
        let idle = self.job.is_none() && !self.crop.dragging();
        egui::Panel::right("crop")
            .resizable(true)
            .default_size(280.)
            .show(ui, |ui| {
                ui.strong(format!("{} {}", icon::CUBE_FOCUS, t.box_title));
                ui.small(t.box_hint);
                ui.add_space(4.);
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("{} {}", icon::CROSSHAIR_SIMPLE, t.box_to_view))
                        .on_hover_text(t.box_to_view_hint)
                        .clicked()
                    {
                        self.crop.cancel();
                        let target = self.camera.target;
                        if let Some(region) = &mut self.crop.region {
                            region.center = target;
                        }
                    }
                    if ui.button(t.box_reset).clicked() {
                        self.crop.cancel();
                        self.place_box();
                    }
                });
                if ui
                    .checkbox(&mut self.crop.highlight, t.box_highlight)
                    .changed()
                {
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
    /// The box's edges, while the box tool is active.
    pub(super) fn draw_box(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some(region) = self.crop.region else {
            return;
        };
        if self.selection.tool != Tool::Box {
            return;
        }
        let painter = ui.painter_at(rect);
        gizmo::draw_box_edges(&painter, &self.camera, rect, &region.corners(), EDGE);
        if self.job.is_some() {
            return;
        }
        let active = self
            .crop
            .drag
            .as_ref()
            .map(|d| d.handle)
            .or(self.crop.hover);
        if let Some(handles) = Handles::new(&self.camera, rect, DVec3::from(region.center)) {
            handles.draw(
                &painter,
                active.and_then(|h| match h {
                    BoxHandle::Transform(h) => Some(h),
                    _ => None,
                }),
            );
        }
        // Face handles are drawn last and take priority where handles overlap.
        for (handle, pos, _, _) in faces(&self.camera, rect, region) {
            let BoxHandle::Face(i, _) = handle else {
                continue;
            };
            let color = if active == Some(handle) {
                EDGE
            } else {
                AXES[i].1
            };
            painter.rect_filled(
                egui::Rect::from_center_size(pos, egui::vec2(12., 12.)),
                1.,
                egui::Color32::BLACK,
            );
            painter.rect_filled(
                egui::Rect::from_center_size(pos, egui::vec2(8., 8.)),
                1.,
                color,
            );
        }
        if let Some(drag) = &self.crop.drag
            && let Some(pos) = ui.ctx().pointer_hover_pos()
        {
            let text = match drag.handle {
                BoxHandle::Transform(h) => h.describe(drag.amount, drag.offset),
                BoxHandle::Face(i, _) => format!("{} {:.3} m", ["X", "Y", "Z"][i], region.size[i]),
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
    /// Capture a box handle before camera input, and preview directly on the box.
    pub(super) fn crop_input(&mut self, response: &egui::Response) {
        if self.selection.tool != Tool::Box || self.project.is_none() || self.job.is_some() {
            self.crop.cancel();
            return;
        }
        let Some(region) = self.crop.region else {
            return;
        };
        let rect = response.rect;
        if let Some(drag) = &mut self.crop.drag {
            response.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            if (response.dragged_by(egui::PointerButton::Primary) || response.drag_stopped())
                && let Some(pos) = response.interact_pointer_pos()
            {
                let mut next = drag.original;
                match drag.handle {
                    BoxHandle::Transform(Handle::Turn(_)) => {
                        let (origin, dir) = gizmo::ray(&self.camera, rect, pos);
                        let center = DVec3::from(drag.original.center);
                        if let (Some(a), Some(b)) = (
                            drag.start_hit,
                            gizmo::plane_hit(origin, dir, center, drag.axis),
                        ) {
                            let (a, b) = (a - center, b - center);
                            drag.amount = drag.axis.dot(a.cross(b)).atan2(a.dot(b));
                            next.rotation = (DQuat::from_axis_angle(drag.axis, drag.amount)
                                * DQuat::from_array(drag.original.rotation))
                            .normalize()
                            .to_array();
                        }
                    }
                    BoxHandle::Transform(Handle::Plane) => {
                        let (origin, dir) = gizmo::ray(&self.camera, rect, pos);
                        let center = DVec3::from(drag.original.center);
                        let hit = gizmo::plane_hit(origin, dir, center, drag.axis);
                        if let Some(offset) = gizmo::plane_offset(drag.start_hit, hit) {
                            drag.offset = offset;
                        }
                        next.center = (center + drag.offset).to_array();
                    }
                    _ => {
                        drag.amount = ((pos - drag.start).dot(drag.per_metre)
                            / drag.per_metre.length_sq().max(1e-9))
                            as f64;
                        match drag.handle {
                            BoxHandle::Transform(Handle::Move(_)) => {
                                next.center =
                                    (DVec3::from(next.center) + drag.axis * drag.amount).to_array()
                            }
                            BoxHandle::Face(i, _) => {
                                next = resize_face(drag.original, i, drag.axis, drag.amount)
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                self.crop.region = Some(next);
                response.ctx.request_repaint();
            }
            if response.drag_stopped() {
                self.crop.drag = None;
            }
            return;
        }
        let start = if response.drag_started_by(egui::PointerButton::Primary) {
            response
                .ctx
                .input(|i| i.pointer.press_origin())
                .or(response.hover_pos())
        } else {
            response.hover_pos()
        };
        let Some(start) = start else {
            self.crop.hover = None;
            return;
        };
        let center = DVec3::from(region.center);
        let hit = faces(&self.camera, rect, region)
            .into_iter()
            .filter(|(_, pos, _, _)| pos.distance(start) <= 9.)
            .min_by(|a, b| a.1.distance(start).total_cmp(&b.1.distance(start)))
            .map(|(h, _, axis, per_metre)| (h, axis, per_metre))
            .or_else(|| {
                Handles::new(&self.camera, rect, center).and_then(|h| {
                    let handle = h.hit(start)?;
                    let i = handle.axis();
                    let per_metre = match handle {
                        Handle::Move(_) => h.tips[i]
                            .map_or(egui::Vec2::ZERO, |tip| (tip - h.center) / h.arm as f32),
                        Handle::Turn(_) | Handle::Plane => egui::Vec2::ZERO,
                    };
                    if matches!(handle, Handle::Move(_)) && per_metre.length_sq() < 1e-9 {
                        return None;
                    }
                    let (origin, dir) = gizmo::ray(&self.camera, rect, start);
                    if matches!(handle, Handle::Turn(_) | Handle::Plane)
                        && gizmo::plane_hit(origin, dir, center, AXES[i].0).is_none()
                    {
                        return None;
                    }
                    Some((BoxHandle::Transform(handle), AXES[i].0, per_metre))
                })
            });
        self.crop.hover = hit.map(|(h, _, _)| h);
        if self.crop.hover.is_some() {
            response.ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some((handle, axis, per_metre)) = hit
        {
            self.flight = None;
            let (origin, dir) = gizmo::ray(&self.camera, rect, start);
            self.crop.drag = Some(BoxDrag {
                handle,
                original: region,
                start,
                axis,
                per_metre,
                start_hit: gizmo::plane_hit(origin, dir, center, axis),
                amount: 0.,
                offset: DVec3::ZERO,
            });
        }
    }
    /// For smoke tests: a 2 m slice at the median height of the displayed points.
    pub(super) fn smoke_box(&mut self, tilted: bool) {
        self.selection.tool = Tool::Box;
        self.place_box();
        let mut heights: Vec<f64> = self.shown_points(false).map(|(.., p)| p.z).collect();
        heights.sort_by(f64::total_cmp);
        let median = heights.get(heights.len() / 2).copied();
        if let Some(region) = &mut self.crop.region {
            let factor = if tilted { 1. } else { 3. };
            region.size = [region.size[0] * factor, region.size[1] * factor, 2.];
            if tilted {
                region.rotation = DQuat::from_euler(glam::EulerRot::XYZ, 0.3, -0.2, 0.4).to_array();
            }
            if let Some(z) = median {
                region.center[2] = z;
            }
        }
        self.crop.highlight = true;
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_rotated_faces_keeps_the_opposite_face_fixed() {
        let rotation = DQuat::from_euler(glam::EulerRot::XYZ, 0.4, -0.6, 0.8);
        let original = CropBox {
            center: [12., -7., 3.],
            size: [4., 6., 8.],
            rotation: rotation.to_array(),
        };
        for (i, (axis, _)) in AXES.iter().enumerate() {
            for sign in [-1., 1.] {
                let outward = rotation * *axis * sign;
                let fixed = DVec3::from(original.center) - outward * original.size[i] * 0.5;
                for amount in [2., -1., -100.] {
                    let resized = resize_face(original, i, outward, amount);
                    let opposite = DVec3::from(resized.center) - outward * resized.size[i] * 0.5;
                    assert!(fixed.distance(opposite) < 1e-10);
                    assert!(resized.size[i] >= 0.001);
                    assert_eq!(resized.rotation, original.rotation);
                    for j in (0..3).filter(|j| *j != i) {
                        assert_eq!(resized.size[j], original.size[j]);
                    }
                }
            }
        }
    }

    #[test]
    fn escape_restores_the_box_before_the_drag() {
        let original = CropBox {
            center: [1., 2., 3.],
            size: [4., 5., 6.],
            rotation: DQuat::from_rotation_x(0.7).to_array(),
        };
        let mut crop = Crop {
            region: Some(resize_face(
                original,
                2,
                DQuat::from_array(original.rotation) * DVec3::Z,
                2.,
            )),
            drag: Some(BoxDrag {
                handle: BoxHandle::Face(2, true),
                original,
                start: egui::Pos2::ZERO,
                axis: DVec3::Z,
                per_metre: egui::Vec2::X,
                start_hit: None,
                amount: 2.,
                offset: DVec3::ZERO,
            }),
            ..Default::default()
        };
        crop.cancel();
        assert_eq!(crop.region, Some(original));
        assert!(!crop.dragging());
    }
}
