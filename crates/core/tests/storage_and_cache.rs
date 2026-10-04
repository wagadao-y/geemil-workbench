use geemil_core::{
    Camera, ImportOptions, JobControl, Pose, Project, Selection, SelectionMode, Stage, ViewCache,
    interchange,
};
use std::sync::{Arc, atomic::Ordering};

trait Points {
    /// All loaded points, in scan coordinates.
    fn points(&self) -> Vec<geemil_core::Sample>;
}
impl Points for geemil_core::LoadedView {
    fn points(&self) -> Vec<geemil_core::Sample> {
        self.nodes
            .iter()
            .flat_map(|n| n.samples.iter().cloned())
            .collect()
    }
}

fn fixture(root: &std::path::Path) -> Project {
    let input = root.join("demo.e57");
    interchange::create_demo(&input).unwrap();
    let mut p = Project::create(&root.join("project"), "Test").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    p.save_revision("Imported".into()).unwrap();
    p
}

#[test]
fn parallel_view_matches_serial_with_hidden_layers_quotas_and_cache_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = fixture(dir.path());
    let camera = Camera {
        target: p.bounds().center().to_array(),
        distance: p.bounds().radius() * 3.,
        ..Camera::default()
    };
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    let job = JobControl::default();
    for hidden in [false, true] {
        if hidden {
            p.move_selection(
                &Selection {
                    camera,
                    polygon: vec![[0., 0.], [0.5, 0.], [0.5, 1.], [0., 1.]],
                    depth_meters: None,
                    mode: SelectionMode::ExcludeInside,
                },
                &ids,
                &p.layer_named("Deleted"),
                &job,
            )
            .unwrap();
        }
        let project = Arc::new(p.clone());
        for budget in [20, 100_000] {
            let picks = project.select_view(&camera, budget, &ids, &job).unwrap();
            let mut serial_cache = ViewCache::new(16 * 1024 * 1024);
            let expected: Vec<_> = picks
                .iter()
                .map(|pick| {
                    let node = project.view_node(pick, &job, &mut serial_cache).unwrap();
                    (node.scan, node.node, node.samples)
                })
                .collect();
            let mut cache = ViewCache::new(16 * 1024 * 1024);
            for _ in 0..2 {
                let mut actual = vec![];
                project
                    .load_view_nodes(&picks, &mut cache, &job, 4, |node| {
                        actual.push((node.scan, node.node, node.samples));
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(expected, actual);
            }
            assert_eq!(cache.stats().misses, picks.len() as u64);
            assert_eq!(cache.stats().hits, picks.len() as u64);
        }
    }
}

#[test]
fn parallel_view_stops_after_cancellation_and_reports_missing_data() {
    let dir = tempfile::tempdir().unwrap();
    let project = Arc::new(fixture(dir.path()));
    let picks: Vec<_> = project
        .scans()
        .flat_map(|s| {
            (0..s.nodes.len() as u32).map(|node| geemil_core::ViewPick {
                scan: s.id,
                node,
                quota: None,
            })
        })
        .collect();
    let job = JobControl::default();
    let mut received = 0;
    let error = project
        .load_view_nodes(&picks, &mut ViewCache::new(0), &job, 4, |_| {
            received += 1;
            job.cancel.store(true, Ordering::Relaxed);
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<geemil_core::CoreError>(),
        Some(geemil_core::CoreError::Cancelled)
    ));
    assert_eq!(received, 1);
    let scan = project.scans().next().unwrap();
    std::fs::remove_file(project.root.join(&scan.view_file)).unwrap();
    assert!(
        project
            .load_view_nodes(
                &picks,
                &mut ViewCache::new(0),
                &JobControl::default(),
                4,
                |_| Ok(())
            )
            .is_err()
    );
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
                left.read_view(left_scan, i as u32).unwrap(),
                right.read_view(right_scan, i as u32).unwrap()
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
        view_grid: 4,
        view_leaf_points: 32,
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
        view_grid: 4,
        view_leaf_points: 32,
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
        .points();
    let misses = cache.stats().misses;
    let b = p
        .load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
        .unwrap()
        .points();
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
    // Nodes are in scan coordinates, so moving a scan reads nothing and the
    // same points come back; the renderer places them with the new matrix.
    assert_eq!(cache.stats().misses, misses);
    assert!(moved.nodes.iter().all(|n| n.scan == id));
    assert_eq!(a, moved.points());
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
            .points()
            .is_empty()
    );
    // Hidden masks and empty node overhead share the same tiny budget.
    let mut tiny = ViewCache::new(128);
    for _ in 0..3 {
        assert!(
            p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut tiny)
                .unwrap()
                .points()
                .is_empty()
        );
        assert!(tiny.stats().resident_bytes <= 128);
        assert!(tiny.stats().hidden_bytes <= tiny.stats().resident_bytes);
    }
    tiny.set_point_limit(0);
    assert_eq!(tiny.stats().resident_bytes, 0);
    assert_eq!(tiny.stats().hidden_bytes, 0);
    p.switch(base).unwrap();
    assert_eq!(
        a,
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
            .unwrap()
            .points()
    );
    let mut small = ViewCache::new(128);
    assert_eq!(
        a,
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut small)
            .unwrap()
            .points()
    );
    assert!(small.stats().resident_bytes <= 128);
}

