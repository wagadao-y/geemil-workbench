//! The left panel: the scan tree, properties of the selected item and the
//! exclusion layers. Tree operations are queued while drawing and applied after.
use super::{Workbench, dialogs::Dialog, revisions::layer_label};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{Group, Pose, Project};
use glam::DQuat;
use std::sync::Arc;
use uuid::Uuid;

enum TreeAction {
    Select(Uuid),
    SetVisible(Vec<Uuid>, bool),
    ShowOnly(Vec<Uuid>),
    ShowAll,
    Move(Vec<Uuid>, Option<Uuid>),
    NewFolder(Option<Uuid>),
    Rename(Uuid, String),
    Ungroup(Uuid),
    Remove(Vec<Uuid>),
}

/// Inputs of the transform editor and the item and state they were loaded from.
#[derive(Default)]
pub(super) struct TransformEdit {
    loaded: Option<(Uuid, Uuid)>,
    translation: [f64; 3],
    rotation: [f64; 3],
    /// The inputs as loaded; differing inputs are previewed until applied.
    initial: ([f64; 3], [f64; 3]),
}
impl TransformEdit {
    /// The applied transform of `id` in the current state.
    fn load(p: &Project, id: Uuid) -> Self {
        let pose = p.current().transforms.get(&id).copied().unwrap_or_default();
        let (x, y, z) = DQuat::from_array(pose.rotation_xyzw).to_euler(glam::EulerRot::XYZ);
        // Adding zero turns -0.0 into 0.0 for display.
        let rotation = [x, y, z].map(|v| v.to_degrees() + 0.);
        Self {
            loaded: Some((id, p.current().id)),
            translation: pose.translation,
            rotation,
            initial: (pose.translation, rotation),
        }
    }
    fn changed(&self) -> bool {
        (self.translation, self.rotation) != self.initial
    }
    fn pose(&self) -> Pose {
        let [x, y, z] = self.rotation.map(f64::to_radians);
        Pose {
            translation: self.translation,
            rotation_xyzw: DQuat::from_euler(glam::EulerRot::XYZ, x, y, z).to_array(),
        }
    }
}

fn within(p: &Project, id: Uuid, ancestor: Uuid) -> bool {
    let mut current = Some(id);
    for _ in 0..=p.groups().len() {
        match current {
            Some(c) if c == ancestor => return true,
            Some(c) => current = p.parent_of(c),
            None => return false,
        }
    }
    false
}

/// Folders with their depth, depth first, for the "move to" menu.
fn folder_list(p: &Project) -> Vec<(usize, &Group)> {
    fn walk<'a>(
        p: &'a Project,
        parent: Option<Uuid>,
        depth: usize,
        out: &mut Vec<(usize, &'a Group)>,
    ) {
        for g in p.children(parent).0 {
            out.push((depth, g));
            walk(p, Some(g.id), depth + 1, out);
        }
    }
    let mut out = vec![];
    walk(p, None, 0, &mut out);
    out
}

