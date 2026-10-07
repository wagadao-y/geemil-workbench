//! Viewing placed panoramas: a marker in the point cloud where each photo was
//! taken, and a mode showing one photo in the main view. There the view turns
//! and zooms but stays where the photo was taken; markers of the other
//! photos lead on to them.
use super::{
    Workbench,
    actions::MAX_PITCH,
    panorama::{Lens, PHOTO, View, direction, look_input},
    selection::Tool,
};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{Camera, Project};
use glam::{DMat4, DVec3};
use uuid::Uuid;

/// Pointer distance in points within which a marker takes a click.
const MARKER_REACH: f32 = 12.;

/// Viewing one panorama in the main view.
pub(super) struct Tour {
    pub(super) panorama: Uuid,
    view: View,
    /// The point cloud camera to return to.
    before: Camera,
}

impl Tour {
    pub(super) fn view_mut(&mut self) -> &mut View {
        &mut self.view
    }
}

/// Turns a view in one panorama's frame into the same look in another's.
fn carried(view: &View, from: DMat4, to: DMat4) -> View {
    let world = from.transform_vector3(direction(view.yaw, view.pitch));
    let d = to.inverse().transform_vector3(world).normalize();
    View {
        yaw: d.y.atan2(d.x),
        pitch: d.z.asin(),
        ..*view
    }
}

/// A marker: a ring with the panorama icon, its name beside it when hovered.
fn draw_marker(painter: &egui::Painter, at: egui::Pos2, name: &str, hovered: bool) {
    let radius = if hovered { 11. } else { 9. };
    painter.circle(
        at,
        radius,
        egui::Color32::from_black_alpha(170),
        egui::Stroke::new(2., PHOTO),
    );
    painter.text(
        at,
        egui::Align2::CENTER_CENTER,
        icon::PANORAMA,
        egui::FontId::proportional(radius + 2.),
        PHOTO,
    );
    if hovered {
        let galley = painter.layout_no_wrap(
            name.to_owned(),
            egui::FontId::proportional(13.),
            egui::Color32::WHITE,
        );
        let corner = at + egui::vec2(radius + 6., -galley.size().y / 2.);
        painter.rect_filled(
            egui::Rect::from_min_size(corner, galley.size()).expand(3.),
            3.,
            egui::Color32::from_black_alpha(190),
        );
        painter.galley(corner, galley, egui::Color32::WHITE);
    }
}

