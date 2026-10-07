//! The panorama tool: the photo beside the point cloud, where a click on the
//! photo and one on the points make a correspondence. Four or more place the
//! photo, previewed until applied. The photo shows as a view from the
//! centre of its sphere, or whole as an overview.
use super::{Workbench, actions::MAX_PITCH, selection::Tool};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{
    Camera, MIN_PANORAMA_PAIRS, Panorama, PanoramaPair, Pose, Project, solve_panorama,
};
use glam::{DMat4, DVec3};
use std::{
    collections::HashMap,
    f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU},
    path::PathBuf,
    sync::mpsc,
    time::Instant,
};
use uuid::Uuid;

/// The photo side of a correspondence, and the point side.
const PHOTO: egui::Color32 = egui::Color32::from_rgb(255, 150, 40);
const POINT: egui::Color32 = egui::Color32::from_rgb(70, 170, 255);
/// Overlaid points at most, each a small square.
const OVERLAY_POINTS: usize = 120_000;
/// Points at most that find what is in front for the overlay.
const DEPTH_POINTS: usize = 1_500_000;
/// The overlay's depth cells, in points across.
const DEPTH_CELL: f32 = 4.;
/// Levels below the full photo halve it down to this width.
const SMALLEST_LEVEL: u32 = 2048;

/// A click, numbered so the latest can be taken back.
#[derive(Clone, Copy)]
struct Pick<T> {
    value: T,
    order: u64,
}
#[derive(Clone, Copy, Default)]
struct Pair {
    /// Image coordinates in the full photo.
    pixel: Option<Pick<[f64; 2]>>,
    /// A scan and the point's scan coordinates.
    point: Option<Pick<(Uuid, DVec3)>>,
}
impl Pair {
    fn complete(&self) -> Option<PanoramaPair> {
        let (pixel, point) = (self.pixel?, self.point?);
        Some(PanoramaPair {
            pixel: pixel.value,
            scan: point.value.0,
            local: point.value.1.to_array(),
        })
    }
}
/// The correspondences of one panorama and the placement they give.
#[derive(Default)]
struct Work {
    pairs: Vec<Pair>,
    /// The own transform found, shown until applied.
    preview: Option<Pose>,
    /// The pairs changed since the last solution.
    stale: bool,
    error: Option<String>,
}
impl Work {
    /// Starts from the correspondences the panorama was placed with.
    fn saved(p: &Project, id: Uuid) -> Self {
        let pairs = p
            .panorama_pairs(id)
            .iter()
            .map(|pair| Pair {
                pixel: Some(Pick {
                    value: pair.pixel,
                    order: 0,
                }),
                point: Some(Pick {
                    value: (pair.scan, DVec3::from(pair.local)),
                    order: 0,
                }),
            })
            .collect();
        Self {
            pairs,
            ..Default::default()
        }
    }
    fn complete(&self) -> Vec<PanoramaPair> {
        self.pairs.iter().filter_map(Pair::complete).collect()
    }
}

/// Where the photo viewer looks, in the panorama's frame.
#[derive(Clone, Copy, PartialEq)]
struct View {
    /// Azimuth, growing to the left.
    yaw: f64,
    pitch: f64,
    /// Vertical field of view.
    fov: f64,
    /// The whole photo instead of a view from the centre.
    flat: bool,
}
impl View {
    /// Turns so what is shown follows a drag over a view `height` points high.
    fn turn(&mut self, delta: egui::Vec2, height: f32) {
        let per_point = self.fov / height as f64;
        self.yaw = (self.yaw + delta.x as f64 * per_point).rem_euclid(TAU);
        self.pitch =
            (self.pitch + delta.y as f64 * per_point).clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
    }
    /// Narrows or widens the field of view by a wheel scroll.
    fn zoom(&mut self, scroll: f32) {
        self.fov = (self.fov * (-scroll as f64 * 0.002).exp())
            .clamp(1f64.to_radians(), 120f64.to_radians());
    }
}
impl Default for View {
    fn default() -> Self {
        Self {
            yaw: 0.,
            pitch: 0.,
            fov: 90f64.to_radians(),
            flat: false,
        }
    }
}

/// A part of the photo small enough for one texture.
struct Tile {
    /// Left, top, width and height in the level's pixels.
    area: [u32; 4],
    texture: egui::TextureHandle,
}
struct Level {
    width: u32,
    height: u32,
    tiles: Vec<Tile>,
}
/// A panorama's photo on the GPU: the full photo, then halves of it, since
/// textures here have no mipmaps.
struct Photo {
    panorama: Uuid,
    levels: Vec<Level>,
}

/// A photo being decoded and uploaded on another thread.
struct Loading {
    panorama: Uuid,
    result: mpsc::Receiver<Result<Photo, String>>,
    started: Instant,
}

pub(super) struct PanoramaTool {
    /// The panorama being placed; follows the tree selection.
    item: Option<Uuid>,
    works: HashMap<Uuid, Work>,
    photo: Option<Photo>,
    loading: Option<Loading>,
    load_error: Option<(Uuid, String)>,
    view: View,
    /// Find only position and heading.
    level: bool,
    /// The point cloud camera follows the photo viewer.
    linked: bool,
    overlay: bool,
    /// The overlay as last built, and what it was built for.
    overlay_mesh: Option<(OverlayKey, std::sync::Arc<egui::Mesh>)>,
    picks: u64,
}
/// What the overlay depends on.
#[derive(PartialEq)]
struct OverlayKey {
    view: View,
    rect: egui::Rect,
    world: DMat4,
    points: u64,
    shown: usize,
    /// Points finding the front, fewer while the view moves.
    depth_points: usize,
}
impl Default for PanoramaTool {
    fn default() -> Self {
        Self {
            item: None,
            works: HashMap::new(),
            photo: None,
            loading: None,
            load_error: None,
            view: View::default(),
            level: true,
            linked: false,
            overlay: false,
            overlay_mesh: None,
            picks: 0,
        }
    }
}
impl PanoramaTool {
    fn work(&mut self) -> Option<&mut Work> {
        self.works.get_mut(&self.item?)
    }
}

