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
    let mut cache = ViewCache::new(256 * 1024 * 1024);
    for step in 0..8 {
        camera.yaw += 0.025;
        let start = Instant::now();
        let points =
            project.load_view_cached(&camera, budget, &ids, &JobControl::default(), &mut cache)?;
        println!(
            "step={step} points={} elapsed_ms={:.1}",
            points.len(),
            start.elapsed().as_secs_f64() * 1000.
        );
        println!("cache {:?}", cache.stats());
    }
    Ok(())
}
