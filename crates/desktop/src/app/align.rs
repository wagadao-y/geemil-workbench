//! The registration tool: pick point pairs between the scan or folder being
//! moved and the visible reference scans, fit them, refine by ICP, and see each
//! proposal live before applying it as the item's transform.
use super::{Workbench, dialogs::Dialog, selection::Tool};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{IcpOptions, IcpResult, Pose, Project, rigid_fit};
use glam::DVec3;
use std::sync::mpsc;
use uuid::Uuid;

/// sRGB tints that tell the moved points from the reference.
const MOVING: egui::Color32 = egui::Color32::from_rgb(255, 150, 40);
const REFERENCE: egui::Color32 = egui::Color32::from_rgb(70, 170, 255);

/// A picked point: its scan and its scan coordinates, which stay valid however
/// the scan is moved.
#[derive(Clone, Copy)]
struct Pick {
    scan: Uuid,
    local: DVec3,
    /// Increases with every pick, so the latest can be taken back.
    order: u64,
}
#[derive(Default)]
struct Pair {
    moving: Option<Pick>,
    reference: Option<Pick>,
}
enum Report {
    Pairs { rms: f64 },
    Icp(IcpResult),
}

pub(super) struct Align {
    /// The scan or folder being moved; follows the tree selection.
    item: Option<Uuid>,
    pairs: Vec<Pair>,
    /// The proposed own transform of `item`, shown until applied or dropped.
    preview: Option<Pose>,
    report: Option<Report>,
    icp: Option<mpsc::Receiver<IcpResult>>,
    /// Colour the moved and the reference points apart.
    tint: bool,
    /// Which side to show, so a point hidden behind the other side can be picked.
    show: Show,
    picks: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Show {
    Both,
    Moving,
    Reference,
}
impl Default for Align {
    fn default() -> Self {
        Self {
            item: None,
            pairs: vec![],
            preview: None,
            report: None,
            icp: None,
            tint: true,
            show: Show::Both,
            picks: 0,
        }
    }
}
impl Align {
    pub(super) fn clear(&mut self) {
        self.pairs.clear();
        self.preview = None;
        self.report = None;
    }
}

fn tint(color: egui::Color32, amount: f32) -> [f32; 4] {
    [
        color.r() as f32 / 255.,
        color.g() as f32 / 255.,
        color.b() as f32 / 255.,
        amount,
    ]
}

impl Workbench {
    fn aligning(&self) -> bool {
        self.selection.tool == Tool::Align && self.project.is_some()
    }
    /// The scans the item moves, and the visible scans it is aligned to.
    fn align_scans(&self, p: &Project) -> (Vec<Uuid>, Vec<Uuid>) {
        let moving = self
            .align
            .item
            .map(|i| p.scans_within(i))
            .unwrap_or_default();
        let reference = self
            .visible
            .iter()
            .copied()
            .filter(|id| !moving.contains(id))
            .collect();
        (moving, reference)
    }
    /// The proposed transform the viewport shows in place of the applied one.
    pub(super) fn align_preview(&self) -> Option<(Uuid, Pose)> {
        if !self.aligning() {
            return None;
        }
        Some((self.align.item?, self.align.preview?))
    }
    /// Whether the registration tool hides this scan for picking.
    pub(super) fn align_hides(&self, scan: Uuid) -> bool {
        let Some(p) = self.project.as_ref().filter(|_| self.aligning()) else {
            return false;
        };
        let Some(item) = self.align.item else {
            return false;
        };
        let moving = p.scans_within(item).contains(&scan);
        match self.align.show {
            Show::Both => false,
            Show::Moving => !moving,
            Show::Reference => moving,
        }
    }
    /// Takes back the most recent pick (Backspace).
    pub(super) fn align_undo_pick(&mut self) {
        let pairs = &mut self.align.pairs;
        let latest = pairs
            .iter_mut()
            .flat_map(|pair| [&mut pair.moving, &mut pair.reference])
            .filter(|slot| slot.is_some())
            .max_by_key(|slot| slot.map_or(0, |p| p.order));
        if let Some(slot) = latest {
            *slot = None;
        }
        pairs.retain(|pair| pair.moving.is_some() || pair.reference.is_some());
    }
    pub(super) fn align_tint(&self, scan: Uuid) -> [f32; 4] {
        let Some(p) = self
            .project
            .as_ref()
            .filter(|_| self.aligning() && self.align.tint)
        else {
            return [0.; 4];
        };
        match self.align.item {
            Some(item) if p.scans_within(item).contains(&scan) => tint(MOVING, 0.55),
            Some(_) => tint(REFERENCE, 0.45),
            None => [0.; 4],
        }
    }
    /// Follows the tree selection and collects a finished ICP run. An
    /// unapplied result is not dropped silently: selecting something else
    /// asks first, and outside the tool the result waits for its return.
    pub(super) fn align_update(&mut self) {
        let Some(p) = self.project.clone() else {
            self.align.item = None;
            return;
        };
        let item = self
            .single_tree_item()
            .filter(|id| !p.scans_within(*id).is_empty());
        if item != self.align.item {
            let pending = self.align.preview.is_some()
                && self
                    .align
                    .item
                    .is_some_and(|i| !p.scans_within(i).is_empty());
            if !pending {
                self.align_switch(item);
            } else if self.aligning() && self.dialog.is_none() {
                self.select_tree_item(self.align.item);
                self.dialog = Some(Dialog::AlignPending { next: item });
            }
        }
        if let Some(rx) = &self.align.icp
            && let Ok(result) = rx.try_recv()
        {
            self.align.icp = None;
            self.status =
                (self.t.align_icp_result)(result.rms, result.overlap * 100., result.iterations);
            self.align.preview = Some(result.pose);
            self.align.report = Some(Report::Icp(result));
        }
    }
    /// Where a pick is now, with the preview or as applied.
    fn pick_world(&self, p: &Project, pick: Pick, preview: bool) -> Option<DVec3> {
        let scan = p.scans().find(|s| s.id == pick.scan)?;
        let world = match (preview, self.align_preview()) {
            (true, Some((item, pose))) => p.world_matrix_with(scan, item, pose),
            _ => p.world_matrix(scan),
        };
        Some(world.transform_point3(pick.local))
    }
    /// Picks the displayed point under a click and files it as the moving or
    /// the reference point of the first pair that lacks one.
    pub(super) fn align_input(&mut self, response: &egui::Response) {
        if !self.aligning() || self.align.item.is_none() || !response.clicked() {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        let rect = response.rect;
        let projector = self.camera.projector();
        let radius = self.settings.point_size as f64 / 2.;
        let near = radius + 6.;
        let mut covering: Option<(f64, Pick)> = None;
        let mut closest: Option<(f64, Pick)> = None;
        for (scan, sample, world) in self.shown_points(true) {
            let Some((uv, depth)) = projector.project(world) else {
                continue;
            };
            let dx = uv[0] * rect.width() as f64 - (pos.x - rect.left()) as f64;
            let dy = uv[1] * rect.height() as f64 - (pos.y - rect.top()) as f64;
            let d2 = dx * dx + dy * dy;
            let pick = || Pick {
                scan,
                local: DVec3::from(sample.position),
                order: 0,
            };
            if d2 <= radius * radius {
                if covering.as_ref().is_none_or(|(best, _)| depth < *best) {
                    covering = Some((depth, pick()));
                }
            } else if d2 <= near * near && closest.as_ref().is_none_or(|(best, _)| d2 < *best) {
                closest = Some((d2, pick()));
            }
        }
        let Some((_, mut pick)) = covering.or(closest) else {
            return;
        };
        self.align.picks += 1;
        pick.order = self.align.picks;
        let Some(p) = self.project.clone() else {
            return;
        };
        let (moving, _) = self.align_scans(&p);
        let is_moving = moving.contains(&pick.scan);
        let pairs = &mut self.align.pairs;
        let slot = pairs.iter_mut().find(|pair| {
            if is_moving {
                pair.moving.is_none()
            } else {
                pair.reference.is_none()
            }
        });
        let slot = match slot {
            Some(slot) => slot,
            None => {
                pairs.push(Pair::default());
                pairs.last_mut().unwrap()
            }
        };
        if is_moving {
            slot.moving = Some(pick);
        } else {
            slot.reference = Some(pick);
        }
    }
    /// Complete pairs as applied positions of the moving and reference points.
    fn complete_pairs(&self, p: &Project) -> (Vec<DVec3>, Vec<DVec3>) {
        self.align
            .pairs
            .iter()
            .filter_map(|pair| {
                Some((
                    self.pick_world(p, pair.moving?, false)?,
                    self.pick_world(p, pair.reference?, false)?,
                ))
            })
            .unzip()
    }
    fn fit_pairs(&mut self) {
        let (Some(p), Some(item)) = (self.project.clone(), self.align.item) else {
            return;
        };
        let (moving, reference) = self.complete_pairs(&p);
        match rigid_fit(&moving, &reference) {
            Ok(motion) => {
                let own = p
                    .current()
                    .transforms
                    .get(&item)
                    .copied()
                    .unwrap_or_default();
                let squared: f64 = moving
                    .iter()
                    .zip(&reference)
                    .map(|(m, r)| motion.transform_point3(*m).distance_squared(*r))
                    .sum();
                self.align.preview = Some(p.moved_pose(item, own, motion));
                self.align.report = Some(Report::Pairs {
                    rms: (squared / moving.len() as f64).sqrt(),
                });
            }
            Err(e) => self.error = Some(super::jobs::Notice::new(self.t, &e)),
        }
    }
    fn run_icp(&mut self, ctx: &egui::Context) {
        let (Some(p), Some(item)) = (self.project.clone(), self.align.item) else {
            return;
        };
        let initial = self.align.preview.unwrap_or_else(|| {
            p.current()
                .transforms
                .get(&item)
                .copied()
                .unwrap_or_default()
        });
        let (_, reference) = self.align_scans(&p);
        let options = IcpOptions {
            max_distance: self.settings.icp_distance,
            samples: self.settings.icp_samples,
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel();
        self.align.icp = Some(rx);
        let project = (*p).clone();
        self.start(ctx, false, move |job| {
            let result = project.icp(item, initial, &reference, &options, &job)?;
            let _ = tx.send(result);
            Ok(project)
        });
    }
    /// Starts over on `item`, dropping the picks and any unapplied result.
    pub(super) fn align_switch(&mut self, item: Option<Uuid>) {
        self.align.clear();
        self.align.item = item;
    }
    /// The name of the item being aligned, for messages.
    pub(super) fn align_item_name(&self) -> String {
        let (Some(p), Some(id)) = (&self.project, self.align.item) else {
            return String::new();
        };
        p.groups()
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .or_else(|| p.scan(id).map(|s| s.name.clone()))
            .unwrap_or_default()
    }
    pub(super) fn align_apply(&mut self) {
        if let Some((item, pose)) = self.align_preview() {
            self.apply_edit(|p| p.set_transform(item, pose));
            self.align.preview = None;
        }
    }
    /// The right-hand panel of the registration tool.
    pub(super) fn align_panel(&mut self, ui: &mut egui::Ui) {
        if !self.aligning() {
            return;
        }
        let t = self.t;
        let ctx = ui.ctx().clone();
        let Some(p) = self.project.clone() else {
            return;
        };
        let idle = self.job.is_none();
        egui::Panel::right("align")
            .resizable(true)
            .default_size(300.)
            .show(ui, |ui| {
                ui.strong(format!("{} {}", icon::CROSSHAIR, t.align_title));
                ui.add_space(4.);
                let Some(item) = self.align.item else {
                    ui.label(t.align_choose_item);
                    return;
                };
                let name = p
                    .groups()
                    .iter()
                    .find(|g| g.id == item)
                    .map(|g| g.name.clone())
                    .or_else(|| p.scans().find(|s| s.id == item).map(|s| s.name.clone()))
                    .unwrap_or_default();
                let (_, reference) = self.align_scans(&p);
                egui::Grid::new("align roles")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.colored_label(MOVING, format!("{} {}", icon::CIRCLE, t.align_moving));
                        ui.strong(name);
                        ui.end_row();
                        ui.colored_label(
                            REFERENCE,
                            format!("{} {}", icon::CIRCLE, t.align_reference),
                        );
                        ui.label((t.align_reference_scans)(reference.len()));
                        ui.end_row();
                    });
                ui.checkbox(&mut self.align.tint, t.align_tint);
                ui.horizontal(|ui| {
                    ui.label(t.align_show);
                    ui.selectable_value(&mut self.align.show, Show::Both, t.align_show_both);
                    ui.selectable_value(&mut self.align.show, Show::Moving, t.align_moving);
                    ui.selectable_value(&mut self.align.show, Show::Reference, t.align_reference);
                });
                ui.separator();

                ui.strong(t.align_pairs);
                ui.small(t.align_pairs_hint);
                let preview = self.align.preview.is_some();
                let mut remove = None;
                egui::Grid::new("align pairs")
                    .num_columns(5)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("#");
                        ui.colored_label(MOVING, t.align_moving);
                        ui.colored_label(REFERENCE, t.align_reference);
                        ui.label(t.align_residual);
                        ui.label("");
                        ui.end_row();
                        for (i, pair) in self.align.pairs.iter().enumerate() {
                            let mark = |set: bool| if set { icon::CHECK } else { "—" };
                            ui.label((i + 1).to_string());
                            ui.label(mark(pair.moving.is_some()));
                            ui.label(mark(pair.reference.is_some()));
                            let residual = pair.moving.zip(pair.reference).and_then(|(m, r)| {
                                Some(
                                    self.pick_world(&p, m, preview)?
                                        .distance(self.pick_world(&p, r, false)?),
                                )
                            });
                            ui.label(residual.map_or("".into(), |d| format!("{d:.3} m")));
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
                if let Some(i) = remove {
                    self.align.pairs.remove(i);
                }
                let complete = self
                    .align
                    .pairs
                    .iter()
                    .filter(|pair| pair.moving.is_some() && pair.reference.is_some())
                    .count();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            idle && complete >= 3,
                            egui::Button::new(format!("{} {}", icon::LINK, t.align_fit_pairs)),
                        )
                        .on_disabled_hover_text(t.align_need_three)
                        .clicked()
                    {
                        self.fit_pairs();
                    }
                    if ui
                        .add_enabled(
                            !self.align.pairs.is_empty(),
                            egui::Button::new(t.align_clear_pairs),
                        )
                        .clicked()
                    {
                        self.align.pairs.clear();
                    }
                });
                ui.separator();

                ui.strong(t.align_icp);
                ui.small(t.align_icp_hint);
                egui::Grid::new("icp options")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label(t.align_icp_distance);
                        ui.add(
                            egui::DragValue::new(&mut self.settings.icp_distance)
                                .range(0.005..=20.)
                                .speed(0.005)
                                .max_decimals(3),
                        );
                        ui.end_row();
                        ui.label(t.align_icp_samples);
                        ui.add(
                            egui::DragValue::new(&mut self.settings.icp_samples)
                                .range(5_000..=1_000_000)
                                .speed(1000.),
                        );
                        ui.end_row();
                    });
                if ui
                    .add_enabled(
                        idle && !reference.is_empty(),
                        egui::Button::new(format!("{} {}", icon::MAGNET, t.align_run_icp)),
                    )
                    .clicked()
                {
                    self.run_icp(&ctx);
                }
                ui.separator();

