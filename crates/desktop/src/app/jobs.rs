//! Background jobs that produce a new committed project state.
use super::Workbench;
use crate::i18n::Strings;
use eframe::egui;
use geemil_core::{CoreError, JobControl, Project, Revision, Stage};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use uuid::Uuid;

enum JobEvent {
    Progress(Stage, u64, u64),
    Complete(Result<Box<Project>, JobFailure>),
}
enum JobFailure {
    Panicked,
    Failed(anyhow::Error),
}
pub(super) struct ActiveJob {
    rx: mpsc::Receiver<JobEvent>,
    cancel: Arc<AtomicBool>,
    /// For jobs that edit the working state: the state before, for undo.
    undo_before: Option<(Option<Revision>, Uuid)>,
}
impl ActiveJob {
    pub(super) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// An error shown under the status bar until dismissed.
#[derive(Debug)]
pub(super) struct Notice {
    pub(super) message: String,
    /// English error chain from the core, for reports and diagnosis.
    pub(super) detail: Option<String>,
}
impl Notice {
    pub(super) fn new(t: &Strings, error: &anyhow::Error) -> Self {
        Self {
            message: CoreError::find(error)
                .map_or_else(|| t.unexpected_error.into(), |e| (t.core_error)(e)),
            detail: Some(format!("{error:#}")),
        }
    }
}
pub(super) fn is_cancelled(error: &anyhow::Error) -> bool {
    CoreError::find(error) == Some(&CoreError::Cancelled)
}

impl Workbench {
    /// Runs `task` on a worker thread; its project becomes current when done.
    /// `edit` records the state before for undo.
    pub(super) fn start(
        &mut self,
        ctx: &egui::Context,
        edit: bool,
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
                let _ = progress_tx.try_send(JobEvent::Progress(stage, done, total));
                progress_ctx.request_repaint();
            }),
        };
        let undo_before = self
            .project
            .as_ref()
            .filter(|_| edit)
            .map(|p| (p.manifest.draft.clone(), p.current().id));
        self.job = Some(ActiveJob {
            rx,
            cancel,
            undo_before,
        });
        self.view.invalidate();
        self.view.next_generation();
        self.progress = 0.;
        self.error = None;
        self.status = self.t.status_working.into();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(job)))
                .map_err(|_| JobFailure::Panicked)
                .and_then(|r| r.map_err(JobFailure::Failed));
            let _ = tx.send(JobEvent::Complete(result.map(Box::new)));
            repaint.request_repaint();
        });
    }
    pub(super) fn poll_job(&mut self) {
        let mut done = None;
        if let Some(job) = &self.job {
            while let Ok(event) = job.rx.try_recv() {
                match event {
                    JobEvent::Progress(stage, done, total) => {
                        let t = self.t;
                        self.status = format!(
                            "{}: {} / {}",
                            (t.stage)(stage),
                            t.count(done),
                            t.count(total)
                        );
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
        if let Some(rx) = &self.cleanup_report
            && let Ok(report) = rx.try_recv()
        {
            self.cleanup_report = None;
            let size = self.t.bytes(report.bytes);
            self.status = (self.t.cleanup_done)(&report, &size);
        }
        let Some(result) = done else {
            return;
        };
        let undo_before = self.job.take().and_then(|j| j.undo_before);
        match result {
            Ok(project) => {
                let project = *project;
                self.error = None;
                let before = self.project.as_ref().map(|p| p.current().clone());
                let fit = before
                    .as_ref()
                    .is_none_or(|b| b.scans != project.current().scans);
                // An edit job that added a layer reports it; one that changed
                // nothing says so.
                let added = project
                    .current()
                    .layers
                    .last()
                    .filter(|id| before.as_ref().is_some_and(|b| !b.layers.contains(id)))
                    .and_then(|id| project.manifest.layers.iter().find(|l| l.id == *id))
                    .map(|l| (self.t.layer_added)(&super::revisions::layer_label(self.t, l)));
                let unchanged =
                    undo_before.is_some() && before.is_some_and(|b| b.id == project.current().id);
                self.install(project, fit);
                // The selection was used up by the job (e.g. an exclusion).
                self.selection.clear();
                self.record_job_edit(undo_before);
                if self.cleanup_report.is_none() {
                    self.status = match added {
                        Some(text) => text,
                        None if unchanged => self.t.status_no_change.into(),
                        None => self.t.status_done.into(),
                    };
                }
            }
            Err(e) => {
                // A batch may have committed earlier files before cancellation/error.
                if let Some(project) = &self.project
                    && let Ok(latest) = Project::load(&project.root)
                {
                    let fit = project.current().scans != latest.current().scans;
                    self.install(latest, fit);
                    self.record_job_edit(undo_before);
                }
                self.cleanup_report = None;
                match e {
                    JobFailure::Failed(e) if is_cancelled(&e) => {
                        self.status = self.t.status_cancelled.into();
                    }
                    JobFailure::Failed(e) => {
                        self.status = self.t.status_failed.into();
                        self.error = Some(Notice::new(self.t, &e));
                    }
                    JobFailure::Panicked => {
                        self.status = self.t.status_failed.into();
                        self.error = Some(Notice {
                            message: self.t.job_panicked.into(),
                            detail: None,
                        });
                    }
                }
            }
        }
    }
    /// Records an edit made by a job for undo, if it changed the state.
    fn record_job_edit(&mut self, before: Option<(Option<Revision>, Uuid)>) {
        if let (Some((state, id)), Some(p)) = (before, &self.project)
            && p.current().id != id
        {
            self.undo.record(state);
        }
    }
}