impl Workbench {
    /// Placed panoramas and where they are in the project frame.
    fn placed_panoramas(&self, p: &Project) -> Vec<(Uuid, DMat4)> {
        p.panoramas()
            .filter(|pano| p.registration(pano.id).is_some())
            .map(|pano| (pano.id, p.correction(pano.id)))
            .collect()
    }
    /// The markers on the point cloud, where `rect` shows it: each placed
    /// panorama with its place on screen. The one the panorama tool places
    /// has a marker of its own.
    fn panorama_markers(&self, rect: egui::Rect) -> Vec<(Uuid, egui::Pos2)> {
        let Some(p) = self
            .project
            .as_ref()
            .filter(|_| self.settings.show_panoramas)
        else {
            return vec![];
        };
        let placing = self
            .placing_panorama()
            .then_some(self.panorama.item)
            .flatten();
        let projector = self.camera.projector();
        self.placed_panoramas(p)
            .into_iter()
            .filter(|(id, _)| Some(*id) != placing)
            .filter_map(|(id, world)| {
                let (uv, _) = projector.project(world.transform_point3(DVec3::ZERO))?;
                let at = rect.left_top()
                    + egui::vec2(uv[0] as f32 * rect.width(), uv[1] as f32 * rect.height());
                rect.contains(at).then_some((id, at))
            })
            .collect()
    }
    /// The marker under the pointer, if any.
    fn hovered_marker(&self, response: &egui::Response) -> Option<Uuid> {
        let pointer = response.hover_pos()?;
        self.panorama_markers(response.rect)
            .into_iter()
            .map(|(id, at)| (id, at.distance(pointer)))
            .filter(|(_, d)| *d <= MARKER_REACH)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }
    /// A click on a marker in camera mode opens its panorama. Returns
    /// whether the click was taken.
    pub(super) fn panorama_marker_input(&mut self, response: &egui::Response) -> bool {
        if self.selection.tool != Tool::Navigate {
            return false;
        }
        let Some(id) = self.hovered_marker(response) else {
            return false;
        };
        response.ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        if response.clicked() {
            self.enter_tour(id);
            return true;
        }
        false
    }
    pub(super) fn draw_panorama_markers(&self, ui: &egui::Ui, response: &egui::Response) {
        let Some(p) = self.project.as_ref() else {
            return;
        };
        let hovered = self
            .hovered_marker(response)
            .filter(|_| self.selection.tool == Tool::Navigate);
        let painter = ui.painter_at(response.rect);
        for (id, at) in self.panorama_markers(response.rect) {
            if let Some(pano) = p.panorama(id) {
                draw_marker(&painter, at, p.panorama_name(pano), hovered == Some(id));
            }
        }
    }
    /// Shows a placed panorama in the main view, looking level the way the
    /// camera faces.
    pub(super) fn enter_tour(&mut self, id: Uuid) {
        let Some(p) = self.project.clone() else {
            return;
        };
        let world = p.correction(id);
        let forward = (DVec3::from(self.camera.target) - self.camera.eye()).normalize();
        let d = world.inverse().transform_vector3(forward).normalize();
        let before = self.panorama.tour.take().map_or(self.camera, |t| t.before);
        self.panorama.tour = Some(Tour {
            panorama: id,
            view: View {
                yaw: d.y.atan2(d.x),
                // Level: the camera often looks down on the points.
                pitch: 0.,
                fov: self
                    .camera
                    .fov
                    .clamp(30f64.to_radians(), 100f64.to_radians()),
                flat: false,
            },
            before,
        });
        self.dirty = true;
    }
    /// Back to the point cloud, as the camera was.
    pub(super) fn exit_tour(&mut self) {
        if let Some(tour) = self.panorama.tour.take() {
            self.camera = tour.before;
            self.flight = None;
            self.dirty = true;
            self.selection.camera_moved();
        }
    }
    /// Loads the photo and keeps the point cloud camera where it was taken,
    /// looking the same way, so the points for the overlay are those seen
    /// from there. Leaves when the panorama is gone or no longer placed.
    pub(super) fn tour_update(&mut self, ctx: &egui::Context, p: &Project) {
        let Some((id, view)) = self.panorama.tour.as_ref().map(|t| (t.panorama, t.view)) else {
            return;
        };
        if p.panorama(id).is_none() || p.registration(id).is_none() {
            self.exit_tour();
            return;
        }
        self.want_photo(ctx, p, id);
        let world = p.correction(id);
        let forward = world
            .transform_vector3(direction(view.yaw, view.pitch))
            .normalize();
        let eye = world.transform_point3(DVec3::ZERO);
        let camera = Camera {
            target: (eye + forward).to_array(),
            yaw: (-forward.y).atan2(-forward.x),
            pitch: (-forward.z).asin().clamp(-MAX_PITCH, MAX_PITCH),
            distance: 1.,
            fov: view.fov,
            ortho: false,
            aspect: self.camera.aspect,
        };
        if camera != self.camera {
            self.camera = camera;
            self.dirty = true;
        }
    }
    /// The main view while viewing a panorama: the photo, the points over
    /// it if asked, and markers of the other panoramas in sight.
    pub(super) fn tour_view(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let (Some(p), Some(tour)) = (self.project.clone(), self.panorama.tour.as_ref()) else {
            return;
        };
        let id = tour.panorama;
        let view = tour.view;
        let Some(panorama) = p.panorama(id).cloned() else {
            return;
        };
        let size = ui.available_size().max(egui::vec2(1., 1.));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0., egui::Color32::from_rgb(6, 9, 13));
        let lens = Lens::new(&view, rect);
        let shown = self.draw_sphere(&painter, &panorama, &view, &lens);
        let world = p.correction(id);
        if shown && self.panorama.overlay {
            let shape = self.overlay_shape(ui, rect, view, world, |seen| lens.project(seen));
            painter.add(shape);
        }
        // The other panoramas in sight lead on to them.
        let to_photo = world.inverse();
        let pointer = response.hover_pos();
        let mut next = None;
        let mut nearest = MARKER_REACH;
        let others: Vec<(Uuid, DMat4, egui::Pos2)> = self
            .placed_panoramas(&p)
            .into_iter()
            .filter(|(other, _)| *other != id)
            .filter_map(|(other, at)| {
                let seen = to_photo.transform_point3(at.transform_point3(DVec3::ZERO));
                Some((other, at, lens.project(seen).filter(|s| rect.contains(*s))?))
            })
            .collect();
        for (other, at, screen) in &others {
            if let Some(d) = pointer.map(|p| p.distance(*screen))
                && d <= nearest
            {
                nearest = d;
                next = Some((*other, *at));
            }
        }
        for (other, _, screen) in &others {
            if let Some(pano) = p.panorama(*other) {
                let hovered = next.is_some_and(|(n, _)| n == *other);
                draw_marker(&painter, *screen, p.panorama_name(pano), hovered);
            }
        }
        if next.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        painter.text(
            rect.left_bottom() + egui::vec2(10., -8.),
            egui::Align2::LEFT_BOTTOM,
            t.tour_hint,
            egui::FontId::proportional(12.),
            egui::Color32::from_white_alpha(170),
        );
        // The bar over the top of the view.
        let mut close = false;
        let bar = egui::Rect::from_min_size(
            rect.min + egui::vec2(8., 8.),
            egui::vec2(rect.width() - 16., 28.),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(170))
                .corner_radius(4.)
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(format!("{} {}", icon::PANORAMA, p.panorama_name(&panorama)));
                        ui.separator();
                        ui.checkbox(&mut self.panorama.overlay, t.panorama_overlay)
                            .on_hover_text(t.panorama_overlay_hint);
                        ui.weak(format!("{:.0}°", view.fov.to_degrees()));
                        ui.separator();
                        close = ui.button(format!("{} {}", icon::X, t.tour_close)).clicked();
                    });
                });
        });
        if close {
            self.exit_tour();
            return;
        }
        if let Some(tour) = &mut self.panorama.tour {
            look_input(&mut tour.view, &response);
        }
        if response.clicked()
            && let Some((other, at)) = next
            && let Some(tour) = &mut self.panorama.tour
        {
            tour.view = carried(&tour.view, world, at);
            tour.panorama = other;
        }
    }
    /// For smoke tests: views the first placed panorama.
    pub(super) fn smoke_tour(&mut self) {
        let first = self
            .project
            .as_ref()
            .and_then(|p| self.placed_panoramas(p).first().map(|(id, _)| *id));
        match first {
            Some(id) => self.enter_tour(id),
            None => eprintln!("Smoke tour: no placed panorama"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{View, carried, direction};
    use glam::{DMat4, DQuat, DVec3};

    #[test]
    fn a_look_carries_over_to_another_panorama() {
        let view = View {
            yaw: 0.4,
            pitch: 0.2,
            fov: 1.,
            flat: false,
        };
        let a = DMat4::from_rotation_translation(DQuat::from_rotation_z(1.), DVec3::ONE);
        let b = DMat4::from_rotation_translation(DQuat::from_rotation_z(-0.5), DVec3::X);
        let moved = carried(&view, a, b);
        let world_a = a.transform_vector3(direction(view.yaw, view.pitch));
        let world_b = b.transform_vector3(direction(moved.yaw, moved.pitch));
        assert!((world_a - world_b).length() < 1e-9);
        assert_eq!(moved.fov, view.fov);
    }
}
