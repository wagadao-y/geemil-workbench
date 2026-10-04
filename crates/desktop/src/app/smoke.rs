//! Automated GUI smoke tests: capture the window, optionally while orbiting,
//! and check sRGB colour probes end to end. See README "CLIと検証".
use super::Workbench;
use eframe::egui;
use geemil_core::{Camera, Sample, SelectionMode};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct SmokeOptions {
    /// Save a screenshot here and close the window.
    pub screenshot: Option<PathBuf>,
    /// The point budget instead of the default.
    pub budget: Option<usize>,
    /// Orbit for the first two seconds; at least one view must load while moving.
    pub orbit: bool,
    /// Longer orbit runs, with UI callback interval percentiles in the log.
    pub orbit_seconds: Option<f64>,
    /// Replace the view with fixed colour probes and check them in the capture.
    pub colors: bool,
    /// Select the centre of the view after one second and preview this move
    /// (mode, whether an inside selection limits its depth).
    pub select: Option<(SelectionMode, bool)>,
    /// Open this dialog for the capture: revisions, shortcuts, new-project,
    /// cleanup, save-as, subsample, noise, export or export-las. Also selects the first folder of the tree.
    pub dialog: Option<String>,
    /// Steps run one by one once the view loaded, each followed by a state
    /// line: exclude (move the selection to the "deleted" layer), undo, redo,
    /// save, folder (new folder with the first scan),
    /// remove (take out the first scan), measure (measure two picked points),
    /// preview (edit the first scan's transform without applying it),
    /// apply-transform (apply the edited transform), subsample (5 cm voxels),
    /// subsample-merged (5 cm voxels over all visible scans),
    /// noise (0.1 m radius, 4 neighbours), sor (6 neighbours, 1 sigma), align-icp (ICP of the first scan
    /// against the others, previewed), align-pairs (fit four coinciding pairs),
    /// align-apply (apply the previewed result), ortho (parallel projection,
    /// top view), box (a 2 m slice at the median height, inside highlighted),
    /// box-crop (move everything outside that box to the "deleted" layer),
    /// transform and transform-folder (the move and rotate tool on the first
    /// scan or folder), focus and focus-folder (frame the first scan or folder),
    /// show-layers (show every layer), restore (move every point of the newest
    /// layer back to the default one), color-height and color-scan (colour modes).
    pub script: Vec<String>,
}

pub(super) struct SmokeTest {
    screenshot: Option<PathBuf>,
    orbit: bool,
    orbit_duration: Duration,
    last_frame: Option<Instant>,
    frame_intervals_ms: Vec<f64>,
    budget: Option<usize>,
    pub(super) colors: bool,
    select: Option<(SelectionMode, bool)>,
    dialog: Option<String>,
    script: std::collections::VecDeque<String>,
    requested: bool,
    probes: Vec<(egui::Pos2, [u8; 4])>,
    camera: Camera,
    moving_updates: u64,
    started: Instant,
    view_started: Option<Instant>,
    cpu_finished: Option<Duration>,
}
impl SmokeTest {
    pub(super) fn new(options: SmokeOptions) -> Self {
        Self {
            screenshot: options.screenshot,
            orbit: options.orbit,
            orbit_duration: Duration::from_secs_f64(
                options.orbit_seconds.unwrap_or(2.).clamp(2., 3600.),
            ),
            last_frame: None,
            frame_intervals_ms: vec![],
            budget: options.budget,
            colors: options.colors,
            select: options.select,
            dialog: options.dialog,
            script: options.script.into(),
            requested: false,
            probes: vec![],
            camera: Camera::default(),
            moving_updates: 0,
            started: Instant::now(),
            view_started: None,
            cpu_finished: None,
        }
    }
    /// Whether this run is a smoke test rather than an interactive session.
    pub(super) fn active(&self) -> bool {
        self.screenshot.is_some() || self.colors
    }
    pub(super) fn view_requested(&mut self) {
        if self.active() {
            self.view_started = Some(Instant::now());
            self.cpu_finished = None;
        }
    }
    pub(super) fn view_loaded(&mut self, interactive: bool, points: usize, partial: bool) {
        if !partial {
            self.cpu_finished = self.view_started.map(|start| start.elapsed());
        }
        if interactive && points > 0 && self.started.elapsed() < self.orbit_duration {
            self.moving_updates += 1;
        }
    }
    /// Submission timing, not GPU fence or display presentation timing.
    pub(super) fn view_rendered(&mut self, pending: bool, points: usize) {
        if !pending && let (Some(start), Some(cpu)) = (self.view_started, self.cpu_finished) {
            eprintln!(
                "Smoke view ready: points {points}, CPU {:.1} ms, all uploads submitted {:.1} ms",
                cpu.as_secs_f64() * 1000.,
                start.elapsed().as_secs_f64() * 1000.
            );
            self.view_started = None;
            self.cpu_finished = None;
        }
    }
}

