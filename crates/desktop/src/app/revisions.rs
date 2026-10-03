//! The revision list: a git-style graph of saved revisions to open, rename or delete.
use super::{
    Workbench,
    dialogs::{AfterDiscard, Dialog},
    jobs::Notice,
};
use crate::i18n::Strings;
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{Layer, LayerKind, Project, Revision};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct RevisionsState {
    selected: Option<Uuid>,
    renaming: Option<(Uuid, String)>,
    /// Delete needs a second click.
    confirm_delete: Option<Uuid>,
}
impl RevisionsState {
    pub(super) fn new(project: Option<&Project>) -> Self {
        Self {
            selected: project.map(|p| p.manifest.current),
            ..Default::default()
        }
    }
}

/// One row of the revision graph, laid out like a git history graph: newest
/// first, a straight line while history is linear, a new lane per branch.
#[derive(Debug, PartialEq)]
struct GraphRow {
    id: Uuid,
    /// The lane this row's dot sits in.
    lane: usize,
    /// Lanes running through this row to rows further down.
    through: Vec<usize>,
    /// Lanes of children above that end in this dot.
    from_above: Vec<usize>,
    /// Whether a line continues down to the parent.
    to_parent: bool,
}

/// Lays out `(id, parent)` pairs given newest first, so children come before
/// their parents. A parent missing from the list ends the line.
fn graph_layout(nodes: &[(Uuid, Option<Uuid>)]) -> Vec<GraphRow> {
    let ids: Vec<_> = nodes.iter().map(|(id, _)| *id).collect();
    // The id each lane is waiting for, top to bottom.
    let mut lanes: Vec<Option<Uuid>> = vec![];
    let mut rows = vec![];
    for (id, parent) in nodes {
        let from_above: Vec<_> = (0..lanes.len())
            .filter(|l| lanes[*l] == Some(*id))
            .collect();
        let through: Vec<_> = (0..lanes.len())
            .filter(|l| lanes[*l].is_some_and(|w| w != *id))
            .collect();
        let lane = from_above.first().copied().unwrap_or_else(|| {
            lanes.iter().position(Option::is_none).unwrap_or_else(|| {
                lanes.push(None);
                lanes.len() - 1
            })
        });
        for l in &from_above {
            lanes[*l] = None;
        }
        let parent = parent.filter(|p| ids.contains(p));
        lanes[lane] = parent;
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
        rows.push(GraphRow {
            id: *id,
            lane,
            through,
            from_above,
            to_parent: parent.is_some(),
        });
    }
    rows
}

const LANE: f32 = 16.;
const ROW: f32 = 26.;

fn lane_color(lane: usize) -> egui::Color32 {
    const COLORS: [egui::Color32; 6] = [
        egui::Color32::from_rgb(86, 156, 214),
        egui::Color32::from_rgb(206, 145, 120),
        egui::Color32::from_rgb(106, 190, 120),
        egui::Color32::from_rgb(197, 134, 192),
        egui::Color32::from_rgb(220, 200, 90),
        egui::Color32::from_rgb(78, 201, 176),
    ];
    COLORS[lane % COLORS.len()]
}

/// Draws one row's lines and dot into `rect`.
fn paint_graph(
    painter: &egui::Painter,
    rect: egui::Rect,
    row: &GraphRow,
    current: bool,
    draft: bool,
) {
    let x = |lane: usize| rect.left() + LANE / 2. + lane as f32 * LANE;
    let (top, mid, bottom) = (rect.top(), rect.center().y, rect.bottom());
    let stroke = |lane| egui::Stroke::new(2., lane_color(lane));
    for &l in &row.through {
        painter.line_segment([egui::pos2(x(l), top), egui::pos2(x(l), bottom)], stroke(l));
    }
    let dot = egui::pos2(x(row.lane), mid);
    for &l in &row.from_above {
        // Branches above curve into the dot they started from.
        let start = egui::pos2(x(l), top);
        let shape = egui::epaint::CubicBezierShape::from_points_stroke(
            [start, egui::pos2(x(l), mid), egui::pos2(dot.x, top), dot],
            false,
            egui::Color32::TRANSPARENT,
            stroke(l),
        );
        painter.add(shape);
    }
    if row.to_parent {
        painter.line_segment([dot, egui::pos2(dot.x, bottom)], stroke(row.lane));
    }
    let color = lane_color(row.lane);
    if draft {
        painter.circle(dot, 4.5, egui::Color32::BLACK, egui::Stroke::new(2., color));
    } else if current {
        painter.circle(dot, 6., egui::Color32::BLACK, egui::Stroke::new(2.5, color));
        painter.circle_filled(dot, 3., color);
    } else {
        painter.circle_filled(dot, 4.5, color);
    }
}

