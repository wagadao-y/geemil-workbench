//! Import timing by stage, using the same code path as the desktop app.
use anyhow::{Result, ensure};
use geemil_core::{ImportOptions, JobControl, Project};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() >= 2,
        "Usage: import_bench SOURCE NEW_PROJECT [WORKERS] [MEMORY_MIB] [COMPARE_PROJECT]"
    );
    let options = ImportOptions {
        worker_threads: args.get(2).map(|v| v.parse()).transpose()?.unwrap_or(0),
        worker_memory_bytes: args
            .get(3)
            .map(|v| v.parse::<usize>())
            .transpose()?
            .unwrap_or(256)
            * 1024
            * 1024,
        ..Default::default()
    };
    let start = Instant::now();
    let timing = Arc::new(Mutex::new((
        start,
        "Reading E57".to_owned(),
        BTreeMap::<String, f64>::new(),
    )));
    let progress_timing = timing.clone();
    let job = JobControl {
        progress: Arc::new(move |stage, _, _| {
            let mut state = progress_timing.lock().unwrap();
            if state.1 != stage {
                let now = Instant::now();
                let elapsed = now.duration_since(state.0).as_secs_f64();
                let old = state.1.clone();
                *state.2.entry(old).or_default() += elapsed;
                state.0 = now;
                state.1 = stage.to_owned();
            }
        }),
        ..Default::default()
    };
    ensure!(
        !Path::new(&args[1]).exists(),
        "Benchmark output already exists"
    );
    let mut project = Project::create(Path::new(&args[1]), "Import benchmark")?;
    project.import_file(Path::new(&args[0]), options, &job)?;
    let total = start.elapsed().as_secs_f64();
    let mut state = timing.lock().unwrap();
    let last = state.1.clone();
    let last_time = state.0.elapsed().as_secs_f64();
    *state.2.entry(last).or_default() += last_time;
    println!(
        "workers={} memory_mib={} total_s={total:.3}",
        options.worker_threads,
        options.worker_memory_bytes / (1024 * 1024)
    );
    for (stage, seconds) in &state.2 {
        println!("{stage}: {seconds:.3}s (wall time; stages overlap)");
    }
    if let Some(reference) = args.get(4) {
        let reference = Project::load(Path::new(reference))?;
        ensure!(
            project.manifest.scans.len() == reference.manifest.scans.len(),
            "Scan count differs"
        );
        for (actual, expected) in project.scans().zip(reference.scans()) {
            ensure!(
                actual.stride == expected.stride
                    && actual.records == expected.records
                    && actual.chunks.len() == expected.chunks.len()
                    && actual.nodes.len() == expected.nodes.len(),
                "Scan structure differs"
            );
            for i in 0..actual.chunks.len() {
                ensure!(
                    project.read_chunk(actual, i as u32)?
                        == reference.read_chunk(expected, i as u32)?,
                    "Original chunk {i} differs"
                );
            }
            for i in 0..actual.nodes.len() {
                ensure!(
                    actual.nodes[i].children == expected.nodes[i].children
                        && actual.nodes[i].chunk == expected.nodes[i].chunk,
                    "Node {i} structure differs"
                );
                ensure!(
                    project.read_lod(actual, i as u32)?
                        == reference.read_lod(expected, i as u32)?,
                    "Node {i} LOD differs"
                );
            }
        }
        println!(
            "All original chunks, point references, tree topology and LOD samples match the reference."
        );
    }
    Ok(())
}
