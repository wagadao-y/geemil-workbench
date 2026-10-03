//! Background LOD loading for the viewport. Requests carry a generation and an
//! epoch: a newer request supersedes refinement, and a new epoch (project,
//! visibility or budget change) cancels everything in flight.
use super::{Workbench, jobs::Notice, jobs::is_cancelled};
use crate::render::Segment;
use eframe::egui;
use geemil_core::{Camera, JobControl, LoadedView, Project, ViewCache};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

struct ViewRequest {
    project: Arc<Project>,
    camera: Camera,
    budget: usize,
    visible: Vec<Uuid>,
    generation: u64,
    epoch: u64,
    interactive: bool,
}
struct ViewResult {
    generation: u64,
    result: anyhow::Result<LoadedView>,
    origin: [f64; 3],
    elapsed_ms: f64,
    epoch: u64,
    interactive: bool,
}

pub(super) struct ViewLoader {
    tx: mpsc::SyncSender<ViewRequest>,
    rx: mpsc::Receiver<ViewResult>,
    generation: Arc<AtomicU64>,
    epoch: Arc<AtomicU64>,
}
impl ViewLoader {
    pub(super) fn spawn(ctx: egui::Context) -> Self {
        let (tx, requests) = mpsc::sync_channel::<ViewRequest>(1);
        let (results, rx) = mpsc::sync_channel(2);
        let generation = Arc::new(AtomicU64::new(0));
        let epoch = Arc::new(AtomicU64::new(0));
        let live_epoch = epoch.clone();
        let current = generation.clone();
        std::thread::spawn(move || {
            let mut cache = ViewCache::new(256 * 1024 * 1024);
            while let Ok(mut request) = requests.recv() {
                while let Ok(new) = requests.try_recv() {
                    request = new;
                }
                if live_epoch.load(Ordering::Relaxed) != request.epoch {
                    continue;
                }
                let cancel = Arc::new(AtomicBool::new(false));
                let flag = cancel.clone();
                let live = current.clone();
                let expected = request.generation;
                let epoch = request.epoch;
                let epoch_source = live_epoch.clone();
                let interactive = request.interactive;
                let job = JobControl {
                    cancel,
                    progress: Arc::new(move |_, _, _| {
                        if epoch_source.load(Ordering::Relaxed) != epoch
                            || (!interactive && live.load(Ordering::Relaxed) != expected)
                        {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }),
                };
                let started = Instant::now();
                let result = request.project.load_view_cached(
                    &request.camera,
                    request.budget,
                    &request.visible,
                    &job,
                    &mut cache,
                );
                if live_epoch.load(Ordering::Relaxed) == request.epoch
                    && (interactive || current.load(Ordering::Relaxed) == request.generation)
                {
                    let _ = results.try_send(ViewResult {
                        generation: request.generation,
                        result,
                        origin: request.camera.target,
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
                        epoch: request.epoch,
                        interactive,
                    });
                    ctx.request_repaint();
                }
            }
        });
        Self {
            tx,
            rx,
            generation,
            epoch,
        }
    }
    /// Cancels in-flight loads and discards their results.
    pub(super) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }
    /// Supersedes in-flight refinement; returns the new generation.
    pub(super) fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }
}
impl Drop for ViewLoader {
    fn drop(&mut self) {
        self.next_generation();
        self.invalidate();
    }
}

impl Workbench {
    pub(super) fn request_view(&mut self) {
        if self.job.is_some() || self.smoke.colors {
            return;
        }
        if self.camera != self.last_camera {
            self.last_camera = self.camera;
            self.last_motion = Instant::now();
            self.refine_pending = true;
            self.dirty = true;
        }
        let moving = self.last_motion.elapsed() < Duration::from_millis(150);
        if !(self.dirty || self.refine_pending && !moving)
            || self.last_request.elapsed() < Duration::from_millis(33)
        {
            return;
        }
        if let Some(project) = &self.project {
            let generation = self.view.next_generation();
            if self
                .view
                .tx
                .try_send(ViewRequest {
                    project: project.clone(),
                    camera: self.camera,
                    // Moving shows a coarser view so updates keep up with the camera.
                    budget: if moving {
                        (self.settings.point_budget / 8).clamp(32_000, 300_000)
                    } else {
                        self.settings.point_budget
                    },
                    visible: self.visible.iter().copied().collect(),
                    generation,
                    epoch: self.view.epoch.load(Ordering::Relaxed),
                    interactive: moving,
                })
                .is_ok()
            {
                self.dirty = false;
                self.refine_pending = moving;
            }
            self.last_request = Instant::now();
        }
    }
    pub(super) fn poll_view(&mut self) {
        while let Ok(result) = self.view.rx.try_recv() {
            if result.epoch != self.view.epoch.load(Ordering::Relaxed)
                || result.generation <= self.points_generation
            {
                continue;
            }
            match result.result {
                Ok(view) => {
                    self.smoke
                        .view_loaded(result.interactive, view.samples.len());
                    self.points = view.samples;
                    self.points_segments = view.segments;
                    self.points_generation = result.generation;
                    self.points_origin = result.origin;
                    self.view_ms = result.elapsed_ms;
                }
                // A superseded view request is not a failure.
                Err(e) if is_cancelled(&e) => {}
                Err(e) => self.error = Some(Notice::new(self.t, &e)),
            }
        }
    }
    /// Where to draw each loaded scan now: transforms applied since the load,
    /// a transform being previewed and visibility take effect before the next
    /// load arrives. `None` draws the points as loaded.
    pub(super) fn draw_segments(&self) -> Option<Vec<Segment>> {
        let project = self.project.as_ref()?;
        if self.points_segments.is_empty() {
            return None;
        }
        let preview = self.transform_preview();
        Some(
            self.points_segments
                .iter()
                .filter(|s| self.visible.contains(&s.scan))
                .filter_map(|s| {
                    let scan = project.scans().find(|scan| scan.id == s.scan)?;
                    let world = match preview {
                        Some((item, pose)) => project.world_matrix_with(scan, item, pose),
                        None => project.world_matrix(scan),
                    };
                    Some(Segment {
                        range: s.range.start as u32..s.range.end as u32,
                        motion: world * s.world.inverse(),
                    })
                })
                .collect(),
        )
    }
}