/// A revision's title: the user's name, or for revisions from versions that
/// committed every edit, a label from the recorded operation.
pub(super) fn revision_title(t: &Strings, p: &Project, r: &Revision) -> String {
    let op = &r.operation;
    let text = |key| op.get(key).and_then(|v| v.as_str());
    let id = |key| text(key).and_then(|s| Uuid::parse_str(s).ok());
    let layer = |id: Uuid| p.manifest.layers.iter().find(|l| l.id == id);
    let legacy = match text("kind") {
        Some("edits") => None,
        Some("create") => Some(t.revision_created.into()),
        Some("import") => text("file").map(t.revision_import),
        Some("selection") => r.layers.last().and_then(|id| layer(*id)).map(|l| {
            let crop = op["selection"]["mode"] == "exclude_outside";
            let label = if crop {
                t.revision_crop
            } else {
                t.revision_exclude
            };
            label(&t.count(l.excluded))
        }),
        Some("layer") => id("id").and_then(layer).map(|l| {
            let enabled = op.get("enabled").and_then(|v| v.as_bool());
            (t.revision_layer)(&layer_label(t, l), enabled.unwrap_or(true))
        }),
        Some("transform") => id("scan")
            .and_then(|id| p.manifest.scans.iter().find(|s| s.id == id))
            .map(|s| (t.revision_transform)(&s.name)),
        _ => None,
    };
    legacy.unwrap_or_else(|| r.name.clone())
}