/// The direction of an azimuth and elevation.
fn direction(yaw: f64, pitch: f64) -> DVec3 {
    DVec3::new(
        pitch.cos() * yaw.cos(),
        pitch.cos() * yaw.sin(),
        pitch.sin(),
    )
}

/// A pinhole looking out from the centre of the photo's sphere.
struct Lens {
    forward: DVec3,
    right: DVec3,
    up: DVec3,
    /// Points per unit of tangent.
    focal: f64,
    centre: egui::Pos2,
}
impl Lens {
    fn new(view: &View, rect: egui::Rect) -> Self {
        let forward = direction(view.yaw, view.pitch);
        let right = forward.cross(DVec3::Z).normalize();
        Self {
            forward,
            right,
            up: right.cross(forward),
            focal: rect.height() as f64 / 2. / (view.fov / 2.).tan(),
            centre: rect.center(),
        }
    }
    /// Where a direction shows, if in front.
    fn project(&self, d: DVec3) -> Option<egui::Pos2> {
        let z = d.dot(self.forward);
        if z < 0.02 * d.length() {
            return None;
        }
        let s = self.focal / z;
        Some(self.centre + egui::vec2((d.dot(self.right) * s) as f32, (-d.dot(self.up) * s) as f32))
    }
    /// The direction shown at a screen position.
    fn ray(&self, pos: egui::Pos2) -> DVec3 {
        let d = pos - self.centre;
        (self.forward + self.right * (d.x as f64 / self.focal)
            - self.up * (d.y as f64 / self.focal))
            .normalize()
    }
}

/// Where the whole photo shows in the overview: as large as fits, 2:1.
fn flat_rect(rect: egui::Rect) -> egui::Rect {
    let width = rect.width().min(rect.height() * 2.);
    egui::Rect::from_center_size(rect.center(), egui::vec2(width, width / 2.))
}

/// Decodes a photo and uploads it as textures at most `side` wide: the full
/// photo and halves of it down to [`SMALLEST_LEVEL`].
fn load_photo(
    ctx: &egui::Context,
    path: &PathBuf,
    panorama: Uuid,
    side: u32,
) -> Result<Photo, String> {
    let mut image = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let mut levels = vec![];
    loop {
        let (width, height) = image.dimensions();
        let mut tiles = vec![];
        for top in (0..height).step_by(side as usize) {
            for left in (0..width).step_by(side as usize) {
                let (w, h) = (side.min(width - left), side.min(height - top));
                let part = image::imageops::crop_imm(&image, left, top, w, h).to_image();
                let pixels = egui::ColorImage::from_rgba_unmultiplied(
                    [w as usize, h as usize],
                    part.as_raw(),
                );
                let texture = ctx.load_texture(
                    format!("panorama {panorama} {width} {left} {top}"),
                    pixels,
                    egui::TextureOptions::LINEAR,
                );
                tiles.push(Tile {
                    area: [left, top, w, h],
                    texture,
                });
            }
        }
        levels.push(Level {
            width,
            height,
            tiles,
        });
        if width <= SMALLEST_LEVEL {
            break;
        }
        image = image::imageops::thumbnail(&image, width / 2, height / 2);
    }
    ctx.request_repaint();
    Ok(Photo { panorama, levels })
}

/// The sphere seen through `lens`: each tile as a mesh of cells about a
/// degree across, fine enough for the straight edges of the triangles.
fn sphere_shapes(
    panorama: &Panorama,
    level: &Level,
    lens: &Lens,
    rect: egui::Rect,
) -> Vec<egui::Shape> {
    let scale = [
        panorama.width as f64 / level.width as f64,
        panorama.height as f64 / level.height as f64,
    ];
    // Far off-screen corners only cost precision.
    let reach = rect.expand(rect.width().max(rect.height()) * 4.);
    let mut shapes = vec![];
    for tile in &level.tiles {
        let [left, top, w, h] = tile.area.map(|v| v as f64);
        let nx = (w / level.width as f64 * 360.).ceil().max(1.) as usize;
        let ny = (h / level.height as f64 * 180.).ceil().max(1.) as usize;
        let mut corners = Vec::with_capacity((nx + 1) * (ny + 1));
        for j in 0..=ny {
            for i in 0..=nx {
                let (u, v) = (i as f64 / nx as f64, j as f64 / ny as f64);
                let pixel = [(left + u * w) * scale[0], (top + v * h) * scale[1]];
                let at = lens
                    .project(panorama.bearing(pixel))
                    .filter(|p| reach.contains(*p));
                corners.push((at, egui::pos2(u as f32, v as f32)));
            }
        }
        let mut mesh = egui::Mesh::with_texture(tile.texture.id());
        for j in 0..ny {
            for i in 0..nx {
                let k = j * (nx + 1) + i;
                let quad = [k, k + 1, k + nx + 2, k + nx + 1].map(|k| corners[k]);
                let Some(points) = quad.iter().map(|(at, _)| *at).collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let cell = egui::Rect::from_points(&points);
                if !cell.intersects(rect) {
                    continue;
                }
                let base = mesh.vertices.len() as u32;
                for (pos, (_, uv)) in points.into_iter().zip(quad) {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos,
                        uv,
                        color: egui::Color32::WHITE,
                    });
                }
                mesh.indices
                    .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        if !mesh.is_empty() {
            shapes.push(egui::Shape::mesh(mesh));
        }
    }
    shapes
}

