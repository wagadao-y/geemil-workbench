use geemil_core::{
    BlockCodec, Camera, ImportOptions, JobControl, Pose, Project, Selection, ViewCache, interchange,
};
use std::{
    fs::File,
    io::Write,
    sync::{Arc, atomic::Ordering},
};

fn fixture(root: &std::path::Path) -> Project {
    let input = root.join("demo.e57");
    interchange::create_demo(&input).unwrap();
    let mut p = Project::create(&root.join("project"), "Test").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
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
        if stage == "Indexing" && done > 0 {
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

/// Build the exact version-1 disk layout so compatibility is tested independently
/// of the new compressor. References/order are those of the imported project.
fn make_legacy(p: &mut Project) {
    for i in 0..p.manifest.scans.len() {
        let source = p.manifest.scans[i].clone();
        let mut raw = source.clone();
        raw.points_file += ".raw";
        raw.lod_file += ".raw";
        let mut file = File::create(p.path(&raw.points_file).unwrap()).unwrap();
        let mut offset = 0;
        for (chunk, meta) in raw.chunks.iter_mut().enumerate() {
            let data = p.read_chunk(&source, chunk as u32).unwrap();
            meta.offset = offset;
            meta.codec = BlockCodec::Raw;
            meta.stored_bytes = 0;
            file.write_all(&data).unwrap();
            offset += data.len() as u64;
        }
        drop(file);
        let mut file = File::create(p.path(&raw.lod_file).unwrap()).unwrap();
        offset = 0;
        for (node, meta) in raw.nodes.iter_mut().enumerate() {
            meta.lod_offset = offset;
            meta.lod_codec = BlockCodec::Raw;
            meta.lod_bytes = 0;
            for sample in p.read_lod(&source, node as u32).unwrap() {
                file.write_all(&sample.chunk.to_le_bytes()).unwrap();
                file.write_all(&sample.index.to_le_bytes()).unwrap();
                for x in sample.position {
                    file.write_all(&x.to_le_bytes()).unwrap();
                }
                file.write_all(&sample.color).unwrap();
                offset += 36;
            }
        }
        p.manifest.scans[i] = raw;
    }
    p.manifest.format_version = 1;
    p.save().unwrap();
    // Version 1 had no codec/length fields at all.
    let path = p.root.join("project.json");
    let mut json = serde_json::to_value(&p.manifest).unwrap();
    for scan in json["scans"].as_array_mut().unwrap() {
        for chunk in scan["chunks"].as_array_mut().unwrap() {
            let map = chunk.as_object_mut().unwrap();
            map.remove("codec");
            map.remove("stored_bytes");
        }
        for node in scan["nodes"].as_array_mut().unwrap() {
            let map = node.as_object_mut().unwrap();
            map.remove("lod_codec");
            map.remove("lod_bytes");
        }
    }
    serde_json::to_writer(File::create(path).unwrap(), &json).unwrap();
    *p = Project::load(&p.root).unwrap();
}

#[test]
fn legacy_conversion_preserves_references_revisions_and_masks() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = fixture(dir.path());
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    let bounds = p.bounds();
    let camera = Camera {
        target: bounds.center().to_array(),
        distance: 20.,
        ..Camera::default()
    };
    p.delete_selection(
        &Selection {
            camera,
            polygon: vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            depth_meters: 0.1,
        },
        &ids,
        &JobControl::default(),
    )
    .unwrap();
    make_legacy(&mut p);
    let before = p.manifest.clone();
    let scan = p.scans().next().unwrap().clone();
    let bytes = p.read_chunk(&scan, 0).unwrap();
    let mask = p.exclusion_mask(&scan, 0).unwrap();
    p.compress_storage(&JobControl::default()).unwrap();
    let reopened = Project::load(&p.root).unwrap();
    assert_eq!(reopened.manifest.format_version, 2);
    assert_eq!(reopened.manifest.current, before.current);
    assert_eq!(reopened.manifest.revisions.len(), before.revisions.len());
    let new_scan = reopened.scans().next().unwrap();
    assert_eq!(new_scan.id, scan.id);
    assert_eq!(reopened.read_chunk(new_scan, 0).unwrap(), bytes);
    assert_eq!(reopened.exclusion_mask(new_scan, 0).unwrap(), mask);
    assert_eq!(new_scan.chunks[0].codec, BlockCodec::ZstdShuffle);
    assert!(new_scan.chunks[0].stored_bytes < bytes.len() as u32);
    assert!(!p.path(&scan.points_file).unwrap().exists());
    let path = new_scan.points_file.clone();
    p.compress_storage(&JobControl::default()).unwrap();
    assert_eq!(p.scans().next().unwrap().points_file, path);
}

#[test]
fn cancelled_conversion_keeps_legacy_assets_and_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = fixture(dir.path());
    make_legacy(&mut p);
    let original = serde_json::to_value(&p.manifest).unwrap();
    let mut job = JobControl::default();
    let cancel = job.cancel.clone();
    job.progress = Arc::new(move |stage, _, _| {
        if stage == "Compressing LOD" {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert!(p.compress_storage(&job).is_err());
    let reopened = Project::load(&p.root).unwrap();
    assert_eq!(serde_json::to_value(&reopened.manifest).unwrap(), original);
    assert!(
        reopened
            .read_chunk(reopened.scans().next().unwrap(), 0)
            .is_ok()
    );
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
        .unwrap();
    let misses = cache.stats().misses;
    let b = p
        .load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(cache.stats().misses, misses);
    assert!(cache.stats().hits > 0);
    p.set_transform(
        id,
        Pose {
            translation: [0.25, 0., 0.],
            ..Pose::default()
        },
    )
    .unwrap();
    let moved = p
        .load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
        .unwrap();
    assert_eq!(a.len(), moved.len());
    for (a, b) in a.iter().zip(moved) {
        assert!((a.position[0] + 0.25 - b.position[0]).abs() < 1e-12);
    }
    p.delete_selection(
        &Selection {
            camera,
            polygon: vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            depth_meters: 1000.,
        },
        &[id],
        &JobControl::default(),
    )
    .unwrap();
    assert!(
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
            .unwrap()
            .is_empty()
    );
    p.switch(base).unwrap();
    assert_eq!(
        a,
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut cache)
            .unwrap()
    );
    let mut small = ViewCache::new(128);
    assert_eq!(
        a,
        p.load_view_cached(&camera, 100_000, &[id], &JobControl::default(), &mut small)
            .unwrap()
    );
    assert!(small.stats().resident_bytes <= 128);
}