                match &self.align.report {
                    Some(Report::Pairs { rms }) => {
                        ui.label((t.align_pairs_result)(*rms));
                    }
                    Some(Report::Icp(r)) => {
                        ui.label((t.align_icp_result)(r.rms, r.overlap * 100., r.iterations));
                        if r.overlap < 0.1 {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 190, 80),
                                t.align_low_overlap,
                            );
                        }
                    }
                    None => {}
                }
                if preview {
                    ui.small(t.transform_previewing);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            idle && preview,
                            egui::Button::new(format!("{} {}", icon::CHECK, t.apply)),
                        )
                        .clicked()
                    {
                        self.align_apply();
                    }
                    if ui
                        .add_enabled(
                            preview,
                            egui::Button::new(format!("{} {}", icon::X, t.align_discard)),
                        )
                        .clicked()
                    {
                        self.align.preview = None;
                        self.align.report = None;
                    }
                });
            });
    }
    /// Numbered markers on the picked points, joined within each pair.
    pub(super) fn draw_align(&self, ui: &egui::Ui, rect: egui::Rect) {
        let Some(p) = self.project.as_ref().filter(|_| self.aligning()) else {
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
        for (i, pair) in self.align.pairs.iter().enumerate() {
            let at = |pick: Option<Pick>| {
                pick.and_then(|pick| self.pick_world(p, pick, true))
                    .and_then(screen)
            };
            let (m, r) = (at(pair.moving), at(pair.reference));
            if let (Some(m), Some(r)) = (m, r) {
                painter.line_segment([m, r], egui::Stroke::new(1.5, egui::Color32::WHITE));
            }
            for (point, color) in [(m, MOVING), (r, REFERENCE)] {
                let Some(point) = point else { continue };
                painter.circle(
                    point,
                    6.,
                    color,
                    egui::Stroke::new(1.5, egui::Color32::BLACK),
                );
                painter.text(
                    point + egui::vec2(8., -8.),
                    egui::Align2::LEFT_BOTTOM,
                    (i + 1).to_string(),
                    egui::FontId::proportional(14.),
                    color,
                );
            }
        }
    }
    /// For smoke tests: aligns the first scan to the others by ICP.
    pub(super) fn smoke_icp(&mut self, ctx: &egui::Context) {
        let first = self
            .project
            .as_ref()
            .and_then(|p| p.scans().next().map(|s| s.id));
        self.selection.tool = Tool::Align;
        self.select_tree_item(first);
        self.align_update();
        self.run_icp(ctx);
    }
    /// For smoke tests: four pairs between displayed points of the first scan
    /// and the same places expressed in another scan, then a fit. The points
    /// already coincide, so the fit must not move anything.
    pub(super) fn smoke_pairs(&mut self) {
        let Some(p) = self.project.clone() else {
            return;
        };
        let ids: Vec<_> = p.scans().map(|s| s.id).collect();
        let (Some(&first), Some(&other)) = (ids.first(), ids.get(1)) else {
            return;
        };
        self.selection.tool = Tool::Align;
        self.select_tree_item(Some(first));
        self.align_update();
        let world = |id| p.world_matrix(p.scans().find(|s| s.id == id).unwrap());
        let (to_other, from_first) = (world(other).inverse(), world(first));
        let locals: Vec<DVec3> = self
            .shown_points(false)
            .filter(|(scan, ..)| *scan == first)
            .map(|(_, s, _)| DVec3::from(s.position))
            .collect();
        if locals.is_empty() {
            return;
        }
        for k in 0..4 {
            let local = locals[(locals.len() - 1) * k / 3];
            let reference = to_other.transform_point3(from_first.transform_point3(local));
            self.align.pairs.push(Pair {
                moving: Some(Pick {
                    scan: first,
                    local,
                    order: 0,
                }),
                reference: Some(Pick {
                    scan: other,
                    local: reference,
                    order: 0,
                }),
            });
        }
        self.fit_pairs();
    }
    pub(super) fn align_summary(&self) -> String {
        match &self.align.report {
            Some(Report::Icp(r)) => format!(
                "icp rms {:.4} m, overlap {:.0}%, {} iterations, preview {}",
                r.rms,
                r.overlap * 100.,
                r.iterations,
                self.align_preview().is_some()
            ),
            Some(Report::Pairs { rms }) => format!("pairs rms {rms:.4} m"),
            None => "no result".into(),
        }
    }
}
