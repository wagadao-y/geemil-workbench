use geemil_core::{Camera, ImportOptions, JobControl, Pose, Project, Selection, interchange};
use glam::DVec3;
use std::{collections::BTreeMap, fs::File, io::BufReader, sync::atomic::Ordering};

fn records(path: &std::path::Path) -> Vec<BTreeMap<(i64, i64), Vec<e57::RecordValue>>> {
    let mut reader = e57::E57Reader::from_file(path).unwrap();
    reader
        .pointclouds()
        .iter()
        .map(|pc| {
            reader
                .pointcloud_raw(pc)
                .unwrap()
                .map(|v| {
                    let v = v.unwrap();
                    let row = v[3].to_i64(&pc.prototype[3].data_type).unwrap();
                    let col = v[4].to_i64(&pc.prototype[4].data_type).unwrap();
                    ((row, col), v)
                })
                .collect()
        })
        .collect()
}

#[test]
fn portable_e57_roundtrip_preserves_structure_images_and_independent_poses() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.e57");
    interchange::create_demo(&source).unwrap();
    let expected = records(&source);
    let mut project = Project::create(&dir.path().join("project"), "Test").unwrap();
    project
        .import_file(
            &source,
            ImportOptions {
                chunk_points: 256,
                lod_points: 32,
                ..Default::default()
            },
            &JobControl::default(),
        )
        .unwrap();
    assert_eq!(project.scans().count(), 2);
    assert_eq!(project.manifest.images.len(), 4);
    assert!(project.scans().all(|s| s.chunks.len() > 1));
    let initial = project.manifest.current;
    let first = project.scans().next().unwrap().id;
    project
        .set_transform(
            first,
            Pose {
                translation: [0.75, 0., 0.],
                ..Pose::default()
            },
        )
        .unwrap();
    std::fs::remove_file(&source).unwrap();
    let project = Project::load(&project.root).unwrap();
    let output = dir.path().join("output.e57");
    project.export_e57(&output, &JobControl::default()).unwrap();
    assert_eq!(records(&output), expected);
    let mut reader = e57::E57Reader::from_file(&output).unwrap();
    assert_eq!(
        reader.pointclouds()[0]
            .transform
            .as_ref()
            .unwrap()
            .translation
            .x,
        0.75
    );
    assert_eq!(
        reader.pointclouds()[1]
            .transform
            .as_ref()
            .unwrap()
            .translation
            .x,
        3.
    );
    let mut template =
        e57::E57Reader::from_file(project.path(&project.manifest.scans[0].template).unwrap())
            .unwrap();
    for (a, b) in reader.images().iter().zip(template.images().iter()) {
        assert_eq!(a.pointcloud_guid, b.pointcloud_guid);
        let mut a_bytes = vec![];
        let mut b_bytes = vec![];
        let e57::Projection::Spherical(a_rep) = a.projection.as_ref().unwrap() else {
            panic!()
        };
        let e57::Projection::Spherical(b_rep) = b.projection.as_ref().unwrap() else {
            panic!()
        };
        reader.blob(&a_rep.blob.data, &mut a_bytes).unwrap();
        template.blob(&b_rep.blob.data, &mut b_bytes).unwrap();
        assert_eq!(a_bytes, b_bytes);
        let expected_delta =
            if a.pointcloud_guid.as_deref() == Some(&project.manifest.scans[0].guid) {
                0.75
            } else {
                0.
            };
        assert!(
            (a.transform.as_ref().unwrap().translation.x
                - b.transform.as_ref().unwrap().translation.x
                - expected_delta)
                .abs()
                < 1e-12
        );
    }
    let mut project = project;
    project.switch(initial).unwrap();
    assert_eq!(project.current().transforms.len(), 0);
}

