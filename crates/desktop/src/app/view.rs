//! Background loading of the viewport's display octree nodes, like Potree:
//! each request chooses the nodes to show for the camera, and the loader
//! reads the ones not cached yet, best first, sending what is ready every so
//! often. Loaded nodes stay cached and resident on the GPU, so moving the
//! camera only adds what comes into view. A new epoch (project, visibility or
//! budget change) cancels everything in flight.
use super::ColorMode;
use super::{Workbench, jobs::Notice, jobs::is_cancelled};
use crate::render::DrawNode;
use eframe::egui;
use geemil_core::{Camera, JobControl, LoadedNode, Project, Sample, ViewCache};
use glam::{DMat4, DVec3};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

/// The points of `nodes` whose scan has a world matrix in `worlds`, with
/// their scan, scan coordinates and place in the project frame.
pub(super) fn node_points(
    nodes: &[LoadedNode],
    worlds: HashMap<Uuid, DMat4>,
) -> impl Iterator<Item = (Uuid, &Sample, DVec3)> + '_ {
    nodes.iter().flat_map(move |node| {
        let world = worlds.get(&node.scan).copied();
        node.samples.iter().filter_map(move |s| {
            let world = world?;
            Some((
                node.scan,
                s,
                world.transform_point3(DVec3::from(s.position)),
            ))
        })
    })
}

/// Distinct sRGB colours for colouring by scan (Tableau 10).
const SCAN_COLORS: [[u8; 3]; 10] = [
    [78, 121, 167],
    [242, 142, 43],
    [225, 87, 89],
    [118, 183, 178],
    [89, 161, 79],
    [237, 201, 72],
    [176, 122, 161],
    [255, 157, 167],
    [156, 117, 95],
    [186, 176, 172],
];
/// How often the loader sends the nodes it has so far.
const PARTIAL_EVERY: Duration = Duration::from_millis(40);

struct ViewRequest {
    project: Arc<Project>,
    camera: Camera,
    budget: usize,
    visible: Vec<Uuid>,
    epoch: u64,
}
struct ViewResult {
    /// Increases with every result sent.
    serial: u64,
    result: anyhow::Result<Vec<LoadedNode>>,
    elapsed_ms: f64,
    epoch: u64,
    /// Whether nodes are still loading for this request.
    partial: bool,
}

pub(super) struct ViewLoader {
    tx: mpsc::SyncSender<ViewRequest>,
    rx: mpsc::Receiver<ViewResult>,
    epoch: Arc<AtomicU64>,
    /// The serial of the last result shown.
    shown: u64,
}
impl ViewLoader {
    pub(super) fn spawn(ctx: egui::Context) -> Self {
        let (tx, requests) = mpsc::sync_channel::<ViewRequest>(1);
        let (results, rx) = mpsc::sync_channel(4);
        let epoch = Arc::new(AtomicU64::new(0));
        let live_epoch = epoch.clone();
        std::thread::spawn(move || {
            let mut cache = ViewCache::new(0);
            let mut serial = 0;
            let Ok(mut request) = requests.recv() else {
                return;
            };
            loop {
                while let Ok(new) = requests.try_recv() {
                    request = new;
                }
                let started = Instant::now();
                let send = |nodes, partial, serial: &mut u64, result_epoch| {
                    *serial += 1;
                    let result = ViewResult {
                        serial: *serial,
                        result: nodes,
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
                        epoch: result_epoch,
                        partial,
                    };
                    let sent = if partial {
                        results.try_send(result).is_ok()
                    } else {
                        results.send(result).is_ok()
                    };
                    ctx.request_repaint();
                    sent
                };
                let outcome = load(&request, &mut cache, &live_epoch, &requests, |nodes| {
                    send(Ok(nodes), true, &mut serial, request.epoch);
                });
                match outcome {
                    Outcome::Done(nodes) => {
                        if live_epoch.load(Ordering::Relaxed) == request.epoch
                            && !send(Ok(nodes), false, &mut serial, request.epoch)
                        {
                            return;
                        }
                    }
                    Outcome::Failed(e) => {
                        if !send(Err(e), false, &mut serial, request.epoch) {
                            return;
                        }
                    }
                    Outcome::Superseded(new) => {
                        request = new;
                        continue;
                    }
                    Outcome::Stale => {}
                }
                match requests.recv() {
                    Ok(new) => request = new,
                    Err(_) => return,
                }
            }
        });
        Self {
            tx,
            rx,
            epoch,
            shown: 0,
        }
    }
    /// Cancels in-flight loads and discards their results.
    pub(super) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }
}
impl Drop for ViewLoader {
    fn drop(&mut self) {
        self.invalidate();
    }
}

