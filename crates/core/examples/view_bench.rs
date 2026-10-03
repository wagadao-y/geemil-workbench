//! Repeatable CPU-side view loading measurement; excludes GPU upload/rendering.
use anyhow::{Result, ensure};
use geemil_core::{Camera, JobControl, Project, ViewCache};
use std::{path::Path, time::Instant};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(!args.is_empty(), "Usage: view_bench PROJECT [BUDGET]");
    let project = Project::load(Path::new(&args[0]))?;
    let budget = args
        .get(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(200_000);
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
        for pick in &picks {
            points += project.view_node(pick, &job, &mut cache)?.samples.len();
        }
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