#[test]
fn original_resolution_depth_selection_masks_forks_and_revision_switches() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.e57");
    interchange::create_demo(&source).unwrap();
    let mut project = Project::create(&dir.path().join("p"), "Test").unwrap();
    project
        .import_file(
            &source,
            ImportOptions {
                chunk_points: 128,
                lod_points: 2,
                ..Default::default()
            },
            &JobControl::default(),
        )
        .unwrap();
    let base = project.manifest.current;
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    let bounds = project.bounds();
    let camera = Camera {
        target: bounds.center().to_array(),
        yaw: 0.,
        pitch: 0.01,
        distance: 20.,
        ..Camera::default()
    };
    let selection = Selection {
        camera,
        polygon: vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
        depth_meters: 0.1,
    };
    let mut all_depths = vec![];
    for scan in project.scans() {
        let world = project.world_matrix(scan);
        for c in 0..scan.chunks.len() {
            let data = project.read_chunk(scan, c as u32).unwrap();
            for p in data.chunks_exact(scan.stride) {
                let pos = std::array::from_fn(|i| {
                    f64::from_le_bytes(p[i * 8..i * 8 + 8].try_into().unwrap())
                });
                if let Some(depth) = selection.contains(world.transform_point3(DVec3::from(pos))) {
                    all_depths.push(depth);
                }
            }
        }
    }
    let min = all_depths.iter().copied().fold(f64::INFINITY, f64::min);
    let expected = all_depths.iter().filter(|d| **d <= min + 0.1).count() as u64;
    let count = project
        .delete_selection(&selection, &ids, &JobControl::default())
        .unwrap();
    assert_eq!(count, expected);
    assert!(count > 0 && count < 32768);
    let branch_a = project.manifest.current;
    let output = dir.path().join("deleted.e57");
    project.export_e57(&output, &JobControl::default()).unwrap();
    let reader = e57::E57Reader::from_file(&output).unwrap();
    assert_eq!(
        reader.pointclouds().iter().map(|p| p.records).sum::<u64>(),
        32768 - count
    );
    project.switch(base).unwrap();
    project.fork("Alternative".into()).unwrap();
    assert!(project.current().layers.is_empty());
    let branch_b = project.manifest.current;
    assert_ne!(branch_a, branch_b);
    project.switch(branch_a).unwrap();
    assert_eq!(project.current().layers.len(), 1);
    let reloaded = Project::load(&project.root).unwrap();
    assert_eq!(reloaded.manifest.current, branch_a);
    assert!(reloaded.manifest.revisions.iter().any(|r| r.id == branch_b));
}

