//! Current-format end-to-end benchmark. Always creates a NEW project; keeps
//! inputs unchanged. CPU view timings exclude GPU upload and presentation.
#[path = "support/metrics.rs"]
mod metrics;
use anyhow::{Result, ensure};
use geemil_core::{Camera, ImportOptions, JobControl, Project, ViewCache};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() >= 2,
        "Usage: workflow_bench NEW_PROJECT SOURCE_FILE_OR_DIR [VIEW_STEPS]"
    );
    let root = Path::new(&args[0]);
    let source = Path::new(&args[1]);
    let steps: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(600);
    ensure!(!root.exists(), "Benchmark project already exists");
    let mut files: Vec<PathBuf> = if source.is_dir() {
        std::fs::read_dir(source)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| {
                    ["las", "laz", "e57"]
                        .iter()
                        .any(|s| e.eq_ignore_ascii_case(s))
                })
            })
            .collect()
    } else {
        vec![source.to_owned()]
    };
    files.sort();
    ensure!(!files.is_empty(), "No source files");
    let job = JobControl::default();
    let mut project = metrics::phase("import", root, || {
        let mut p = Project::create(root, "Workflow benchmark")?;
        for file in files {
            p.import_file(&file, ImportOptions::default(), &job)?;
        }
        p.save_revision("Imported".into())?;
        Ok(p)
    })?;
    println!(
        "{}",
        serde_json::json!({"records": project.scans().map(|s| s.records).sum::<u64>(),
        "scans": project.scans().count(), "filter_memory_bytes": project.filter_options.memory_bytes})
    );
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    let bounds = project.bounds();
    let camera = Camera {
        target: bounds.center().to_array(),
        distance: (bounds.radius() * 2.8).max(1.),
        aspect: 1.5,
        ..Default::default()
    };
    view("initial_view", root, &project, camera, &ids, 1)?;
    let before = project.manifest.draft.clone();
    metrics::phase("subsample_2cm", root, || {
        let moved = project.subsample(0.02, &ids, &project.layer_named("Thinned"), &job)?;
        println!("{}", serde_json::json!({"moved_subsample": moved}));
        Ok(())
    })?;
    let after = project.manifest.draft.clone();
    metrics::phase("undo_redo_20", root, || {
        for _ in 0..20 {
            project.restore_working_state(before.clone())?;
            project.restore_working_state(after.clone())?;
        }
        ensure!(
            project.manifest.draft.as_ref().map(|r| r.id) == after.as_ref().map(|r| r.id),
            "Redo state differs"
        );
        Ok(())
    })?;
    metrics::phase("noise_5cm_4", root, || {
        let moved = project.remove_noise(0.05, 4, &ids, &project.layer_named("Noise"), &job)?;
        println!("{}", serde_json::json!({"moved_noise": moved}));
        Ok(())
    })?;
    view(
        "edited_camera_orbit_cpu",
        root,
        &project,
        camera,
        &ids,
        steps,
    )?;
    for extension in ["las", "laz"] {
        metrics::phase(&format!("export_{extension}"), root, || {
            let written = project.export_las(&root.join(format!("edited.{extension}")), &job)?;
            println!(
                "{}",
                serde_json::json!({"written": written, "extension": extension})
            );
            Ok(())
        })?;
    }
    project.save_revision("Filtered".into())?;
    Ok(())
}

fn view(
    name: &str,
    root: &Path,
    project: &Project,
    mut camera: Camera,
    ids: &[uuid::Uuid],
    steps: usize,
) -> Result<()> {
    metrics::phase(name, root, || {
        let project = std::sync::Arc::new(project.clone());
        let mut cache = ViewCache::new(0);
        cache.set_point_limit(4_000_000);
        let mut times = Vec::with_capacity(steps);
        let mut points = 0;
        for _ in 0..steps {
            camera.yaw += 0.01;
            let start = Instant::now();
            let picks = project.select_view(&camera, 2_000_000, ids, &JobControl::default())?;
            points = 0;
            project.load_view_nodes(&picks, &mut cache, &JobControl::default(), 0, |node| {
                points += node.samples.len();
                Ok(())
            })?;
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        if !times.is_empty() {
            let first = times[0];
            times.sort_by(f64::total_cmp);
            let percentile = |p: f64| {
                times[((times.len() as f64 * p).ceil() as usize)
                    .saturating_sub(1)
                    .min(times.len() - 1)]
            };
            println!(
                "{}",
                serde_json::json!({"view_phase": name, "steps": steps, "first_cpu_ms": first,
                "p50_cpu_ms": percentile(0.5), "p95_cpu_ms": percentile(0.95), "p99_cpu_ms": percentile(0.99),
                "final_points": points, "cache_bytes": cache.stats().resident_bytes, "hidden_bytes": cache.stats().hidden_bytes})
            );
        }
        Ok(())
    })
}
