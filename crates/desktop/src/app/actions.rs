//! Everything the menus, the toolbar and the keyboard can do, in one place.
use super::{
    Workbench,
    dialogs::{AfterDiscard, AlignNext, Dialog, ExportFormat, Filter},
    jobs::Notice,
    revisions::listed_revision,
    selection::Tool,
};
use crate::i18n::Strings;
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use egui_phosphor::regular as icon;
use geemil_core::{ImportOptions, LasExportPolicy, Project};
use std::{f64::consts::FRAC_PI_2, path::PathBuf};

/// The steepest camera pitch; exactly vertical has no defined screen "up".
pub(super) const MAX_PITCH: f64 = FRAC_PI_2 - 1e-3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewPreset {
    Top,
    Front,
    Side,
    Iso,
}

#[derive(Clone, PartialEq)]
pub(super) enum Action {
    NewProject,
    Open,
    OpenRecent(PathBuf),
    Import,
    Export,
    Quit,
    Undo,
    Redo,
    ClearSelection,
    Exclude,
    NewFolder,
    FitView,
    View(ViewPreset),
    ToggleEdl,
    ToggleOrtho,
    Save,
    Revisions,
    Discard,
    Cleanup,
    Subsample,
    RemoveNoise,
    RemoveOutliers,
    ReduceOverlap,
    RemoveMoving,
    Scatter,
    RegistrationList,
    Tool(Tool),
    Shortcuts,
    About,
}
impl Action {
    pub(super) fn icon(&self) -> &'static str {
        match self {
            Self::NewProject => icon::FOLDER_PLUS,
            Self::Open | Self::OpenRecent(_) => icon::FOLDER_OPEN,
            Self::Import => icon::FILE_ARROW_DOWN,
            Self::Export => icon::EXPORT,
            Self::Quit => icon::SIGN_OUT,
            Self::Undo => icon::ARROW_U_UP_LEFT,
            Self::Redo => icon::ARROW_U_UP_RIGHT,
            Self::ClearSelection => icon::SELECTION_SLASH,
            Self::Exclude => icon::ERASER,
            Self::NewFolder => icon::FOLDER_SIMPLE_PLUS,
            Self::FitView => icon::CORNERS_OUT,
            Self::View(ViewPreset::Top) => icon::ARROW_LINE_DOWN,
            Self::View(ViewPreset::Front) => icon::ARROW_FAT_DOWN,
            Self::View(ViewPreset::Side) => icon::ARROW_RIGHT,
            Self::View(ViewPreset::Iso) => icon::CUBE,
            Self::ToggleEdl => icon::CIRCLE_HALF,
            Self::ToggleOrtho => icon::PERSPECTIVE,
            Self::Save => icon::FLOPPY_DISK,
            Self::Revisions => icon::GIT_BRANCH,
            Self::Discard => icon::ARROW_COUNTER_CLOCKWISE,
            Self::Cleanup => icon::BROOM,
            Self::Subsample => icon::DOTS_NINE,
            Self::RemoveNoise => icon::FUNNEL,
            Self::RemoveOutliers => icon::CHART_SCATTER,
            Self::ReduceOverlap => icon::INTERSECT,
            Self::RemoveMoving => icon::PERSON_SIMPLE_WALK,
            Self::Scatter => icon::SHUFFLE,
            Self::RegistrationList => icon::LIST_CHECKS,
            Self::Tool(Tool::Navigate) => icon::HAND,
            Self::Tool(Tool::Rect) => icon::SELECTION,
            Self::Tool(Tool::Polygon) => icon::POLYGON,
            Self::Tool(Tool::Measure) => icon::RULER,
            Self::Tool(Tool::Align) => icon::CROSSHAIR,
            Self::Tool(Tool::Box) => icon::CUBE_FOCUS,
            Self::Tool(Tool::Transform) => icon::ARROWS_OUT_CARDINAL,
            Self::Shortcuts => icon::KEYBOARD,
            Self::About => icon::INFO,
        }
    }
    pub(super) fn label(&self, t: &Strings) -> String {
        match self {
            Self::NewProject => t.new_project.into(),
            Self::Open => t.open.into(),
            Self::OpenRecent(path) => path.display().to_string(),
            Self::Import => t.import.into(),
            Self::Export => t.export.into(),
            Self::Quit => t.quit.into(),
            Self::Undo => t.undo.into(),
            Self::Redo => t.redo.into(),
            Self::ClearSelection => t.clear_selection.into(),
            Self::Exclude => t.exclude_selection.into(),
            Self::NewFolder => t.new_folder.into(),
            Self::FitView => t.fit_view.into(),
            Self::View(ViewPreset::Top) => t.view_top.into(),
            Self::View(ViewPreset::Front) => t.view_front.into(),
            Self::View(ViewPreset::Side) => t.view_side.into(),
            Self::View(ViewPreset::Iso) => t.view_iso.into(),
            Self::ToggleEdl => t.edl.into(),
            Self::ToggleOrtho => t.ortho.into(),
            Self::Save => t.save.into(),
            Self::Revisions => t.revisions.into(),
            Self::Discard => t.discard.into(),
            Self::Cleanup => t.cleanup.into(),
            Self::Subsample => t.subsample.into(),
            Self::RemoveNoise => t.remove_noise.into(),
            Self::RemoveOutliers => t.remove_outliers.into(),
            Self::ReduceOverlap => t.reduce_overlap.into(),
            Self::RemoveMoving => t.remove_moving.into(),
            Self::Scatter => t.scatter.into(),
            Self::RegistrationList => t.align_list.into(),
            Self::Tool(Tool::Navigate) => t.navigate.into(),
            Self::Tool(Tool::Rect) => t.tool_rect.into(),
            Self::Tool(Tool::Polygon) => t.tool_polygon.into(),
            Self::Tool(Tool::Measure) => t.tool_measure.into(),
            Self::Tool(Tool::Align) => t.tool_align.into(),
            Self::Tool(Tool::Box) => t.tool_box.into(),
            Self::Tool(Tool::Transform) => t.tool_transform.into(),
            Self::Shortcuts => t.shortcuts.into(),
            Self::About => t.about.into(),
        }
    }
    /// Shortcuts with modifiers; they work even while a text field has focus.
    pub(super) fn command(&self) -> Option<KeyboardShortcut> {
        let ctrl = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        Some(match self {
            Self::NewProject => ctrl(Key::N),
            Self::Open => ctrl(Key::O),
            Self::Import => ctrl(Key::I),
            Self::Save => ctrl(Key::S),
            Self::Undo => ctrl(Key::Z),
            Self::Redo => ctrl(Key::Y),
            _ => return None,
        })
    }
    /// Single keys, ignored while typing.
    pub(super) fn key(&self) -> Option<Key> {
        Some(match self {
            Self::Tool(Tool::Navigate) => Key::V,
            Self::Tool(Tool::Rect) => Key::R,
            Self::Tool(Tool::Polygon) => Key::P,
            Self::Tool(Tool::Measure) => Key::M,
            Self::Tool(Tool::Align) => Key::A,
            Self::Tool(Tool::Box) => Key::B,
            Self::Tool(Tool::Transform) => Key::T,
            Self::Exclude => Key::Delete,
            Self::ClearSelection => Key::Escape,
            Self::FitView => Key::F,
            Self::View(ViewPreset::Top) => Key::Num7,
            Self::View(ViewPreset::Front) => Key::Num1,
            Self::View(ViewPreset::Side) => Key::Num3,
            Self::View(ViewPreset::Iso) => Key::Num5,
            Self::ToggleEdl => Key::E,
            Self::ToggleOrtho => Key::O,
            _ => return None,
        })
    }
    pub(super) fn shortcut_text(&self, ctx: &egui::Context) -> Option<String> {
        self.command()
            .map(|c| ctx.format_shortcut(&c))
            .or_else(|| self.key().map(|k| k.symbol_or_name().to_owned()))
    }
}

