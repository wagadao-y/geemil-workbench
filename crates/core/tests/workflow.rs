use geemil_core::{
    Camera, CoreError, ImportOptions, JobControl, Pose, Project, Selection, SelectionMode, Stage,
    interchange,
};
use glam::DVec3;
use std::{collections::BTreeMap, fs::File, io::BufReader, sync::atomic::Ordering};

/// The latest edit recorded in the working state.
fn last_operation(project: &Project) -> serde_json::Value {
    project.current().operation["operations"]
        .as_array()
        .and_then(|ops| ops.last())
        .cloned()
        .unwrap()
}

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
                view_grid: 4,
                view_leaf_points: 32,
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
                view_grid: 4,
                view_leaf_points: 32,
                ..Default::default()
            },
            &JobControl::default(),
        )
        .unwrap();
    let base = project.save_revision("Imported".into()).unwrap();
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
        depth_meters: Some(0.1),
        mode: SelectionMode::ExcludeInside,
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
        .move_selection(
            &selection,
            &ids,
            &project.layer_named("Deleted"),
            &JobControl::default(),
        )
        .unwrap();
    assert_eq!(count, expected);
    assert!(count > 0 && count < 32768);
    // Edits stay in the working state until saved.
    assert!(project.has_unsaved_changes());
    assert_eq!(project.manifest.current, base);
    let branch_a = project.save_revision("A".into()).unwrap();
    let output = dir.path().join("deleted.e57");
    project.export_e57(&output, &JobControl::default()).unwrap();
    let reader = e57::E57Reader::from_file(&output).unwrap();
    assert_eq!(
        reader.pointclouds().iter().map(|p| p.records).sum::<u64>(),
        32768 - count
    );
    // Saving from an older revision starts a branch.
    project.switch(base).unwrap();
    assert!(project.current().labels.is_empty());
    let first = project.scans().next().unwrap().id;
    project
        .set_transform(
            first,
            Pose {
                translation: [1., 0., 0.],
                ..Pose::default()
            },
        )
        .unwrap();
    let branch_b = project.save_revision("B".into()).unwrap();
    assert_ne!(branch_a, branch_b);
    for id in [branch_a, branch_b] {
        let r = project.manifest.revisions.iter().find(|r| r.id == id);
        assert_eq!(r.unwrap().parent, Some(base));
    }
    project.switch(branch_a).unwrap();
    assert_eq!(project.current().labels.len(), 1);
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
        if stage == Stage::ReadingE57 && done >= 8192 {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    let error = p
        .import_file(&source, ImportOptions::default(), &job)
        .unwrap_err();
    // The import context wraps the cancellation; UIs still recognise it.
    assert_eq!(CoreError::find(&error), Some(&CoreError::Cancelled));
    let reopened = Project::load(&p.root).unwrap();
    assert_eq!(reopened.manifest.current, initial);
    assert!(reopened.manifest.scans.is_empty());
    assert!(!reopened.has_unsaved_changes());
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
    let samples = p.read_view(scan, 0).unwrap();
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
                view_grid: 4,
                view_leaf_points: 32,
                ..Default::default()
            },
            &JobControl::default(),
        )
        .unwrap();
        assert_eq!(p.scans().count(), 1);
        assert_eq!(p.scans().next().unwrap().records, 100);
        let scan = p.scans().next().unwrap();
        let view = p.read_view(scan, 0).unwrap();
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

#[test]
fn user_facing_errors_are_typed() {
    let dir = tempfile::tempdir().unwrap();
    let find = |e: anyhow::Error| CoreError::find(&e).cloned();
    let root = dir.path().join("p");
    let p = Project::create(&root, "Test").unwrap();
    assert_eq!(
        find(Project::create(&root, "Test").unwrap_err()),
        Some(CoreError::ProjectExists(root.clone()))
    );
    assert_eq!(
        find(Project::load(dir.path()).unwrap_err()),
        Some(CoreError::NotAProject(dir.path().to_owned()))
    );
    let text = dir.path().join("points.txt");
    std::fs::write(&text, "0 0 0").unwrap();
    let mut imported = p.clone();
    assert_eq!(
        find(
            imported
                .import_file(&text, ImportOptions::default(), &JobControl::default())
                .unwrap_err()
        ),
        Some(CoreError::UnsupportedFormat)
    );
    assert_eq!(
        find(p.export_e57(&text, &JobControl::default()).unwrap_err()),
        Some(CoreError::OutputExists(text.clone()))
    );
}

