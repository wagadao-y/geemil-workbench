//! Screen-space selection tools, the exclusion preview and running exclusions.
//!
//! The preview marks the displayed points an exclusion would remove. Displayed
//! points are original points, so once the nearest original depth is known
//! (computed in the background, like the exclusion's first pass) the marks match
//! the committed result exactly; until then the nearest displayed point stands in.
use super::{Workbench, jobs::Notice, jobs::is_cancelled};
use eframe::egui;
use geemil_core::{Camera, JobControl, Project, Selection, SelectionMode};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tool {
    Navigate,
    Rect,
    Polygon,
}

/// The selection being drawn, in normalized viewport coordinates of `camera`.
pub(super) struct SelectionState {
    pub(super) tool: Tool,
    mode: SelectionMode,
    /// Limit an inside exclusion to `depth` behind the nearest point.
    limit_depth: bool,
    depth: f64,
    polygon: Vec<egui::Pos2>,
    /// A closed polygon takes no more vertices; the next click starts a new one.
    closed: bool,
    drag_start: Option<egui::Pos2>,
    camera: Option<Camera>,
    preview: Preview,
    /// Increases whenever preview marks change, across clears, so the renderer
    /// never mistakes new marks for ones it already uploaded.
    revision: u64,
}
impl Default for SelectionState {
    fn default() -> Self {
        Self {
            tool: Tool::Navigate,
            mode: SelectionMode::ExcludeInside,
            limit_depth: true,
            depth: 0.5,
            polygon: vec![],
            closed: false,
            drag_start: None,
            camera: None,
            preview: Preview::default(),
            revision: 0,
        }
    }
}
impl SelectionState {
    pub(super) fn clear(&mut self) {
        self.polygon.clear();
        self.closed = false;
        self.drag_start = None;
        self.camera = None;
        self.preview = Preview::default();
    }
    /// A closed rectangle, as if dragged from `min` to `max` with `camera`.
    pub(super) fn select_rect(
        &mut self,
        camera: Camera,
        min: [f32; 2],
        max: [f32; 2],
        mode: SelectionMode,
        limit_depth: bool,
    ) {
        self.clear();
        self.tool = Tool::Rect;
        self.mode = mode;
        self.limit_depth = limit_depth;
        self.camera = Some(camera);
        self.polygon = vec![
            egui::pos2(min[0], min[1]),
            egui::pos2(max[0], min[1]),
            egui::pos2(max[0], max[1]),
            egui::pos2(min[0], max[1]),
        ];
        self.closed = true;
    }
    /// Marked and displayed point counts and the nearest depth, for diagnostics.
    pub(super) fn preview_summary(&self) -> Option<String> {
        let preview = &self.preview;
        preview.marks_for.as_ref().map(|_| {
            format!(
                "{} of {} displayed points marked, nearest {:?}",
                preview.marked,
                preview.marks.len(),
                preview.nearest
            )
        })
    }
    /// Points to highlight, parallel to the displayed points, and their revision.
    pub(super) fn marks(&self) -> Option<(&[bool], u64)> {
        let preview = &self.preview;
        preview
            .marks_for
            .as_ref()
            .map(|_| (preview.marks.as_slice(), self.revision))
    }
    fn selection(&self) -> Option<Selection> {
        if self.polygon.len() < 3 {
            return None;
        }
        Some(Selection {
            camera: self.camera?,
            polygon: self
                .polygon
                .iter()
                .map(|p| [p.x as f64, p.y as f64])
                .collect(),
            depth_meters: (self.mode == SelectionMode::ExcludeInside && self.limit_depth)
                .then_some(self.depth),
            mode: self.mode,
        })
    }
}

