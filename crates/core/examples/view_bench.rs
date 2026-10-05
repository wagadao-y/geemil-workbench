//! Repeatable CPU-side view loading measurement; excludes GPU upload/rendering.
#[path = "support/metrics.rs"]
mod metrics;
use anyhow::{Result, ensure};
use geemil_core::{Camera, JobControl, Project, ViewCache};
use std::{path::Path, sync::Arc, time::Instant};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        !args.is_empty(),
        "Usage: view_bench PROJECT [BUDGET] [WORKERS] [REVISION_NAME]"
    );
    metrics::phase("cpu_view", Path::new(&args[0]), || run(&args))
}
fn run(args: &[String]) -> Result<()> {
    let mut project = Project::load(Path::new(&args[0]))?;
    if let Some(name) = args.get(3) {
        let revision = project
            .manifest
            .revisions
            .iter()
            .find(|r| &r.name == name)
            .ok_or_else(|| anyhow::anyhow!("Revision name not found"))?
            .id;
        // Benchmark a saved state without writing or switching the project on disk.
        project.manifest.current = revision;
        project.manifest.draft = None;
    }
    let project = Arc::new(project);
    let budget = args
        .get(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(200_000);
    let workers = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(0);
    let bounds = project.bounds();
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    let mut camera = Camera {
        target: bounds.center().to_array(),
        distance: (bounds.radius() * 2.8).max(1.),
        aspect: 1.5,
        ..Camera::default()
    };
    // As the app: room for twice the point budget.
    let mut cache = ViewCache::new(0);
    cache.set_point_limit(budget * 2);
    for step in 0..8 {
        camera.yaw += 0.025;
        let start = Instant::now();
        let job = JobControl::default();
        let picks = project.select_view(&camera, budget, &ids, &job)?;
        let select_ms = start.elapsed().as_secs_f64() * 1000.;
        let load_start = Instant::now();
        let mut points = 0;
        project.load_view_nodes(&picks, &mut cache, &job, workers, |node| {
            points += node.points.len();
            Ok(())
        })?;
        let load_ms = load_start.elapsed().as_secs_f64() * 1000.;
        println!(
            "step={step} nodes={} points={points} select_ms={select_ms:.1} load_ms={load_ms:.1} elapsed_ms={:.1}",
            picks.len(),
            start.elapsed().as_secs_f64() * 1000.
        );
        println!("cache {:?}", cache.stats());
    }
    Ok(())
}