#[test]
fn cancellation_does_not_publish_an_import_revision() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.e57");
    interchange::create_demo(&source).unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Test").unwrap();
    let initial = p.manifest.current;
    let mut job = JobControl::default();
    let cancel = job.cancel.clone();
    job.progress = std::sync::Arc::new(move |stage, done, _| {
        if stage == "Reading E57" && done >= 8192 {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert!(
        p.import_file(&source, ImportOptions::default(), &job)
            .is_err()
    );
    let reopened = Project::load(&p.root).unwrap();
    assert_eq!(reopened.manifest.current, initial);
    assert!(reopened.manifest.scans.is_empty());
    assert!(job.cancel.load(Ordering::Relaxed));
}

#[test]
fn small_view_budget_represents_both_scans() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.e57");
    interchange::create_demo(&source).unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Test").unwrap();
    p.import_file(&source, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    p.set_transform(
        ids[1],
        Pose {
            translation: [100., 0., 0.],
            ..Pose::default()
        },
    )
    .unwrap();
    let camera = Camera {
        target: p.bounds().center().to_array(),
        distance: 200.,
        ..Camera::default()
    };
    let view = p
        .load_view(&camera, 32, &ids, &JobControl::default())
        .unwrap();
    assert!(view.len() <= 32);
    assert!(view.iter().any(|s| s.position[0] < 10.));
    assert!(view.iter().any(|s| s.position[0] > 90.));
}

#[test]
fn spherical_scaled_integer_and_invalid_records_survive_roundtrip() {
    use e57::{Record, RecordDataType as T, RecordName as N, RecordValue as V};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("spherical.e57");
    let mut writer = e57::E57Writer::from_file(&input, "fixture").unwrap();
    let schema = vec![
        Record {
            name: N::SphericalRange,
            data_type: T::ScaledInteger {
                min: 0,
                max: 10000,
                scale: 0.001,
                offset: 0.,
            },
        },
        Record {
            name: N::SphericalAzimuth,
            data_type: T::F64,
        },
        Record {
            name: N::SphericalElevation,
            data_type: T::F32,
        },
        Record {
            name: N::SphericalInvalidState,
            data_type: T::Integer { min: 0, max: 2 },
        },
        Record {
            name: N::RowIndex,
            data_type: T::Integer {
                min: 0,
                max: 1i64 << 60,
            },
        },
    ];
    let rows = vec![
        vec![
            V::ScaledInteger(1234),
            V::Double(0.5),
            V::Single(0.2),
            V::Integer(0),
            V::Integer((1i64 << 54) + 1),
        ],
        vec![
            V::ScaledInteger(5000),
            V::Double(1.),
            V::Single(-0.1),
            V::Integer(1),
            V::Integer((1i64 << 54) + 3),
        ],
    ];
    let mut pc = writer.add_pointcloud("scan", schema).unwrap();
    for row in &rows {
        pc.add_point(row.clone()).unwrap();
    }
    pc.finalize().unwrap();
    writer.finalize().unwrap();
    drop(writer);
    let mut p = Project::create(&dir.path().join("p"), "Test").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let scan = p.scans().next().unwrap();
    assert_eq!(scan.records, 2);
    assert_eq!(scan.valid_points, 1);
    let samples = p.read_lod(scan, 0).unwrap();
    assert_eq!(samples.len(), 1);
    assert!((samples[0].position[0] - 1.234 * 0.5f64.cos() * (0.2f32 as f64).cos()).abs() < 1e-12);
    let output = dir.path().join("out.e57");
    p.export_e57(&output, &JobControl::default()).unwrap();
    let mut reader = e57::E57Reader::from_file(&output).unwrap();
    let pc = reader.pointclouds()[0].clone();
    let mut actual: Vec<_> = reader
        .pointcloud_raw(&pc)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    actual.sort_by_key(|r| match r[4] {
        V::Integer(v) => v,
        _ => unreachable!(),
    });
    assert_eq!(actual, rows);
}

#[test]
fn las_file_is_one_scan_and_laz_is_readable() {
    let dir = tempfile::tempdir().unwrap();
    for ext in ["las", "laz"] {
        let source = dir.path().join(format!("test.{ext}"));
        let mut builder = las::Builder::from((1, 2));
        builder.point_format = las::point::Format::new(2).unwrap();
        builder.transforms.y.offset = 4000000.;
        let mut writer = las::Writer::from_path(&source, builder.into_header().unwrap()).unwrap();
        for i in 0..100 {
            writer
                .write_point(las::Point {
                    x: 500000. + i as f64 * 0.01,
                    y: 4000000.,
                    z: 2.,
                    intensity: i,
                    color: Some(las::Color::new(65535, 0, 0)),
                    ..Default::default()
                })
                .unwrap();
        }
        writer.close().unwrap();
        drop(writer);
        let mut p = Project::create(&dir.path().join(format!("project-{ext}")), "LAS").unwrap();
        p.import_file(
            &source,
            ImportOptions {
                chunk_points: 16,
                lod_points: 4,
                ..Default::default()
            },
            &JobControl::default(),
        )
        .unwrap();
        assert_eq!(p.scans().count(), 1);
        assert_eq!(p.scans().next().unwrap().records, 100);
        let scan = p.scans().next().unwrap();
        let view = p.read_lod(scan, 0).unwrap();
        assert!(
            view.iter()
                .all(|p| p.position[0] >= 500000. && p.color[0] == 255)
        );
    }
}

#[test]
fn e57_template_contains_no_duplicate_point_payload() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.e57");
    interchange::create_demo(&input).unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Test").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let scan = p.scans().next().unwrap();
    let template = e57::E57Reader::new(BufReader::new(
        File::open(p.path(&scan.template).unwrap()).unwrap(),
    ))
    .unwrap();
    assert!(template.pointclouds().iter().all(|pc| pc.records == 0));
    assert_eq!(template.images().len(), 4);
}