#[test]
fn partially_hidden_masks_share_the_view_cache_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = fixture(dir.path());
    let id = p.scans().next().unwrap().id;
    let camera = Camera {
        target: p.bounds().center().to_array(),
        distance: 30.,
        ..Camera::default()
    };
    let job = JobControl::default();
    p.move_selection(
        &Selection {
            camera,
            polygon: vec![[0., 0.], [0.5, 0.], [0.5, 1.], [0., 1.]],
            depth_meters: None,
            mode: SelectionMode::ExcludeInside,
        },
        &[id],
        &p.layer_named("Hidden"),
        &job,
    )
    .unwrap();
    let mut cache = ViewCache::new(8 * 1024 * 1024);
    let reference = p
        .load_view_cached(&camera, 100_000, &[id], &job, &mut cache)
        .unwrap()
        .points();
    assert!(!reference.is_empty());
    assert!(cache.stats().hidden_bytes > 0);
    for limit in [0, 128, 512, 8192] {
        let mut tiny = ViewCache::new(limit);
        for _ in 0..3 {
            assert_eq!(
                reference,
                p.load_view_cached(&camera, 100_000, &[id], &job, &mut tiny)
                    .unwrap()
                    .points()
            );
            assert!(tiny.stats().resident_bytes <= limit);
            assert!(tiny.stats().hidden_bytes <= tiny.stats().resident_bytes);
        }
        tiny.set_point_limit(0);
        assert_eq!(tiny.stats().resident_bytes, 0);
        assert_eq!(tiny.stats().hidden_bytes, 0);
    }
}

/// The display octree is additive like Potree 2's: every valid original point
/// is in exactly one node, with its own position and colour, and the per-chunk
/// counts of each node match its points. Nodes refine: deeper nodes are
/// smaller, and the tree goes below the chunks.
#[test]
fn display_octree_holds_every_valid_point_once() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("demo.e57");
    interchange::create_demo(&input).unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Test").unwrap();
    let options = ImportOptions {
        chunk_points: 4096,
        view_grid: 8,
        view_leaf_points: 256,
        ..Default::default()
    };
    p.import_file(&input, options, &JobControl::default())
        .unwrap();
    for scan in p.scans() {
        let mut seen = std::collections::HashSet::new();
        for (i, node) in scan.nodes.iter().enumerate() {
            let samples = p.read_view(scan, i as u32).unwrap();
            assert_eq!(samples.len(), node.count as usize);
            let mut counts = std::collections::BTreeMap::new();
            for s in &samples {
                assert!(seen.insert((s.chunk, s.index)), "point in two nodes");
                *counts.entry(s.chunk).or_insert(0u32) += 1;
                for axis in 0..3 {
                    assert!(s.position[axis] >= node.bounds.min[axis]);
                    assert!(s.position[axis] <= node.bounds.max[axis]);
                }
            }
            assert_eq!(counts.into_iter().collect::<Vec<_>>(), node.chunks);
            for &c in &node.children {
                assert!(scan.nodes[c as usize].bounds.radius() <= node.bounds.radius() + 1e-9);
            }
        }
        let mut valid = 0;
        for chunk in 0..scan.chunks.len() as u32 {
            for s in p.points(scan, chunk).unwrap() {
                valid += 1;
                assert!(seen.contains(&(s.chunk, s.index)), "point missing");
            }
        }
        assert_eq!(seen.len(), valid);
        assert_eq!(valid as u64, scan.valid_points);
        // Below the chunks: more nodes than chunks, and the root is sparse.
        assert!(scan.nodes.len() > scan.chunks.len());
        assert!((scan.nodes[0].count as u64) < scan.valid_points / 4);
    }
}