#[test]
fn cropping_excludes_everything_outside_the_polygon_at_any_depth() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.e57");
    interchange::create_demo(&source).unwrap();
    let mut project = Project::create(&dir.path().join("p"), "Test").unwrap();
    let options = ImportOptions {
        chunk_points: 128,
        view_grid: 4,
        view_leaf_points: 32,
        ..Default::default()
    };
    let job = JobControl::default();
    project.import_file(&source, options, &job).unwrap();
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    let bounds = project.bounds();
    // Close to the cloud, so the far plane of `contains` would cut it off.
    let camera = Camera {
        target: bounds.center().to_array(),
        yaw: 0.3,
        pitch: 0.4,
        distance: 0.05,
        ..Camera::default()
    };
    let crop = Selection {
        camera,
        polygon: vec![[0.3, 0.2], [0.8, 0.35], [0.6, 0.9], [0.25, 0.7]],
        depth_meters: Some(0.1),
        mode: SelectionMode::ExcludeOutside,
    };
    let test = crop.prepare();
    let (mut total, mut outside) = (0u64, 0u64);
    for scan in project.scans() {
        let world = project.world_matrix(scan);
        for c in 0..scan.chunks.len() {
            let data = project.read_chunk(scan, c as u32).unwrap();
            for p in data.chunks_exact(scan.stride).filter(|p| p[28] != 0) {
                let pos = std::array::from_fn(|i| {
                    f64::from_le_bytes(p[i * 8..i * 8 + 8].try_into().unwrap())
                });
                let p = world.transform_point3(DVec3::from(pos));
                total += 1;
                if !test.covers(p) {
                    outside += 1;
                    assert!(test.excludes(p, 0.), "depth must not matter");
                } else {
                    assert!(!test.excludes(p, f64::INFINITY));
                }
            }
        }
    }
    assert!(outside > 0 && outside < total, "{outside} of {total}");
    // Points beyond the far plane are kept when seen through the polygon.
    let far = DVec3::from(camera.target) + (DVec3::from(camera.target) - camera.eye()) * 1e6;
    assert!(test.covers(far) && crop.contains(far).is_none());
    let excluded = project
        .move_selection(&crop, &ids, &project.layer_named("Deleted"), &job)
        .unwrap();
    assert_eq!(excluded, outside);
    // The recorded operation names the mode; older records default to inside.
    let op = &last_operation(&project);
    assert_eq!(op["selection"]["mode"], "exclude_outside");
    assert!(op["nearest"].is_null());
    let mut legacy = op["selection"].clone();
    legacy.as_object_mut().unwrap().remove("mode");
    let legacy: Selection = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.mode, SelectionMode::ExcludeInside);
    // Cropping again removes nothing more.
    assert_eq!(
        project
            .move_selection(&crop, &ids, &project.layer_named("Deleted"), &job)
            .unwrap(),
        0
    );
}

#[test]
fn selection_nearest_matches_the_recorded_exclusion() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.e57");
    interchange::create_demo(&source).unwrap();
    let mut project = Project::create(&dir.path().join("p"), "Test").unwrap();
    let job = JobControl::default();
    project
        .import_file(&source, ImportOptions::default(), &job)
        .unwrap();
    let ids: Vec<_> = project.scans().map(|s| s.id).collect();
    let bounds = project.bounds();
    let selection = Selection {
        camera: Camera {
            target: bounds.center().to_array(),
            distance: bounds.radius() * 2.,
            ..Camera::default()
        },
        polygon: vec![[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]],
        depth_meters: Some(0.2),
        mode: SelectionMode::ExcludeInside,
    };
    let nearest = project
        .selection_nearest(&selection, &ids, &job)
        .unwrap()
        .unwrap();
    assert!(
        project
            .move_selection(&selection, &ids, &project.layer_named("Deleted"), &job)
            .unwrap()
            > 0
    );
    assert_eq!(last_operation(&project)["nearest"], nearest);
}

