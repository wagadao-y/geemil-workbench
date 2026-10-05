//! Per-request and per-frame costs that grow with the number of scans and
//! edits rather than points: listing scans, folder summaries and choosing the
//! nodes to show, with many label patches in effect.
//!
//! The first run creates PROJECT from the LAS files in LAS_DIR, puts the scans
//! into FOLDERS folders and moves small selections EDITS times; later runs
//! reuse it.
//!
//! cargo run -p geemil-core --release --example scale_bench -- PROJECT LAS_DIR FOLDERS EDITS
use anyhow::{Result, ensure};
use geemil_core::{
    Camera, ImportOptions, JobControl, LayerTarget, Project, Selection, SelectionMode, ViewCache,
};
use std::{path::Path, sync::Arc, time::Instant};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(args.len() == 4, "PROJECT LAS_DIR FOLDERS EDITS");
    let root = Path::new(&args[0]);
    if !root.exists() {
        create(
            root,
            Path::new(&args[1]),
            args[2].parse()?,
            args[3].parse()?,
        )?;
    }
    let project = Arc::new(Project::load(root)?);
    let scans = project.scans().count();
    println!(
        "scans={scans} folders={} label_patches={}",
        project.groups().len(),
        project.current().labels.len()
    );
    let ms = |start: Instant| start.elapsed().as_secs_f64() * 1000.;

    let start = Instant::now();
    let mut listed = 0;
    for _ in 0..100 {
        listed += project.scans().count();
    }
    println!("scans_x100_ms={:.2} ({listed})", ms(start));

    // What the tree panel derives each frame: every level and every folder's
    // scans and points.
    let start = Instant::now();
    let mut points = 0u64;
    for _ in 0..10 {
        project.children(None);
        for group in project.groups() {
            project.children(Some(group.id));
            let inside = project.scans_within(group.id);
            points += project
                .scans()
                .filter(|s| inside.contains(&s.id))
                .map(|s| s.records)
                .sum::<u64>();
        }
    }
    println!(
        "tree_summary_per_frame_ms={:.2} ({points})",
        ms(start) / 10.
    );

    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    let bounds = project.bounds();
    let mut camera = Camera {
        target: bounds.center().to_array(),
        distance: (bounds.radius() * 1.2).max(1.),
        aspect: 1.5,
        ..Camera::default()
    };
    let mut cache = ViewCache::new(0);
    cache.set_point_limit(4_000_000);
    let job = JobControl::default();
    // Counting every chunk's layers again, as after an edit.
    let start = Instant::now();
    project.select_view(&camera, 2_000_000, &ids, &job)?;
    println!("select_uncached_ms={:.2}", ms(start));
    // As the app's view loader: estimates kept with the cache between moves.
    for step in 0..6 {
        camera.yaw += 0.02;
        let start = Instant::now();
        let picks = project.select_view_cached(&camera, 2_000_000, &ids, &job, &mut cache)?;
        let select_ms = ms(start);
        let start = Instant::now();
        let mut loaded = 0;
        project.load_view_nodes(&picks, &mut cache, &job, 0, |node| {
            loaded += node.points.len();
            Ok(())
        })?;
        println!(
            "step={step} nodes={} points={loaded} select_ms={select_ms:.2} load_ms={:.2}",
            picks.len(),
            ms(start)
        );
    }
    Ok(())
}

fn create(root: &Path, las: &Path, folders: usize, edits: usize) -> Result<()> {
    let mut project = Project::create(root, "Scale benchmark")?;
    let job = JobControl::default();
    let mut files: Vec<_> = std::fs::read_dir(las)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    files.sort();
    let start = Instant::now();
    for file in &files {
        project.import_file(file, ImportOptions::default(), &job)?;
    }
    println!(
        "imported {} files in {:.1} s",
        files.len(),
        start.elapsed().as_secs_f64()
    );
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    let per_folder = ids.len().div_ceil(folders.max(1));
    for (i, scans) in ids.chunks(per_folder).enumerate() {
        let group = project.create_group(format!("Folder {i}"), None)?;
        project.move_to_group(scans, Some(group))?;
    }
    // Small top-down selections spread over the site, so each edit writes a
    // patch of few chunks, as manual cleaning does.
    let start = Instant::now();
    for e in 0..edits {
        let angle = e as f64 * 2.4;
        let radius = 5. + 45. * (e as f64 / edits.max(1) as f64);
        let selection = Selection {
            camera: Camera {
                target: [radius * angle.cos(), radius * angle.sin(), 0.],
                pitch: 1.5,
                distance: 20.,
                aspect: 1.5,
                ..Camera::default()
            },
            polygon: vec![[0.48, 0.48], [0.52, 0.48], [0.52, 0.52], [0.48, 0.52]],
            depth_meters: None,
            mode: SelectionMode::ExcludeInside,
        };
        let target = LayerTarget::New("Deleted".into());
        let target = match project.layer_named("Deleted") {
            LayerTarget::Existing(code) => LayerTarget::Existing(code),
            LayerTarget::New(_) => target,
        };
        project.move_selection(&selection, &ids, &target, &job)?;
    }
    println!("{edits} edits in {:.1} s", start.elapsed().as_secs_f64());
    Ok(())
}