/// A colour for a distance in metres: near red through yellow and green to
/// far blue, on a log scale from 0.5 m to 50 m.
fn distance_color(d: f64) -> egui::Color32 {
    let t = ((d.max(0.5) / 0.5).ln() / 100f64.ln()).clamp(0., 1.) as f32;
    let hue = t * 0.66;
    egui::ecolor::Hsva::new(hue, 0.85, 1., 1.).into()
}

/// Draws a numbered marker.
fn marker(painter: &egui::Painter, at: egui::Pos2, color: egui::Color32, number: usize) {
    painter.circle(at, 6., color, egui::Stroke::new(1.5, egui::Color32::BLACK));
    let text = number.to_string();
    let font = egui::FontId::proportional(14.);
    let offset = egui::vec2(8., -8.);
    for shadow in [egui::vec2(1., 1.), egui::vec2(-1., -1.)] {
        painter.text(
            at + offset + shadow,
            egui::Align2::LEFT_BOTTOM,
            &text,
            font.clone(),
            egui::Color32::BLACK,
        );
    }
    painter.text(at + offset, egui::Align2::LEFT_BOTTOM, text, font, color);
}

/// How far a correspondence is off with the panorama at `world`: the angle
/// in degrees and the distance in pixels.
fn residual(
    p: &Project,
    panorama: &Panorama,
    world: DMat4,
    pair: &PanoramaPair,
) -> Option<(f64, f64)> {
    let scan = p.scan(pair.scan)?;
    let point = p
        .world_matrix(scan)
        .transform_point3(DVec3::from(pair.local));
    let seen = world.inverse().transform_point3(point);
    let bearing = panorama.bearing(pair.pixel);
    let angle = bearing.cross(seen).length().atan2(bearing.dot(seen));
    Some((
        angle.to_degrees(),
        panorama.pixel_distance(panorama.pixel(seen), pair.pixel),
    ))
}