impl Workbench {
    pub(super) fn side_panel(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        egui::Panel::left("project")
            .resizable(true)
            .default_size(300.)
            .show(ui, |ui| {
                let Some(p) = self.project.clone() else {
                    ui.add_space(8.);
                    ui.label(t.no_project_hint);
                    return;
                };
                let mut actions = vec![];
                ui.horizontal(|ui| {
                    ui.strong(format!("{} {}", icon::TREE_STRUCTURE, p.manifest.name));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(icon::FOLDER_SIMPLE_PLUS)
                            .on_hover_text(t.new_folder)
                            .clicked()
                        {
                            let parent = self
                                .selected
                                .filter(|id| p.groups().iter().any(|g| g.id == *id));
                            actions.push(TreeAction::NewFolder(parent));
                        }
                    });
                });
                ui.separator();
                let height = (ui.available_height() * 0.5).max(140.);
                egui::ScrollArea::vertical()
                    .id_salt("tree")
                    .max_height(height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        self.tree_level(ui, &p, None, &mut actions);
                        if egui::DragAndDrop::has_payload_of_type::<Uuid>(ui.ctx()) {
                            let (_, dropped) = ui.dnd_drop_zone::<Uuid, ()>(
                                egui::Frame::group(ui.style()),
                                |ui| {
                                    ui.small(t.drop_to_top);
                                },
                            );
                            if let Some(id) = dropped {
                                actions.push(TreeAction::Move(vec![*id], None));
                            }
                        }
                    });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("details")
                    .show(ui, |ui| {
                        egui::CollapsingHeader::new(format!("{} {}", icon::INFO, t.properties))
                            .default_open(true)
                            .show(ui, |ui| self.properties(ui, &p, &mut actions));
                        egui::CollapsingHeader::new(format!("{} {}", icon::ERASER, t.layers))
                            .default_open(true)
                            .show(ui, |ui| self.layers(ui, &p));
                    });
                for action in actions {
                    self.tree_action(action);
                }
            });
    }

    fn tree_level(
        &self,
        ui: &mut egui::Ui,
        p: &Arc<Project>,
        parent: Option<Uuid>,
        actions: &mut Vec<TreeAction>,
    ) {
        let t = self.t;
        let (groups, scans) = p.children(parent);
        for group in groups {
            let id = ui.make_persistent_id(group.id);
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
                .show_header(ui, |ui| {
                    let inside = p.scans_within(group.id);
                    let shown = inside.iter().filter(|s| self.visible.contains(s)).count();
                    let mut all = !inside.is_empty() && shown == inside.len();
                    let checkbox = egui::Checkbox::new(&mut all, "")
                        .indeterminate(shown > 0 && shown < inside.len());
                    if ui.add(checkbox).changed() {
                        actions.push(TreeAction::SetVisible(inside.clone(), all));
                    }
                    let points: u64 = p
                        .scans()
                        .filter(|s| inside.contains(&s.id))
                        .map(|s| s.records)
                        .sum();
                    let label = format!("{} {}", icon::FOLDER, group.name);
                    let row = self.tree_row(ui, group.id, label, actions);
                    if let Some(item) = row.dnd_release_payload::<Uuid>()
                        && *item != group.id
                    {
                        actions.push(TreeAction::Move(vec![*item], Some(group.id)));
                    }
                    if row.dnd_hover_payload::<Uuid>().is_some() {
                        ui.painter().rect_stroke(
                            row.rect,
                            2.,
                            ui.visuals().selection.stroke,
                            egui::StrokeKind::Outside,
                        );
                    }
                    row.on_hover_text((t.folder_summary)(inside.len(), &t.count(points)))
                        .context_menu(|ui| {
                            if ui
                                .button(format!("{} {}", icon::FOLDER_SIMPLE_PLUS, t.new_folder))
                                .clicked()
                            {
                                actions.push(TreeAction::NewFolder(Some(group.id)));
                            }
                            if ui
                                .button(format!("{} {}", icon::PENCIL_SIMPLE, t.rename))
                                .clicked()
                            {
                                actions.push(TreeAction::Rename(group.id, group.name.clone()));
                            }
                            self.move_menu(ui, p, group.id, actions);
                            if ui
                                .button(format!("{} {}", icon::FOLDER_NOTCH_OPEN, t.ungroup))
                                .clicked()
                            {
                                actions.push(TreeAction::Ungroup(group.id));
                            }
                            ui.separator();
                            self.visibility_menu(ui, inside, actions);
                        });
                })
                .body(|ui| self.tree_level(ui, p, Some(group.id), actions));
        }
        for scan in scans {
            ui.horizontal(|ui| {
                let mut shown = self.visible.contains(&scan.id);
                if ui.checkbox(&mut shown, "").changed() {
                    actions.push(TreeAction::SetVisible(vec![scan.id], shown));
                }
                let label = format!("{} {}", icon::CUBE_TRANSPARENT, scan.name);
                let row = self.tree_row(ui, scan.id, label, actions);
                row.on_hover_text((t.scan_points)(&t.count(scan.records)))
                    .context_menu(|ui| {
                        self.move_menu(ui, p, scan.id, actions);
                        ui.separator();
                        self.visibility_menu(ui, vec![scan.id], actions);
                        ui.separator();
                        if ui
                            .button(format!("{} {}", icon::TRASH, t.remove_scans))
                            .clicked()
                        {
                            actions.push(TreeAction::Remove(vec![scan.id]));
                        }
                    });
            });
        }
    }
    /// A selectable, draggable tree label.
    fn tree_row(
        &self,
        ui: &mut egui::Ui,
        id: Uuid,
        label: String,
        actions: &mut Vec<TreeAction>,
    ) -> egui::Response {
        let selected = self.selected == Some(id);
        let row =
            ui.add(egui::Button::selectable(selected, label).sense(egui::Sense::click_and_drag()));
        if row.clicked() {
            actions.push(TreeAction::Select(id));
        }
        if row.dragged() {
            row.dnd_set_drag_payload(id);
        }
        row
    }
    fn move_menu(&self, ui: &mut egui::Ui, p: &Project, item: Uuid, actions: &mut Vec<TreeAction>) {
        let t = self.t;
        ui.menu_button(
            format!("{} {}", icon::ARROW_BEND_DOWN_RIGHT, t.move_to),
            |ui| {
                if ui.button(t.top_level).clicked() {
                    actions.push(TreeAction::Move(vec![item], None));
                }
                for (depth, g) in folder_list(p) {
                    let allowed = !within(p, g.id, item) && p.parent_of(item) != Some(g.id);
                    let label = format!("{}{} {}", "　".repeat(depth), icon::FOLDER, g.name);
                    if ui.add_enabled(allowed, egui::Button::new(label)).clicked() {
                        actions.push(TreeAction::Move(vec![item], Some(g.id)));
                    }
                }
            },
        );
    }
    fn visibility_menu(&self, ui: &mut egui::Ui, scans: Vec<Uuid>, actions: &mut Vec<TreeAction>) {
        let t = self.t;
        if ui
            .button(format!("{} {}", icon::EYE, t.show_only))
            .clicked()
        {
            actions.push(TreeAction::ShowOnly(scans));
        }
        if ui.button(format!("{} {}", icon::EYE, t.show_all)).clicked() {
            actions.push(TreeAction::ShowAll);
        }
    }
    fn tree_action(&mut self, action: TreeAction) {
        match action {
            TreeAction::Select(id) => self.selected = Some(id),
            TreeAction::SetVisible(ids, shown) => {
                for id in ids {
                    if shown {
                        self.visible.insert(id);
                    } else {
                        self.visible.remove(&id);
                    }
                }
                self.visibility_changed();
            }
            TreeAction::ShowOnly(ids) => {
                self.visible = ids.into_iter().collect();
                self.visibility_changed();
            }
            TreeAction::ShowAll => {
                if let Some(p) = &self.project {
                    self.visible = p.scans().map(|s| s.id).collect();
                }
                self.visibility_changed();
            }
            TreeAction::Move(ids, target) => {
                self.apply_edit(|p| p.move_to_group(&ids, target));
            }
            TreeAction::NewFolder(parent) => {
                let name = self.t.default_folder_name.to_owned();
                if let Some(id) = self.apply_edit(|p| p.create_group(name.clone(), parent)) {
                    self.selected = Some(id);
                    self.dialog = Some(Dialog::RenameGroup { id, name });
                }
            }
            TreeAction::Rename(id, name) => self.dialog = Some(Dialog::RenameGroup { id, name }),
            TreeAction::Ungroup(id) => {
                self.apply_edit(|p| p.ungroup(id));
            }
            TreeAction::Remove(ids) => {
                self.apply_edit(|p| p.remove_scans(&ids));
            }
        }
    }
    fn visibility_changed(&mut self) {
        self.view.invalidate();
        self.dirty = true;
    }

    fn properties(&mut self, ui: &mut egui::Ui, p: &Project, actions: &mut Vec<TreeAction>) {
        let t = self.t;
        let Some(id) = self.selected else {
            ui.weak(t.nothing_selected);
            return;
        };
        if let Some(scan) = p.scans().find(|s| s.id == id) {
            egui::Grid::new("scan properties")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(t.prop_name);
                    ui.label(&scan.name);
                    ui.end_row();
                    ui.label(t.prop_source);
                    ui.label(&scan.source_name);
                    ui.end_row();
                    ui.label(t.prop_points);
                    ui.label(t.count(scan.records));
                    ui.end_row();
                    ui.label(t.prop_valid);
                    ui.label(t.count(scan.valid_points));
                    ui.end_row();
                    ui.label(t.prop_chunks);
                    ui.label(t.count(scan.chunks.len() as u64));
                    ui.end_row();
                });
            let images: Vec<_> = p
                .manifest
                .images
                .iter()
                .filter(|i| i.scan_id == Some(id))
                .collect();
            if !images.is_empty() {
                ui.collapsing(
                    format!("{} {} ({})", icon::IMAGE, t.images, images.len()),
                    |ui| {
                        for image in images {
                            ui.small(format!(
                                "{} ({})",
                                image.name.as_deref().unwrap_or(t.unnamed_image),
                                image.projection
                            ));
                        }
                    },
                );
            }
            if !scan.omitted_attributes.is_empty() {
                ui.collapsing(t.omitted_attributes, |ui| {
                    for attribute in &scan.omitted_attributes {
                        ui.small(attribute);
                    }
                });
            }
        } else if let Some(group) = p.groups().iter().find(|g| g.id == id) {
            let inside = p.scans_within(id);
            let points: u64 = p
                .scans()
                .filter(|s| inside.contains(&s.id))
                .map(|s| s.records)
                .sum();
            egui::Grid::new("folder properties")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(t.prop_name);
                    ui.horizontal(|ui| {
                        ui.label(&group.name);
                        if ui
                            .small_button(icon::PENCIL_SIMPLE)
                            .on_hover_text(t.rename)
                            .clicked()
                        {
                            actions.push(TreeAction::Rename(id, group.name.clone()));
                        }
                    });
                    ui.end_row();
                    ui.label(t.prop_scans);
                    ui.label(t.count(inside.len() as u64));
                    ui.end_row();
                    ui.label(t.prop_points);
                    ui.label(t.count(points));
                    ui.end_row();
                });
        } else {
            ui.weak(t.nothing_selected);
            return;
        }
        ui.add_space(6.);
        self.transform_editor(ui, p, id);
    }
    /// The selected item's own transform, relative to its folder.
    fn transform_editor(&mut self, ui: &mut egui::Ui, p: &Project, id: Uuid) {
        let t = self.t;
        let key = (id, p.current().id);
        let edit = &mut self.transform_edit;
        if edit.loaded != Some(key) {
            *edit = TransformEdit::load(p, id);
        }
        ui.strong(format!("{} {}", icon::ARROWS_OUT_CARDINAL, t.transform));
        if p.groups().iter().any(|g| g.id == id) {
            ui.small(t.transform_folder_hint);
        }
        egui::Grid::new("transform").num_columns(4).show(ui, |ui| {
            ui.label(t.translation);
            for v in &mut edit.translation {
                ui.add(egui::DragValue::new(v).speed(0.01).max_decimals(4));
            }
            ui.end_row();
            ui.label(t.rotation);
            for v in &mut edit.rotation {
                ui.add(egui::DragValue::new(v).speed(0.1).max_decimals(3));
            }
            ui.end_row();
        });
        let idle = self.job.is_none();
        let pose = edit.pose();
        let changed = edit.changed();
        if changed {
            ui.small(t.transform_previewing);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(format!("{} {}", icon::CHECK, t.apply)),
                )
                .clicked()
            {
                self.apply_edit(|p| p.set_transform(id, pose));
            }
            if ui
                .add_enabled(
                    idle,
                    egui::Button::new(format!("{} {}", icon::ARROW_COUNTER_CLOCKWISE, t.reset)),
                )
                .clicked()
            {
                self.apply_edit(|p| p.set_transform(id, Pose::default()));
            }
            if changed && ui.button(format!("{} {}", icon::X, t.revert)).clicked() {
                let edit = &mut self.transform_edit;
                (edit.translation, edit.rotation) = edit.initial;
            }
        });
    }
    /// Fills the transform editor of `id` as if the user had typed the values.
    pub(super) fn edit_transform_inputs(
        &mut self,
        id: Uuid,
        translation: [f64; 3],
        rotation: [f64; 3],
    ) {
        let Some(p) = &self.project else { return };
        self.transform_edit = TransformEdit {
            translation,
            rotation,
            ..TransformEdit::load(p, id)
        };
    }
    /// The edited, not yet applied transform of the selected item, which the
    /// viewport shows in place of the applied one.
    pub(super) fn transform_preview(&self) -> Option<(Uuid, Pose)> {
        if let Some(preview) = self.align_preview() {
            return Some(preview);
        }
        let (project, id) = (self.project.as_ref()?, self.selected?);
        let edit = &self.transform_edit;
        (edit.loaded == Some((id, project.current().id)) && edit.changed())
            .then(|| (id, edit.pose()))
    }
    fn layers(&mut self, ui: &mut egui::Ui, p: &Project) {
        let t = self.t;
        let active = &p.current().layers;
        if p.manifest.layers.is_empty() {
            ui.weak(t.no_layers);
            return;
        }
        // Active layers first, newest first.
        let mut layers: Vec<_> = p.manifest.layers.iter().collect();
        layers.reverse();
        layers.sort_by_key(|l| !active.contains(&l.id));
        for layer in layers {
            let mut enabled = active.contains(&layer.id);
            let label = layer_label(t, layer);
            if ui
                .add_enabled(self.job.is_none(), egui::Checkbox::new(&mut enabled, label))
                .changed()
            {
                let id = layer.id;
                self.apply_edit(|p| p.set_layer_enabled(id, enabled));
            }
        }
    }
}
