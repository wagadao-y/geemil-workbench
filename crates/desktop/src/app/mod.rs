//! Application state and frame orchestration. Panels, jobs, view loading and
//! smoke tests live in submodules as further `impl Workbench` blocks.
mod actions;
mod align;
mod crop;
mod dialogs;
mod gizmo;
mod jobs;
mod layers;
mod measure;
mod menu;
mod navigation;
mod revisions;
mod selection;
mod smoke;
mod status;
mod tree;
mod undo;
mod view;
mod viewport;
mod welcome;

pub use smoke::SmokeOptions;

use crate::i18n::{self, Strings};
use crate::render::PointRenderer;
use eframe::egui;
use geemil_core::{Bounds, Camera, CleanupReport, LoadedNode, Project};
use jobs::{ActiveJob, Notice};
use serde::{Deserialize, Serialize};
use smoke::SmokeTest;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use uuid::Uuid;
use view::ViewLoader;

/// What the points are coloured by.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(super) enum ColorMode {
    /// The colour stored with each point.
    #[default]
    Rgb,
    /// A colour ramp over the height of the displayed points.
    Height,
    /// One colour per scan.
    Scan,
}

/// Preferences kept between sessions.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Settings {
    recent: Vec<PathBuf>,
    last_location: Option<PathBuf>,
    point_budget: usize,
    point_size: f32,
    /// Potree's adaptive point size instead of a fixed one.
    adaptive_size: bool,
    edl: bool,
    edl_strength: f32,
    color_mode: ColorMode,
    /// Last filter parameters, offered again next time.
    subsample_size: f64,
    subsample_merged: bool,
    overlap_size: f64,
    noise_radius: f64,
    noise_neighbours: u32,
    /// Maximum correspondence distances, coarse to fine, each with its own
    /// run button: ICP from a rough manual placement, then closer.
    icp_distances: [f64; 4],
    icp_samples: usize,
    outlier_neighbours: u32,
    outlier_deviations: f64,
    outlier_reach: f64,
    filter_memory_mib: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            recent: vec![],
            last_location: None,
            point_budget: 2_000_000,
            point_size: 2.,
            adaptive_size: false,
            edl: true,
            edl_strength: 1.,
            color_mode: ColorMode::Rgb,
            subsample_size: 0.01,
            subsample_merged: false,
            overlap_size: 0.1,
            noise_radius: 0.05,
            noise_neighbours: 4,
            icp_distances: [0.5, 0.1, 0.03, 0.01],
            icp_samples: 60_000,
            // CloudCompare's defaults.
            outlier_neighbours: 6,
            outlier_deviations: 1.0,
            outlier_reach: 0.5,
            filter_memory_mib: 768,
        }
    }
}
const SETTINGS_KEY: &str = "settings";

pub struct Workbench {
    t: &'static Strings,
    project: Option<Arc<Project>>,
    /// Taken out while drawing, which reads the rest of the state.
    renderer: Option<PointRenderer>,
    camera: Camera,
    visible: BTreeSet<Uuid>,
    /// The scan or folder selected in the tree.
    selected: Option<Uuid>,
    /// An item selected outside the tree, which the tree opens its folders
    /// for and scrolls to once.
    reveal: Option<Uuid>,
    tree_selection: tree::TreeSelection,
    folder_summary: tree::FolderSummary,
    layer_counts: layers::LayerCounts,
    settings: Settings,

    // Background work and status bar.
    job: Option<ActiveJob>,
    status: String,
    error: Option<Notice>,
    progress: f32,
    cleanup_report: Option<mpsc::Receiver<CleanupReport>>,

    // Displayed samples and view refinement.
    view: ViewLoader,
    /// The display octree nodes shown, in scan coordinates.
    nodes: Vec<LoadedNode>,
    /// With adaptive point size, each shown point's spacing, by node.
    spacings: Vec<Arc<[f32]>>,
    /// Increases whenever `nodes` changes.
    points_generation: u64,
    /// Height range for colouring by height, and what it was taken from.
    height_range: Option<[f64; 2]>,
    height_for: Option<(Uuid, Vec<Uuid>)>,
    view_ms: f64,
    /// Of the viewport, as last drawn.
    pixels_per_point: f32,
    dirty: bool,
    last_request: Instant,
    last_camera: Camera,
    last_preview: Option<(Uuid, geemil_core::Pose)>,
    last_motion: Instant,
    refine_pending: bool,
    flight: Option<navigation::Flight>,