impl Workbench {
    pub(super) fn placing_panorama(&self) -> bool {
        self.selection.tool == Tool::Panorama && self.project.is_some()
    }
    /// The panorama the tool works on, if it is in the project.
    fn panorama_item<'a>(&self, p: &'a Project) -> Option<&'a Panorama> {
        p.panorama(self.panorama.item?)
    }
    /// Where the panorama is in the project frame: as previewed or applied.
    fn panorama_world(&self, p: &Project, id: Uuid) -> DMat4 {
        let preview = self.panorama.works.get(&id).and_then(|w| w.preview);
        p.panorama_matrix(id, preview)
    }
    /// Whether the photo of the panorama being placed is still loading.
    pub(super) fn panorama_loading(&self) -> bool {
        self.placing_panorama() && self.panorama.loading.is_some()
    }
    /// Follows the tree selection, loads the photo, places the panorama when
    /// its pairs changed and moves a linked camera. Call once a frame.
    pub(super) fn panorama_update(&mut self, ctx: &egui::Context) {
        let Some(p) = self.project.clone() else {
            self.panorama.item = None;
            return;
        };
        if !self.placing_panorama() {
            return;
        }
        if let Some(selected) = self
            .single_tree_item()
            .filter(|id| p.panorama(*id).is_some())
        {
            self.panorama.item = Some(selected);
        }
        let tool = &mut self.panorama;
        tool.item = tool.item.filter(|id| p.panorama(*id).is_some());
        tool.works.retain(|id, _| p.panorama(*id).is_some());
        let Some(id) = tool.item else { return };
        tool.works.entry(id).or_insert_with(|| Work::saved(&p, id));
        if let Some(Loading {
            panorama: loading,
            result,
            started,
        }) = &tool.loading
            && let Ok(result) = result.try_recv()
        {
            let loading = *loading;
            if self.smoke.active() {
                eprintln!(
                    "Smoke panorama photo loaded in {:.0} ms",
                    started.elapsed().as_secs_f64() * 1000.
                );
            }
            tool.loading = None;
            match result {
                Ok(photo) => tool.photo = Some(photo),
                Err(e) => tool.load_error = Some((loading, e)),
            }
        }
        let shown = tool.photo.as_ref().map(|photo| photo.panorama);
        let failed = tool.load_error.as_ref().map(|(id, _)| *id);
        if shown != Some(id) && tool.loading.is_none() && failed != Some(id) {
            let panorama = p.panorama(id).unwrap();
            match p.path(&panorama.file) {
                Ok(path) => {
                    let (tx, rx) = mpsc::channel();
                    let side = ctx.input(|i| i.max_texture_side).min(4096) as u32;
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(load_photo(&ctx, &path, id, side));
                    });
                    tool.loading = Some(Loading {
                        panorama: id,
                        result: rx,
                        started: Instant::now(),
                    });
                    tool.view = View::default();
                }
                Err(e) => tool.load_error = Some((id, e.to_string())),
            }
        }
        if tool.works.get(&id).is_some_and(|w| w.stale) {
            self.panorama_solve(&p, id);
        }
        if self.panorama.linked {
            self.link_camera(&p, id);
        }
    }
    /// Places the panorama from its complete pairs, or drops the preview
    /// with too few.
    fn panorama_solve(&mut self, p: &Project, id: Uuid) {
        let level = self.panorama.level;
        let Some(panorama) = p.panorama(id) else {
            return;
        };
        let Some(work) = self.panorama.works.get_mut(&id) else {
            return;
        };
        work.stale = false;
        work.error = None;
        let pairs = work.complete();
        if pairs.len() < MIN_PANORAMA_PAIRS {
            work.preview = None;
            return;
        }
        let (bearings, points) = p.panorama_rays(panorama, &pairs);
        match solve_panorama(&bearings, &points, level) {
            Ok(solution) => work.preview = Some(p.panorama_own_pose(id, solution.world)),
            Err(e) => {
                work.preview = None;
                work.error = Some(super::jobs::Notice::new(self.t, &e).message);
            }
        }
    }
    /// Puts the point cloud camera where the photo was taken, looking where
    /// the photo viewer looks with its field of view.
    fn link_camera(&mut self, p: &Project, id: Uuid) {
        let world = self.panorama_world(p, id);
        let view = self.panorama.view;
        let forward = world
            .transform_vector3(direction(view.yaw, view.pitch))
            .normalize();
        let eye = world.transform_point3(DVec3::ZERO);
        let distance = 1.;
        let camera = Camera {
            target: (eye + forward * distance).to_array(),
            yaw: (-forward.y).atan2(-forward.x),
            pitch: (-forward.z).asin().clamp(-MAX_PITCH, MAX_PITCH),
            distance,
            fov: view.fov,
            ortho: false,
            aspect: self.camera.aspect,
        };
        if camera != self.camera {
            self.camera = camera;
            self.flight = None;
            self.dirty = true;
            self.selection.camera_moved();
        }
    }
    /// The shown points as seen from the photo, coloured by distance: only
    /// those near the front where they fall, so points behind walls stay
    /// hidden. A coarse depth buffer from many points finds the front; a
    /// point counts as in front when within a few percent of the nearest
    /// depth around its cell.
    fn overlay_mesh(
        &self,
        rect: egui::Rect,
        (count, depth_points): (usize, usize),
        project: impl Fn(DVec3) -> Option<egui::Pos2>,
        to_photo: DMat4,
    ) -> egui::Mesh {
        let columns = (rect.width() / DEPTH_CELL).ceil().max(1.) as usize;
        let rows = (rect.height() / DEPTH_CELL).ceil().max(1.) as usize;
        let cell = |at: egui::Pos2| {
            let c = ((at.x - rect.left()) / DEPTH_CELL) as usize;
            let r = ((at.y - rect.top()) / DEPTH_CELL) as usize;
            (c.min(columns - 1), r.min(rows - 1))
        };
        let seen = |point: DVec3| {
            let seen = to_photo.transform_point3(point);
            project(seen)
                .filter(|at| rect.contains(*at))
                .map(|at| (at, seen.length()))
        };
        let mut nearest = vec![f64::INFINITY; columns * rows];
        let step = count.div_ceil(depth_points).max(1);
        for (.., point) in self.shown_points(false).step_by(step) {
            if let Some((at, depth)) = seen(point) {
                let (c, r) = cell(at);
                let slot = &mut nearest[r * columns + c];
                *slot = slot.min(depth);
            }
        }
        // The nearest around each cell, so gaps between the points of a
        // near surface still hide what is behind.
        let mut front = vec![f64::INFINITY; columns * rows];
        for r in 0..rows {
            for c in 0..columns {
                let mut d = f64::INFINITY;
                for rr in r.saturating_sub(1)..(r + 2).min(rows) {
                    for cc in c.saturating_sub(1)..(c + 2).min(columns) {
                        d = d.min(nearest[rr * columns + cc]);
                    }
                }
                front[r * columns + c] = d;
            }
        }
        let mut mesh = egui::Mesh::default();
        let size = egui::vec2(2., 2.);
        let step = count.div_ceil(OVERLAY_POINTS).max(1);
        for (.., point) in self.shown_points(false).step_by(step) {
            let Some((at, depth)) = seen(point) else {
                continue;
            };
            let (c, r) = cell(at);
            if depth <= front[r * columns + c] * 1.05 + 0.05 {
                let color = distance_color(depth).gamma_multiply(0.8);
                mesh.add_colored_rect(egui::Rect::from_center_size(at, size), color);
            }
        }
        mesh
    }
    /// While the views are linked, a drag or the wheel on the point cloud
    /// turns or zooms both, the camera staying where the photo was taken.
    /// Returns whether it took the input.
    pub(super) fn panorama_look(&mut self, ctx: &egui::Context, response: &egui::Response) -> bool {
        if !self.placing_panorama() || !self.panorama.linked {
            return false;
        }
        let (Some(p), Some(id)) = (self.project.clone(), self.panorama.item) else {
            return false;
        };
        let view = &mut self.panorama.view;
        if response.dragged_by(egui::PointerButton::Primary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            view.turn(response.drag_delta(), response.rect.height());
        }
        if response.hovered() {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0. {
                view.zoom(scroll);
            }
        }
        self.link_camera(&p, id);
        true
    }
    /// Stops the camera following the photo and gives it its usual field of view.
    fn unlink_camera(&mut self) {
        self.panorama.linked = false;
        self.camera.fov = FRAC_PI_4;
        self.dirty = true;
    }
    /// Called when another tool is chosen.
    pub(super) fn leave_panorama(&mut self) {
        if self.panorama.linked {
            self.unlink_camera();
        }
    }
    /// Files a click as the photo or point side of the first pair lacking it.
    fn panorama_pick(&mut self, pixel: Option<[f64; 2]>, point: Option<(Uuid, DVec3)>) {
        self.panorama.picks += 1;
        let order = self.panorama.picks;
        let Some(work) = self.panorama.work() else {
            return;
        };
        let slot = work.pairs.iter().position(|pair| {
            (pixel.is_some() && pair.pixel.is_none()) || (point.is_some() && pair.point.is_none())
        });
        let slot = match slot {
            Some(i) => &mut work.pairs[i],
            None => {
                work.pairs.push(Pair::default());
                work.pairs.last_mut().unwrap()
            }
        };
        if let Some(value) = pixel {
            slot.pixel = Some(Pick { value, order });
        }
        if let Some(value) = point {
            slot.point = Some(Pick { value, order });
        }
        work.stale = true;
    }
    /// Takes back the latest click (Backspace).
    pub(super) fn panorama_undo_pick(&mut self) {
        let Some(work) = self.panorama.work() else {
            return;
        };
        let latest = work
            .pairs
            .iter()
            .enumerate()
            .flat_map(|(i, pair)| {
                [
                    pair.pixel.map(|p| (p.order, i, true)),
                    pair.point.map(|p| (p.order, i, false)),
                ]
            })
            .flatten()
            .max_by_key(|(order, ..)| *order);
        if let Some((_, i, photo)) = latest {
            if photo {
                work.pairs[i].pixel = None;
            } else {
                work.pairs[i].point = None;
            }
            work.pairs
                .retain(|pair| pair.pixel.is_some() || pair.point.is_some());
            work.stale = true;
        }
    }
    pub(super) fn panorama_clear(&mut self) {
        if let Some(work) = self.panorama.work() {
            work.pairs.clear();
            work.stale = true;
        }
    }
    /// A click on the point cloud: the point side of a pair.
    pub(super) fn panorama_input(&mut self, response: &egui::Response) {
        if !self.placing_panorama() || self.panorama.item.is_none() || !response.clicked() {
            return;
        }
        if let Some(picked) = self.pick_at(response) {
            self.panorama_pick(None, Some((picked.scan, picked.local)));
        }
    }
    /// Applies the previewed placement with its correspondences.
    pub(super) fn panorama_apply(&mut self) {
        let (Some(p), Some(id)) = (self.project.clone(), self.panorama.item) else {
            return;
        };
        let Some(panorama) = p.panorama(id) else {
            return;
        };
        let Some(work) = self.panorama.works.get(&id) else {
            return;
        };
        let Some(own) = work.preview else { return };
        let pairs = work.complete();
        let world = p.panorama_matrix(id, Some(own));
        let residuals: Vec<f64> = pairs
            .iter()
            .filter_map(|pair| residual(&p, panorama, world, pair).map(|(a, _)| a))
            .collect();
        let rms =
            (residuals.iter().map(|r| r * r).sum::<f64>() / residuals.len().max(1) as f64).sqrt();
        if self
            .apply_edit(|p| p.place_panorama(id, own, pairs, rms))
            .is_some()
            && let Some(work) = self.panorama.works.get_mut(&id)
        {
            work.preview = None;
        }
    }
    /// The options panel and the photo viewer, right of the point cloud.
    pub(super) fn panorama_panels(&mut self, ui: &mut egui::Ui) {
        if !self.placing_panorama() {
            return;
        }
        let Some(p) = self.project.clone() else {
            return;
        };
        egui::Panel::right("panorama")
            .resizable(true)
            .default_size(300.)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.panorama_options(ui, &p));
            });
        let half = (ui.available_width() * 0.5).max(200.);
        egui::Panel::right("panorama view")
            .resizable(true)
            .default_size(half)
            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(6, 9, 13)))
            .show(ui, |ui| self.panorama_view(ui, &p));
    }
    fn panorama_options(&mut self, ui: &mut egui::Ui, p: &Project) {
        let t = self.t;
        ui.strong(format!("{} {}", icon::PANORAMA, t.panorama_title));
        ui.add_space(4.);
        let Some(panorama) = self.panorama_item(p).cloned() else {
            ui.label(t.panorama_choose);
            return;
        };
        let id = panorama.id;
        ui.strong(p.panorama_name(&panorama));
        match p.registration(id) {
            Some(state) if state.moved => ui.weak(t.panorama_moved),
            Some(state) => ui.weak((t.registration_panorama)(
                state.registration.fit.rms,
                state.registration.fit.references,
            )),
            None => ui.weak(t.panorama_unplaced),
        };
        ui.separator();
        ui.strong(t.panorama_pairs);
        ui.small(t.panorama_pairs_hint);
        let world = self.panorama_world(p, id);
        let Some(work) = self.panorama.works.get(&id) else {
            return;
        };
        let has_pose = work.preview.is_some() || p.registration(id).is_some();
        let mut remove = None;
        let mut residuals = vec![];
        egui::Grid::new("panorama pairs")
            .num_columns(6)
            .striped(true)
            .show(ui, |ui| {
                ui.label("#");
                ui.colored_label(PHOTO, t.panorama_photo);
                ui.colored_label(POINT, t.panorama_point);
                ui.label(t.panorama_angle);
                ui.label(t.panorama_pixels);
                ui.label("");
                ui.end_row();
                for (i, pair) in work.pairs.iter().enumerate() {
                    let mark = |set: bool| if set { icon::CHECK } else { "—" };
                    ui.label((i + 1).to_string());
                    ui.label(mark(pair.pixel.is_some()));
                    ui.label(mark(pair.point.is_some()));
                    let off = pair
                        .complete()
                        .filter(|_| has_pose)
                        .and_then(|pair| residual(p, &panorama, world, &pair));
                    if let Some((angle, _)) = off {
                        residuals.push(angle);
                    }
                    ui.label(off.map_or(String::new(), |(a, _)| format!("{a:.3}°")));
                    ui.label(off.map_or(String::new(), |(_, d)| format!("{d:.1}")));
                    if ui
                        .small_button(icon::TRASH)
                        .on_hover_text(t.remove)
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        let complete = work.complete().len();
        let preview = work.preview.is_some();
        let error = work.error.clone();
        if let Some(i) = remove
            && let Some(work) = self.panorama.works.get_mut(&id)
        {
            work.pairs.remove(i);
            work.stale = true;
        }
        if complete < MIN_PANORAMA_PAIRS {
            ui.weak((t.panorama_need_pairs)(MIN_PANORAMA_PAIRS - complete));
        }
        if let Some(error) = error {
            ui.colored_label(egui::Color32::from_rgb(255, 120, 100), error);
        }
        if ui
            .checkbox(&mut self.panorama.level, t.panorama_level)
            .on_hover_text(t.panorama_level_hint)
            .changed()
            && let Some(work) = self.panorama.works.get_mut(&id)
        {
            work.stale = true;
        }
        if ui
            .add_enabled(
                self.panorama
                    .works
                    .get(&id)
                    .is_some_and(|w| !w.pairs.is_empty()),
                egui::Button::new(t.panorama_clear),
            )
            .clicked()
        {
            self.panorama_clear();
        }
        ui.separator();
        if !residuals.is_empty() {
            let rms =
                (residuals.iter().map(|r| r * r).sum::<f64>() / residuals.len() as f64).sqrt();
            let max = residuals.iter().copied().fold(0., f64::max);
            ui.label((t.panorama_result)(rms, max));
        }
        if preview {
            ui.small(t.panorama_previewing);
        } else if p.registration(id).is_some() {
            ui.small(t.panorama_applied);
        }
        let idle = self.job.is_none();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    idle && preview,
                    egui::Button::new(format!("{} {}", icon::CHECK, t.apply)),
                )
                .clicked()
            {
                self.panorama_apply();
            }
            if ui
                .add_enabled(
                    preview,
                    egui::Button::new(format!("{} {}", icon::X, t.align_discard)),
                )
                .clicked()
                && let Some(work) = self.panorama.works.get_mut(&id)
            {
                work.preview = None;
            }
        });
    }
    /// The photo viewer with its options bar.
    fn panorama_view(&mut self, ui: &mut egui::Ui, p: &Project) {
        let t = self.t;
        let Some(panorama) = self.panorama_item(p).cloned() else {
            ui.centered_and_justified(|ui| ui.weak(t.panorama_choose));
            return;
        };
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(format!("{} {}", icon::PANORAMA, p.panorama_name(&panorama)));
                    ui.separator();
                    ui.toggle_value(&mut self.panorama.view.flat, t.panorama_flat)
                        .on_hover_text(t.panorama_flat_hint);
                    let mut linked = self.panorama.linked;
                    if ui
                        .checkbox(&mut linked, t.panorama_link)
                        .on_hover_text(t.panorama_link_hint)
                        .changed()
                    {
                        if linked {
                            self.panorama.linked = true;
                        } else {
                            self.unlink_camera();
                        }
                    }
                    ui.checkbox(&mut self.panorama.overlay, t.panorama_overlay)
                        .on_hover_text(t.panorama_overlay_hint);
                    if !self.panorama.view.flat {
                        ui.weak(format!("{:.0}°", self.panorama.view.fov.to_degrees()));
                    }
                });
            });
        let size = ui.available_size().max(egui::vec2(1., 1.));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let photo = self
            .panorama
            .photo
            .as_ref()
            .filter(|photo| photo.panorama == panorama.id);
        let Some(photo) = photo else {
            let text = match &self.panorama.load_error {
                Some((id, e)) if *id == panorama.id => (t.panorama_load_failed)(e),
                _ => t.panorama_loading.into(),
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(15.),
                ui.visuals().weak_text_color(),
            );
            return;
        };
        let view = self.panorama.view;
        let pixels = ui.ctx().pixels_per_point();
        // Image coordinates at a screen position, and back.
        let lens = Lens::new(&view, rect);
        let full = flat_rect(rect);
        let to_pixel = |pos: egui::Pos2| -> Option<[f64; 2]> {
            if view.flat {
                full.contains(pos).then(|| {
                    [
                        ((pos.x - full.left()) / full.width()) as f64 * panorama.width as f64,
                        ((pos.y - full.top()) / full.height()) as f64 * panorama.height as f64,
                    ]
                })
            } else {
                Some(panorama.pixel(lens.ray(pos)))
            }
        };
        let to_screen = |pixel: [f64; 2]| -> Option<egui::Pos2> {
            if view.flat {
                Some(egui::pos2(
                    full.left() + (pixel[0] / panorama.width as f64) as f32 * full.width(),
                    full.top() + (pixel[1] / panorama.height as f64) as f32 * full.height(),
                ))
            } else {
                lens.project(panorama.bearing(pixel))
            }
        };
        if view.flat {
            // The smallest level at least as wide as shown.
            let shown = (full.width() * pixels) as u32;
            let level = photo
                .levels
                .iter()
                .rev()
                .find(|l| l.width >= shown)
                .unwrap_or(&photo.levels[0]);
            for tile in &level.tiles {
                let [left, top, w, h] = tile.area.map(|v| v as f32);
                let (lw, lh) = (level.width as f32, level.height as f32);
                let area = egui::Rect::from_min_size(
                    full.min + egui::vec2(left / lw * full.width(), top / lh * full.height()),
                    egui::vec2(w / lw * full.width(), h / lh * full.height()),
                );
                painter.image(
                    tile.texture.id(),
                    area,
                    egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.)),
                    egui::Color32::WHITE,
                );
            }
        } else {
            // The smallest level with pixels no larger than the screen's.
            let screen = view.fov / (rect.height() * pixels) as f64;
            let level = photo
                .levels
                .iter()
                .rev()
                .find(|l| TAU / l.width as f64 <= screen)
                .unwrap_or(&photo.levels[0]);
            painter.extend(sphere_shapes(&panorama, level, &lens, rect));
        }
        let world = self.panorama_world(p, panorama.id);
        let has_pose = self
            .panorama
            .works
            .get(&panorama.id)
            .is_some_and(|w| w.preview.is_some())
            || p.registration(panorama.id).is_some();
        let to_photo = world.inverse();
        if self.panorama.overlay && has_pose {
            let key = OverlayKey {
                view,
                rect,
                world,
                points: self.points_generation,
                shown: self.shown_count(),
                // A quarter while a button is held, then the full count.
                depth_points: if ui.input(|i| i.pointer.any_down()) {
                    DEPTH_POINTS / 4
                } else {
                    DEPTH_POINTS
                },
            };
            let cached = self.panorama.overlay_mesh.take().filter(|(k, _)| *k == key);
            let mesh = cached.map_or_else(
                || {
                    let started = Instant::now();
                    let mesh = std::sync::Arc::new(self.overlay_mesh(
                        rect,
                        (key.shown, key.depth_points),
                        |seen| {
                            if view.flat {
                                to_screen(panorama.pixel(seen))
                            } else {
                                lens.project(seen)
                            }
                        },
                        to_photo,
                    ));
                    if self.smoke.active() {
                        eprintln!(
                            "Smoke panorama overlay: {} points in {:.1} ms",
                            mesh.vertices.len() / 4,
                            started.elapsed().as_secs_f64() * 1000.
                        );
                    }
                    mesh
                },
                |(_, mesh)| mesh,
            );
            painter.add(egui::Shape::Mesh(mesh.clone()));
            self.panorama.overlay_mesh = Some((key, mesh));
        }
        if let Some(work) = self.panorama.works.get(&panorama.id) {
            for (i, pair) in work.pairs.iter().enumerate() {
                let photo_at = pair.pixel.and_then(|pick| to_screen(pick.value));
                let point_at = pair.point.filter(|_| has_pose).and_then(|pick| {
                    let scan = p.scan(pick.value.0)?;
                    let point = p.world_matrix(scan).transform_point3(pick.value.1);
                    let seen = to_photo.transform_point3(point);
                    if view.flat {
                        to_screen(panorama.pixel(seen))
                    } else {
                        lens.project(seen)
                    }
                });
                if let (Some(a), Some(b)) = (photo_at, point_at) {
                    painter.line_segment([a, b], egui::Stroke::new(1.5, egui::Color32::WHITE));
                }
                if let Some(at) = point_at {
                    painter.circle(at, 4., POINT, egui::Stroke::new(1., egui::Color32::BLACK));
                }
                if let Some(at) = photo_at {
                    marker(&painter, at, PHOTO, i + 1);
                }
            }
        }
        painter.text(
            rect.left_bottom() + egui::vec2(8., -6.),
            egui::Align2::LEFT_BOTTOM,
            t.panorama_view_hint,
            egui::FontId::proportional(12.),
            egui::Color32::from_white_alpha(150),
        );
        // Input, for the next frame.
        let ctx = ui.ctx().clone();
        let view = &mut self.panorama.view;
        if !view.flat {
            if response.dragged_by(egui::PointerButton::Primary)
                || response.dragged_by(egui::PointerButton::Middle)
            {
                // The photo follows the pointer.
                view.turn(response.drag_delta(), rect.height());
            }
            if response.hovered() {
                let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0. {
                    // Zoom about the pointer: the direction under it stays.
                    let under = response.hover_pos().map(|pos| lens.ray(pos));
                    view.zoom(scroll);
                    if let (Some(under), Some(pos)) = (under, response.hover_pos()) {
                        let after = Lens::new(view, rect).ray(pos);
                        let (yaw_a, pitch_a) = (under.y.atan2(under.x), under.z.asin());
                        let (yaw_b, pitch_b) = (after.y.atan2(after.x), after.z.asin());
                        view.yaw = (view.yaw + yaw_a - yaw_b).rem_euclid(TAU);
                        view.pitch = (view.pitch + pitch_a - pitch_b)
                            .clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
                    }
                }
            }
        } else if response.double_clicked()
            && let Some(pixel) = response.interact_pointer_pos().and_then(to_pixel)
        {
            // Look there, from the centre.
            let d = panorama.bearing(pixel);
            view.yaw = d.y.atan2(d.x).rem_euclid(TAU);
            view.pitch = d.z.asin().clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
            view.fov = view.fov.min(60f64.to_radians());
            view.flat = false;
            return;
        }
        if response.clicked()
            && let Some(pixel) = response.interact_pointer_pos().and_then(to_pixel)
        {
            let pixel = [
                pixel[0].rem_euclid(panorama.width as f64),
                pixel[1].clamp(0., panorama.height as f64),
            ];
            self.panorama_pick(Some(pixel), None);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }
    /// In the point cloud: the points of the pairs and where the photo is.
    pub(super) fn draw_panorama(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some(p) = self.project.as_ref().filter(|_| self.placing_panorama()) else {
            return;
        };
        let Some(panorama) = self.panorama_item(p) else {
            return;
        };
        let projector = self.camera.projector();
        let screen = |world: DVec3| {
            projector.project(world).map(|(uv, _)| {
                rect.left_top()
                    + egui::vec2(uv[0] as f32 * rect.width(), uv[1] as f32 * rect.height())
            })
        };
        let painter = ui.painter_at(rect);
        if let Some(work) = self.panorama.works.get(&panorama.id) {
            for (i, pair) in work.pairs.iter().enumerate() {
                let Some(pick) = pair.point else { continue };
                let Some(scan) = p.scan(pick.value.0) else {
                    continue;
                };
                let world = p.world_matrix(scan).transform_point3(pick.value.1);
                if let Some(at) = screen(world) {
                    marker(&painter, at, POINT, i + 1);
                }
            }
        }
        // Where the photo was taken, unless the camera stands there.
        if !self.panorama.linked {
            let world = self.panorama_world(p, panorama.id);
            let eye = world.transform_point3(DVec3::ZERO);
            let view = self.panorama.view;
            let ahead = world.transform_point3(direction(view.yaw, view.pitch));
            if let (Some(a), Some(b)) = (screen(eye), screen(ahead)) {
                painter.line_segment([a, b], egui::Stroke::new(2., PHOTO));
            }
            if let Some(at) = screen(eye) {
                painter.circle(
                    at,
                    7.,
                    egui::Color32::TRANSPARENT,
                    egui::Stroke::new(2.5, PHOTO),
                );
                painter.text(
                    at + egui::vec2(10., 0.),
                    egui::Align2::LEFT_CENTER,
                    p.panorama_name(panorama),
                    egui::FontId::proportional(13.),
                    PHOTO,
                );
            }
        }
    }
    /// For smoke tests: picks correspondences of the first panorama as seen
    /// from `truth` (x, y, z and heading in degrees): displayed points spread
    /// around it, their pixels computed. Then places it and reports how far
    /// the placement is from the truth.
    pub(super) fn smoke_panorama_pairs(&mut self, ctx: &egui::Context, truth: [f64; 4]) {
        let Some(p) = self.project.clone() else {
            return;
        };
        let Some(id) = p.panoramas().next().map(|pano| pano.id) else {
            eprintln!("Smoke panorama: no panorama");
            return;
        };
        self.set_tool(Tool::Panorama);
        self.select_tree_item(Some(id));
        self.panorama_update(ctx);
        self.panorama_clear();
        let panorama = p.panorama(id).unwrap().clone();
        let [x, y, z, heading] = truth;
        let pose = DMat4::from_rotation_translation(
            glam::DQuat::from_rotation_z(heading.to_radians()),
            DVec3::new(x, y, z),
        );
        let to_photo = pose.inverse();
        // One point per eighth of the horizon, 3 to 30 m away.
        let mut chosen: [Option<(f64, Uuid, DVec3, DVec3)>; 8] = [None; 8];
        for (scan, local, _, world) in self.shown_points(false) {
            let seen = to_photo.transform_point3(world);
            let d = seen.length();
            if !(3. ..30.).contains(&d) {
                continue;
            }
            let sector = ((seen.y.atan2(seen.x) + PI) / TAU * 8.) as usize % 8;
            // Prefer points a little above the horizon, which differ more.
            let score = (seen.z / d - 0.1).abs();
            if chosen[sector].is_none_or(|(s, ..)| score < s) {
                chosen[sector] = Some((score, scan, local, seen));
            }
        }
        for (_, scan, local, seen) in chosen.into_iter().flatten() {
            self.panorama_pick(Some(panorama.pixel(seen)), Some((scan, local)));
        }
        self.panorama_solve(&p, id);
        let work = &self.panorama.works[&id];
        let placed = p.panorama_matrix(id, work.preview);
        let (_, rotation, translation) = placed.to_scale_rotation_translation();
        let (yaw, _, _) = rotation.to_euler(glam::EulerRot::ZYX);
        eprintln!(
            "Smoke panorama: {} pairs, preview {}, position error {:.4} m, heading {:.3} deg (truth {heading})",
            work.complete().len(),
            work.preview.is_some(),
            translation.distance(DVec3::new(x, y, z)),
            yaw.to_degrees().rem_euclid(360.)
        );
        // Look at the first pair in both views.
        if let Some(pixel) = work.pairs.first().and_then(|pair| pair.pixel) {
            let d = panorama.bearing(pixel.value);
            self.panorama.view.yaw = d.y.atan2(d.x);
            self.panorama.view.pitch = d.z.asin();
            self.panorama.view.fov = 70f64.to_radians();
        }
    }
    /// For smoke tests: the overview, or a view in degrees (yaw, pitch, fov).
    pub(super) fn smoke_panorama_view(&mut self, flat: bool, degrees: &[f64]) {
        let view = &mut self.panorama.view;
        view.flat = flat;
        if let [yaw, pitch, fov] = degrees[..] {
            view.yaw = yaw.to_radians();
            view.pitch = pitch.to_radians();
            view.fov = fov.to_radians();
        }
    }
    /// For smoke tests: the linked camera and the overlay.
    pub(super) fn smoke_panorama_link(&mut self) {
        self.panorama.linked = true;
        self.panorama.overlay = true;
        if let (Some(p), Some(id)) = (self.project.clone(), self.panorama.item) {
            self.link_camera(&p, id);
        }
    }
    /// For smoke tests: what the tool holds.
    pub(super) fn panorama_summary(&self) -> String {
        let Some(p) = &self.project else {
            return String::new();
        };
        let Some(id) = self.panorama.item else {
            return "no panorama".into();
        };
        let work = self.panorama.works.get(&id);
        format!(
            "pairs {}, preview {}, saved pairs {}, registered {}, at {:.3?}",
            work.map_or(0, |w| w.complete().len()),
            work.is_some_and(|w| w.preview.is_some()),
            p.panorama_pairs(id).len(),
            p.registration(id).is_some(),
            p.correction(id).transform_point3(DVec3::ZERO).to_array()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Lens, View, direction};
    use eframe::egui;

    #[test]
    fn the_lens_shows_each_direction_where_its_ray_points() {
        let view = View {
            yaw: 2.5,
            pitch: -0.4,
            fov: 70f64.to_radians(),
            flat: false,
        };
        let rect = egui::Rect::from_min_size(egui::pos2(100., 50.), egui::vec2(640., 480.));
        let lens = Lens::new(&view, rect);
        assert!((lens.ray(rect.center()) - direction(view.yaw, view.pitch)).length() < 1e-9);
        for pos in [
            egui::pos2(110., 60.),
            egui::pos2(700., 500.),
            egui::pos2(400., 300.),
        ] {
            let back = lens.project(lens.ray(pos)).unwrap();
            assert!((back - pos).length() < 1e-3, "{pos:?} -> {back:?}");
        }
        // The top of the view is the upper edge of the field of view.
        let top = lens.ray(egui::pos2(rect.center().x, rect.top()));
        let above = top.dot(direction(view.yaw, view.pitch)).acos();
        assert!((above - view.fov / 2.).abs() < 1e-6);
        assert!(lens.project(-direction(view.yaw, view.pitch)).is_none());
    }
}
