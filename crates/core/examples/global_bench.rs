//! Pushes every scan but the first off its place by an error that grows
//! along the scans, as alignment one by one accumulates it, adjusts them all
//! together with the first fixed, and reports how far from their places
//! they land. The project is not modified.
//!
//! cargo run -p geemil-core --release --example global_bench -- PROJECT [ERROR_SIZE] [MAX_DISTANCE] [MIN_DISTANCE] [SAMPLES]
use anyhow::Result;
use geemil_core::{GlobalOptions, JobControl, Project};
use glam::{DMat4, DQuat, DVec3};
use std::time::Instant;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut project = Project::load(std::path::Path::new(&args[0]))?;
    // Error size: 1 is about 0.1 degrees and 1 cm more per scan.
    let size: f64 = args.get(1).map_or(Ok(1.), |s| s.parse())?;
    let options = GlobalOptions {
        max_distance: args.get(2).map_or(Ok(0.1), |s| s.parse())?,
        min_distance: args.get(3).map_or(Ok(0.02), |s| s.parse())?,
        samples: args.get(4).map_or(Ok(50_000), |s| s.parse())?,
        ..Default::default()
    };
    let scans: Vec<_> = project.scans().cloned().collect();
    let truth: Vec<DMat4> = scans.iter().map(|s| project.world_matrix(s)).collect();
    let worst = |p: &Project, k: usize| {
        let world = p.world_matrix(&scans[k]);
        scans[k].nodes[0]
            .bounds
            .corners()
            .map(|c| {
                truth[k]
                    .transform_point3(c)
                    .distance(world.transform_point3(c))
            })
            .fold(0., f64::max)
    };
    // In memory only: the edits go to a copy that is never saved.
    let dir = std::env::temp_dir().join(format!("global-bench-{}", std::process::id()));
    copy_dir(std::path::Path::new(&args[0]), &dir)?;
    project = Project::load(&dir)?;
    for (k, scan) in scans.iter().enumerate().skip(1) {
        let t = k as f64 * size;
        let centre = truth[k].transform_point3(scan.nodes[0].bounds.center());
        let error = DMat4::from_translation(centre)
            * DMat4::from_rotation_translation(
                DQuat::from_euler(glam::EulerRot::XYZ, 0.0004 * t, -0.0003 * t, 0.0017 * t),
                DVec3::new(0.007, -0.006, 0.003) * t,
            )
            * DMat4::from_translation(-centre);
        let own = project
            .current()
            .transforms
            .get(&scan.id)
            .copied()
            .unwrap_or_default();
        let pose = project.moved_pose(scan.id, own, error);
        project.set_transform(scan.id, pose)?;
    }
    for (k, scan) in scans.iter().enumerate() {
        println!(
            "{} before: worst corner error {:.4} m",
            scan.name,
            worst(&project, k)
        );
    }
    let ids: Vec<_> = scans.iter().map(|s| s.id).collect();
    let started = Instant::now();
    let result = project.align_globally(&ids, &ids[..1], &options, &JobControl::default());
    let elapsed = started.elapsed();
    match result {
        Ok(r) => {
            for (id, pose) in &r.poses {
                project.set_transform(*id, *pose)?;
            }
            println!(
                "{:.1?}, steps {:?}, stopped at {:?}",
                elapsed, r.steps, r.stopped_at
            );
            println!("fixed {}, isolated {}", r.fixed.len(), r.isolated.len());
            for (k, scan) in scans.iter().enumerate() {
                let fit = r.scans_after.iter().find(|f| f.scan == scan.id);
                println!(
                    "{} after: worst corner error {:.4} m, {:?}",
                    scan.name,
                    worst(&project, k),
                    fit.map(|f| (f.rms, f.overlap, f.neighbours))
                );
            }
            let mean = |fits: &[geemil_core::PairFit]| {
                let rms: Vec<f64> = fits.iter().filter_map(|f| f.rms).collect();
                rms.iter().sum::<f64>() / rms.len().max(1) as f64
            };
            println!(
                "pairs {}, mean pair rms before {:.4} m, after {:.4} m",
                r.after.len(),
                mean(&r.before),
                mean(&r.after)
            );
        }
        Err(e) => println!("{elapsed:.1?}, failed: {e:#}"),
    }
    drop(project);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if entry.file_name() != "project.lock" {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
