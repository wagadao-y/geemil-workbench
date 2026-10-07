//! The left panel's scan tree and layers, plus item properties in a dialog.
//! Tree operations are queued while drawing and applied after.
use super::{Workbench, dialogs::Dialog};
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{Group, Pose, Project};
use glam::DQuat;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
use uuid::Uuid;

enum TreeAction {
    Select(Uuid, egui::Modifiers),
    Focus(Uuid),
    Properties(Uuid),
    SetVisible(Vec<Uuid>, bool),
    ShowOnly(Vec<Uuid>),
    ShowAll,
    Move(Vec<Uuid>, Option<Uuid>),
    NewFolder(Option<Uuid>),
    Rename(Uuid, String),
    /// Rename these scans together.
    BulkRename(Vec<Uuid>),
    /// Dissolve these folders together.
    Ungroup(Vec<Uuid>),
    /// Take these scans and folders out, the folders with their contents.
    Remove(Vec<Uuid>),
    /// Place this panorama with the panorama tool.
    PlacePanorama(Uuid),
}

#[cfg(test)]
mod selection_tests {
    use super::TreeSelection;
    use eframe::egui::Modifiers;
    use std::collections::BTreeSet;
    use uuid::Uuid;

    #[test]
    fn ctrl_toggles_items_and_plain_click_replaces_the_selection() {
        let [a, b, c] = [(); 3].map(|_| Uuid::new_v4());
        let mut selected = TreeSelection::default();
        selected.select(a, Modifiers::NONE);
        selected.select(b, Modifiers::CTRL);
        assert_eq!(selected.items, BTreeSet::from([a, b]));
        selected.select(a, Modifiers::CTRL);
        assert_eq!(selected.items, BTreeSet::from([b]));
        selected.select(c, Modifiers::NONE);
        assert_eq!(selected.items, BTreeSet::from([c]));
    }