impl Workbench {
    /// Call once after construction, when the project camera is final.
    pub(super) fn smoke_setup(&mut self) {
        if let Some(budget) = self.smoke.budget {
            self.settings.point_budget = budget;
        }
        if self.smoke.colors {
            // End-to-end display fixture, including point shader, sRGB attachment,
            // egui composition and the native window's captured output.
            let colors = [
                [0, 0, 0, 255],
                [1, 1, 1, 255],
                [8, 8, 8, 255],
                [10, 10, 10, 255],
                [11, 11, 11, 255],
                [32, 32, 32, 255],
                [64, 64, 64, 255],
                [128, 128, 128, 255],
                [192, 192, 192, 255],
                [255, 255, 255, 255],
                [128, 64, 32, 255],
                [23, 160, 240, 255],
            ];
            self.camera = Camera {
                yaw: -std::f64::consts::FRAC_PI_2,
                pitch: 0.,
                distance: 5.,
                ..Camera::default()
            };
            let samples: Vec<_> = colors
                .into_iter()
                .enumerate()
                .map(|(i, color)| Sample {
                    chunk: 0,
                    index: i as u32,
                    position: [(i % 4) as f64 * 0.6 - 0.9, 0., 0.6 - (i / 4) as f64 * 0.6],
                    color,
                })
                .collect();
            self.nodes = vec![geemil_core::LoadedNode {
                scan: uuid::Uuid::nil(),
                node: 0,
                samples: samples.into(),
            }];
            self.points_generation += 1;
            self.settings.point_size = 8.;
            self.dirty = false;
        }
        self.smoke.camera = self.camera;
        if let Some(name) = self.smoke.dialog.take() {
            use super::dialogs::Dialog;
            let selected = self
                .project
                .as_ref()
                .and_then(|p| p.groups().first().map(|g| g.id));
            self.select_tree_item(selected);
            self.dialog = match name.as_str() {
                "revisions" => Some(Dialog::revisions(self.project.as_deref())),
                "properties" => selected.map(|id| Dialog::Properties { id }),
                "shortcuts" => Some(Dialog::Shortcuts),
                "new-project" => Some(Dialog::new_project(&self.settings, vec![])),
                "cleanup" => Some(Dialog::Cleanup),
                "export-las" => Some(Dialog::Export {
                    format: super::dialogs::ExportFormat::Laz,
                    per_scan: false,
                    policy: geemil_core::LasExportPolicy::Preserve,
                    compatibility: None,
                }),
                "export" | "export-e57" => Some(Dialog::Export {
                    format: super::dialogs::ExportFormat::E57,
                    per_scan: false,
                    policy: geemil_core::LasExportPolicy::Preserve,
                    compatibility: None,
                }),
                "subsample" => Some(Dialog::Filter(
                    super::dialogs::Filter::Subsample {
                        size: self.settings.subsample_size,
                        merged: self.settings.subsample_merged,
                    },
                    None,
                )),
                "noise" => Some(Dialog::Filter(
                    super::dialogs::Filter::Noise {
                        radius: self.settings.noise_radius,
                        min_neighbours: self.settings.noise_neighbours,
                    },
                    None,
                )),
                "save-as" => Some(Dialog::SaveAs {
                    name: "リビジョン 2".into(),
                }),
                _ => None,
            };
        }
    }
    /// Drives the camera and handles the screenshot at the start of a frame.
    pub(super) fn smoke_frame(&mut self, ctx: &egui::Context) {
        let Some(path) = self.smoke.screenshot.clone() else {
            return;
        };
        let path = &path;
        let smoke = &mut self.smoke;
        if smoke.orbit && smoke.started.elapsed() < smoke.orbit_duration {
            let now = Instant::now();
            if let Some(last) = smoke.last_frame.replace(now)
                && smoke.started.elapsed() > Duration::from_secs(1)
            {
                smoke
                    .frame_intervals_ms
                    .push(now.duration_since(last).as_secs_f64() * 1000.);
            }
            let t = smoke.started.elapsed().as_secs_f64();
            self.camera.yaw = smoke.camera.yaw + t * 0.3;
            self.camera.target[0] =
                smoke.camera.target[0] + (t * 0.7).sin() * smoke.camera.distance * 0.02;
            self.dirty = true;
            ctx.request_repaint();
        }
        // After a second the viewport aspect is final, which the selection keeps.
        if let Some((mode, limit_depth)) = smoke
            .select
            .take_if(|_| smoke.started.elapsed() > Duration::from_secs(1))
        {
            self.selection
                .select_rect(self.camera, [0.35, 0.3], [0.65, 0.7], mode, limit_depth);
        }
        if self.job.is_none()
            && !self.nodes.is_empty()
            && self.smoke.started.elapsed() > Duration::from_millis(1500)
            && let Some(step) = self.smoke.script.pop_front()
        {
            self.smoke_step(ctx, &step);
            ctx.request_repaint();
        }
        let shown = self.shown_count();
        let smoke = &mut self.smoke;
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = event {
                if smoke.colors {
                    assert_eq!(smoke.probes.len(), 12, "Missing color probes");
                    let pixels = ctx.pixels_per_point();
                    for (pos, expected) in &smoke.probes {
                        let x = (pos.x * pixels).round() as usize;
                        let y = (pos.y * pixels).round() as usize;
                        assert!(
                            x < image.size[0] && y < image.size[1],
                            "Color probe outside screenshot"
                        );
                        let actual = image.pixels[y * image.size[0] + x].to_array();
                        assert!(
                            actual
                                .iter()
                                .zip(expected)
                                .all(|(a, b)| a.abs_diff(*b) <= 2),
                            "sRGB display changed input {expected:?} to {actual:?}"
                        );
                        eprintln!("sRGB probe: {expected:?} -> {actual:?}");
                    }
                }
                assert!(
                    !smoke.orbit || smoke.moving_updates > 0,
                    "No view update completed while the camera was moving"
                );
                eprintln!(
                    "Smoke test: {} updates during motion, {} final points, {:.1} ms view load",
                    smoke.moving_updates, shown, self.view_ms
                );
                if !smoke.frame_intervals_ms.is_empty() {
                    smoke.frame_intervals_ms.sort_by(f64::total_cmp);
                    let frames = &smoke.frame_intervals_ms;
                    let percentile = |p: f64| {
                        frames[((frames.len() as f64 * p).ceil() as usize)
                            .saturating_sub(1)
                            .min(frames.len() - 1)]
                    };
                    eprintln!(
                        "Smoke UI frame intervals: samples {}, p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms (UI callbacks; excludes GPU fence/presentation measurement)",
                        frames.len(),
                        percentile(0.5),
                        percentile(0.95),
                        percentile(0.99)
                    );
                }
                if let Some(error) = &self.error {
                    eprintln!("Smoke test error: {} {:?}", error.message, error.detail);
                }
                if let Some(preview) = self.selection.preview_summary() {
                    eprintln!("Smoke test preview: {preview}");
                }
                let bytes: Vec<_> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                if let Err(e) = image::save_buffer(
                    path,
                    &bytes,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                ) {
                    eprintln!("Screenshot failed: {e}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        // Without a project the start screen is the subject, without scans the
        // empty-project hint.
        let empty = self
            .project
            .as_ref()
            .is_none_or(|p| p.scans().next().is_none());
        if !smoke.requested
            && smoke.script.is_empty()
            && self.job.is_none()
            && (!self.nodes.is_empty() || empty)
            && !self.renderer.as_ref().is_some_and(|r| r.pending())
            && smoke.started.elapsed() > smoke.orbit_duration + Duration::from_secs(1)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            smoke.requested = true;
        }
        if smoke.started.elapsed() > smoke.orbit_duration + Duration::from_secs(120) {
            eprintln!("Smoke test timed out: {:?}", self.error);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    /// Records where each fixture point lands in the viewport.
    pub(super) fn smoke_probes(&mut self, rect: egui::Rect) {
        if !self.smoke.colors {
            return;
        }
        let probes = self
            .shown_points(true)
            .filter_map(|(_, sample, world)| {
                self.camera.project(world).map(|(uv, _)| {
                    (
                        egui::pos2(
                            rect.left() + uv[0] as f32 * rect.width(),
                            rect.top() + uv[1] as f32 * rect.height(),
                        ),
                        sample.color,
                    )
                })
            })
            .collect();
        self.smoke.probes = probes;
    }
    fn smoke_step(&mut self, ctx: &egui::Context, step: &str) {
        use super::actions::Action;
        match step {
            "exclude" => self.perform(ctx, Action::Exclude),
            "undo" => self.perform(ctx, Action::Undo),
            "redo" => self.perform(ctx, Action::Redo),
            "save" => {
                if self.enabled(&Action::Save) {
                    let n = self.project.as_ref().unwrap().manifest.revisions.len();
                    self.save_revision((self.t.default_revision_name)(n));
                }
            }
            "folder" => {
                let first = self
                    .project
                    .as_ref()
                    .and_then(|p| p.scans().next().map(|s| s.id));
                if let Some(group) = self.apply_edit(|p| p.create_group("Smoke".into(), None))
                    && let Some(scan) = first
                {
                    self.apply_edit(|p| p.move_to_group(&[scan], Some(group)));
                }
            }
            "remove" => {
                let first = self
                    .project
                    .as_ref()
                    .and_then(|p| p.scans().next().map(|s| s.id));
                if let Some(scan) = first {
                    self.apply_edit(|p| p.remove_scans(&[scan]));
                }
            }
            "measure" => {
                self.selection.tool = super::selection::Tool::Measure;
                let count = self.shown_count();
                let points: Vec<_> = self
                    .shown_points(true)
                    .map(|(.., p)| p)
                    .step_by((count / 2).max(1))
                    .take(2)
                    .collect();
                if let [a, b] = points[..] {
                    self.measure_points(a, b);
                }
            }
            "preview" => {
                let first = self
                    .project
                    .as_ref()
                    .and_then(|p| Some((p.scans().next()?.id, p.bounds().radius())));
                if let Some((id, radius)) = first {
                    self.select_tree_item(Some(id));
                    self.edit_transform_inputs(id, [radius * 0.4, 0., 0.], [0., 0., 30.]);
                }
                let (applied, drawn) = (self.scan_worlds(false), self.scan_worlds(true));
                let moved = self
                    .nodes
                    .iter()
                    .filter(|n| {
                        let (a, d) = (applied.get(&n.scan), drawn.get(&n.scan));
                        a.zip(d).is_some_and(|(a, d)| !a.abs_diff_eq(*d, 1e-9))
                    })
                    .map(|n| n.samples.len())
                    .sum::<usize>();
                eprintln!(
                    "Smoke preview: {:?}, {moved} of {} points moved",
                    self.transform_preview().map(|(_, pose)| pose.translation),
                    self.shown_count()
                );
            }
            "subsample" => self.run_filter(
                ctx,
                super::dialogs::Filter::Subsample {
                    size: 0.05,
                    merged: false,
                },
                None,
            ),
            "subsample-merged" => self.run_filter(
                ctx,
                super::dialogs::Filter::Subsample {
                    size: 0.05,
                    merged: true,
                },
                None,
            ),
            "noise" => self.run_filter(
                ctx,
                super::dialogs::Filter::Noise {
                    radius: 0.1,
                    min_neighbours: 4,
                },
                None,
            ),
            "ortho" => {
                self.perform(ctx, Action::ToggleOrtho);
                self.perform(ctx, Action::View(super::actions::ViewPreset::Top));
            }
            "focus" | "focus-folder" => {
                let selected = self.project.as_ref().and_then(|p| {
                    if step == "focus" {
                        p.scans().next().map(|s| s.id)
                    } else {
                        p.groups().first().map(|g| g.id)
                    }
                });
                self.select_tree_item(selected);
                if let Some(id) = selected {
                    self.focus_tree_item(id);
                }
            }
            "color-height" => self.settings.color_mode = super::ColorMode::Height,
            "color-scan" => self.settings.color_mode = super::ColorMode::Scan,
            "box" | "box-resize" => {
                self.smoke_box(step == "box-resize");
                if let Some(region) = self.crop.region {
                    let inside = self
                        .shown_points(false)
                        .filter(|(.., p)| region.contains(*p))
                        .count();
                    eprintln!("Smoke box: {region:?}, {inside} displayed points inside");
                }
            }
            "box-crop" => self.exclude_box(ctx, false),
            "align-icp" => self.smoke_icp(ctx),
            "align-pairs" => {
                self.smoke_pairs();
                eprintln!("Smoke align: {}", self.align_summary());
            }
            "align-apply" => {
                eprintln!("Smoke align: {}", self.align_summary());
                self.align_apply();
            }
            "sor" => self.run_filter(
                ctx,
                super::dialogs::Filter::Statistical {
                    neighbours: 6,
                    deviations: 1.,
                    reach: 0.5,
                },
                None,
            ),
            "restore" => {
                // Every point of the newest layer back to the default one.
                let newest = self
                    .project
                    .as_ref()
                    .and_then(|p| p.current().layers.iter().map(|l| l.code).max())
                    .filter(|code| *code != geemil_core::DEFAULT_LAYER);
                if let Some(from) = newest {
                    let to = geemil_core::DEFAULT_LAYER;
                    self.layer_action(ctx, super::layers::LayerAction::MoveAll { from, to });
                }
            }
            "transform" | "transform-folder" => {
                // The move and rotate tool on the first scan, or the first folder.
                self.selection.tool = super::selection::Tool::Transform;
                let selected = self.project.as_ref().and_then(|p| {
                    if step == "transform" {
                        p.scans().next().map(|s| s.id)
                    } else {
                        p.groups().first().map(|g| g.id)
                    }
                });
                self.select_tree_item(selected);
            }
            "show-layers" => {
                let codes: Vec<_> = self
                    .project
                    .as_ref()
                    .map(|p| p.current().layers.iter().map(|l| l.code).collect())
                    .unwrap_or_default();
                for code in codes {
                    self.apply_edit(|p| p.set_layer_visible(code, true));
                }
            }
            "apply-transform" => {
                if let Some((id, pose)) = self.transform_preview() {
                    self.apply_edit(|p| p.set_transform(id, pose));
                }
            }
            other => eprintln!("Unknown smoke step {other}"),
        }
        if let Some(p) = &self.project {
            eprintln!(
                "Smoke step {step}: scans {}, folders {}, layers {:?}, revisions {}, unsaved {}, undo {}, redo {}, job {}, measure {:?}",
                p.scans().count(),
                p.groups().len(),
                p.layer_counts(),
                p.manifest.revisions.len(),
                p.has_unsaved_changes(),
                self.undo_available(),
                self.redo_available(),
                self.job.is_some(),
                self.measure_distance(),
            );
        }
    }
}