/// Counts of the edits a revision recorded, e.g. "取り込み ×1・除外 ×3".
fn revision_summary(t: &Strings, r: &Revision) -> String {
    let ops = match r.operation.get("operations").and_then(|o| o.as_array()) {
        Some(ops) => ops.iter().collect(),
        None if r.operation.get("kind").and_then(|k| k.as_str()) == Some("create") => vec![],
        None => vec![&r.operation],
    };
    let mut counts: Vec<(&str, usize)> = vec![];
    for op in ops {
        let kind = (t.operation_kind)(op.get("kind").and_then(|k| k.as_str()).unwrap_or(""));
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
        .iter()
        .map(|(kind, n)| format!("{kind} ×{n}"))
        .collect::<Vec<_>>()
        .join("・")
}

pub(super) fn saved_time(r: &Revision) -> String {
    r.saved_at
        .and_then(|s| chrono::DateTime::from_timestamp(s as i64, 0))
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub(super) fn layer_label(t: &Strings, layer: &Layer) -> String {
    let points = t.count(layer.excluded);
    match layer.kind {
        LayerKind::Manual => (t.exclusion_layer)(&points),
        LayerKind::Subsample { size, merged } => {
            (t.subsample_layer)(&size.to_string(), merged, &points)
        }
        LayerKind::Noise {
            radius,
            min_neighbours,
        } => (t.noise_layer)(&radius.to_string(), min_neighbours, &points),
        LayerKind::Box { inside: true } => (t.box_inside_layer)(&points),
        LayerKind::Box { inside: false } => (t.box_outside_layer)(&points),
        LayerKind::Statistical {
            neighbours,
            deviations,
        } => (t.outlier_layer)(neighbours, deviations, &points),
    }
}

impl Workbench {
    /// Shows the revision list. Returns whether it stays open.
    pub(super) fn revisions_dialog(
        &mut self,
        ctx: &egui::Context,
        state: &mut RevisionsState,
    ) -> bool {
        let t = self.t;
        let Some(project) = self.project.clone() else {
            return false;
        };
        let current = project.manifest.current;
        let draft = project.manifest.draft.as_ref();
        let mut keep = true;
        let mut open = None;
        let mut delete = None;
        let mut rename = None;
        let modal = egui::Modal::new(egui::Id::new("revisions")).show(ctx, |ui| {
            ui.set_width(640.);
            ui.heading(format!("{} {}", icon::GIT_BRANCH, t.revisions_title));
            ui.label(t.revisions_lead);
            ui.add_space(4.);
            // Newest first; the unsaved state sits on top of its revision.
            let mut nodes: Vec<_> = draft.map(|d| (d.id, Some(current))).into_iter().collect();
            nodes.extend(
                project
                    .manifest
                    .revisions
                    .iter()
                    .rev()
                    .map(|r| (r.id, r.parent)),
            );
            let rows = graph_layout(&nodes);
            let lanes = rows
                .iter()
                .flat_map(|r| r.through.iter().chain(&r.from_above).chain([&r.lane]))
                .max()
                .map_or(1, |l| l + 1);
            let graph_width = lanes as f32 * LANE + 4.;
            egui::ScrollArea::vertical()
                .max_height(420.)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.;
                    for row in &rows {
                        let width = ui.available_width();
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(width, ROW), egui::Sense::click());
                        let is_draft = draft.is_some_and(|d| d.id == row.id);
                        let revision = project.manifest.revisions.iter().find(|r| r.id == row.id);
                        let selected = state.selected == Some(row.id) && !is_draft;
                        let painter = ui.painter_at(rect);
                        if selected {
                            painter.rect_filled(rect, 3., ui.visuals().selection.bg_fill);
                        } else if response.hovered() && !is_draft {
                            painter.rect_filled(
                                rect,
                                3.,
                                ui.visuals().widgets.hovered.weak_bg_fill,
                            );
                        }
                        let graph =
                            egui::Rect::from_min_size(rect.min, egui::vec2(graph_width, ROW));
                        paint_graph(&painter, graph, row, row.id == current, is_draft);
                        let text_rect = egui::Rect::from_min_max(
                            egui::pos2(graph.right() + 6., rect.top()),
                            rect.max,
                        );
                        if let Some((id, name)) = &mut state.renaming
                            && *id == row.id
                        {
                            let edit_rect = egui::Rect::from_min_size(
                                text_rect.min + egui::vec2(0., 2.),
                                egui::vec2(text_rect.width().min(320.), ROW - 4.),
                            );
                            let edit = ui.put(edit_rect, egui::TextEdit::singleline(name));
                            edit.request_focus();
                            if edit.lost_focus() {
                                rename = Some((row.id, name.clone()));
                            }
                            continue;
                        }
                        let text = ui.visuals().text_color();
                        let weak = ui.visuals().weak_text_color();
                        let mut job = egui::text::LayoutJob::default();
                        let mut push = |s: &str, size: f32, color, italics, strong: bool| {
                            job.append(
                                s,
                                if job.text.is_empty() { 0. } else { 10. },
                                egui::TextFormat {
                                    font_id: egui::FontId::proportional(size),
                                    color: if strong {
                                        ui.visuals().strong_text_color()
                                    } else {
                                        color
                                    },
                                    italics,
                                    valign: egui::Align::Center,
                                    ..Default::default()
                                },
                            );
                        };
                        match (is_draft, revision, draft) {
                            (true, _, Some(d)) => {
                                push(t.unsaved_state, 14., text, true, false);
                                push(&revision_summary(t, d), 11., weak, false, false);
                            }
                            (false, Some(r), _) => {
                                push(
                                    &revision_title(t, &project, r),
                                    14.,
                                    text,
                                    false,
                                    r.id == current,
                                );
                                if r.id == current {
                                    push(t.shown_revision, 11., text, false, true);
                                }
                                push(&saved_time(r), 11., weak, false, false);
                                push(&revision_summary(t, r), 11., weak, false, false);
                            }
                            _ => {}
                        }
                        let galley = ui.fonts_mut(|f| f.layout_job(job));
                        let pos =
                            egui::pos2(text_rect.left(), rect.center().y - galley.size().y / 2.);
                        painter.galley(pos, galley, text);
                        if is_draft {
                            continue;
                        }
                        if response.clicked() {
                            state.selected = Some(row.id);
                            state.confirm_delete = None;
                        }
                        if response.double_clicked() {
                            open = Some(row.id);
                        }
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                let selected = state.selected;
                let can_open = selected.is_some_and(|id| id != current || draft.is_some());
                if ui
                    .add_enabled(
                        can_open,
                        egui::Button::new(format!("{} {}", icon::FOLDER_OPEN, t.open_revision)),
                    )
                    .clicked()
                {
                    open = selected;
                }
                if ui
                    .add_enabled(
                        selected.is_some(),
                        egui::Button::new(format!("{} {}", icon::PENCIL_SIMPLE, t.rename)),
                    )
                    .clicked()
                    && let Some(id) = selected
                {
                    let r = project.manifest.revisions.iter().find(|r| r.id == id);
                    let name = r
                        .map(|r| revision_title(t, &project, r))
                        .unwrap_or_default();
                    state.renaming = Some((id, name));
                }
                let confirming = state.confirm_delete.is_some() && state.confirm_delete == selected;
                let label = if confirming {
                    format!("{} {}?", icon::TRASH, t.delete_revision)
                } else {
                    format!("{} {}", icon::TRASH, t.delete_revision)
                };
                if ui
                    .add_enabled(
                        selected.is_some_and(|id| id != current),
                        egui::Button::new(label),
                    )
                    .clicked()
                {
                    if confirming {
                        delete = selected;
                        state.confirm_delete = None;
                    } else {
                        state.confirm_delete = selected;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let close = if draft.is_some() {
                        t.continue_unsaved
                    } else {
                        t.close
                    };
                    if ui.button(close).clicked() {
                        keep = false;
                    }
                });
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if let Some((id, name)) = rename {
            state.renaming = None;
            self.update_project(|p| p.rename_revision(id, name));
        }
        if let Some(id) = delete {
            self.update_project(|p| p.delete_revision(id));
            state.selected = Some(current);
        }
        if let Some(id) = open {
            if project.has_unsaved_changes() {
                self.dialog = Some(Dialog::Discard {
                    then: AfterDiscard::Switch(id),
                });
                return false;
            }
            self.switch_revision(id);
            keep = false;
        }
        if !keep {
            self.dialog = None;
        }
        keep
    }
    /// Applies a change that is not an edit of the working state (no undo).
    fn update_project(&mut self, change: impl FnOnce(&mut Project) -> anyhow::Result<()>) {
        let Some(project) = &self.project else { return };
        let mut project = (**project).clone();
        match change(&mut project) {
            Ok(()) => self.install(project, false),
            Err(e) => self.error = Some(Notice::new(self.t, &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphRow, graph_layout};
    use uuid::Uuid;

    #[test]
    fn linear_history_stays_in_one_lane_and_branches_get_their_own() {
        // root <- a <- b, and root <- c (a fork), listed newest first.
        let [root, a, b, c] = [(); 4].map(|_| Uuid::new_v4());
        let rows = graph_layout(&[(c, Some(root)), (b, Some(a)), (a, Some(root)), (root, None)]);
        let row = |id, lane, through: &[usize], from_above: &[usize], to_parent| GraphRow {
            id,
            lane,
            through: through.to_vec(),
            from_above: from_above.to_vec(),
            to_parent,
        };
        assert_eq!(
            rows,
            vec![
                row(c, 0, &[], &[], true),
                // b starts a second lane while c's line passes by.
                row(b, 1, &[0], &[], true),
                row(a, 1, &[0], &[1], true),
                // Both branches meet at their common parent, in the leftmost lane.
                row(root, 0, &[], &[0, 1], false),
            ]
        );
    }

    #[test]
    fn missing_parents_end_lines_and_freed_lanes_are_reused() {
        let [a, b, gone] = [(); 3].map(|_| Uuid::new_v4());
        let rows = graph_layout(&[(a, Some(gone)), (b, None)]);
        assert!(!rows[0].to_parent);
        assert_eq!((rows[1].lane, rows[1].through.len()), (0, 0));
    }
}
