//! Menu bar, icon toolbar and the tool options bar below it.
use super::{
    Workbench,
    actions::{Action, ViewPreset},
    selection::Tool,
};
use eframe::egui;
use egui_phosphor::regular as icon;

impl Workbench {
    /// A menu entry with icon, label and shortcut. Returns the action if chosen.
    fn menu_item(&self, ui: &mut egui::Ui, action: Action, chosen: &mut Option<Action>) {
        let mut button = egui::Button::new(format!("{}  {}", action.icon(), action.label(self.t)));
        if let Some(text) = action.shortcut_text(ui.ctx()) {
            button = button.shortcut_text(text);
        }
        if ui.add_enabled(self.enabled(&action), button).clicked() {
            *chosen = Some(action);
        }
    }
    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let mut chosen = None;
        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(t.menu_file, |ui| {
                    self.menu_item(ui, Action::NewProject, &mut chosen);
                    self.menu_item(ui, Action::Open, &mut chosen);
                    ui.menu_button(
                        format!("{}  {}", icon::CLOCK_COUNTER_CLOCKWISE, t.recent),
                        |ui| {
                            if self.settings.recent.is_empty() {
                                ui.weak(t.recent_empty);
                            }
                            for path in self.settings.recent.clone() {
                                let exists = path.join("project.json").is_file();
                                if ui
                                    .add_enabled(
                                        exists && self.job.is_none(),
                                        egui::Button::new(path.display().to_string()),
                                    )
                                    .clicked()
                                {
                                    chosen = Some(Action::OpenRecent(path));
                                }
                            }
                        },
                    );
                    ui.separator();
                    // Revisions are how a project is saved, so they live here.
                    self.menu_item(ui, Action::Save, &mut chosen);
                    self.menu_item(ui, Action::Revisions, &mut chosen);
                    self.menu_item(ui, Action::Discard, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::Import, &mut chosen);
                    self.menu_item(ui, Action::Export, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::Cleanup, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::Quit, &mut chosen);
                });
                ui.menu_button(t.menu_edit, |ui| {
                    self.menu_item(ui, Action::Undo, &mut chosen);
                    self.menu_item(ui, Action::Redo, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::Exclude, &mut chosen);
                    self.menu_item(ui, Action::ClearSelection, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::NewFolder, &mut chosen);
                });
                ui.menu_button(t.menu_view, |ui| {
                    self.menu_item(ui, Action::ToggleTree, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::FitView, &mut chosen);
                    for preset in [
                        ViewPreset::Top,
                        ViewPreset::Front,
                        ViewPreset::Side,
                        ViewPreset::Iso,
                    ] {
                        self.menu_item(ui, Action::View(preset), &mut chosen);
                    }
                    ui.separator();
                    ui.label(t.color_by);
                    for (mode, label) in [
                        (super::ColorMode::Rgb, t.color_rgb),
                        (super::ColorMode::Height, t.color_height),
                        (super::ColorMode::Scan, t.color_scan),
                    ] {
                        ui.radio_value(&mut self.settings.color_mode, mode, label);
                    }
                    ui.separator();
                    self.menu_item(ui, Action::ToggleOrtho, &mut chosen);
                    self.menu_item(ui, Action::ToggleEdl, &mut chosen);
                    ui.add_enabled(
                        self.settings.edl,
                        egui::Slider::new(&mut self.settings.edl_strength, 0.1..=5.0)
                            .text(t.edl_strength),
                    );
                });
                ui.menu_button(t.menu_tools, |ui| {
                    for tool in super::selection::TOOLS {
                        self.menu_item(ui, Action::Tool(tool), &mut chosen);
                    }
                    ui.separator();
                    self.menu_item(ui, Action::Subsample, &mut chosen);
                    self.menu_item(ui, Action::RemoveNoise, &mut chosen);
                    self.menu_item(ui, Action::RemoveOutliers, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::ReduceOverlap, &mut chosen);
                    self.menu_item(ui, Action::RemoveMoving, &mut chosen);
                    ui.separator();
                    self.menu_item(ui, Action::RegistrationList, &mut chosen);
                    ui.separator();
                    // Out of the way: it throws the registration away.
                    ui.menu_button(t.menu_practice, |ui| {
                        self.menu_item(ui, Action::Scatter, &mut chosen);
                    });
                });
                ui.menu_button(t.menu_help, |ui| {
                    self.menu_item(ui, Action::Shortcuts, &mut chosen);
                    self.menu_item(ui, Action::About, &mut chosen);
                });
            });
        });
        if let Some(action) = chosen {
            let ctx = ui.ctx().clone();
            self.perform(&ctx, action);
        }
    }

    /// An icon button with the action's name and shortcut as tooltip.
    fn tool_button(
        &self,
        ui: &mut egui::Ui,
        action: Action,
        selected: bool,
        chosen: &mut Option<Action>,
    ) {
        let mut tip = action.label(self.t);
        if let Some(text) = action.shortcut_text(ui.ctx()) {
            tip = format!("{tip}  ({text})");
        }
        let button =
            egui::Button::selectable(selected, egui::RichText::new(action.icon()).size(18.))
                .min_size(egui::vec2(30., 28.));
        if ui
            .add_enabled(self.enabled(&action), button)
            .on_hover_text(&tip)
            .on_disabled_hover_text(&tip)
            .clicked()
        {
            *chosen = Some(action);
        }
    }
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let mut chosen = None;
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                for action in [Action::Import, Action::Save, Action::Export] {
                    self.tool_button(ui, action, false, &mut chosen);
                }
                ui.separator();
                self.tool_button(ui, Action::Undo, false, &mut chosen);
                self.tool_button(ui, Action::Redo, false, &mut chosen);
                ui.separator();
                for tool in super::selection::TOOLS {
                    let selected = self.selection.tool == tool;
                    self.tool_button(ui, Action::Tool(tool), selected, &mut chosen);
                }
                ui.separator();
                // Fit and the view presets move the camera at once, so they
                // stay in the View menu and on keys, away from the tools.
                let ortho = self.camera.ortho;
                self.tool_button(ui, Action::ToggleOrtho, ortho, &mut chosen);
                let edl = self.settings.edl;
                self.tool_button(ui, Action::ToggleEdl, edl, &mut chosen);
                ui.separator();
                ui.label(t.point_budget);
                let budget =
                    egui::Slider::new(&mut self.settings.point_budget, 100_000..=20_000_000)
                        .logarithmic(true)
                        .step_by(100_000.)
                        .custom_formatter(|n, _| format!("{:.1} M", n / 1e6))
                        .custom_parser(|s| {
                            let s = s.trim().trim_end_matches(['M', 'm']).trim();
                            s.parse::<f64>().ok().map(|m| (m * 1e6).round())
                        });
                if ui.add(budget).changed() {
                    self.view.invalidate();
                    self.dirty = true;
                }
                ui.label(t.point_size);
                ui.add(egui::Slider::new(&mut self.settings.point_size, 1.0..=8.0).step_by(0.5));
                let adaptive = &mut self.settings.adaptive_size;
                let before = *adaptive;
                egui::ComboBox::from_id_salt("point size mode")
                    .selected_text(if *adaptive {
                        t.point_size_adaptive
                    } else {
                        t.point_size_fixed
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(adaptive, false, t.point_size_fixed);
                        ui.selectable_value(adaptive, true, t.point_size_adaptive)
                            .on_hover_text(t.point_size_adaptive_hint);
                    });
                if *adaptive != before {
                    self.view.invalidate();
                    self.dirty = true;
                }
                ui.label(t.color_by);
                let mode = &mut self.settings.color_mode;
                egui::ComboBox::from_id_salt("color mode")
                    .selected_text(match mode {
                        super::ColorMode::Rgb => t.color_rgb,
                        super::ColorMode::Height => t.color_height,
                        super::ColorMode::Scan => t.color_scan,
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(mode, super::ColorMode::Rgb, t.color_rgb);
                        ui.selectable_value(mode, super::ColorMode::Height, t.color_height);
                        ui.selectable_value(mode, super::ColorMode::Scan, t.color_scan);
                    });
            });
        });
        if let Some(action) = chosen {
            let ctx = ui.ctx().clone();
            self.perform(&ctx, action);
        }
    }
    /// Options of the active tool, or its usage hint.
    pub(super) fn tool_options(&mut self, ui: &mut egui::Ui) {
        if self.project.is_none() {
            return;
        }
        let t = self.t;
        let ctx = ui.ctx().clone();
        egui::Panel::top("tool options").show(ui, |ui| {
            ui.horizontal(|ui| match self.selection.tool {
                Tool::Navigate => {
                    ui.weak(t.hint_navigate);
                }
                Tool::Rect | Tool::Polygon => {
                    self.selection_options(ui, &ctx);
                }
                Tool::Align => {
                    ui.weak(t.hint_align);
                }
                Tool::Box => {
                    ui.weak(t.hint_box);
                }
                Tool::Transform => {
                    ui.weak(t.hint_transform);
                }
                Tool::Panorama => {
                    ui.weak(t.hint_panorama);
                }
                Tool::Measure => {
                    match self.measure.distance() {
                        Some((d, h, v)) => ui.strong((t.measure_result)(d, h, v)),
                        None => ui.weak(t.hint_measure),
                    };
                }
            });
        });
    }
}