/// What the app previews: displayed samples judged with the exact nearest depth.
/// They must carry exactly the exclusion bits that committing produces.
#[test]
fn preview_of_displayed_points_matches_the_committed_exclusion() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.e57");
    interchange::create_demo(&source).unwrap();
    let job = JobControl::default();
    let cases = [
        (SelectionMode::ExcludeInside, Some(0.3)),
        (SelectionMode::ExcludeInside, None),
        (SelectionMode::ExcludeOutside, None),
    ];
    for (i, (mode, depth_meters)) in cases.into_iter().enumerate() {
        let mut project = Project::create(&dir.path().join(i.to_string()), "Test").unwrap();
        let options = ImportOptions {
            chunk_points: 256,
            view_grid: 4,
            view_leaf_points: 32,
            ..Default::default()
        };
        project.import_file(&source, options, &job).unwrap();
        let ids: Vec<_> = project.scans().map(|s| s.id).collect();
        let bounds = project.bounds();
        let camera = Camera {
            target: bounds.center().to_array(),
            yaw: 0.8,
            pitch: 0.3,
            distance: bounds.radius() * 1.5,
            aspect: 1.5,
            ..Camera::default()
        };
        let selection = Selection {
            camera,
            polygon: vec![[0.3, 0.25], [0.75, 0.35], [0.6, 0.8], [0.35, 0.65]],
            depth_meters,
            mode,
        };
        let test = selection.prepare();
        let limit = if test.depth_limited() {
            project
                .selection_nearest(&selection, &ids, &job)
                .unwrap()
                .unwrap()
                + depth_meters.unwrap()
        } else {
            f64::INFINITY
        };
        // A small budget, so the view mixes LOD samples and full chunks.
        let mut previews = vec![];
        for id in &ids {
            let samples = project.load_view(&camera, 3000, &[*id], &job).unwrap();
            let marks: Vec<_> = samples
                .iter()
                .map(|s| test.excludes(DVec3::from(s.position), limit))
                .collect();
            previews.push((*id, samples, marks));
        }
        let excluded = project
            .move_selection(&selection, &ids, &project.layer_named("Deleted"), &job)
            .unwrap();
        assert!(excluded > 0, "{mode:?} {depth_meters:?}");
        let (mut marked, mut checked) = (0, 0);
        for (id, samples, marks) in previews {
            let scan = project.scans().find(|s| s.id == id).unwrap();
            for (sample, mark) in samples.iter().zip(marks) {
                let mask = project.hidden_mask(scan, sample.chunk).unwrap();
                let bit = mask[sample.index as usize / 8] & (1 << (sample.index % 8)) != 0;
                assert_eq!(
                    bit, mark,
                    "{mode:?} chunk {} index {}",
                    sample.chunk, sample.index
                );
                marked += mark as u32;
                checked += 1;
            }
        }
        assert!(
            marked > 0 && marked < checked,
            "{mode:?}: {marked} of {checked}"
        );
    }
}

/// Without a depth, excluding inside removes exactly what cropping keeps, at
/// any distance in front of the camera.
#[test]
fn unlimited_inside_exclusion_is_the_complement_of_cropping() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.e57");
    interchange::create_demo(&source).unwrap();
    let job = JobControl::default();
    let options = ImportOptions {
        chunk_points: 128,
        view_grid: 4,
        view_leaf_points: 32,
        ..Default::default()
    };
    let mut counts = vec![];
    for mode in [SelectionMode::ExcludeInside, SelectionMode::ExcludeOutside] {
        let mut project = Project::create(&dir.path().join(format!("{mode:?}")), "Test").unwrap();
        project.import_file(&source, options, &job).unwrap();
        let ids: Vec<_> = project.scans().map(|s| s.id).collect();
        let bounds = project.bounds();
        // Inside the cloud: some points lie behind the camera, some beyond the far plane.
        let selection = Selection {
            camera: Camera {
                target: bounds.center().to_array(),
                yaw: 0.3,
                pitch: 0.4,
                distance: 0.05,
                ..Camera::default()
            },
            polygon: vec![[0.3, 0.2], [0.8, 0.35], [0.6, 0.9], [0.25, 0.7]],
            depth_meters: None,
            mode,
        };
        let total: u64 = project.scans().map(|s| s.records).sum();
        counts.push((
            project
                .move_selection(&selection, &ids, &project.layer_named("Deleted"), &job)
                .unwrap(),
            total,
        ));
        assert!(last_operation(&project)["nearest"].is_null());
        assert!(last_operation(&project)["selection"]["depth_meters"].is_null());
    }
    let [(inside, total), (outside, _)] = counts[..] else {
        unreachable!()
    };
    // The demo has no invalid records, so every point is in exactly one result.
    assert!(inside > 0 && outside > 0);
    assert_eq!(inside + outside, total);
}

#[test]
fn uncoloured_e57_shows_intensity_as_grey() {
    use e57::{Record, RecordDataType as T, RecordName as N, RecordValue as V};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("intensity.e57");
    let mut writer = e57::E57Writer::from_file(&input, "fixture").unwrap();
    let schema = vec![
        Record {
            name: N::CartesianX,
            data_type: T::F64,
        },
        Record {
            name: N::CartesianY,
            data_type: T::F64,
        },
        Record {
            name: N::CartesianZ,
            data_type: T::F64,
        },
        Record {
            name: N::Intensity,
            data_type: T::Integer { min: 0, max: 1000 },
        },
    ];
    let mut scan = writer.add_pointcloud("scan", schema).unwrap();
    for (i, intensity) in [0i64, 500, 1000].into_iter().enumerate() {
        scan.add_point(vec![
            V::Double(i as f64),
            V::Double(0.),
            V::Double(0.),
            V::Integer(intensity),
        ])
        .unwrap();
    }
    scan.finalize().unwrap();
    writer.finalize().unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Intensity").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let scan = p.scans().next().unwrap();
    let mut colours: Vec<_> = p
        .points(scan, 0)
        .unwrap()
        .into_iter()
        .map(|s| (s.position[0] as i64, s.color))
        .collect();
    colours.sort();
    assert_eq!(
        colours,
        vec![
            (0, [0, 0, 0, 255]),
            (1, [128, 128, 128, 255]),
            (2, [255, 255, 255, 255])
        ]
    );
}
