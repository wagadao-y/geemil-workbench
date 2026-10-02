use crate::render::PointRenderer;
use eframe::egui;
use geemil_core::{Camera, ImportOptions, JobControl, Pose, Project, Sample, Selection, ViewCache};
use glam::{DQuat, DVec3};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

enum JobEvent {
    Progress(String, u64, u64),
    Complete(Result<Project, String>),
}
struct ActiveJob {
    rx: mpsc::Receiver<JobEvent>,
    cancel: Arc<AtomicBool>,
}
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
    result: Result<Vec<Sample>, String>,
    origin: [f64; 3],
    elapsed_ms: f64,
    epoch: u64,
    interactive: bool,
}

pub struct Workbench {
    project: Option<Arc<Project>>,
    renderer: PointRenderer,
    camera: Camera,
    visible: BTreeSet<Uuid>,
    selected: Option<Uuid>,
    job: Option<ActiveJob>,
    status: String,
    error: Option<String>,
    progress: f32,
    view_tx: mpsc::SyncSender<ViewRequest>,
    view_rx: mpsc::Receiver<ViewResult>,
    generation: Arc<AtomicU64>,
    view_epoch: Arc<AtomicU64>,
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
    select_mode: bool,
    lasso: bool,
    polygon: Vec<egui::Pos2>,
    drag_start: Option<egui::Pos2>,
    selection_camera: Option<Camera>,
    depth: f64,
    translation: [f64; 3],
    rotation: [f64; 3],
    branch_name: String,
    screenshot: Option<PathBuf>,
    screenshot_requested: bool,
    smoke_orbit: bool,
    smoke_colors: bool,
    color_probes: Vec<(egui::Pos2, [u8; 4])>,
    smoke_camera: Camera,
    moving_updates: u64,
    started: Instant,
}
impl Workbench {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: Option<PathBuf>,
        screenshot: Option<PathBuf>,
        smoke_orbit: bool,
        smoke_colors: bool,
    ) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
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
        cc.egui_ctx.set_fonts(fonts);
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("wgpu renderer selected");
        let renderer = PointRenderer::new(rs.device.clone(), rs.queue.clone());
        let (view_tx, requests) = mpsc::sync_channel::<ViewRequest>(1);
        let (results, view_rx) = mpsc::sync_channel(2);
        let generation = Arc::new(AtomicU64::new(0));
        let view_epoch = Arc::new(AtomicU64::new(0));
        let live_epoch = view_epoch.clone();
        let current = generation.clone();
        let ctx = cc.egui_ctx.clone();
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
                let result = request
                    .project
                    .load_view_cached(
                        &request.camera,
                        request.budget,
                        &request.visible,
                        &job,
                        &mut cache,
                    )
                    .map_err(|e| format!("{e:#}"));
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
        let mut app = Self {
            project: None,
            renderer,
            camera: Camera::default(),
            visible: BTreeSet::new(),
            selected: None,
            job: None,
            status: "プロジェクトを作成するか、既存のプロジェクトを開いてください。".into(),
            error: None,
            progress: 0.,
            view_tx,
            view_rx,
            generation,
            view_epoch,
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
            branch_name: "新しい分岐".into(),
            screenshot,
            screenshot_requested: false,
            smoke_orbit,
            smoke_colors,
            color_probes: vec![],
            smoke_camera: Camera::default(),
            moving_updates: 0,
            started: Instant::now(),
        };
        if let Some(path) = path {
            match Project::load(&path) {
                Ok(p) => app.install(p, true),
                Err(e) => app.error = Some(format!("{e:#}")),
            }
        }
        if smoke_colors {
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
            app.camera = Camera {
                yaw: -std::f64::consts::FRAC_PI_2,
                pitch: 0.,
                distance: 5.,
                ..Camera::default()
            };
            app.points = colors
                .into_iter()
                .enumerate()
                .map(|(i, color)| Sample {
                    chunk: 0,
                    index: i as u32,
                    position: [(i % 4) as f64 * 0.6 - 0.9, 0., 0.6 - (i / 4) as f64 * 0.6],
                    color,
                })
                .collect();
            app.points_origin = [0.; 3];
            app.points_generation += 1;
            app.point_size = 8.;
            app.dirty = false;
        }
        app.smoke_camera = app.camera;
        app
    }
    fn install(&mut self, project: Project, fit: bool) {
        let ids: BTreeSet<_> = project.scans().map(|s| s.id).collect();
        self.visible.retain(|id| ids.contains(id));
        if self.project.is_none() || fit {
            self.visible = ids;
        }
        self.selected = project.scans().next().map(|s| s.id);
        if fit {
            let bounds = project.bounds();
            self.camera.target = bounds.center().to_array();
            self.camera.distance = (bounds.radius() * 2.8).max(1.);
        }
        self.points_generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.view_epoch.fetch_add(1, Ordering::Relaxed);
        self.points.clear();
        self.points_origin = self.camera.target;
        self.project = Some(Arc::new(project));
        self.status = "プロジェクトは保存済みです。".into();
        self.polygon.clear();
        self.selection_camera = None;
        self.dirty = true;
        self.sync_pose();
    }
    fn sync_pose(&mut self) {
        if let (Some(p), Some(id)) = (&self.project, self.selected) {
            let pose = p.current().transforms.get(&id).copied().unwrap_or_default();
            self.translation = pose.translation;
            let (x, y, z) = DQuat::from_array(pose.rotation_xyzw).to_euler(glam::EulerRot::XYZ);
            self.rotation = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
        }
    }
    fn start(
        &mut self,
        ctx: &egui::Context,
        task: impl FnOnce(JobControl) -> anyhow::Result<Project> + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        let (tx, rx) = mpsc::sync_channel(16);
        let progress_tx = tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let repaint = ctx.clone();
        let progress_ctx = repaint.clone();
        let job = JobControl {
            cancel: cancel.clone(),
            progress: Arc::new(move |stage, done, total| {
                let _ = progress_tx.try_send(JobEvent::Progress(stage.into(), done, total));
                progress_ctx.request_repaint();
            }),
        };
        self.job = Some(ActiveJob { rx, cancel });
        self.view_epoch.fetch_add(1, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.progress = 0.;
        self.error = None;
        self.status = "処理中…".into();
        std::thread::spawn(move || {
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||task(job))).map_err(|_|"処理スレッドで予期しないエラーが発生しました。直前の確定状態は保持されています。".to_owned()).and_then(|r|r.map_err(|e|format!("{e:#}")));
            let _ = tx.send(JobEvent::Complete(result));
            repaint.request_repaint();
        });
    }
    fn poll(&mut self) {
        let mut done = None;
        if let Some(job) = &self.job {
            while let Ok(event) = job.rx.try_recv() {
                match event {
                    JobEvent::Progress(stage, done, total) => {
                        self.status = format!("{stage}: {done} / {total}");
                        self.progress = if total > 0 {
                            done as f32 / total as f32
                        } else {
                            0.
                        };
                    }
                    JobEvent::Complete(result) => done = Some(result),
                }
            }
        }
        if let Some(result) = done {
            self.job = None;
            match result {
                Ok(project) => {
                    self.error = None;
                    let fit = self
                        .project
                        .as_ref()
                        .is_none_or(|p| p.current().scans != project.current().scans);
                    self.install(project, fit);
                    self.status = "完了しました。プロジェクトは保存済みです。".into();
                }
                Err(e) => {
                    // A batch may have committed earlier files before cancellation/error.
                    if let Some(project) = &self.project
                        && let Ok(latest) = Project::load(&project.root)
                    {
                        let fit = project.current().scans != latest.current().scans;
                        self.install(latest, fit);
                    }
                    self.status = "処理を終了しました。".into();
                    self.error = Some(e);
                }
            }
        }
        while let Ok(result) = self.view_rx.try_recv() {
            if result.epoch == self.view_epoch.load(Ordering::Relaxed)
                && result.generation > self.points_generation
            {
                match result.result {
                    Ok(points) => {
                        if result.interactive
                            && !points.is_empty()
                            && self.started.elapsed() < Duration::from_secs(2)
                        {
                            self.moving_updates += 1;
                        }
                        self.points = points;
                        self.points_generation = result.generation;
                        self.points_origin = result.origin;
                        self.view_ms = result.elapsed_ms;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }
    fn request_view(&mut self) {
        if self.job.is_some() || self.smoke_colors {
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
            let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
            if self
                .view_tx
                .try_send(ViewRequest {
                    project: project.clone(),
                    camera: self.camera,
                    budget: if moving {
                        self.point_budget.min(32_000)
                    } else {
                        self.point_budget
                    },
                    visible: self.visible.iter().copied().collect(),
                    generation,
                    epoch: self.view_epoch.load(Ordering::Relaxed),
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
    fn delete(&mut self, ctx: &egui::Context) {
        if self.polygon.len() < 3 || self.job.is_some() {
            return;
        }
        if let Some(p) = &self.project {
            let mut project = (**p).clone();
            let selection = Selection {
                camera: self.selection_camera.unwrap_or(self.camera),
                polygon: self
                    .polygon
                    .iter()
                    .map(|p| [p.x as f64, p.y as f64])
                    .collect(),
                depth_meters: self.depth,
            };
            let ids = self.visible.iter().copied().collect::<Vec<_>>();
            self.start(ctx, move |job| {
                project.delete_selection(&selection, &ids, &job)?;
                Ok(project)
            });
        }
    }
    fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Geemil Workbench");
                ui.separator();
                let busy = self.job.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new("新規プロジェクト"))
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("新しいプロジェクトの保存先（新規フォルダー名）")
                        .set_file_name("point-project")
                        .save_file()
                {
                    match Project::create(
                        &path,
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                    ) {
                        Ok(p) => self.install(p, true),
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
                if ui.add_enabled(!busy, egui::Button::new("開く")).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("プロジェクトフォルダーを開く")
                        .pick_folder()
                {
                    match Project::load(&path) {
                        Ok(p) => self.install(p, true),
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
                if ui
                    .add_enabled(
                        !busy && self.project.is_some(),
                        egui::Button::new("点群を取り込む"),
                    )
                    .clicked()
                    && let Some(files) = rfd::FileDialog::new()
                        .add_filter("点群", &["e57", "las", "laz"])
                        .pick_files()
                {
                    let mut project = (**self.project.as_ref().unwrap()).clone();
                    self.start(ctx, move |job| {
                        for file in files {
                            project.import_file(&file, ImportOptions::default(), &job)?;
                        }
                        Ok(project)
                    });
                }
                if ui
                    .add_enabled(
                        !busy
                            && self
                                .project
                                .as_ref()
                                .is_some_and(|p| p.scans().next().is_some()),
                        egui::Button::new("E57書き出し"),
                    )
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("E57", &["e57"])
                        .set_file_name("export.e57")
                        .save_file()
                {
                    let project = (**self.project.as_ref().unwrap()).clone();
                    self.start(ctx, move |job| {
                        project.export_e57(&path, &job)?;
                        Ok(project)
                    });
                }
                if ui
                    .add_enabled(
                        !busy && self.project.is_some(),
                        egui::Button::new("内部データを圧縮"),
                    )
                    .clicked()
                {
                    let mut project = (**self.project.as_ref().unwrap()).clone();
                    self.start(ctx, move |job| {
                        project.compress_storage(&job)?;
                        Ok(project)
                    });
                }
                if ui.button("全体表示").clicked()
                    && let Some(p) = &self.project
                {
                    let b = p.bounds();
                    self.camera.target = b.center().to_array();
                    self.camera.distance = (b.radius() * 2.8).max(1.);
                    self.dirty = true;
                    self.polygon.clear();
                }
            })
        });
    }
    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("scans")
            .default_width(270.)
            .show(ctx, |ui| {
                ui.heading("スキャン");
                if let Some(p) = self.project.clone() {
                    ui.label(&p.manifest.name);
                    for scan in p.scans() {
                        ui.horizontal(|ui| {
                            let mut shown = self.visible.contains(&scan.id);
                            if ui.checkbox(&mut shown, "").changed() {
                                self.view_epoch.fetch_add(1, Ordering::Relaxed);
                                if shown {
                                    self.visible.insert(scan.id);
                                } else {
                                    self.visible.remove(&scan.id);
                                }
                                self.dirty = true;
                            }
                            if ui
                                .selectable_label(self.selected == Some(scan.id), &scan.name)
                                .clicked()
                            {
                                self.selected = Some(scan.id);
                                self.sync_pose();
                            }
                        });
                        ui.small(format!(
                            "{} 点 / {} チャンク",
                            scan.records,
                            scan.chunks.len()
                        ));
                    }
                    if let Some(id) = self.selected
                        && let Some(scan) = p.scans().find(|s| s.id == id)
                    {
                        ui.separator();
                        ui.label("対応画像");
                        for image in p.manifest.images.iter().filter(|i| i.scan_id == Some(id)) {
                            ui.small(format!(
                                "{} ({})",
                                image.name.as_deref().unwrap_or("Image"),
                                image.projection
                            ));
                        }
                        ui.collapsing("取り込み時に省略した属性", |ui| {
                            for attribute in &scan.omitted_attributes {
                                ui.small(attribute);
                            }
                        });
                    }
                    ui.separator();
                    ui.collapsing("位置合わせ（追加変換）", |ui| {
                        ui.label("平行移動（元データの座標単位）");
                        for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(*label);
                                ui.add(egui::DragValue::new(&mut self.translation[i]).speed(0.01));
                            });
                        }
                        ui.label("回転（度）");
                        for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(*label);
                                ui.add(egui::DragValue::new(&mut self.rotation[i]).speed(0.1));
                            });
                        }
                        if ui
                            .add_enabled(
                                self.job.is_none() && self.selected.is_some(),
                                egui::Button::new("変換を確定"),
                            )
                            .clicked()
                        {
                            let mut project = (*p).clone();
                            let id = self.selected.unwrap();
                            let q = DQuat::from_euler(
                                glam::EulerRot::XYZ,
                                self.rotation[0].to_radians(),
                                self.rotation[1].to_radians(),
                                self.rotation[2].to_radians(),
                            );
                            let pose = Pose {
                                translation: self.translation,
                                rotation_xyzw: q.to_array(),
                            };
                            self.start(ctx, move |_| {
                                project.set_transform(id, pose)?;
                                Ok(project)
                            });
                        }
                    });
                    ui.separator();
                    ui.heading("除外レイヤー");
                    for layer in &p.manifest.layers {
                        let mut enabled = p.current().layers.contains(&layer.id);
                        if ui
                            .add_enabled(
                                self.job.is_none(),
                                egui::Checkbox::new(&mut enabled, &layer.name),
                            )
                            .changed()
                        {
                            let mut project = (*p).clone();
                            let id = layer.id;
                            self.start(ctx, move |_| {
                                project.set_layer_enabled(id, enabled)?;
                                Ok(project)
                            });
                        }
                    }
                    ui.separator();
                    ui.heading("履歴");
                    egui::ScrollArea::vertical()
                        .max_height(230.)
                        .show(ui, |ui| {
                            for r in &p.manifest.revisions {
                                let depth = revision_depth(&p, r.id);
                                let label = format!("{}{}", "  ".repeat(depth.min(8)), r.name);
                                if ui
                                    .add_enabled(
                                        self.job.is_none(),
                                        egui::Button::new(label)
                                            .selected(r.id == p.manifest.current),
                                    )
                                    .clicked()
                                {
                                    let mut project = (*p).clone();
                                    match project.switch(r.id) {
                                        Ok(()) => {
                                            let fit = p.current().scans != project.current().scans;
                                            self.install(project, fit);
                                        }
                                        Err(e) => self.error = Some(format!("{e:#}")),
                                    }
                                }
                            }
                        });
                    ui.text_edit_singleline(&mut self.branch_name);
                    if ui
                        .add_enabled(self.job.is_none(), egui::Button::new("現在の状態から分岐"))
                        .clicked()
                    {
                        let mut project = (*p).clone();
                        let name = self.branch_name.clone();
                        self.start(ctx, move |_| {
                            project.fork(name)?;
                            Ok(project)
                        });
                    }
                    ui.small("切り替え後の編集も、自動的に別の枝になります。");
                } else {
                    ui.label("元ファイルを移動しても使える作業プロジェクトを作成します。");
                }
            });
    }
}

