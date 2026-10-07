use anyhow::{Result, bail};
use geemil_core::{ImportOptions, JobControl, Project, interchange};
use std::path::Path;
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let job = JobControl {
        progress: std::sync::Arc::new(|stage, done, total| eprintln!("{stage}: {done}/{total}")),
        ..Default::default()
    };
    match args.first().map(String::as_str) {
        Some("demo") if args.len() == 2 => interchange::create_demo(Path::new(&args[1]))?,
        Some("import") if args.len() >= 3 => {
            let root = Path::new(&args[1]);
            let mut project = if root.exists() {
                Project::load(root)?
            } else {
                Project::create(
                    root,
                    root.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref(),
                )?
            };
            for file in &args[2..] {
                let file = Path::new(file);
                let extension = file
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if matches!(extension.as_str(), "jpg" | "jpeg" | "png") {
                    project.import_panorama(file)?;
                } else {
                    project.import_file(file, ImportOptions::default(), &job)?;
                }
            }
            println!(
                "{} scans, {} images, {} panoramas",
                project.scans().count(),
                project.manifest.images.len(),
                project.panoramas().count()
            );
        }
        Some("inspect") if args.len() == 2 => {
            let p = Project::load(Path::new(&args[1]))?;
            println!("{} — revision {}", p.manifest.name, p.manifest.current);
            for s in p.scans() {
                println!(
                    "{}: {} records, {} valid, {} chunks",
                    s.name,
                    s.records,
                    s.valid_points,
                    s.chunks.len()
                );
            }
            println!("{} images", p.manifest.images.len());
            for panorama in p.panoramas() {
                let place = p
                    .correction(panorama.id)
                    .transform_point3(glam::DVec3::ZERO);
                println!(
                    "panorama {}: {} x {}, at {:.3?}, {} correspondences",
                    p.panorama_name(panorama),
                    panorama.width,
                    panorama.height,
                    place.to_array(),
                    p.panorama_pairs(panorama.id).len()
                );
            }
            let counts = p.layer_counts();
            for layer in &p.current().layers {
                println!(
                    "layer {} {}: {} points{}",
                    layer.code,
                    layer.name,
                    counts[&layer.code],
                    if layer.visible { "" } else { ", hidden" }
                );
            }
        }
        Some("export") if args.len() == 3 => {
            Project::load(Path::new(&args[1]))?.export_e57(Path::new(&args[2]), &job)?
        }
        Some("export-las") if args.len() == 3 => {
            let started = std::time::Instant::now();
            let n = Project::load(Path::new(&args[1]))?.export_las(Path::new(&args[2]), &job)?;
            println!("{n} points written in {:.2?}", started.elapsed());
        }
        Some("subsample") if args.len() == 3 => {
            let mut p = Project::load(Path::new(&args[1]))?;
            let scans: Vec<_> = p.scans().map(|s| s.id).collect();
            let started = std::time::Instant::now();
            let target = p.layer_named("Subsampled");
            let removed = p.subsample(args[2].parse()?, &scans, &target, &job)?;
            println!("{removed} points moved in {:.2?}", started.elapsed());
        }
        Some("subsample-merged") if args.len() == 3 => {
            let mut p = Project::load(Path::new(&args[1]))?;
            let scans: Vec<_> = p.scans().map(|s| s.id).collect();
            let started = std::time::Instant::now();
            let target = p.layer_named("Subsampled");
            let removed = p.subsample_merged(args[2].parse()?, &scans, &target, &job)?;
            println!("{removed} points moved in {:.2?}", started.elapsed());
        }
        Some("reduce-overlap") if args.len() == 3 => {
            let mut p = Project::load(Path::new(&args[1]))?;
            let scans: Vec<_> = p.scans().map(|s| s.id).collect();
            let started = std::time::Instant::now();
            let target = p.layer_named("Overlap");
            let moved = p.reduce_overlap(args[2].parse()?, &scans, &target, &job)?;
            println!("{moved} points moved in {:.2?}", started.elapsed());
        }
        Some("noise") if args.len() == 4 => {
            let mut p = Project::load(Path::new(&args[1]))?;
            let scans: Vec<_> = p.scans().map(|s| s.id).collect();
            let started = std::time::Instant::now();
            let target = p.layer_named("Noise");
            let removed =
                p.remove_noise(args[2].parse()?, args[3].parse()?, &scans, &target, &job)?;
            println!("{removed} points moved in {:.2?}", started.elapsed());
        }
        Some("sor") if args.len() == 5 => {
            let mut p = Project::load(Path::new(&args[1]))?;
            let scans: Vec<_> = p.scans().map(|s| s.id).collect();
            let started = std::time::Instant::now();
            let target = p.layer_named("Noise");
            let removed = p.remove_outliers(
                args[2].parse()?,
                args[3].parse()?,
                args[4].parse()?,
                &scans,
                &target,
                &job,
            )?;
            println!("{removed} points moved in {:.2?}", started.elapsed());
        }
        _ => bail!(
            "Usage: geemil demo FILE.e57 | import PROJECT FILE... | inspect PROJECT | export PROJECT FILE.e57 | export-las PROJECT FILE.las|laz | subsample PROJECT VOXEL_M | subsample-merged PROJECT VOXEL_M | reduce-overlap PROJECT CELL_M | noise PROJECT RADIUS_M MIN_NEIGHBOURS | sor PROJECT NEIGHBOURS SIGMAS REACH_M"
        ),
    }
    Ok(())
}
