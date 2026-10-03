use super::{Workbench, history::layer_label};
use eframe::egui;
use geemil_core::{Pose, Project};
use glam::DQuat;
use std::sync::Arc;

impl Workbench {
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        let t = self.t;
        egui::Panel::left("scans")
            .default_size(270.)
            .show(ui, |ui| {
                ui.heading(t.scans);
                let Some(p) = self.project.clone() else {
                    ui.label(t.no_project_hint);
                    return;
                };
                ui.label(&p.manifest.name);
                self.scan_list(ui, &p);
                self.scan_details(ui, &p);
                ui.separator();
                self.alignment(ui, ctx, &p);
                ui.separator();
                self.layers(ui, ctx, &p);
                ui.separator();
                self.history(ui, ctx, &p);
            });
    }
    fn scan_list(&mut self, ui: &mut egui::Ui, p: &Project) {
        let t = self.t;
        for scan in p.scans() {
            ui.horizontal(|ui| {
                let mut shown = self.visible.contains(&scan.id);
                if ui.checkbox(&mut shown, "").changed() {
                    self.view.invalidate();
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
            ui.small((t.scan_summary)(
                &t.count(scan.records),
                &t.count(scan.chunks.len() as u64),
            ));
        }
    }
    fn scan_details(&self, ui: &mut egui::Ui, p: &Project) {
        let t = self.t;
        let Some(id) = self.selected else {
            return;
        };
        let Some(scan) = p.scans().find(|s| s.id == id) else {
            return;
        };
        ui.separator();
        ui.label(t.images);
        for image in p.manifest.images.iter().filter(|i| i.scan_id == Some(id)) {
            ui.small(format!(
                "{} ({})",
                image.name.as_deref().unwrap_or(t.unnamed_image),
                image.projection
            ));
        }
        ui.collapsing(t.omitted_attributes, |ui| {
            for attribute in &scan.omitted_attributes {
                ui.small(attribute);
            }
        });
    }
    fn alignment(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: &Arc<Project>) {
        let t = self.t;
        ui.collapsing(t.alignment, |ui| {
            ui.label(t.translation);
            for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(*label);
                    ui.add(egui::DragValue::new(&mut self.translation[i]).speed(0.01));
                });
            }
            ui.label(t.rotation);
            for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(*label);
                    ui.add(egui::DragValue::new(&mut self.rotation[i]).speed(0.1));
                });
            }
            if ui
                .add_enabled(
                    self.job.is_none() && self.selected.is_some(),
                    egui::Button::new(t.apply_transform),
                )
                .clicked()
            {
                let mut project = (**p).clone();
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
    }
    fn layers(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: &Arc<Project>) {
        let t = self.t;
        ui.heading(t.layers);
        for layer in &p.manifest.layers {
            let mut enabled = p.current().layers.contains(&layer.id);
            if ui
                .add_enabled(
                    self.job.is_none(),
                    egui::Checkbox::new(&mut enabled, layer_label(t, layer)),
                )
                .changed()
            {
                let mut project = (**p).clone();
                let id = layer.id;
                self.start(ctx, move |_| {
                    project.set_layer_enabled(id, enabled)?;
                    Ok(project)
                });
            }
        }
    }
}
