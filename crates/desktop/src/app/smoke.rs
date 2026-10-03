//! Automated GUI smoke tests: capture the window, optionally while orbiting,
//! and check sRGB colour probes end to end. See README "CLIと検証".
use super::Workbench;
use eframe::egui;
use geemil_core::{Camera, Sample, SelectionMode};
use glam::DVec3;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct SmokeOptions {
    /// Save a screenshot here and close the window.
    pub screenshot: Option<PathBuf>,
    /// Orbit for the first two seconds; at least one view must load while moving.
    pub orbit: bool,
    /// Replace the view with fixed colour probes and check them in the capture.
    pub colors: bool,
    /// Select the centre of the view after one second and preview this exclusion
    /// (mode, whether an inside exclusion limits its depth).
    pub select: Option<(SelectionMode, bool)>,
    /// Open this dialog for the capture: revisions, shortcuts, new-project,
    /// cleanup, save-as, subsample or noise. Also selects the first folder of the tree.
    pub dialog: Option<String>,
    /// Steps run one by one once the view loaded, each followed by a state
    /// line: exclude, undo, redo, save, folder (new folder with the first scan),
    /// remove (take out the first scan), measure (measure two picked points),
    /// preview (edit the first scan's transform without applying it),
    /// apply-transform (apply the edited transform), subsample (5 cm voxels),
    /// noise (0.1 m radius, 4 neighbours).
    pub script: Vec<String>,
}

pub(super) struct SmokeTest {
    screenshot: Option<PathBuf>,
    orbit: bool,
    pub(super) colors: bool,
    select: Option<(SelectionMode, bool)>,
    dialog: Option<String>,
    script: std::collections::VecDeque<String>,
    requested: bool,
    probes: Vec<(egui::Pos2, [u8; 4])>,
    camera: Camera,
    moving_updates: u64,
    started: Instant,
}
impl SmokeTest {
    pub(super) fn new(options: SmokeOptions) -> Self {
        Self {
            screenshot: options.screenshot,
            orbit: options.orbit,
            colors: options.colors,
            select: options.select,
            dialog: options.dialog,
            script: options.script.into(),
            requested: false,
            probes: vec![],
            camera: Camera::default(),
            moving_updates: 0,
            started: Instant::now(),
        }
    }
    /// Whether this run is a smoke test rather than an interactive session.
    pub(super) fn active(&self) -> bool {
        self.screenshot.is_some() || self.colors
    }
    pub(super) fn view_loaded(&mut self, interactive: bool, points: usize) {
        if interactive && points > 0 && self.started.elapsed() < Duration::from_secs(2) {
            self.moving_updates += 1;
        }
    }
}

impl Workbench {
    /// Call once after construction, when the project camera is final.
    pub(super) fn smoke_setup(&mut self) {
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
            self.points = colors
                .into_iter()
                .enumerate()
                .map(|(i, color)| Sample {
                    chunk: 0,
                    index: i as u32,
                    position: [(i % 4) as f64 * 0.6 - 0.9, 0., 0.6 - (i / 4) as f64 * 0.6],
                    color,
                })
                .collect();
            self.points_segments.clear();
            self.points_origin = [0.; 3];
            self.points_generation += 1;
            self.settings.point_size = 8.;
            self.dirty = false;
        }
        self.smoke.camera = self.camera;
        if let Some(name) = self.smoke.dialog.take() {
            use super::dialogs::Dialog;
            self.selected = self
                .project
                .as_ref()
                .and_then(|p| p.groups().first().map(|g| g.id));
            self.dialog = match name.as_str() {
                "revisions" => Some(Dialog::revisions(self.project.as_deref())),
                "shortcuts" => Some(Dialog::Shortcuts),
                "new-project" => Some(Dialog::new_project(&self.settings, vec![])),
                "cleanup" => Some(Dialog::Cleanup),
                "subsample" => Some(Dialog::Filter(super::dialogs::Filter::Subsample {
                    size: self.settings.subsample_size,
                })),
                "noise" => Some(Dialog::Filter(super::dialogs::Filter::Noise {
                    radius: self.settings.noise_radius,
                    min_neighbours: self.settings.noise_neighbours,
                })),
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
        if smoke.orbit && smoke.started.elapsed() < Duration::from_secs(2) {
            let t = smoke.started.elapsed().as_secs_f64();
            self.camera.yaw = smoke.camera.yaw + t * 0.3;
            self.camera.target[0] = smoke.camera.target[0] + t * smoke.camera.distance * 0.01;
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
            && !self.points.is_empty()
            && self.smoke.started.elapsed() > Duration::from_millis(1500)
            && let Some(step) = self.smoke.script.pop_front()
        {
            self.smoke_step(ctx, &step);
            ctx.request_repaint();
        }
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
                    smoke.moving_updates,
                    self.points.len(),
                    self.view_ms
                );
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
        // Without a project the start screen is the subject.
        if !smoke.requested
            && smoke.script.is_empty()
            && self.job.is_none()
            && (!self.points.is_empty() || self.project.is_none())
            && smoke.started.elapsed() > Duration::from_secs(3)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            smoke.requested = true;
        }
        if smoke.started.elapsed() > Duration::from_secs(20) {
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
        self.smoke.probes = self
            .points
            .iter()
            .filter_map(|sample| {
                self.camera
                    .project(DVec3::from(sample.position))
                    .map(|(uv, _)| {
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
    }
    fn smoke_step(&mut self, ctx: &egui::Context, step: &str) {
        use super::actions::Action;
        match step {
            "exclude" => self.perform(ctx, Action::Exclude),
            "undo" => self.perform(ctx, Action::Undo),
            "redo" => self.perform(ctx, Action::Redo),
            "save" => self.perform(ctx, Action::Save),
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
                let mut points = self.points.iter().step_by((self.points.len() / 2).max(1));
                if let (Some(a), Some(b)) = (points.next(), points.next()) {
                    self.measure_points(a.position.into(), b.position.into());
                }
            }
            "preview" => {
                let first = self
                    .project
                    .as_ref()
                    .and_then(|p| Some((p.scans().next()?.id, p.bounds().radius())));
                if let Some((id, radius)) = first {
                    self.selected = Some(id);
                    self.edit_transform_inputs(id, [radius * 0.4, 0., 0.], [0., 0., 30.]);
                }
                let moved = self.draw_segments().unwrap_or_default();
                let moved = moved
                    .iter()
                    .filter(|s| !s.motion.abs_diff_eq(glam::DMat4::IDENTITY, 1e-9))
                    .map(|s| s.range.len())
                    .sum::<usize>();
                eprintln!(
                    "Smoke preview: {:?}, {moved} of {} points moved",
                    self.transform_preview().map(|(_, pose)| pose.translation),
                    self.points.len()
                );
            }
            "subsample" => self.run_filter(ctx, super::dialogs::Filter::Subsample { size: 0.05 }),
            "noise" => self.run_filter(
                ctx,
                super::dialogs::Filter::Noise {
                    radius: 0.1,
                    min_neighbours: 4,
                },
            ),
            "apply-transform" => {
                if let Some((id, pose)) = self.transform_preview() {
                    self.apply_edit(|p| p.set_transform(id, pose));
                }
            }
            other => eprintln!("Unknown smoke step {other}"),
        }
        if let Some(p) = &self.project {
            eprintln!(
                "Smoke step {step}: scans {}, folders {}, layers {}, revisions {}, unsaved {}, undo {}, redo {}, job {}, measure {:?}",
                p.scans().count(),
                p.groups().len(),
                p.current().layers.len(),
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