enum Outcome {
    Done(Vec<LoadedNode>),
    Failed(anyhow::Error),
    /// A newer request came in; it is handled next.
    Superseded(ViewRequest),
    /// The epoch changed; nothing is sent.
    Stale,
}

/// Chooses the nodes for a request and loads them, cached ones first, then
/// the rest best first, passing what is ready to `partial` every so often.
fn load(
    request: &ViewRequest,
    cache: &mut ViewCache,
    epoch: &Arc<AtomicU64>,
    requests: &mpsc::Receiver<ViewRequest>,
    mut partial: impl FnMut(Vec<LoadedNode>),
) -> Outcome {
    // Like Potree: keep twice the point budget of decoded samples.
    cache.set_point_limit(request.budget.saturating_mul(2));
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let (live, expected) = (epoch.clone(), request.epoch);
    let job = JobControl {
        cancel,
        progress: Arc::new(move |_, _, _| {
            if live.load(Ordering::Relaxed) != expected {
                flag.store(true, Ordering::Relaxed);
            }
        }),
    };
    let project = &request.project;
    let picks = match project.select_view(&request.camera, request.budget, &request.visible, &job) {
        Ok(picks) => picks,
        Err(e) => return Outcome::Failed(e),
    };
    let (cached, missing): (Vec<_>, Vec<_>) = picks
        .into_iter()
        .partition(|p| p.quota.is_some() || cache.contains(project, p.scan, p.node));
    let mut nodes = Vec::with_capacity(cached.len() + missing.len());
    for pick in &cached {
        match project.view_node(pick, &job, cache) {
            Ok(node) => nodes.push(node),
            Err(e) => return Outcome::Failed(e),
        }
    }
    let mut last = Instant::now();
    if !missing.is_empty() {
        partial(nodes.clone());
    }
    for pick in &missing {
        if epoch.load(Ordering::Relaxed) != request.epoch {
            return Outcome::Stale;
        }
        if let Ok(mut newer) = requests.try_recv() {
            while let Ok(new) = requests.try_recv() {
                newer = new;
            }
            return Outcome::Superseded(newer);
        }
        job.report(geemil_core::Stage::ViewPoints, 0, 0);
        match project.view_node(pick, &job, cache) {
            Ok(node) => nodes.push(node),
            Err(e) if is_cancelled(&e) => return Outcome::Stale,
            Err(e) => return Outcome::Failed(e),
        }
        if last.elapsed() >= PARTIAL_EVERY {
            partial(nodes.clone());
            last = Instant::now();
        }
    }
    Outcome::Done(nodes)
}

