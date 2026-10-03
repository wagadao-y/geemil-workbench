use super::{Workbench, jobs::Notice};
use eframe::egui;
use geemil_core::{ImportOptions, Project};

impl Workbench {
    pub(super) fn toolbar(&mut self, ctx: &egui::Context) {
        let t = self.t;
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Geemil Workbench");
                ui.separator();
                let busy = self.job.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new(t.new_project))
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title(t.new_project_dialog)
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
                        Err(e) => self.error = Some(Notice::new(t, &e)),
                    }
                }
                if ui.add_enabled(!busy, egui::Button::new(t.open)).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title(t.open_dialog)
                        .pick_folder()
                {
                    match Project::load(&path) {
                        Ok(p) => self.install(p, true),
                        Err(e) => self.error = Some(Notice::new(t, &e)),
                    }
                }
                if ui
                    .add_enabled(!busy && self.project.is_some(), egui::Button::new(t.import))
                    .clicked()
                    && let Some(files) = rfd::FileDialog::new()
                        .add_filter(t.point_cloud_filter, &["e57", "las", "laz"])
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
                        egui::Button::new(t.export_e57),
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
                        egui::Button::new(t.compress_storage),
                    )
                    .clicked()
                {
                    let mut project = (**self.project.as_ref().unwrap()).clone();
                    self.start(ctx, move |job| {
                        project.compress_storage(&job)?;
                        Ok(project)
                    });
                }
                if ui.button(t.fit_view).clicked() {
                    self.fit_view();
                }
            })
        });
    }
}
