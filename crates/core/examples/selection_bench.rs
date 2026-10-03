//! Times a depth-limited selection exclusion. It commits a layer, so run it on
//! a copy of a project: selection_bench PROJECT [HALF_WIDTH] [DEPTH_METERS] [inside|outside]
//! HALF_WIDTH is the half size of a centred square in viewport units (0.5 = all).
use anyhow::{Result, ensure};
use geemil_core::{Camera, JobControl, Project, Selection, SelectionMode};
use std::{path::Path, time::Instant};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        !args.is_empty(),
        "Usage: selection_bench PROJECT [HALF_WIDTH] [DEPTH_METERS] [inside|outside]"
    );
    let mut project = Project::load(Path::new(&args[0]))?;
    let half: f64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(0.05);
    // "inf" (or any non-finite depth) excludes at any depth.
    let depth = Some(args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(0.5))
        .filter(|d: &f64| d.is_finite());
    let mode = match args.get(3).map(String::as_str) {
        None | Some("inside") => SelectionMode::ExcludeInside,
        Some("outside") => SelectionMode::ExcludeOutside,
        Some(other) => anyhow::bail!("Unknown mode {other}; use inside or outside"),
    };
    let bounds = project.bounds();
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    // The app's initial "fit" view.
    let camera = Camera {
        target: bounds.center().to_array(),
        distance: (bounds.radius() * 2.8).max(1.),
        aspect: 1.5,
        ..Camera::default()
    };
    let (lo, hi) = (0.5 - half, 0.5 + half);
    let selection = Selection {
        camera,
        polygon: vec![[lo, lo], [hi, lo], [hi, hi], [lo, hi]],
        depth_meters: depth,
        mode,
    };
    let chunks: usize = project.scans().map(|s| s.chunks.len()).sum();
    let start = Instant::now();
    let excluded = project.delete_selection(&selection, &ids, &JobControl::default())?;
    println!(
        "chunks={chunks} excluded={excluded} elapsed_ms={:.1}",
        start.elapsed().as_secs_f64() * 1000.
    );
    Ok(())
}
