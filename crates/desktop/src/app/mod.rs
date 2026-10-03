//! Application state and frame orchestration. Panels, jobs, view loading and
//! smoke tests live in submodules as further `impl Workbench` blocks.
mod history;
mod jobs;
mod sidebar;
mod smoke;
mod status;
mod toolbar;
mod view;
mod viewport;

pub use smoke::SmokeOptions;

use crate::i18n::{self, Strings};
use crate::render::PointRenderer;
use eframe::egui;
use geemil_core::{Bounds, Camera, Project, Sample};
use glam::DQuat;
use jobs::{ActiveJob, Notice};
use smoke::SmokeTest;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;
use view::ViewLoader;

pub struct Workbench {
    t: &'static Strings,
    project: Option<Arc<Project>>,
    renderer: PointRenderer,
    camera: Camera,
    visible: BTreeSet<Uuid>,
    selected: Option<Uuid>,

    // Background work and status bar.
    job: Option<ActiveJob>,
    status: String,
    error: Option<Notice>,
    progress: f32,

    // Displayed samples and view refinement.
    view: ViewLoader,
    points: Vec<Sample>,
    points_generation: u64,
    points_origin: [f64; 3],
    view_ms: f64,
    dirty: bool,
    last_request: Instant,
    last_camera: Camera,
    last_motion: Instant,
    refine_pending: bool,
    point_budget: usize,
    point_size: f32,

    // Screen-space selection.
    select_mode: bool,
    lasso: bool,
    polygon: Vec<egui::Pos2>,
    drag_start: Option<egui::Pos2>,
    selection_camera: Option<Camera>,
    depth: f64,

    // Sidebar inputs.
    translation: [f64; 3],
    rotation: [f64; 3],
    branch_name: String,

    smoke: SmokeTest,
}
impl Workbench {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: Option<PathBuf>,
        smoke: SmokeOptions,
    ) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        install_fonts(&cc.egui_ctx);
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("wgpu renderer selected");
        let t = &i18n::JA;
        let mut app = Self {
            t,
            project: None,
            renderer: PointRenderer::new(rs.device.clone(), rs.queue.clone()),
            camera: Camera::default(),
            visible: BTreeSet::new(),
            selected: None,
            job: None,
            status: t.status_start.into(),
            error: None,
            progress: 0.,
            view: ViewLoader::spawn(cc.egui_ctx.clone()),
            points: vec![],
            points_generation: 0,
            points_origin: [0.; 3],
            view_ms: 0.,
            dirty: false,
            last_request: Instant::now(),
            last_camera: Camera::default(),
            last_motion: Instant::now(),
            refine_pending: false,
            point_budget: 200_000,
            point_size: 3.,
            select_mode: false,
            lasso: false,
            polygon: vec![],
            drag_start: None,
            selection_camera: None,
            depth: 0.5,
            translation: [0.; 3],
            rotation: [0.; 3],
            branch_name: t.default_branch_name.into(),
            smoke: SmokeTest::new(smoke),
        };
        if let Some(path) = path {
            match Project::load(&path) {
                Ok(p) => app.install(p, true),
                Err(e) => app.error = Some(Notice::new(t, &e)),
            }
        }
        app.smoke_setup();
        app
    }
    /// Replaces the project state after open, create, a finished job or a revision switch.
    fn install(&mut self, project: Project, fit: bool) {
        let previous: Option<Vec<_>> = self
            .project
            .as_ref()
            .filter(|p| p.root == project.root)
            .map(|p| p.scans().map(|s| s.id).collect());
        let scans: Vec<_> = project.scans().map(|s| s.id).collect();
        carry_scan_state(
            &mut self.visible,
            &mut self.selected,
            previous.as_deref(),
            &scans,
        );
        if fit {
            frame_bounds(&mut self.camera, &project.bounds());
        }
        self.points_generation = self.view.next_generation();
        self.view.invalidate();
        self.points.clear();
        self.points_origin = self.camera.target;
        self.project = Some(Arc::new(project));
        self.status = self.t.status_saved.into();
        self.polygon.clear();
        self.selection_camera = None;
        self.dirty = true;
        self.sync_pose();
    }
    fn fit_view(&mut self) {
        if let Some(p) = &self.project {
            frame_bounds(&mut self.camera, &p.bounds());
            self.dirty = true;
            self.polygon.clear();
        }
    }
    /// Loads the selected scan's stored transform into the alignment inputs.
    fn sync_pose(&mut self) {
        if let (Some(p), Some(id)) = (&self.project, self.selected) {
            let pose = p.current().transforms.get(&id).copied().unwrap_or_default();
            self.translation = pose.translation;
            let (x, y, z) = DQuat::from_array(pose.rotation_xyzw).to_euler(glam::EulerRot::XYZ);
            self.rotation = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
        }
    }
}

impl eframe::App for Workbench {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.smoke_frame(ctx);
        self.poll_job();
        self.poll_view();
        self.toolbar(ctx);
        self.sidebar(ctx);
        self.status_bar(ctx);
        self.viewport(ctx, frame);
        self.request_view();
        if self.dirty || self.refine_pending || self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}
impl Drop for Workbench {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in [
        "C:/Windows/Fonts/meiryo.ttc",
        "C:/Windows/Fonts/YuGothM.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("Japanese".into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("Japanese".into());
            break;
        }
    }
    ctx.set_fonts(fonts);
}

fn frame_bounds(camera: &mut Camera, bounds: &Bounds) {
    camera.target = bounds.center().to_array();
    camera.distance = (bounds.radius() * 2.8).max(1.);
}

/// Carries scan visibility and selection into a new project state. `previous`
/// holds the replaced state's scans when it belongs to the same project.
fn carry_scan_state(
    visible: &mut BTreeSet<Uuid>,
    selected: &mut Option<Uuid>,
    previous: Option<&[Uuid]>,
    scans: &[Uuid],
) {
    match previous {
        // Keep what the user hid, and show scans that just appeared.
        Some(previous) => {
            visible.retain(|id| scans.contains(id));
            visible.extend(scans.iter().filter(|id| !previous.contains(id)));
        }
        None => *visible = scans.iter().copied().collect(),
    }
    if !selected.is_some_and(|id| scans.contains(&id)) {
        *selected = scans.first().copied();
    }
}

#[cfg(test)]
mod tests {
    use super::carry_scan_state;
    use std::collections::BTreeSet;
    use uuid::Uuid;

    #[test]
    fn import_keeps_hidden_scans_and_selection() {
        let [a, b, c] = [(); 3].map(|_| Uuid::new_v4());
        let mut visible = BTreeSet::from([a]);
        let mut selected = Some(b);
        carry_scan_state(&mut visible, &mut selected, Some(&[a, b]), &[a, b, c]);
        assert_eq!(visible, BTreeSet::from([a, c]));
        assert_eq!(selected, Some(b));
    }

    #[test]
    fn removed_or_foreign_scans_fall_back_to_the_first() {
        let [a, b, c] = [(); 3].map(|_| Uuid::new_v4());
        let mut visible = BTreeSet::from([a, b]);
        let mut selected = Some(b);
        // Switching to a revision without `b` drops it from both.
        carry_scan_state(&mut visible, &mut selected, Some(&[a, b]), &[a]);
        assert_eq!(visible, BTreeSet::from([a]));
        assert_eq!(selected, Some(a));
        // Another project shows everything and selects its first scan.
        carry_scan_state(&mut visible, &mut selected, None, &[c, b]);
        assert_eq!(visible, BTreeSet::from([b, c]));
        assert_eq!(selected, Some(c));
    }
}
