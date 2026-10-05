//! Moves each scan of a project by a known error, runs ICP against the other
//! scans and reports how far from the original pose it lands, and how long it
//! takes. The project is not modified.
//!
//! cargo run -p geemil-core --release --example align_bench -- PROJECT [MAX_DISTANCE] [ERROR_SIZE] [SAMPLES] [ITERATIONS] [MIN_DISTANCE]
use anyhow::Result;
use geemil_core::{IcpOptions, JobControl, Project};
use glam::{DMat4, DQuat, DVec3};
use std::time::Instant;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let project = Project::load(std::path::Path::new(&args[0]))?;
    let max_distance = args.get(1).map_or(Ok(0.5), |s| s.parse())?;
    // Error size: 1 is about 1.8 degrees and 15 cm.
    let size: f64 = args.get(2).map_or(Ok(1.), |s| s.parse())?;
    let samples = args.get(3).map_or(Ok(60_000), |s| s.parse())?;
    let iterations = args.get(4).map_or(Ok(50), |s| s.parse())?;
    let min_distance = args.get(5).map_or(Ok(0.02), |s| s.parse())?;
    let error = DMat4::from_rotation_translation(
        DQuat::from_euler(
            glam::EulerRot::XYZ,
            0.005 * size,
            -0.004 * size,
            0.03 * size,
        ),
        DVec3::new(0.12, -0.08, 0.04) * size,
    );
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    for scan in project.scans() {
        let own = project
            .current()
            .transforms
            .get(&scan.id)
            .copied()
            .unwrap_or_default();
        // Turn about the scan's centre, as a real misalignment would be.
        let centre = project
            .world_matrix(scan)
            .transform_point3(scan.nodes[0].bounds.center());
        let error = DMat4::from_translation(centre) * error * DMat4::from_translation(-centre);
        let start = project.moved_pose(scan.id, own, error);
        let others: Vec<_> = ids.iter().copied().filter(|id| *id != scan.id).collect();
        let started = Instant::now();
        let result = project.icp(
            scan.id,
            start,
            &others,
            &IcpOptions {
                max_distance,
                min_distance,
                samples,
                iterations,
            },
            &JobControl::default(),
        );
        let elapsed = started.elapsed();
        match result {
            Ok(r) => {
                // Error at the corners of the scan's bounding box.
                let truth = project.world_matrix(scan);
                let found = project.world_matrix_with(scan, scan.id, r.pose);
                let worst = scan.nodes[0]
                    .bounds
                    .corners()
                    .map(|c| {
                        truth
                            .transform_point3(c)
                            .distance(found.transform_point3(c))
                    })
                    .fold(0., f64::max);
                println!(
                    "{}: {:.1?}, {} iterations, rms {:.4} m at {:.3} m, overlap {:.0}%, worst corner error {:.4} m",
                    scan.name,
                    elapsed,
                    r.iterations,
                    r.rms,
                    r.steps.last().map_or(0., |s| s.distance),
                    r.overlap * 100.,
                    worst
                );
            }
            Err(e) => println!("{}: {:.1?}, failed: {e:#}", scan.name, elapsed),
        }
    }
    Ok(())
}