    #[test]
    fn shift_ranges_follow_visible_rows_in_both_directions_and_can_be_added() {
        let [a, b, c, d, e] = [(); 5].map(|_| Uuid::new_v4());
        let mut selected = TreeSelection {
            order: vec![a, b, c, d, e],
            ..Default::default()
        };
        selected.select(d, Modifiers::NONE);
        selected.select(b, Modifiers::SHIFT);
        assert_eq!(selected.items, BTreeSet::from([b, c, d]));
        selected.select(e, Modifiers::SHIFT);
        assert_eq!(selected.items, BTreeSet::from([d, e]));
        selected.select(a, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(selected.items, BTreeSet::from([a, b, c, d, e]));
        // A collapsed/removed anchor must not select an unrelated range.
        selected.order = vec![a, b];
        selected.retain(&[a, b]);
        selected.select(b, Modifiers::SHIFT);
        assert_eq!(selected.items, BTreeSet::from([b]));
    }
}

#[derive(Default)]
pub(super) struct TreeSelection {
    items: BTreeSet<Uuid>,
    anchor: Option<Uuid>,
    order: Vec<Uuid>,
}
impl TreeSelection {
    pub(super) fn first(&self) -> Option<Uuid> {
        self.items.iter().next().copied()
    }
    fn select(&mut self, id: Uuid, modifiers: egui::Modifiers) {
        let additive = modifiers.command || modifiers.ctrl;
        if modifiers.shift
            && let Some(start) = self
                .anchor
                .and_then(|id| self.order.iter().position(|i| *i == id))
            && let Some(end) = self.order.iter().position(|i| *i == id)
        {
            if !additive {
                self.items.clear();
            }
            self.items
                .extend(self.order[start.min(end)..=start.max(end)].iter().copied());
        } else if additive {
            if !self.items.remove(&id) {
                self.items.insert(id);
            }
            self.anchor = Some(id);
        } else {
            self.items = BTreeSet::from([id]);
            self.anchor = Some(id);
        }
    }
    pub(super) fn retain(&mut self, items: &[Uuid]) {
        self.items.retain(|id| items.contains(id));
        self.anchor = self.anchor.filter(|id| items.contains(id));
    }
}

/// Each folder's scans at any depth and their points, derived once per project
/// state rather than for every folder row in every frame.
#[derive(Default)]
pub(super) struct FolderSummary {
    state: Option<Uuid>,
    folders: HashMap<Uuid, (Vec<Uuid>, u64)>,
}
impl FolderSummary {
    fn update(&mut self, p: &Project) {
        let state = p.current();
        if self.state == Some(state.id) {
            return;
        }
        let parents: HashMap<Uuid, Option<Uuid>> =
            state.groups.iter().map(|g| (g.id, g.parent)).collect();
        let mut folders: HashMap<Uuid, (Vec<Uuid>, u64)> = state
            .groups
            .iter()
            .map(|g| (g.id, Default::default()))
            .collect();
        for scan in p.scans() {
            let mut folder = state.scan_groups.get(&scan.id).copied();
            // Validated states are acyclic; the bound only guards corrupt input.
            for _ in 0..=parents.len() {
                let Some(id) = folder else { break };
                if let Some((scans, points)) = folders.get_mut(&id) {
                    scans.push(scan.id);
                    *points += scan.records;
                }
                folder = parents.get(&id).copied().flatten();
            }
        }
        self.folders = folders;
        self.state = Some(state.id);
    }
    /// The scans in a folder and below it, in import order, and their points.
    fn get(&self, folder: Uuid) -> (&[Uuid], u64) {
        self.folders
            .get(&folder)
            .map_or((&[][..], 0), |(scans, points)| (scans.as_slice(), *points))
    }
}

#[derive(Clone)]
struct TreeDrag(Vec<Uuid>);

/// Visible folder regions, children before parents so the deepest target wins.
struct FolderDropTarget {
    folder: Uuid,
    rect: egui::Rect,
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
    pub(super) fn select_tree_item(&mut self, id: Option<Uuid>) {
        self.selected = id;
        self.tree_selection.items = id.into_iter().collect();
        self.tree_selection.anchor = id;
    }
    /// Single-item tools never silently operate on one item of a multi-selection.
    pub(super) fn single_tree_item(&self) -> Option<Uuid> {
        (self.tree_selection.items.len() <= 1)
            .then_some(self.selected)
            .flatten()
    }
    fn context_items(&self, p: &Project, item: Uuid) -> Vec<Uuid> {
        let items = if self.tree_selection.items.contains(&item) {
            self.tree_selection
                .items
                .iter()
                .copied()
                .collect::<Vec<_>>()
        } else {
            vec![item]
        };
        // A selected folder already carries its descendants during a move.
        items
            .iter()
            .copied()
            .filter(|id| {
                !items
                    .iter()
                    .any(|ancestor| ancestor != id && within(p, *id, *ancestor))
            })
            .collect()
    }
    /// The folders of the selection when it holds `item`, folders inside
    /// selected ones too, else `item` alone.
    fn context_folders(&self, p: &Project, item: Uuid) -> Vec<Uuid> {
        if !self.tree_selection.items.contains(&item) {
            return vec![item];
        }
        p.groups()
            .iter()
            .map(|g| g.id)
            .filter(|id| self.tree_selection.items.contains(id))
            .collect()
    }
    /// The scans and panoramas of the selection when it holds `item`, else
    /// of `item`, inside selected folders too: what visibility and the bulk
    /// rename apply to.
    fn context_shown(&self, p: &Project, item: Uuid) -> Vec<Uuid> {
        let mut ids = self.context_scans(p, item);
        let items = self.context_items(p, item);
        ids.extend(
            p.panoramas()
                .map(|pano| pano.id)
                .filter(|id| items.iter().any(|item| within(p, *id, *item))),
        );
        ids
    }
    fn context_scans(&self, p: &Project, item: Uuid) -> Vec<Uuid> {
        self.context_items(p, item)
            .into_iter()
            .flat_map(|id| p.scans_within(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    /// The project tree, or a strip with a button to open it when folded.
    pub(super) fn side_panel(&mut self, ui: &mut egui::Ui) {
        let t = self.t;
        let mut expanded = !self.settings.tree_collapsed;
        let mut toggle = false;
        egui::Panel::show_switched(
            ui,
            &mut expanded,
            egui::Panel::left("project folded")
                .resizable(false)
                .exact_size(32.),
            egui::Panel::left("project")
                .resizable(true)
                .default_size(300.),
            |ui, open| {
                if !open {
                    ui.add_space(4.);
                    toggle = ui
                        .small_button(icon::CARET_DOUBLE_RIGHT)
                        .on_hover_text(t.tree_expand)
                        .clicked();
                    ui.add_space(4.);
                    ui.label(icon::TREE_STRUCTURE).on_hover_text(t.tree_expand);
                    return;
                }
                let Some(p) = self.project.clone() else {
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            toggle = ui
                                .small_button(icon::CARET_DOUBLE_LEFT)
                                .on_hover_text(t.tree_collapse)
                                .clicked();
                        });
                    });
                    ui.add_space(8.);
                    ui.label(t.no_project_hint);
                    return;
                };
                self.folder_summary.update(&p);
                let mut actions = vec![];
                let mut layer_actions = vec![];
                ui.horizontal(|ui| {
                    ui.strong(format!("{} {}", icon::TREE_STRUCTURE, p.manifest.name));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        toggle = ui
                            .small_button(icon::CARET_DOUBLE_LEFT)
                            .on_hover_text(t.tree_collapse)
                            .clicked();
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
                if self.tree_selection.items.len() > 1 {
                    ui.small((t.tree_selected)(self.tree_selection.items.len()));
                }
                ui.separator();
                egui::Panel::bottom("tree layers")
                    .resizable(true)
                    .default_size(180.)
                    .min_size(80.)
                    .max_size((ui.available_height() - 120.).max(80.))
                    .show(ui, |ui| {
                        ui.strong(format!("{} {}", icon::STACK, t.layers));
                        egui::ScrollArea::vertical()
                            .id_salt("layers")
                            .show(ui, |ui| {
                                self.layers(ui, &p, &mut layer_actions);
                            });
                    });
                let mut order = vec![];
                egui::ScrollArea::vertical()
                    .id_salt("tree")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut targets = vec![];
                        self.tree_level(ui, &p, None, &mut actions, &mut targets, &mut order);
                        self.folder_drop(ui, &p, &targets, &mut actions);
                        if egui::DragAndDrop::has_payload_of_type::<TreeDrag>(ui.ctx()) {
                            let (_, dropped) = ui.dnd_drop_zone::<TreeDrag, ()>(
                                egui::Frame::group(ui.style()),
                                |ui| {
                                    ui.small(t.drop_to_top);
                                },
                            );
                            if let Some(id) = dropped {
                                actions.push(TreeAction::Move(id.0.clone(), None));
                            }
                        }
                    });
                self.tree_selection.order = order;
                self.reveal = None;
                for action in actions {
                    self.tree_action(action);
                }
                let ctx = ui.ctx().clone();
                for action in layer_actions {
                    self.layer_action(&ctx, action);
                }
            },
        );
        // Dragging the edge past its limit folds the tree as well.
        self.settings.tree_collapsed = !(expanded ^ toggle);
    }