fn revision_depth(p: &Project, id: Uuid) -> usize {
    let mut depth = 0;
    let mut next = p
        .manifest
        .revisions
        .iter()
        .find(|r| r.id == id)
        .and_then(|r| r.parent);
    while let Some(id) = next {
        depth += 1;
        if depth > p.manifest.revisions.len() {
            break;
        }
        next = p
            .manifest
            .revisions
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.parent);
    }
    depth
}

impl eframe::App for Workbench {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.screenshot.is_some()
            && self.smoke_orbit
            && self.started.elapsed() < Duration::from_secs(2)
        {
            let t = self.started.elapsed().as_secs_f64();
            self.camera.yaw = self.smoke_camera.yaw + t * 0.3;
            self.camera.target[0] =
                self.smoke_camera.target[0] + t * self.smoke_camera.distance * 0.01;
            self.dirty = true;
            ctx.request_repaint();
        }
        if let Some(path) = &self.screenshot {
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    if self.smoke_colors {
                        assert_eq!(self.color_probes.len(), 12, "Missing color probes");
                        let pixels = ctx.pixels_per_point();
                        for (pos, expected) in &self.color_probes {
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
                        !self.smoke_orbit || self.moving_updates > 0,
                        "No view update completed while the camera was moving"
                    );
                    eprintln!(
                        "Smoke test: {} updates during motion, {} final points, {:.1} ms view load",
                        self.moving_updates,
                        self.points.len(),
                        self.view_ms
                    );
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
            if !self.screenshot_requested
                && !self.points.is_empty()
                && self.started.elapsed() > Duration::from_secs(3)
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.screenshot_requested = true;
            }
            if self.started.elapsed() > Duration::from_secs(20) {
                eprintln!("Smoke test timed out: {:?}", self.error);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.poll();
        self.toolbar(ctx);
        self.sidebar(ctx);
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if let Some(job) = &self.job {
                    ui.add(egui::ProgressBar::new(self.progress).desired_width(160.));
                    if ui.button("キャンセル").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                } else {
                    ui.label(format!(
                        "表示 {} 点 / 更新 {:.1} ms",
                        self.points.len(),
                        self.view_ms
                    ));
                }
            });
            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.select_mode, false, "カメラ操作");
                ui.selectable_value(&mut self.select_mode, true, "範囲選択");
                ui.checkbox(&mut self.lasso, "多角形");
                ui.label("奥行き");
                ui.add(egui::DragValue::new(&mut self.depth).speed(0.05).range(0.001..=1_000_000.).suffix(" m"));
                if ui.add_enabled(self.job.is_none() && self.polygon.len() >= 3, egui::Button::new("選択範囲を除外")).clicked() {
                    self.delete(ctx);
                }
                if ui.button("選択解除").clicked() {
                    self.polygon.clear();
                }
            });
            ui.horizontal(|ui| {
                ui.label("描画点数上限");
                if ui.add(egui::DragValue::new(&mut self.point_budget).speed(1000).range(2048..=2_000_000)).changed() {
                    self.view_epoch.fetch_add(1, Ordering::Relaxed);
                    self.dirty = true;
                }
                ui.label("点サイズ");
                ui.add(egui::Slider::new(&mut self.point_size, 1.0..=8.0));
            });
            ui.small("左ドラッグ: 回転 / 矩形選択    右・中ドラッグ: 平行移動    ホイール: 拡大縮小    多角形: 左クリックで頂点を追加");
            let size = ui.available_size().max(egui::vec2(1., 1.));
            let aspect = (size.x / size.y) as f64;
            if (self.camera.aspect - aspect).abs() > 1e-6 {
                self.camera.aspect = aspect;
                self.dirty = true;
            }
            self.renderer.upload(&self.points, DVec3::from(self.points_origin), self.points_generation);
            let pixels = ctx.pixels_per_point();
            let rs = frame.wgpu_render_state().unwrap();
            let id = self.renderer.draw(rs, &self.camera, [(size.x * pixels) as u32, (size.y * pixels) as u32], self.point_size * pixels);
            let response = ui.add(egui::Image::new((id, size)).sense(egui::Sense::click_and_drag()));
            let rect = response.rect;
            if self.smoke_colors {
                self.color_probes = self.points.iter().filter_map(|sample| {
                    self.camera.project(DVec3::from(sample.position)).map(|(uv,_)| (
                        egui::pos2(rect.left() + uv[0] as f32 * rect.width(), rect.top() + uv[1] as f32 * rect.height()), sample.color,
                    ))
                }).collect();
            }
            let normalize = |p: egui::Pos2| egui::pos2(((p.x - rect.left()) / rect.width()).clamp(0., 1.), ((p.y - rect.top()) / rect.height()).clamp(0., 1.));
            let orbit = !self.select_mode && response.dragged_by(egui::PointerButton::Primary);
            if orbit {
                let delta = ctx.input(|i| i.pointer.delta());
                self.camera.yaw -= delta.x as f64 * 0.007;
                self.camera.pitch = (self.camera.pitch + delta.y as f64 * 0.007).clamp(-1.5, 1.5);
                self.dirty = true;
                self.polygon.clear();
            }
            if response.dragged_by(egui::PointerButton::Secondary) || response.dragged_by(egui::PointerButton::Middle) {
                let delta = ctx.input(|i| i.pointer.delta());
                let forward = (DVec3::from(self.camera.target) - self.camera.eye()).normalize();
                let right = forward.cross(DVec3::Z).normalize();
                let up = right.cross(forward);
                self.camera.target = (DVec3::from(self.camera.target) + (right * (-delta.x as f64) + up * delta.y as f64) * self.camera.distance / rect.height() as f64).to_array();
                self.dirty = true;
                self.polygon.clear();
            }
            if response.hovered() {
                let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0. {
                    self.camera.distance = (self.camera.distance * (-scroll as f64 * 0.003).exp()).clamp(0.001, 1e10);
                    self.dirty = true;
                    self.polygon.clear();
                }
            }
            if self.select_mode && self.job.is_none() {
                if self.lasso {
                    if response.clicked_by(egui::PointerButton::Primary) && let Some(pos) = response.interact_pointer_pos() {
                        if self.polygon.is_empty() { self.selection_camera = Some(self.camera); }
                        self.polygon.push(normalize(pos));
                    }
                } else {
                    if response.drag_started_by(egui::PointerButton::Primary) && let Some(pos) = response.interact_pointer_pos() {
                        self.drag_start = Some(normalize(pos));
                        self.selection_camera = Some(self.camera);
                        self.polygon.clear();
                    }
                    if response.dragged_by(egui::PointerButton::Primary) && let (Some(start), Some(pos)) = (self.drag_start, response.interact_pointer_pos()) {
                        let end = normalize(pos);
                        self.polygon = vec![start, egui::pos2(end.x, start.y), end, egui::pos2(start.x, end.y)];
                    }
                    if response.drag_stopped_by(egui::PointerButton::Primary) { self.drag_start = None; }
                }
            }
            if self.polygon.len() > 1 {
                let points: Vec<_> = self.polygon.iter().map(|p| egui::pos2(rect.left() + p.x * rect.width(), rect.top() + p.y * rect.height())).collect();
                ui.painter().add(egui::Shape::closed_line(points, egui::Stroke::new(2., egui::Color32::from_rgb(80, 220, 190))));
            }
        });
        self.request_view();
        if self.dirty || self.refine_pending || self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}
impl Drop for Workbench {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.view_epoch.fetch_add(1, Ordering::Relaxed);
    }
}
