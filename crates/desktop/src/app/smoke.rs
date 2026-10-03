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
    /// Select the centre of the view after one second and preview this exclusion.
    pub select: Option<SelectionMode>,
}

pub(super) struct SmokeTest {
    screenshot: Option<PathBuf>,
    orbit: bool,
    pub(super) colors: bool,
    select: Option<SelectionMode>,
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
            requested: false,
            probes: vec![],
            camera: Camera::default(),
            moving_updates: 0,
            started: Instant::now(),
        }
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
            self.points_origin = [0.; 3];
            self.points_generation += 1;
            self.point_size = 8.;
            self.dirty = false;
        }
        self.smoke.camera = self.camera;
    }
    /// Drives the camera and handles the screenshot at the start of a frame.
    pub(super) fn smoke_frame(&mut self, ctx: &egui::Context) {
        let smoke = &mut self.smoke;
        let Some(path) = &smoke.screenshot else {
            return;
        };
        if smoke.orbit && smoke.started.elapsed() < Duration::from_secs(2) {
            let t = smoke.started.elapsed().as_secs_f64();
            self.camera.yaw = smoke.camera.yaw + t * 0.3;
            self.camera.target[0] = smoke.camera.target[0] + t * smoke.camera.distance * 0.01;
            self.dirty = true;
            ctx.request_repaint();
        }
        // After a second the viewport aspect is final, which the selection keeps.
        if let Some(mode) = smoke
            .select
            .take_if(|_| smoke.started.elapsed() > Duration::from_secs(1))
        {
            self.selection
                .select_rect(self.camera, [0.35, 0.3], [0.65, 0.7], mode);
        }
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
        if !smoke.requested
            && !self.points.is_empty()
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
}