impl Workbench {
    pub(super) fn request_view(&mut self) {
        if self.job.is_some() || self.smoke.colors {
            return;
        }
        if self.camera != self.last_camera {
            self.last_camera = self.camera;
            self.last_motion = Instant::now();
            self.dirty = true;
        }
        if !self.dirty || self.last_request.elapsed() < Duration::from_millis(33) {
            return;
        }
        if let Some(project) = &self.project
            && self
                .view
                .tx
                .try_send(ViewRequest {
                    project: project.clone(),
                    camera: self.camera,
                    budget: self.settings.point_budget,
                    visible: self.visible.iter().copied().collect(),
                    epoch: self.view.epoch.load(Ordering::Relaxed),
                })
                .is_ok()
        {
            self.dirty = false;
            self.last_request = Instant::now();
        }
    }
    pub(super) fn poll_view(&mut self) {
        while let Ok(result) = self.view.rx.try_recv() {
            if result.epoch != self.view.epoch.load(Ordering::Relaxed)
                || result.serial <= self.view.shown
            {
                continue;
            }
            match result.result {
                Ok(nodes) => {
                    self.view.shown = result.serial;
                    let moving = self.last_motion.elapsed() < Duration::from_millis(150);
                    let points = nodes.iter().map(|n| n.samples.len()).sum();
                    self.smoke.view_loaded(moving, points);
                    self.nodes = nodes;
                    self.points_generation += 1;
                    self.update_height_range();
                    if !result.partial {
                        self.view_ms = result.elapsed_ms;
                    }
                }
                // A superseded view request is not a failure.
                Err(e) if is_cancelled(&e) => {}
                Err(e) => self.error = Some(Notice::new(self.t, &e)),
            }
        }
    }
    /// The world matrix each loaded scan is drawn with now: its applied
    /// transforms, or with `previewed` the transform being previewed. Scans
    /// hidden in the tree or by the alignment tool are left out. Without a
    /// project (the colour fixture) points are drawn as they are.
    pub(super) fn scan_worlds(&self, previewed: bool) -> HashMap<Uuid, DMat4> {
        let Some(project) = &self.project else {
            return self
                .nodes
                .iter()
                .map(|n| (n.scan, DMat4::IDENTITY))
                .collect();
        };
        let preview = self.transform_preview().filter(|_| previewed);
        project
            .scans()
            .filter(|s| self.visible.contains(&s.id) && !(previewed && self.align_hides(s.id)))
            .map(|scan| {
                let world = match preview {
                    Some((item, pose)) => project.world_matrix_with(scan, item, pose),
                    None => project.world_matrix(scan),
                };
                (scan.id, world)
            })
            .collect()
    }
    /// The shown points with their scan, scan coordinates and place in the
    /// project frame: as drawn with `previewed`, else as applied.
    pub(super) fn shown_points(
        &self,
        previewed: bool,
    ) -> impl Iterator<Item = (Uuid, &Sample, DVec3)> + '_ {
        node_points(&self.nodes, self.scan_worlds(previewed))
    }
    /// The number of points drawn.
    pub(super) fn shown_count(&self) -> usize {
        let worlds = self.scan_worlds(true);
        self.nodes
            .iter()
            .filter(|n| worlds.contains_key(&n.scan))
            .map(|n| n.samples.len())
            .sum()
    }
    /// What the renderer draws: each loaded node of a shown scan with the
    /// scan's world matrix and tint, and its preview marks.
    pub(super) fn draw_nodes(&self) -> Vec<DrawNode<'_>> {
        let worlds = self.scan_worlds(true);
        let scans: Vec<_> = self
            .project
            .as_ref()
            .map(|p| p.scans().map(|s| s.id).collect())
            .unwrap_or_default();
        let marks = self.selection.marks();
        let mut offset = 0;
        let mut result = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let range = offset..offset + node.samples.len();
            offset = range.end;
            let Some(world) = worlds.get(&node.scan) else {
                continue;
            };
            let mut tint = self.align_tint(node.scan);
            if tint[3] == 0. && self.settings.color_mode == ColorMode::Scan {
                let i = scans.iter().position(|id| *id == node.scan).unwrap_or(0);
                let [r, g, b] = SCAN_COLORS[i % SCAN_COLORS.len()];
                tint = [r as f32 / 255., g as f32 / 255., b as f32 / 255., 1.];
            }
            result.push(DrawNode {
                samples: &node.samples,
                world: *world,
                tint,
                marks: marks.and_then(|(m, _)| m.get(range)),
            });
        }
        result
    }
    /// The height range to colour by: the 1st to 99th percentile of the
    /// displayed points' heights, taken once per project state and visibility
    /// so colours stay put while the camera moves.
    fn update_height_range(&mut self) {
        let Some(p) = &self.project else { return };
        let key = (p.current().id, self.visible.iter().copied().collect());
        if self.height_for.as_ref() == Some(&key) {
            return;
        }
        let count = self.shown_count();
        if count == 0 {
            return;
        }
        let step = (count / 20_000).max(1);
        let mut heights: Vec<f64> = self
            .shown_points(false)
            .step_by(step)
            .map(|(_, _, p)| p.z)
            .collect();
        heights.sort_by(f64::total_cmp);
        let at = |q: f64| heights[((heights.len() - 1) as f64 * q) as usize];
        let (low, high) = (at(0.01), at(0.99));
        self.height_range = Some([low, high.max(low + 1e-3)]);
        self.height_for = Some(key);
    }
}