    selection: selection::SelectionState,
    measure: measure::Measure,
    align: align::Align,
    crop: crop::Crop,
    gizmo: gizmo::Gizmo,
    transform_edit: tree::TransformEdit,
    undo: undo::UndoStack,
    dialog: Option<dialogs::Dialog>,
    title: String,

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
        let smoke = SmokeTest::new(smoke);
        // Smoke tests run with defaults and leave the user's settings alone.
        let settings = cc
            .storage
            .filter(|_| !smoke.active())
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();
        let mut app = Self {
            t,
            project: None,
            renderer: Some(PointRenderer::new(rs.device.clone(), rs.queue.clone())),
            camera: Camera::default(),
            visible: BTreeSet::new(),
            selected: None,
            reveal: None,
            tree_selection: tree::TreeSelection::default(),
            folder_summary: Default::default(),
            layer_counts: None,
            settings,
            job: None,
            status: t.status_start.into(),
            error: None,
            progress: 0.,
            cleanup_report: None,
            view: ViewLoader::spawn(cc.egui_ctx.clone()),
            nodes: vec![],
            spacings: vec![],
            points_generation: 0,
            height_range: None,
            height_for: None,
            view_ms: 0.,
            pixels_per_point: 1.,
            dirty: false,
            last_request: Instant::now(),
            last_camera: Camera::default(),
            last_preview: None,
            last_motion: Instant::now(),
            refine_pending: false,
            flight: None,
            selection: Default::default(),
            measure: Default::default(),
            align: Default::default(),
            crop: Default::default(),
            gizmo: Default::default(),
            transform_edit: Default::default(),
            undo: Default::default(),
            dialog: None,
            title: String::new(),
            smoke,
        };
        if let Some(path) = path {
            if app.smoke.active() {
                match Project::load(&path) {
                    Ok(p) => {
                        app.install(p, true);
                        app.status = t.status_opened.into();
                    }
                    Err(e) => app.error = Some(Notice::new(t, &e)),
                }
            } else {
                app.open_project(&path);
            }
        }
        app.smoke_setup();
        app
    }
    /// Replaces the project state after open, create, an edit, a finished job,
    /// undo or a revision switch.
    fn install(&mut self, project: Project, fit: bool) {
        let previous: Option<Vec<_>> = self
            .project
            .as_ref()
            .filter(|p| p.root == project.root)
            .map(|p| p.scans().map(|s| s.id).collect());
        let scans: Vec<_> = project.scans().map(|s| s.id).collect();
        let items: Vec<_> = scans
            .iter()
            .copied()
            .chain(project.groups().iter().map(|g| g.id))
            .collect();
        carry_scan_state(&mut self.visible, previous.as_deref(), &scans);
        if previous.is_none() {
            self.tree_selection = tree::TreeSelection::default();
            self.selected = None;
        }
        self.tree_selection.retain(&items);
        if !self.selected.is_some_and(|id| items.contains(&id)) {
            self.selected = self.tree_selection.first();
        }
        if fit || previous.is_none() {
            frame_bounds(&mut self.camera, &project.bounds());
            self.selection.clear();
            self.measure.clear();
        }
        self.view.invalidate();
        self.project = Some(Arc::new(project));
        self.dirty = true;
    }
    fn fit_view(&mut self) {
        if let Some(p) = &self.project {
            frame_bounds(&mut self.camera, &p.bounds());
            self.flight = None;
            self.dirty = true;
            self.selection.clear();
        }
    }
    /// Starts imports for files dropped on the window.
    fn dropped_files(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_owned())
                .filter(|p| {
                    let ext = p
                        .extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase();
                    matches!(ext.as_str(), "e57" | "las" | "laz")
                })
                .collect()
        });
        if files.is_empty() || self.job.is_some() || self.dialog.is_some() {
            return;
        }
        if self.project.is_none() {
            self.status = self.t.dropped_without_project.into();
        }
        self.import(ctx, files);
    }
    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.project {
            Some(p) => format!(
                "{}{} — Geemil Workbench",
                p.manifest.name,
                if p.has_unsaved_changes() { " *" } else { "" }
            ),
            None => "Geemil Workbench".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for Workbench {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.smoke_frame(&ctx);
        self.poll_job();
        self.poll_view();
        self.align_update();
        if self.dialog.is_none() {
            self.keyboard(&ctx);
        }
        self.dropped_files(&ctx);
        self.menu_bar(ui);
        self.toolbar(ui);
        self.tool_options(ui);
        self.status_bar(ui);
        self.side_panel(ui);
        self.align_panel(ui);
        self.crop_panel(ui);
        if self.project.is_some() || self.smoke.colors {
            self.viewport(ui, frame);
        } else {
            self.welcome(ui);
        }
        self.dialogs(&ctx);
        self.request_view();
        self.update_title(&ctx);
        // Nodes left for later frames upload at the frame rate.
        if self.renderer.as_ref().is_some_and(|r| r.pending()) {
            ctx.request_repaint();
        } else if self.dirty || self.refine_pending || self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
    fn persist_egui_memory(&self) -> bool {
        !self.smoke.active()
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.smoke.active() {
            eframe::set_value(storage, SETTINGS_KEY, &self.settings);
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
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

fn frame_bounds(camera: &mut Camera, bounds: &Bounds) {
    camera.target = bounds.center().to_array();
    camera.distance = (bounds.radius() * 2.8).max(1.);
}

/// Carries scan visibility into a new project state. `previous` holds the
/// replaced state's scans when it belongs to the same project.
fn carry_scan_state(visible: &mut BTreeSet<Uuid>, previous: Option<&[Uuid]>, scans: &[Uuid]) {
    match previous {
        // Keep what the user hid, and show scans that just appeared.
        Some(previous) => {
            visible.retain(|id| scans.contains(id));
            visible.extend(scans.iter().filter(|id| !previous.contains(id)));
        }
        None => *visible = scans.iter().copied().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::carry_scan_state;
    use std::collections::BTreeSet;
    use uuid::Uuid;

    #[test]
    fn import_keeps_hidden_scans() {
        let [a, b, c] = [(); 3].map(|_| Uuid::new_v4());
        let mut visible = BTreeSet::from([a]);
        carry_scan_state(&mut visible, Some(&[a, b]), &[a, b, c]);
        assert_eq!(visible, BTreeSet::from([a, c]));
    }

    #[test]
    fn removed_scans_disappear_and_other_projects_show_everything() {
        let [a, b, c] = [(); 3].map(|_| Uuid::new_v4());
        let mut visible = BTreeSet::from([a, b]);
        // Switching to a revision without `b` drops it.
        carry_scan_state(&mut visible, Some(&[a, b]), &[a]);
        assert_eq!(visible, BTreeSet::from([a]));
        carry_scan_state(&mut visible, None, &[c, b]);
        assert_eq!(visible, BTreeSet::from([b, c]));
    }
}