#[derive(Default)]
struct Preview {
    /// Inputs of the nearest depth below; a change restarts the search.
    nearest_for: Option<NearestInput>,
    nearest: Nearest,
    search: Option<NearestSearch>,
    /// Inputs of `marks`; a change recomputes them.
    marks_for: Option<MarksInput>,
    /// Parallel to the displayed points; true for points the exclusion removes.
    marks: Vec<bool>,
    marked: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Nearest {
    #[default]
    Unknown,
    /// From displayed points only; the original nearest may be closer.
    Displayed(f64),
    Exact(Option<f64>),
}
#[derive(PartialEq)]
struct NearestInput {
    polygon: Vec<egui::Pos2>,
    camera: Camera,
    visible: Vec<Uuid>,
    root: PathBuf,
    revision: Uuid,
}
#[derive(PartialEq)]
struct MarksInput {
    polygon: Vec<egui::Pos2>,
    camera: Camera,
    mode: SelectionMode,
    depth: Option<f64>,
    nearest: Nearest,
    points: u64,
}
struct NearestSearch {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<anyhow::Result<Option<f64>>>,
}
impl Drop for NearestSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Workbench {
    pub(super) fn selection_tools(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let t = self.t;
        ui.horizontal(|ui| {
            let s = &mut self.selection;
            ui.selectable_value(&mut s.tool, Tool::Navigate, t.navigate);
            ui.selectable_value(&mut s.tool, Tool::Rect, t.tool_rect);
            ui.selectable_value(&mut s.tool, Tool::Polygon, t.tool_polygon);
            ui.separator();
            ui.selectable_value(&mut s.mode, SelectionMode::ExcludeInside, t.exclude_inside);
            ui.selectable_value(
                &mut s.mode,
                SelectionMode::ExcludeOutside,
                t.exclude_outside,
            );
            let inside = s.mode == SelectionMode::ExcludeInside;
            ui.add_enabled(inside, egui::Checkbox::new(&mut s.limit_depth, t.depth));
            ui.add_enabled(
                inside && s.limit_depth,
                egui::DragValue::new(&mut s.depth)
                    .speed(0.05)
                    .range(0.001..=1_000_000.)
                    .suffix(" m"),
            );
            let ready = self.job.is_none() && self.selection.selection().is_some();
            if ui
                .add_enabled(ready, egui::Button::new(t.exclude_selection))
                .clicked()
            {
                self.exclude(ctx);
            }
            if ui.button(t.clear_selection).clicked() {
                self.selection.clear();
            }
            let preview = &self.selection.preview;
            if self.selection.selection().is_some() && preview.marks_for.is_some() {
                ui.label((t.preview_count)(&t.count(preview.marked as u64)));
                if preview.search.is_some() {
                    ui.spinner();
                    ui.small(t.preview_searching);
                }
            }
        });
    }
    /// Rectangle drag or polygon clicks, plus Enter, Escape and Delete.
    pub(super) fn selection_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        if self.job.is_some() {
            return;
        }
        if !ctx.wants_keyboard_input() {
            let (enter, escape, delete) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                    i.key_pressed(egui::Key::Delete),
                )
            });
            if escape {
                self.selection.clear();
            }
            if enter && self.selection.polygon.len() >= 3 {
                self.selection.closed = true;
            }
            if delete && self.selection.selection().is_some() {
                self.exclude(ctx);
                return;
            }
        }
        let rect = response.rect;
        let normalize = |p: egui::Pos2| {
            egui::pos2(
                ((p.x - rect.left()) / rect.width()).clamp(0., 1.),
                ((p.y - rect.top()) / rect.height()).clamp(0., 1.),
            )
        };
        let camera = self.camera;
        let s = &mut self.selection;
        match s.tool {
            Tool::Navigate => {}
            Tool::Polygon => {
                // The first click of a double click already added the last vertex.
                if response.double_clicked() {
                    if s.polygon.len() >= 3 {
                        s.closed = true;
                    }
                } else if response.clicked_by(egui::PointerButton::Primary)
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    if s.closed || s.polygon.is_empty() {
                        s.clear();
                        s.camera = Some(camera);
                    }
                    s.polygon.push(normalize(pos));
                }
            }
            Tool::Rect => {
                if response.drag_started_by(egui::PointerButton::Primary)
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    s.clear();
                    s.drag_start = Some(normalize(pos));
                    s.camera = Some(camera);
                }
                if response.dragged_by(egui::PointerButton::Primary)
                    && let (Some(start), Some(pos)) =
                        (s.drag_start, response.interact_pointer_pos())
                {
                    let end = normalize(pos);
                    s.polygon = vec![
                        start,
                        egui::pos2(end.x, start.y),
                        end,
                        egui::pos2(start.x, end.y),
                    ];
                }
                if response.drag_stopped_by(egui::PointerButton::Primary) {
                    s.drag_start = None;
                    s.closed = true;
                }
            }
        }
    }
    /// Brings the preview up to date with the selection and the displayed points.
    pub(super) fn update_preview(&mut self, ctx: &egui::Context) {
        let selection = self
            .selection
            .selection()
            .filter(|_| self.job.is_none() && self.selection.drag_start.is_none());
        let (Some(selection), Some(project)) = (selection, self.project.clone()) else {
            if self.selection.preview.marks_for.is_some() {
                self.selection.preview = Preview::default();
            }
            return;
        };
        let s = &mut self.selection;
        let preview = &mut s.preview;
        let test = selection.prepare();
        if test.depth_limited() {
            let input = NearestInput {
                polygon: s.polygon.clone(),
                camera: selection.camera,
                visible: self.visible.iter().copied().collect(),
                root: project.root.clone(),
                revision: project.manifest.current,
            };
            if preview.nearest_for.as_ref() != Some(&input) {
                preview.nearest = displayed_nearest(&selection, &self.points);
                preview.search = Some(start_search(ctx, project, &selection, &input.visible));
                preview.nearest_for = Some(input);
            }
        }
        if let Some(search) = &preview.search
            && let Ok(result) = search.rx.try_recv()
        {
            preview.search = None;
            match result {
                Ok(nearest) => preview.nearest = Nearest::Exact(nearest),
                Err(e) if is_cancelled(&e) => {}
                Err(e) => self.error = Some(Notice::new(self.t, &e)),
            }
        }
        let input = MarksInput {
            polygon: s.polygon.clone(),
            camera: selection.camera,
            mode: selection.mode,
            depth: selection.depth_meters,
            nearest: preview.nearest,
            points: self.points_generation,
        };
        if preview.marks_for.as_ref() != Some(&input) {
            let limit = match (selection.depth_meters, preview.nearest) {
                _ if !test.depth_limited() => f64::INFINITY,
                (Some(depth), Nearest::Displayed(d) | Nearest::Exact(Some(d))) => d + depth,
                // Nothing inside the polygon: nothing to exclude.
                _ => f64::NEG_INFINITY,
            };
            preview.marks = self
                .points
                .iter()
                .map(|p| test.excludes(p.position.into(), limit))
                .collect();
            preview.marked = preview.marks.iter().filter(|m| **m).count();
            preview.marks_for = Some(input);
            s.revision += 1;
        }
    }
    pub(super) fn draw_selection(&self, ui: &egui::Ui, response: &egui::Response) {
        let s = &self.selection;
        if s.polygon.is_empty() {
            return;
        }
        let rect = response.rect;
        let mut points: Vec<_> = s
            .polygon
            .iter()
            .map(|p| {
                egui::pos2(
                    rect.left() + p.x * rect.width(),
                    rect.top() + p.y * rect.height(),
                )
            })
            .collect();
        let stroke = egui::Stroke::new(2., egui::Color32::from_rgb(80, 220, 190));
        let painter = ui.painter();
        if s.closed || s.tool == Tool::Rect {
            painter.add(egui::Shape::closed_line(points, stroke));
            return;
        }
        for p in &points {
            painter.circle_filled(*p, 3., stroke.color);
        }
        // An open polygon follows the cursor to show the next edge.
        if let Some(hover) = response.hover_pos() {
            points.push(hover);
        }
        painter.add(egui::Shape::line(points, stroke));
    }
    /// Excludes the selection from the visible scans, judged on original points
    /// with the camera captured when the selection started.
    fn exclude(&mut self, ctx: &egui::Context) {
        let (Some(selection), Some(p)) = (self.selection.selection(), &self.project) else {
            return;
        };
        if self.job.is_some() {
            return;
        }
        let mut project = (**p).clone();
        let ids = self.visible.iter().copied().collect::<Vec<_>>();
        self.start(ctx, move |job| {
            project.delete_selection(&selection, &ids, &job)?;
            Ok(project)
        });
    }
}

fn displayed_nearest(selection: &Selection, points: &[geemil_core::Sample]) -> Nearest {
    let test = selection.prepare();
    let nearest = points
        .iter()
        .filter_map(|p| test.contains(p.position.into()))
        .fold(f64::INFINITY, f64::min);
    if nearest.is_finite() {
        Nearest::Displayed(nearest)
    } else {
        Nearest::Unknown
    }
}

fn start_search(
    ctx: &egui::Context,
    project: Arc<Project>,
    selection: &Selection,
    visible: &[Uuid],
) -> NearestSearch {
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel(1);
    let job = JobControl {
        cancel: cancel.clone(),
        ..Default::default()
    };
    let (selection, visible, ctx) = (selection.clone(), visible.to_vec(), ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(project.selection_nearest(&selection, &visible, &job));
        ctx.request_repaint();
    });
    NearestSearch { cancel, rx }
}
