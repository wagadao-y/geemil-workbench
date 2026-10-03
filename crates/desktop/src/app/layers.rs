//! The layer list and the choice of where an operation moves points. Every
//! point is in one layer; work applies to the points of visible layers.
use super::{Workbench, dialogs::Dialog};
use crate::i18n::Strings;
use eframe::egui;
use egui_phosphor::regular as icon;
use geemil_core::{DEFAULT_LAYER, Layer, LayerTarget, Project};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A layer's name as shown; the default layer's is translated.
pub(super) fn layer_name<'a>(t: &'a Strings, layer: &'a Layer) -> &'a str {
    if layer.code == DEFAULT_LAYER {
        t.default_layer
    } else {
        &layer.name
    }
}

/// Where an operation moves points: the chosen layer while it exists, else
/// the layer named `default`, which the operation creates if missing.
pub(super) fn destination(p: &Project, chosen: Option<u8>, default: &str) -> LayerTarget {
    match chosen.filter(|code| p.layer(*code).is_some()) {
        Some(code) => LayerTarget::Existing(code),
        None => p.layer_named(default),
    }
}

/// A labelled combo box choosing the layer an operation moves points to.
pub(super) fn destination_combo(
    ui: &mut egui::Ui,
    t: &Strings,
    p: &Project,
    id_salt: &str,
    default: &str,
    chosen: &mut Option<u8>,
) {
    let current = destination(p, *chosen, default);
    let text = match &current {
        LayerTarget::Existing(code) => p.layer(*code).map_or("", |l| layer_name(t, l)).to_owned(),
        LayerTarget::New(name) => (t.new_layer_suffix)(name),
    };
    ui.label(t.destination);
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(text)
        .show_ui(ui, |ui| {
            if let LayerTarget::New(name) = p.layer_named(default) {
                let selected = matches!(current, LayerTarget::New(_));
                if ui
                    .selectable_label(selected, (t.new_layer_suffix)(&name))
                    .clicked()
                {
                    *chosen = None;
                }
            }
            for layer in &p.current().layers {
                let selected = current == LayerTarget::Existing(layer.code);
                if ui
                    .selectable_label(selected, layer_name(t, layer))
                    .clicked()
                {
                    *chosen = Some(layer.code);
                }
            }
        });
}

pub(super) enum LayerAction {
    SetVisible(u8, bool),
    Rename(u8, String),
    New,
    MoveAll { from: u8, to: u8 },
    Delete(u8),
}

impl Workbench {
    /// Points per layer of the current state, counted once per state.
    fn layer_counts(&mut self, p: &Project) -> &BTreeMap<u8, u64> {
        let id = p.current().id;
        if self.layer_counts.as_ref().is_none_or(|(k, _)| *k != id) {
            self.layer_counts = Some((id, p.layer_counts()));
        }
        &self.layer_counts.as_ref().unwrap().1
    }
    pub(super) fn layers(
        &mut self,
        ui: &mut egui::Ui,
        p: &Project,
        actions: &mut Vec<LayerAction>,
    ) {
        let t = self.t;
        let idle = self.job.is_none();
        let counts = self.layer_counts(p).clone();
        let mut layers: Vec<_> = p.current().layers.iter().collect();
        layers.sort_by_key(|l| l.code);
        for layer in &layers {
            ui.horizontal(|ui| {
                let mut visible = layer.visible;
                if ui
                    .add_enabled(idle, egui::Checkbox::without_text(&mut visible))
                    .changed()
                {
                    actions.push(LayerAction::SetVisible(layer.code, visible));
                }
                let name = layer_name(t, layer);
                let row = ui.add(egui::Label::new(name).sense(egui::Sense::click()));
                let points = counts.get(&layer.code).copied().unwrap_or(0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(t.count(points));
                });
                row.on_hover_text((t.scan_points)(&t.count(points)))
                    .context_menu(|ui| {
                        let deletable = layer.code != DEFAULT_LAYER;
                        if ui
                            .add_enabled(
                                idle && deletable,
                                egui::Button::new(format!("{} {}", icon::PENCIL_SIMPLE, t.rename)),
                            )
                            .clicked()
                        {
                            actions.push(LayerAction::Rename(layer.code, layer.name.clone()));
                        }
                        ui.add_enabled_ui(idle && points > 0, |ui| {
                            ui.menu_button(
                                format!("{} {}", icon::ARROW_BEND_DOWN_RIGHT, t.move_all_points),
                                |ui| {
                                    for other in layers.iter().filter(|l| l.code != layer.code) {
                                        if ui.button(layer_name(t, other)).clicked() {
                                            actions.push(LayerAction::MoveAll {
                                                from: layer.code,
                                                to: other.code,
                                            });
                                        }
                                    }
                                },
                            );
                        });
                        ui.separator();
                        if ui
                            .add_enabled(
                                idle && deletable,
                                egui::Button::new(format!("{} {}", icon::TRASH, t.delete_layer)),
                            )
                            .clicked()
                        {
                            actions.push(LayerAction::Delete(layer.code));
                        }
                    });
            });
        }
        if ui
            .add_enabled(
                idle,
                egui::Button::new(format!("{} {}", icon::PLUS, t.new_layer)).small(),
            )
            .clicked()
        {
            actions.push(LayerAction::New);
        }
        ui.small(t.layers_hint);
    }
    pub(super) fn layer_action(&mut self, ctx: &egui::Context, action: LayerAction) {
        match action {
            LayerAction::SetVisible(code, visible) => {
                self.apply_edit(|p| p.set_layer_visible(code, visible));
            }
            LayerAction::Rename(code, name) => {
                self.dialog = Some(Dialog::RenameLayer {
                    code: Some(code),
                    name,
                })
            }
            LayerAction::New => {
                self.dialog = Some(Dialog::RenameLayer {
                    code: None,
                    name: self.t.new_layer.into(),
                })
            }
            LayerAction::MoveAll { from, to } => {
                let Some(p) = &self.project else { return };
                let mut project = (**p).clone();
                self.start(ctx, true, move |job| {
                    project.move_layer_points(from, to, &job)?;
                    Ok(project)
                });
            }
            LayerAction::Delete(code) => {
                let Some(p) = &self.project else { return };
                let mut project = (**p).clone();
                self.start(ctx, true, move |job| {
                    project.delete_layer(code, &job)?;
                    Ok(project)
                });
            }
        }
    }
    /// The status after an edit job: how many points it moved, and where.
    pub(super) fn moved_status(&self, p: &Project) -> Option<String> {
        let op = p.current().operation["operations"].as_array()?.last()?;
        let moved = op.get("moved")?.as_u64()?;
        let layer = p.layer(op.get("target")?.as_u64()? as u8)?;
        Some((self.t.points_moved)(
            &self.t.count(moved),
            layer_name(self.t, layer),
        ))
    }
}

/// The state whose layer counts are cached, and the counts.
pub(super) type LayerCounts = Option<(Uuid, BTreeMap<u8, u64>)>;