    fn tree_level(
        &self,
        ui: &mut egui::Ui,
        p: &Arc<Project>,
        parent: Option<Uuid>,
        actions: &mut Vec<TreeAction>,
        targets: &mut Vec<FolderDropTarget>,
        order: &mut Vec<Uuid>,
    ) {
        let t = self.t;
        let (groups, scans) = p.children(parent);
        for group in groups {
            order.push(group.id);
            let id = ui.make_persistent_id(group.id);
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                true,
            );
            if self.reveals(p, group.id) {
                state.set_open(true);
            }
            let (_, header, body) = state
                .show_header(ui, |ui| {
                    let (inside, points) = self.folder_summary.get(group.id);
                    let panoramas: Vec<Uuid> = p
                        .panoramas()
                        .map(|pano| pano.id)
                        .filter(|id| within(p, *id, group.id))
                        .collect();
                    let shown = inside.iter().filter(|s| self.visible.contains(s)).count()
                        + panoramas
                            .iter()
                            .filter(|id| !self.hidden_panoramas.contains(id))
                            .count();
                    let total = inside.len() + panoramas.len();
                    let mut all = total > 0 && shown == total;
                    let checkbox =
                        egui::Checkbox::new(&mut all, "").indeterminate(shown > 0 && shown < total);
                    if ui.add(checkbox).changed() {
                        actions.push(TreeAction::SetVisible(self.context_shown(p, group.id), all));
                    }
                    let label = format!("{} {}", icon::FOLDER, group.name);
                    let row = self.tree_row(ui, p, group.id, label, actions);
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
                                actions
                                    .push(TreeAction::Ungroup(self.context_folders(p, group.id)));
                            }
                            self.bulk_rename_menu(ui, p, group.id, actions);
                            ui.separator();
                            self.visibility_menu(ui, self.context_shown(p, group.id), actions);
                            ui.separator();
                            self.remove_menu(ui, p, group.id, actions);
                            ui.separator();
                            if ui
                                .button(format!("{} {}", icon::INFO, t.properties))
                                .clicked()
                            {
                                actions.push(TreeAction::Properties(group.id));
                            }
                        });
                    self.registration_mark(ui, p, group.id);
                })
                .body(|ui| {
                    // Indent the contents past the folder's checkbox too, not
                    // just its toggle, so they read as one level deeper.
                    let indent = ui.spacing().indent;
                    let vline = ui.visuals().indent_has_left_vline;
                    let s = ui.spacing_mut();
                    s.indent = s.icon_width + s.item_spacing.x;
                    ui.visuals_mut().indent_has_left_vline = false;
                    ui.indent("contents", |ui| {
                        ui.spacing_mut().indent = indent;
                        ui.visuals_mut().indent_has_left_vline = vline;
                        self.tree_level(ui, p, Some(group.id), actions, targets, order)
                    })
                });
            let bottom = body.as_ref().map_or(header.response.rect.bottom(), |body| {
                body.response.rect.bottom()
            });
            targets.push(FolderDropTarget {
                folder: group.id,
                rect: egui::Rect::from_min_max(
                    egui::pos2(ui.max_rect().left(), header.response.rect.top()),
                    egui::pos2(ui.max_rect().right(), bottom),
                ),
            });
        }
        for scan in scans {
            order.push(scan.id);
            ui.horizontal(|ui| {
                let mut shown = self.visible.contains(&scan.id);
                if ui.checkbox(&mut shown, "").changed() {
                    actions.push(TreeAction::SetVisible(
                        self.context_shown(p, scan.id),
                        shown,
                    ));
                }
                let label = format!("{} {}", icon::CUBE_TRANSPARENT, p.scan_name(scan));
                let row = self.tree_row(ui, p, scan.id, label, actions);
                row.on_hover_text((t.scan_points)(&t.count(scan.records)))
                    .context_menu(|ui| {
                        if self.context_items(p, scan.id) == [scan.id]
                            && ui
                                .button(format!("{} {}", icon::PENCIL_SIMPLE, t.rename))
                                .clicked()
                        {
                            actions.push(TreeAction::Rename(scan.id, p.scan_name(scan).into()));
                        }
                        self.bulk_rename_menu(ui, p, scan.id, actions);
                        self.move_menu(ui, p, scan.id, actions);
                        ui.separator();
                        self.visibility_menu(ui, self.context_shown(p, scan.id), actions);
                        ui.separator();
                        self.remove_menu(ui, p, scan.id, actions);
                        ui.separator();
                        if ui
                            .button(format!("{} {}", icon::INFO, t.properties))
                            .clicked()
                        {
                            actions.push(TreeAction::Properties(scan.id));
                        }
                    });
                self.registration_mark(ui, p, scan.id);
            });
        }
        for panorama in p.panoramas_in(parent) {
            let id = panorama.id;
            order.push(id);
            ui.horizontal(|ui| {
                // Shows or hides its marker.
                let mut shown = !self.hidden_panoramas.contains(&id);
                if ui.checkbox(&mut shown, "").changed() {
                    actions.push(TreeAction::SetVisible(self.context_shown(p, id), shown));
                }
                let label = format!("{} {}", icon::PANORAMA, p.panorama_name(panorama));
                let row = self.tree_row(ui, p, id, label, actions);
                let hover = format!(
                    "{} × {}・{}",
                    panorama.width, panorama.height, panorama.source_name
                );
                row.on_hover_text(hover).context_menu(|ui| {
                    if ui
                        .button(format!("{} {}", icon::PANORAMA, t.panorama_place))
                        .clicked()
                    {
                        actions.push(TreeAction::PlacePanorama(id));
                    }
                    if self.context_items(p, id) == [id]
                        && ui
                            .button(format!("{} {}", icon::PENCIL_SIMPLE, t.rename))
                            .clicked()
                    {
                        actions.push(TreeAction::Rename(id, p.panorama_name(panorama).into()));
                    }
                    self.bulk_rename_menu(ui, p, id, actions);
                    self.move_menu(ui, p, id, actions);
                    ui.separator();
                    self.visibility_menu(ui, self.context_shown(p, id), actions);
                    ui.separator();
                    self.remove_menu(ui, p, id, actions);
                    ui.separator();
                    if ui
                        .button(format!("{} {}", icon::INFO, t.properties))
                        .clicked()
                    {
                        actions.push(TreeAction::Properties(id));
                    }
                });
                self.registration_mark(ui, p, id);
            });
        }
    }
    /// Accept drops anywhere in a folder's visible subtree, without intercepting
    /// scan selection, visibility checkboxes or the deeper folders' targets.
    fn folder_drop(
        &self,
        ui: &egui::Ui,
        p: &Project,
        targets: &[FolderDropTarget],
        actions: &mut Vec<TreeAction>,
    ) {
        let Some(items) = egui::DragAndDrop::payload::<TreeDrag>(ui.ctx()) else {
            return;
        };
        let Some(target) = targets
            .iter()
            .find(|target| ui.rect_contains_pointer(target.rect))
        else {
            return;
        };
        // Do not fall back to an outer folder when the innermost target is invalid.
        if items.0.iter().any(|item| within(p, target.folder, *item))
            || items
                .0
                .iter()
                .all(|item| p.parent_of(*item) == Some(target.folder))
        {
            return;
        }
        ui.painter().rect_stroke(
            target.rect.intersect(ui.clip_rect()),
            2.,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Inside,
        );
        if ui.input(|i| i.pointer.any_released())
            && let Some(items) = egui::DragAndDrop::take_payload::<TreeDrag>(ui.ctx())
        {
            actions.push(TreeAction::Move(items.0.clone(), Some(target.folder)));
        }
    }
    /// A selectable, draggable tree label.
    fn tree_row(
        &self,
        ui: &mut egui::Ui,
        p: &Project,
        id: Uuid,
        label: String,
        actions: &mut Vec<TreeAction>,
    ) -> egui::Response {
        let selected = self.tree_selection.items.contains(&id) || self.selected == Some(id);
        let row =
            ui.add(egui::Button::selectable(selected, label).sense(egui::Sense::click_and_drag()));
        if row.double_clicked() {
            actions.push(TreeAction::Select(id, egui::Modifiers::NONE));
            actions.push(TreeAction::Focus(id));
        } else if row.clicked() {
            actions.push(TreeAction::Select(id, ui.input(|i| i.modifiers)));
        }
        if (row.secondary_clicked() || row.drag_started()) && !selected {
            actions.push(TreeAction::Select(id, egui::Modifiers::NONE));
        }
        if row.dragged() {
            row.dnd_set_drag_payload(TreeDrag(self.context_items(p, id)));
        }
        if self.reveal == Some(id) {
            row.scroll_to_me(Some(egui::Align::Center));
        }
        row
    }
    /// Whether `group` holds the item to reveal, so it opens to show it.
    fn reveals(&self, p: &Project, group: Uuid) -> bool {
        let mut at = self.reveal.and_then(|id| p.parent_of(id));
        while let Some(g) = at {
            if g == group {
                return true;
            }
            at = p.parent_of(g);
        }
        false
    }
    /// Renaming the scans of the selection (or of `item`) together, when
    /// there are several.
    fn bulk_rename_menu(
        &self,
        ui: &mut egui::Ui,
        p: &Project,
        item: Uuid,
        actions: &mut Vec<TreeAction>,
    ) {
        let scans = self.context_shown(p, item);
        if scans.len() > 1
            && ui
                .button(format!(
                    "{} {}",
                    icon::PENCIL_SIMPLE_LINE,
                    self.t.bulk_rename
                ))
                .clicked()
        {
            actions.push(TreeAction::BulkRename(scans));
        }
    }
    /// Taking the selection (or `item`) out of the project.
    fn remove_menu(
        &self,
        ui: &mut egui::Ui,
        p: &Project,
        item: Uuid,
        actions: &mut Vec<TreeAction>,
    ) {
        if ui
            .button(format!("{} {}", icon::TRASH, self.t.remove_scans))
            .clicked()
        {
            actions.push(TreeAction::Remove(self.context_items(p, item)));
        }
    }
    fn move_menu(&self, ui: &mut egui::Ui, p: &Project, item: Uuid, actions: &mut Vec<TreeAction>) {
        let t = self.t;
        let items = self.context_items(p, item);
        ui.menu_button(
            format!("{} {}", icon::ARROW_BEND_DOWN_RIGHT, t.move_to),
            |ui| {
                if ui.button(t.top_level).clicked() {
                    actions.push(TreeAction::Move(items.clone(), None));
                }
                for (depth, g) in folder_list(p) {
                    let allowed = !items.iter().any(|id| within(p, g.id, *id))
                        && !items.iter().all(|id| p.parent_of(*id) == Some(g.id));
                    let label = format!("{}{} {}", "　".repeat(depth), icon::FOLDER, g.name);
                    if ui.add_enabled(allowed, egui::Button::new(label)).clicked() {
                        actions.push(TreeAction::Move(items.clone(), Some(g.id)));
                    }
                }
            },
        );
    }
    fn visibility_menu(&self, ui: &mut egui::Ui, scans: Vec<Uuid>, actions: &mut Vec<TreeAction>) {
        let t = self.t;
        if ui.button(format!("{} {}", icon::EYE, t.show)).clicked() {
            actions.push(TreeAction::SetVisible(scans.clone(), true));
        }
        if ui
            .button(format!("{} {}", icon::EYE_SLASH, t.hide))
            .clicked()
        {
            actions.push(TreeAction::SetVisible(scans.clone(), false));
        }
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
            TreeAction::Focus(id) => {
                if self
                    .project
                    .as_ref()
                    .is_some_and(|p| p.panorama(id).is_some())
                {
                    self.set_tool(super::selection::Tool::Panorama);
                } else {
                    self.focus_tree_item(id);
                }
            }
            TreeAction::PlacePanorama(id) => {
                self.select_tree_item(Some(id));
                self.set_tool(super::selection::Tool::Panorama);
            }
            TreeAction::Select(id, modifiers) => {
                self.tree_selection.select(id, modifiers);
                self.selected = self
                    .tree_selection
                    .items
                    .contains(&id)
                    .then_some(id)
                    .or_else(|| self.tree_selection.items.iter().next().copied());
                self.transform_edit = TransformEdit::default();
            }
            TreeAction::Properties(id) => {
                self.dialog = Some(Dialog::Properties { id });
                self.transform_edit = TransformEdit::default();
            }
            TreeAction::SetVisible(ids, shown) => {
                let panoramas = self.panorama_ids();
                for id in ids {
                    // Scans load their points; panoramas show their marker.
                    let (set, add) = if panoramas.contains(&id) {
                        (&mut self.hidden_panoramas, !shown)
                    } else {
                        (&mut self.visible, shown)
                    };
                    if add {
                        set.insert(id);
                    } else {
                        set.remove(&id);
                    }
                }
                self.visibility_changed();
            }
            TreeAction::ShowOnly(ids) => {
                let panoramas = self.panorama_ids();
                self.hidden_panoramas = panoramas
                    .iter()
                    .copied()
                    .filter(|id| !ids.contains(id))
                    .collect();
                self.visible = ids
                    .into_iter()
                    .filter(|id| !panoramas.contains(id))
                    .collect();
                self.visibility_changed();
            }
            TreeAction::ShowAll => {
                if let Some(p) = &self.project {
                    self.visible = p.scans().map(|s| s.id).collect();
                }
                self.hidden_panoramas.clear();
                self.visibility_changed();
            }
            TreeAction::Move(ids, target) => {
                self.apply_edit(|p| p.move_to_group(&ids, target));
            }
            TreeAction::NewFolder(parent) => {
                let name = self.t.default_folder_name.to_owned();
                if let Some(id) = self.apply_edit(|p| p.create_group(name.clone(), parent)) {
                    self.select_tree_item(Some(id));
                    self.dialog = Some(Dialog::Rename { id, name });
                }
            }
            TreeAction::Rename(id, name) => self.dialog = Some(Dialog::Rename { id, name }),
            TreeAction::Ungroup(ids) => {
                self.apply_edit(|p| p.ungroup(&ids));
            }
            TreeAction::BulkRename(scans) => {
                if let Some(p) = &self.project {
                    let rows = rename_rows(p, &scans);
                    self.dialog = Some(Dialog::BulkRename { rows });
                }
            }
            TreeAction::Remove(ids) => {
                self.apply_edit(|p| p.remove_items(&ids));
            }
        }
    }
    /// The panoramas of the current state.
    fn panorama_ids(&self) -> BTreeSet<Uuid> {
        self.project
            .as_ref()
            .map(|p| p.panoramas().map(|pano| pano.id).collect())
            .unwrap_or_default()
    }
    fn visibility_changed(&mut self) {
        self.view.invalidate();
        self.dirty = true;
    }

    pub(super) fn properties_dialog(&mut self, ctx: &egui::Context, id: Uuid) {
        let Some(p) = self.project.clone() else {
            self.dialog = None;
            return;
        };
        let mut actions = vec![];
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("properties dialog")).show(ctx, |ui| {
            ui.set_width(540.);
            ui.heading(format!("{} {}", icon::INFO, self.t.properties));
            egui::ScrollArea::vertical()
                .max_height(480.)
                .show(ui, |ui| {
                    self.properties(ui, &p, id, &mut actions);
                });
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.button(self.t.close).clicked();
            });
        });
        if close || response.should_close() {
            self.dialog = None;
            self.transform_edit = TransformEdit::default();
        }
        for action in actions {
            self.tree_action(action);
        }
    }

    fn properties(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        id: Uuid,
        actions: &mut Vec<TreeAction>,
    ) {
        let t = self.t;
        if let Some(scan) = p.scans().find(|s| s.id == id) {
            egui::Grid::new("scan properties")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(t.prop_name);
                    ui.horizontal(|ui| {
                        ui.label(p.scan_name(scan));
                        if ui
                            .small_button(icon::PENCIL_SIMPLE)
                            .on_hover_text(t.rename)
                            .clicked()
                        {
                            actions.push(TreeAction::Rename(id, p.scan_name(scan).into()));
                        }
                    });
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
        } else if let Some(panorama) = p.panorama(id) {
            egui::Grid::new("panorama properties")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(t.prop_name);
                    ui.horizontal(|ui| {
                        ui.label(p.panorama_name(panorama));
                        if ui
                            .small_button(icon::PENCIL_SIMPLE)
                            .on_hover_text(t.rename)
                            .clicked()
                        {
                            actions.push(TreeAction::Rename(id, p.panorama_name(panorama).into()));
                        }
                    });
                    ui.end_row();
                    ui.label(t.prop_source);
                    ui.label(&panorama.source_name);
                    ui.end_row();
                    ui.label(t.panorama_size);
                    ui.label(format!("{} × {}", panorama.width, panorama.height));
                    ui.end_row();
                    ui.label(t.panorama_pairs);
                    ui.label(p.panorama_pairs(id).len().to_string());
                    ui.end_row();
                    ui.label(t.panorama_position);
                    if p.registration(id).is_some() {
                        let at = p.correction(id).transform_point3(glam::DVec3::ZERO);
                        ui.label(format!("{:.3}, {:.3}, {:.3}", at.x, at.y, at.z));
                    } else {
                        ui.weak(t.panorama_unplaced);
                    }
                    ui.end_row();
                });
        } else if let Some(group) = p.groups().iter().find(|g| g.id == id) {
            self.folder_summary.update(p);
            let (inside, points) = self.folder_summary.get(id);
            let scans = inside.len() as u64;
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
                    ui.label(t.count(scans));
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
        if let Some(preview) = self.align_preview().or_else(|| self.gizmo_preview()) {
            return Some(preview);
        }
        let id = match &self.dialog {
            Some(Dialog::Properties { id }) => *id,
            _ => self.single_tree_item()?,
        };
        let project = self.project.as_ref()?;
        let edit = &self.transform_edit;
        (edit.loaded == Some((id, project.current().id)) && edit.changed())
            .then(|| (id, edit.pose()))
    }
}

