use geemil_core::{
    Camera, ImportOptions, JobControl, Pose, Project, Selection, SelectionMode, Stage, ViewCache,
    interchange,
};
use std::sync::{Arc, atomic::Ordering};

fn fixture(root: &std::path::Path) -> Project {
    let input = root.join("demo.e57");
    interchange::create_demo(&input).unwrap();
    let mut p = Project::create(&root.join("project"), "Test").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    p.save_revision("Imported".into()).unwrap();
    p
}

fn assert_same_storage(left: &Project, right: &Project) {
    assert_eq!(left.manifest.scans.len(), right.manifest.scans.len());
    assert_eq!(left.manifest.images.len(), right.manifest.images.len());
    for (left_scan, right_scan) in left.scans().zip(right.scans()) {
        assert_eq!(left_scan.original_pose, right_scan.original_pose);
        assert_eq!(left_scan.valid_points, right_scan.valid_points);
        assert_eq!(
            serde_json::to_value(&left_scan.nodes).unwrap(),
            serde_json::to_value(&right_scan.nodes).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&left_scan.chunks).unwrap(),
            serde_json::to_value(&right_scan.chunks).unwrap()
        );
        for i in 0..left_scan.chunks.len() {
            assert_eq!(
                left.read_chunk(left_scan, i as u32).unwrap(),
                right.read_chunk(right_scan, i as u32).unwrap()
            );
        }
        for i in 0..left_scan.nodes.len() {
            assert_eq!(
                left.read_lod(left_scan, i as u32).unwrap(),
                right.read_lod(right_scan, i as u32).unwrap()
            );
        }
    }
}

#[test]
fn parallel_import_matches_one_worker_and_cancel_during_indexing_is_not_published() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("demo.e57");
    interchange::create_demo(&input).unwrap();
    let options = ImportOptions {
        chunk_points: 1024,
        lod_points: 32,
        worker_memory_bytes: 1024 * 1024,
        worker_threads: 1,
    };
    let mut one = Project::create(&dir.path().join("one"), "One").unwrap();
    one.import_file(&input, options, &JobControl::default())
        .unwrap();
    let mut many = Project::create(&dir.path().join("many"), "Many").unwrap();
    let parallel = ImportOptions {
        worker_threads: 4,
        ..options
    };
    many.import_file(&input, parallel, &JobControl::default())
        .unwrap();
    assert_same_storage(&one, &many);

    // Preserve an existing committed import, including after reopening.
    let before = serde_json::to_value(&many.manifest).unwrap();
    let saved_before = std::fs::read(many.root.join("project.json")).unwrap();
    let reopened_before =
        serde_json::to_value(Project::load(&many.root).unwrap().manifest).unwrap();
    let mut job = JobControl::default();
    let cancel = job.cancel.clone();
    job.progress = Arc::new(move |stage, done, _| {
        if stage == Stage::Indexing && done > 0 {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert!(many.import_file(&input, parallel, &job).is_err());
    assert_eq!(serde_json::to_value(&many.manifest).unwrap(), before);
    assert_eq!(
        std::fs::read(many.root.join("project.json")).unwrap(),
        saved_before
    );
    assert_eq!(
        serde_json::to_value(Project::load(&many.root).unwrap().manifest).unwrap(),
        reopened_before
    );
}

#[test]
fn parallel_import_handles_coincident_points_with_more_than_eight_children() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("coincident.las");
    let mut writer = las::Writer::from_path(&input, las::Header::default()).unwrap();
    for _ in 0..10_000 {
        writer
            .write_point(las::Point {
                x: 1.,
                y: 2.,
                z: 3.,
                ..Default::default()
            })
            .unwrap();
    }
    writer.close().unwrap();
    drop(writer);
    let options = ImportOptions {
        chunk_points: 256,
        lod_points: 16,
        worker_threads: 1,
        worker_memory_bytes: 1024 * 1024,
    };
    let mut one = Project::create(&dir.path().join("one"), "One").unwrap();
    one.import_file(&input, options, &JobControl::default())
        .unwrap();
    let mut many = Project::create(&dir.path().join("many"), "Many").unwrap();
    many.import_file(
        &input,
        ImportOptions {
            worker_threads: 4,
            ..options
        },
        &JobControl::default(),
    )
    .unwrap();
    assert!(many.scans().next().unwrap().nodes[0].children.len() > 8);
    assert_same_storage(&one, &many);
}

#[test]
fn camera_reuses_cache_and_revision_changes_invalidate_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = fixture(dir.path());
    let id = p.scans().next().unwrap().id;
    let camera = Camera {
        target: p.bounds().center().to_array(),
        distance: 30.,
        ..Camera::default()
    };
    let mut cache = ViewCache::new(8 * 1024 * 1024);
    let base = p.manifest.current;
    let a = p
        .load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
        .unwrap()
        .samples;
    let misses = cache.stats().misses;
    let b = p
        .load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
        .unwrap()
        .samples;
    assert_eq!(a, b);
    assert_eq!(cache.stats().misses, misses);
    assert!(cache.stats().hits > 0);
    let pose = Pose {
        translation: [0.25, 0., 0.],
        ..Pose::default()
    };
    let scan = p.scans().next().unwrap().clone();
    let previewed = p.world_matrix_with(&scan, id, pose);
    p.set_transform(id, pose).unwrap();
    assert!(previewed.abs_diff_eq(p.world_matrix(&scan), 1e-12));
    let misses = cache.stats().misses;
    let moved = p
        .load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
        .unwrap();
    // Samples are cached in scan coordinates, so moving a scan reads nothing.
    assert_eq!(cache.stats().misses, misses);
    assert_eq!(moved.segments.len(), 1);
    assert_eq!(moved.segments[0].scan, id);
    assert_eq!(moved.segments[0].range, 0..moved.samples.len());
    assert!(
        moved.segments[0]
            .world
            .abs_diff_eq(p.world_matrix(&scan), 1e-12)
    );
    let moved = moved.samples;
    assert_eq!(a.len(), moved.len());
    for (a, b) in a.iter().zip(moved) {
        assert!((a.position[0] + 0.25 - b.position[0]).abs() < 1e-12);
    }
    let target = p.layer_named("Deleted");
    p.move_selection(
        &Selection {
            camera,
            polygon: vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            depth_meters: Some(1000.),
            mode: SelectionMode::ExcludeInside,
        },
        &[id],
        &target,
        &JobControl::default(),
    )
    .unwrap();
    assert!(
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
            .unwrap()
            .samples
            .is_empty()
    );
    p.switch(base).unwrap();
    assert_eq!(
        a,
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
            .unwrap()
            .samples
    );
    let mut small = ViewCache::new(128);
    assert_eq!(
        a,
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut small)
            .unwrap()
            .samples
    );
    assert!(small.stats().resident_bytes <= 128);
}