/// Actions reachable from the keyboard.
const KEYED: &[Action] = &[
    Action::NewProject,
    Action::Open,
    Action::Import,
    Action::Save,
    Action::Undo,
    Action::Redo,
    Action::Tool(Tool::Navigate),
    Action::Tool(Tool::Rect),
    Action::Tool(Tool::Polygon),
    Action::Tool(Tool::Measure),
    Action::Tool(Tool::Align),
    Action::Tool(Tool::Box),
    Action::Tool(Tool::Transform),
    Action::Exclude,
    Action::ClearSelection,
    Action::FitView,
    Action::View(ViewPreset::Top),
    Action::View(ViewPreset::Front),
    Action::View(ViewPreset::Side),
    Action::View(ViewPreset::Iso),
    Action::ToggleEdl,
    Action::ToggleOrtho,
];

impl Workbench {
    pub(super) fn enabled(&self, action: &Action) -> bool {
        let idle = self.job.is_none();
        let project = self.project.as_ref();
        let has_scans = project.is_some_and(|p| p.scans().next().is_some());
        let unsaved = project.is_some_and(|p| p.has_unsaved_changes());
        match action {
            Action::NewProject | Action::Open | Action::OpenRecent(_) => idle,
            Action::Import | Action::NewFolder => idle && project.is_some(),
            Action::Export | Action::Scatter => idle && has_scans,
            Action::RegistrationList => has_scans,
            Action::Undo => idle && self.undo.can_undo(),
            Action::Redo => idle && self.undo.can_redo(),
            Action::Exclude => idle && self.selection.is_ready(),
            Action::Save | Action::Discard => idle && unsaved,
            Action::Revisions | Action::Cleanup => idle && project.is_some(),
            Action::Subsample | Action::RemoveNoise | Action::RemoveOutliers => {
                idle && !self.visible.is_empty()
            }
            Action::ReduceOverlap | Action::RemoveMoving => idle && self.visible.len() > 1,
            Action::FitView | Action::View(_) => project.is_some(),
            Action::Quit
            | Action::ClearSelection
            | Action::ToggleEdl
            | Action::ToggleOrtho
            | Action::Tool(_)
            | Action::Shortcuts
            | Action::About => true,
        }
    }
    pub(super) fn undo_available(&self) -> bool {
        self.undo.can_undo()
    }
    pub(super) fn redo_available(&self) -> bool {
        self.undo.can_redo()
    }
    /// Runs keyboard shortcuts; single keys only when no text field has focus.
    pub(super) fn keyboard(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        // Ctrl+Shift+Z is redo as well; check it before Ctrl+Z consumes it.
        let redo = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        if ctx.input_mut(|i| i.consume_shortcut(&redo)) && self.enabled(&Action::Redo) {
            self.perform(ctx, Action::Redo);
        }
        for action in KEYED {
            let pressed = if let Some(command) = action.command() {
                ctx.input_mut(|i| i.consume_shortcut(&command))
            } else if let Some(key) = action.key().filter(|_| !typing) {
                ctx.input(|i| i.key_pressed(key) && i.modifiers.is_none())
            } else {
                false
            };
            if pressed && self.enabled(action) {
                self.perform(ctx, action.clone());
            }
        }
        if !typing && ctx.input(|i| i.key_pressed(Key::Enter)) {
            self.selection.close_polygon();
        }
        if !typing
            && self.selection.tool == Tool::Align
            && ctx.input(|i| i.key_pressed(Key::Backspace))
        {
            self.align_undo_pick();
        }
    }
    pub(super) fn perform(&mut self, ctx: &egui::Context, action: Action) {
        let t = self.t;
        match action {
            Action::NewProject => self.dialog = Some(Dialog::new_project(&self.settings, vec![])),
            Action::Open => {
                if let Some(path) = rfd::FileDialog::new().set_title(t.open).pick_folder() {
                    self.open_project(&path);
                }
            }
            Action::OpenRecent(path) => self.open_project(&path),
            Action::Import => {
                if let Some(files) = rfd::FileDialog::new()
                    .add_filter("E57 / LAS / LAZ", &["e57", "las", "laz"])
                    .pick_files()
                {
                    self.import(ctx, files);
                }
            }
            Action::Export => {
                self.dialog = Some(Dialog::Export {
                    format: ExportFormat::E57,
                    per_scan: false,
                    policy: LasExportPolicy::Preserve,
                    compatibility: None,
                })
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::ClearSelection => {
                self.selection.clear();
                self.measure.clear();
                if self.selection.tool == Tool::Align {
                    self.align.clear();
                }
                self.gizmo.cancel();
                self.crop.cancel();
            }
            Action::Exclude => self.exclude(ctx),
            Action::NewFolder => {
                let parent = self.selected_group();
                let name = t.default_folder_name.to_owned();
                if let Some(id) = self.apply_edit(|p| p.create_group(name, parent)) {
                    self.select_tree_item(Some(id));
                    self.dialog = Some(Dialog::Rename {
                        id,
                        name: t.default_folder_name.into(),
                    });
                }
            }
            Action::FitView => self.fit_view(),
            Action::View(preset) => self.view_preset(preset),
            Action::ToggleEdl => self.settings.edl = !self.settings.edl,
            Action::ToggleOrtho => {
                self.camera.ortho = !self.camera.ortho;
                self.dirty = true;
                self.selection.camera_moved();
            }
            Action::Save => {
                let n = self
                    .project
                    .as_ref()
                    .map_or(1, |p| p.manifest.revisions.len());
                self.dialog = Some(Dialog::SaveAs {
                    name: (t.default_revision_name)(n),
                });
            }
            Action::Revisions => self.dialog = Some(Dialog::revisions(self.project.as_deref())),
            Action::Discard => {
                self.dialog = Some(Dialog::Discard {
                    then: AfterDiscard::Nothing,
                    return_to_revisions: None,
                })
            }
            Action::Cleanup => self.dialog = Some(Dialog::Cleanup),
            Action::Subsample => {
                self.dialog = Some(Dialog::Filter(
                    Filter::Subsample {
                        size: self.settings.subsample_size,
                        merged: self.settings.subsample_merged,
                    },
                    None,
                ))
            }
            Action::RemoveNoise => {
                self.dialog = Some(Dialog::Filter(
                    Filter::Noise {
                        radius: self.settings.noise_radius,
                        min_neighbours: self.settings.noise_neighbours,
                    },
                    None,
                ))
            }
            Action::ReduceOverlap => {
                self.dialog = Some(Dialog::Filter(
                    Filter::Overlap {
                        size: self.settings.overlap_size,
                    },
                    None,
                ))
            }
            Action::RemoveMoving => {
                self.dialog = Some(Dialog::Filter(
                    Filter::Moving {
                        cell_degrees: self.settings.moving_cell_degrees,
                        tolerance: self.settings.moving_tolerance,
                        min_scans: self.settings.moving_min_scans,
                    },
                    None,
                ))
            }
            Action::RemoveOutliers => {
                self.dialog = Some(Dialog::Filter(
                    Filter::Statistical {
                        neighbours: self.settings.outlier_neighbours,
                        deviations: self.settings.outlier_deviations,
                        reach: self.settings.outlier_reach,
                    },
                    None,
                ))
            }
            Action::Tool(tool) => {
                if self.selection.tool == Tool::Align && tool != Tool::Align && self.align_pending()
                {
                    self.dialog = Some(Dialog::AlignPending {
                        next: AlignNext::Tool(tool),
                    });
                } else {
                    self.set_tool(tool);
                }
            }
            Action::Scatter => self.dialog = Some(Dialog::Scatter),
            Action::RegistrationList => self.align.list_open = true,
            Action::Shortcuts => self.dialog = Some(Dialog::Shortcuts),
            Action::About => self.dialog = Some(Dialog::About),
        }
    }
    pub(super) fn set_tool(&mut self, tool: Tool) {
        self.crop.cancel();
        self.selection.tool = tool;
        if tool != Tool::Measure {
            self.measure.clear();
        }
    }

    /// Asks where to write and exports the current state in the selected format.
    pub(super) fn export(
        &mut self,
        ctx: &egui::Context,
        format: ExportFormat,
        per_scan: bool,
        policy: LasExportPolicy,
    ) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let project = (**project).clone();
        let ext = format.extension();
        if per_scan && format != ExportFormat::E57 {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                self.start(ctx, false, move |job| {
                    project.export_las_per_scan(&dir, format == ExportFormat::Laz, &job)?;
                    Ok(project)
                });
            }
        } else if let Some(path) = rfd::FileDialog::new()
            .add_filter(ext.to_uppercase(), &[ext])
            .set_file_name(format!("{}.{ext}", project.manifest.name))
            .save_file()
        {
            let path = path.with_extension(ext);
            self.start(ctx, false, move |job| {
                match format {
                    ExportFormat::E57 => project.export_e57(&path, &job)?,
                    ExportFormat::Las | ExportFormat::Laz => {
                        project.export_las_with_policy(&path, policy, &job)?;
                    }
                }
                Ok(project)
            });
        }
    }
    /// Runs a point filter on the visible scans, moving the points it picks to
    /// the chosen layer or its default one, and remembers its parameters.
    pub(super) fn run_filter(
        &mut self,
        ctx: &egui::Context,
        filter: Filter,
        destination: Option<u8>,
    ) {
        let Some(p) = &self.project else { return };
        let target = super::layers::destination(p, destination, filter.default_layer(self.t));
        let mut project = (**p).clone();
        let ids: Vec<_> = self.visible.iter().copied().collect();
        match filter {
            Filter::Subsample { size, merged } => {
                self.settings.subsample_size = size;
                self.settings.subsample_merged = merged;
            }
            Filter::Overlap { size } => self.settings.overlap_size = size,
            Filter::Moving {
                cell_degrees,
                tolerance,
                min_scans,
            } => {
                self.settings.moving_cell_degrees = cell_degrees;
                self.settings.moving_tolerance = tolerance;
                self.settings.moving_min_scans = min_scans;
            }
            Filter::Noise {
                radius,
                min_neighbours,
            } => {
                self.settings.noise_radius = radius;
                self.settings.noise_neighbours = min_neighbours;
            }
            Filter::Statistical {
                neighbours,
                deviations,
                reach,
            } => {
                self.settings.outlier_neighbours = neighbours;
                self.settings.outlier_deviations = deviations;
                self.settings.outlier_reach = reach;
            }
        }
        project.filter_options.memory_bytes =
            self.settings.filter_memory_mib.clamp(128, 4096) * 1024 * 1024;
        self.start(ctx, true, move |job| {
            match filter {
                Filter::Subsample {
                    size,
                    merged: false,
                } => project.subsample(size, &ids, &target, &job)?,
                Filter::Subsample { size, merged: true } => {
                    project.subsample_merged(size, &ids, &target, &job)?
                }
                Filter::Overlap { size } => project.reduce_overlap(size, &ids, &target, &job)?,
                Filter::Moving {
                    cell_degrees,
                    tolerance,
                    min_scans,
                } => {
                    let options = geemil_core::MovingOptions {
                        cell_degrees,
                        tolerance,
                        min_scans,
                    };
                    project.remove_moving(options, &ids, &target, &job)?
                }
                Filter::Noise {
                    radius,
                    min_neighbours,
                } => project.remove_noise(radius, min_neighbours, &ids, &target, &job)?,
                Filter::Statistical {
                    neighbours,
                    deviations,
                    reach,
                } => project.remove_outliers(neighbours, deviations, reach, &ids, &target, &job)?,
            };
            Ok(project)
        });
    }
    /// The folder new folders go into: the selected folder, or the selected
    /// scan's folder.
    fn selected_group(&self) -> Option<uuid::Uuid> {
        let p = self.project.as_ref()?;
        let id = self.single_tree_item()?;
        if p.groups().iter().any(|g| g.id == id) {
            Some(id)
        } else {
            p.parent_of(id)
        }
    }
    pub(super) fn import(&mut self, ctx: &egui::Context, files: Vec<PathBuf>) {
        let Some(project) = &self.project else {
            self.dialog = Some(Dialog::new_project(&self.settings, files));
            return;
        };
        let mut project = (**project).clone();
        self.start(ctx, true, move |job| {
            for file in files {
                project.import_file(&file, ImportOptions::default(), &job)?;
            }
            Ok(project)
        });
    }
    pub(super) fn open_project(&mut self, path: &std::path::Path) {
        match Project::load(path) {
            Ok(p) => {
                let choose = p
                    .manifest
                    .revisions
                    .iter()
                    .filter(|r| listed_revision(r))
                    .count()
                    > 1
                    || p.has_unsaved_changes();
                self.undo.clear();
                self.install(p, true);
                self.status = self.t.status_opened.into();
                self.remember(path);
                if choose {
                    self.dialog = Some(Dialog::revisions(self.project.as_deref()));
                }
            }
            Err(e) => self.error = Some(Notice::new(self.t, &e)),
        }
    }
    pub(super) fn remember(&mut self, path: &std::path::Path) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let recent = &mut self.settings.recent;
        recent.retain(|p| p != &path);
        recent.insert(0, path);
        recent.truncate(10);
    }
    /// Runs a quick edit on the UI thread and records it for undo.
    pub(super) fn apply_edit<R>(
        &mut self,
        change: impl FnOnce(&mut Project) -> anyhow::Result<R>,
    ) -> Option<R> {
        let mut project = (**self.project.as_ref()?).clone();
        let before = project.manifest.draft.clone();
        let before_id = project.current().id;
        match change(&mut project) {
            Ok(result) => {
                if project.current().id != before_id {
                    self.undo.record(before);
                }
                self.install(project, false);
                Some(result)
            }
            Err(e) => {
                self.error = Some(Notice::new(self.t, &e));
                None
            }
        }
    }
    fn undo(&mut self) {
        let Some(project) = &self.project else { return };
        let current = project.manifest.draft.clone();
        if let Some(state) = self.undo.undo(current)
            && !self.restore(state.clone())
        {
            self.undo.reverse_undo(state);
        }
    }
    fn redo(&mut self) {
        let Some(project) = &self.project else { return };
        let current = project.manifest.draft.clone();
        if let Some(state) = self.undo.redo(current)
            && !self.restore(state.clone())
        {
            self.undo.reverse_redo(state);
        }
    }
    fn restore(&mut self, state: Option<geemil_core::Revision>) -> bool {
        let Some(project) = &self.project else {
            return false;
        };
        let mut project = (**project).clone();
        match project.restore_working_state(state) {
            Ok(()) => {
                self.install(project, false);
                true
            }
            Err(e) => {
                self.error = Some(Notice::new(self.t, &e));
                false
            }
        }
    }
    pub(super) fn save_revision(&mut self, name: String) {
        let Some(project) = &self.project else { return };
        let mut project = (**project).clone();
        match project.save_revision(name.clone()) {
            Ok(_) => {
                self.install(project, false);
                self.status = (self.t.status_saved)(&name);
            }
            Err(e) => self.error = Some(Notice::new(self.t, &e)),
        }
    }
    /// Shows a saved revision; unsaved changes must be confirmed first.
    pub(super) fn switch_revision(&mut self, id: uuid::Uuid) {
        let Some(project) = &self.project else { return };
        let mut project = (**project).clone();
        let fit = project.current().scans
            != project
                .manifest
                .revisions
                .iter()
                .find(|r| r.id == id)
                .map_or(vec![], |r| r.scans.clone());
        match project.switch(id) {
            Ok(()) => {
                self.undo.clear();
                self.install(project, fit);
            }
            Err(e) => self.error = Some(Notice::new(self.t, &e)),
        }
    }
    pub(super) fn create_project(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        imports: Vec<PathBuf>,
    ) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match Project::create(&path, &name) {
            Ok(p) => {
                if let Some(parent) = path.parent() {
                    self.settings.last_location = Some(parent.to_owned());
                }
                self.undo.clear();
                self.install(p, true);
                self.remember(&path);
                if !imports.is_empty() {
                    self.import(ctx, imports);
                }
            }
            Err(e) => self.error = Some(Notice::new(self.t, &e)),
        }
    }
    fn view_preset(&mut self, preset: ViewPreset) {
        use std::f64::consts::{FRAC_PI_4, PI};
        // Yaw places the eye; -90° looks along +Y, so north is up on screen.
        let (yaw, pitch) = match preset {
            ViewPreset::Top => (-FRAC_PI_2, MAX_PITCH),
            ViewPreset::Front => (-FRAC_PI_2, 0.),
            ViewPreset::Side => (0., 0.),
            ViewPreset::Iso => (-3. * FRAC_PI_4, (1f64 / 3f64.sqrt()).asin()),
        };
        self.camera.yaw = yaw.rem_euclid(2. * PI);
        self.camera.pitch = pitch;
        self.flight = None;
        self.dirty = true;
        self.selection.camera_moved();
    }
}
