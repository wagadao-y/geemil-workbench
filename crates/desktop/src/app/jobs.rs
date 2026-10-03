//! Background jobs that produce a new committed project state.
use super::Workbench;
use crate::i18n::Strings;
use eframe::egui;
use geemil_core::{CoreError, JobControl, Project, Stage};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

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
    pub(super) fn start(
        &mut self,
        ctx: &egui::Context,
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
        self.job = Some(ActiveJob { rx, cancel });
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
        let Some(result) = done else {
            return;
        };
        self.job = None;
        match result {
            Ok(project) => {
                let project = *project;
                self.error = None;
                let fit = self
                    .project
                    .as_ref()
                    .is_none_or(|p| p.current().scans != project.current().scans);
                self.install(project, fit);
                self.status = self.t.status_done.into();
            }
            Err(e) => {
                // A batch may have committed earlier files before cancellation/error.
                if let Some(project) = &self.project
                    && let Ok(latest) = Project::load(&project.root)
                {
                    let fit = project.current().scans != latest.current().scans;
                    self.install(latest, fit);
                }
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
}