/// A scan in the bulk rename table: its folder, current name and the new
/// name typed for it, empty to keep the current one.
pub(super) struct RenameRow {
    pub(super) id: Uuid,
    pub(super) folder: String,
    pub(super) old: String,
    pub(super) new: String,
}

/// Rows for `items` (scans and panoramas) in the order the tree shows them.
pub(super) fn rename_rows(p: &Project, items: &[Uuid]) -> Vec<RenameRow> {
    let mut order = vec![];
    tree_scans(p, None, &mut order);
    order
        .into_iter()
        .filter(|id| items.contains(id))
        .filter_map(|id| {
            let name = match (p.scan(id), p.panorama(id)) {
                (Some(scan), _) => p.scan_name(scan),
                (_, Some(panorama)) => p.panorama_name(panorama),
                _ => return None,
            };
            Some(RenameRow {
                id,
                folder: p
                    .parent_of(id)
                    .and_then(|id| p.groups().iter().find(|g| g.id == id))
                    .map_or(String::new(), |g| g.name.clone()),
                old: name.to_owned(),
                new: String::new(),
            })
        })
        .collect()
}

/// The scans and panoramas below `parent` as the tree shows them: each
/// folder's contents first, then the scans and the panoramas at this level.
fn tree_scans(p: &Project, parent: Option<Uuid>, order: &mut Vec<Uuid>) {
    let (groups, scans) = p.children(parent);
    for group in groups {
        tree_scans(p, Some(group.id), order);
    }
    order.extend(scans.iter().map(|s| s.id));
    order.extend(p.panoramas_in(parent).iter().map(|pano| pano.id));
}

/// The rows as tab-separated lines under `header`, which spreadsheets paste
/// as columns.
pub(super) fn rename_table(rows: &[RenameRow], header: [&str; 3]) -> String {
    let cell = |text: &str| text.replace(['\t', '\n', '\r'], " ");
    let mut table = header.join("\t");
    for row in rows {
        table.push('\n');
        table.push_str(&[cell(&row.folder), cell(&row.old), cell(&row.new)].join("\t"));
    }
    table
}

/// New names from pasted spreadsheet cells, one per line: the last column of
/// each, so either the new-name column alone or whole rows of the table.
/// A first line that is the table's header is left out.
pub(super) fn pasted_names(text: &str, header: [&str; 3]) -> Vec<String> {
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines
        .first()
        .is_some_and(|l| l.split('\t').map(str::trim).eq(header))
    {
        lines.remove(0);
    }
    lines
        .iter()
        .map(|line| line.rsplit('\t').next().unwrap_or("").trim().to_owned())
        .collect()
}

#[cfg(test)]
mod rename_tests {
    use super::{RenameRow, pasted_names, rename_table};
    use uuid::Uuid;

    const HEADER: [&str; 3] = ["Folder", "Before", "After"];

    #[test]
    fn tables_copy_as_columns_and_pastes_take_the_last_column() {
        let rows =
            [("B1F", "scan01", ""), ("B1F", "scan\t02", "North")].map(|(f, o, n)| RenameRow {
                id: Uuid::new_v4(),
                folder: f.into(),
                old: o.into(),
                new: n.into(),
            });
        let table = rename_table(&rows, HEADER);
        assert_eq!(
            table,
            "Folder\tBefore\tAfter\nB1F\tscan01\t\nB1F\tscan 02\tNorth"
        );
        // Whole rows back from a spreadsheet, header included.
        assert_eq!(pasted_names(&format!("{table}\r\n"), HEADER), ["", "North"]);
        // One column, with Windows line ends.
        assert_eq!(pasted_names("A \r\nB\r\n", HEADER), ["A", "B"]);
        assert!(pasted_names("", HEADER).is_empty());
    }
}
