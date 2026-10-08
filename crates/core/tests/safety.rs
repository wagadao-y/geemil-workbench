use geemil_core::{CoreError, DEFAULT_LAYER, ImportOptions, JobControl, Pose, Project, Stage};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

// A few small chunks exercise partial writes without large point clouds or sleeps.
fn project(dir: &Path) -> Project {
    let source = dir.join("tiny.las");
    let mut writer = las::Writer::from_path(&source, las::Header::default()).unwrap();
    for i in 0..128 {
        writer
            .write_point(las::Point {
                x: (i % 16) as f64 * 0.1,
                y: (i / 16) as f64 * 0.1,
                z: 0.,
                ..Default::default()
            })
            .unwrap();
    }
    writer.close().unwrap();
    let mut p = Project::create(&dir.join("project"), "Safety").unwrap();
    p.import_file(
        &source,
        ImportOptions {
            chunk_points: 16,
            view_grid: 4,
            view_leaf_points: 16,
            worker_threads: 1,
            ..Default::default()
        },
        &JobControl::default(),
    )
    .unwrap();
    p
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn scan_file(p: &Project) -> PathBuf {
    let file = read_json(&p.root.join("project.json"));
    p.root.join(file["scans"][0].as_str().unwrap())
}

fn state(p: &Project) -> Value {
    serde_json::to_value(&p.manifest).unwrap()
}

fn assert_unchanged(p: &Project, before: &Value, saved: &[u8]) {
    assert_eq!(&state(p), before);
    assert_eq!(fs::read(p.root.join("project.json")).unwrap(), saved);
    assert_eq!(&state(&Project::load(&p.root).unwrap()), before);
}

#[test]
fn malformed_scan_metadata_returns_errors_before_display_or_editing() {
    let dir = tempfile::tempdir().unwrap();
    let p = project(dir.path());
    let path = scan_file(&p);
    let original = read_json(&path);
    assert!(original["nodes"].as_array().unwrap().len() > 1);
    let child = original["nodes"][0]["children"][0].clone();
    let mut bad_coordinates = original["coordinates"].clone();
    bad_coordinates["fields"][0]["offset"] = json!(usize::MAX);
    let mut overlapping_coordinates = original["coordinates"].clone();
    overlapping_coordinates["fields"][0]["offset"] = json!(0);
    for (field, value, message) in [
        (
            "/nodes/0/children",
            json!([u32::MAX]),
            "Invalid display tree child",
        ),
        (
            "/nodes/0/children",
            json!([0]),
            "Invalid display tree child",
        ),
        (
            "/nodes/0/children",
            json!([child.clone(), child]),
            "multiple parents",
        ),
        ("/nodes/0/chunks", json!([[u32::MAX, 1]]), "missing chunk"),
        (
            "/coordinates",
            json!({"kind": "stored", "offset": usize::MAX}),
            "coordinate layout",
        ),
        ("/coordinates", bad_coordinates, "coordinate layout"),
        ("/coordinates", overlapping_coordinates, "coordinate layout"),
        ("/nodes/0/children", json!([]), "Disconnected display tree"),
        ("/template", json!("../outside.e57"), "asset path"),
        (
            "/original_pose",
            json!({"translation": [0., 0., 0.], "rotation_xyzw": [0., 0., 0., 0.]}),
            "Invalid pose",
        ),
    ] {
        let mut bad = original.clone();
        *bad.pointer_mut(field).unwrap() = value;
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        // A panic fails this test; corrupt projects must return an ordinary error.
        let error = Project::load(&p.root).unwrap_err();
        assert!(format!("{error:#}").contains(message), "{field}: {error:#}");
    }
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(state(&Project::load(&p.root).unwrap()), state(&p));
}

#[test]
fn invalid_transforms_are_rejected_without_changing_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::create(&dir.path().join("project"), "Safety").unwrap();
    let folder = p.create_group("Folder".into(), None).unwrap();
    let before = state(&p);
    let saved = fs::read(p.root.join("project.json")).unwrap();
    for pose in [
        Pose {
            rotation_xyzw: [0.; 4],
            ..Default::default()
        },
        Pose {
            rotation_xyzw: [1e-300; 4],
            ..Default::default()
        },
        Pose {
            rotation_xyzw: [1e308; 4],
            ..Default::default()
        },
        Pose {
            rotation_xyzw: [0., 0., f64::NAN, 1.],
            ..Default::default()
        },
        Pose {
            translation: [f64::INFINITY, 0., 0.],
            ..Default::default()
        },
        Pose {
            translation: [0., f64::NAN, 0.],
            ..Default::default()
        },
    ] {
        assert!(p.set_transform(folder, pose).is_err());
        assert_unchanged(&p, &before, &saved);
    }
    // Non-unit but usable quaternions remain supported: matrix() normalizes them.
    p.set_transform(
        folder,
        Pose {
            rotation_xyzw: [0., 0., 0., 2.],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        Project::load(&p.root)
            .unwrap()
            .correction(folder)
            .is_finite()
    );
}

#[test]
fn invalid_saved_transforms_and_folder_cycles_are_rejected_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::create(&dir.path().join("project"), "Safety").unwrap();
    let folder = p.create_group("Folder".into(), None).unwrap();
    let path = p.root.join("project.json");
    let original = read_json(&path);
    let mut bad = original.clone();
    bad["draft"]["transforms"][folder.to_string()] = json!({
        "translation": [0., 0., 0.], "rotation_xyzw": [0., 0., 0., 0.]
    });
    fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(
        Project::load(&p.root)
            .unwrap_err()
            .to_string()
            .contains("Invalid pose")
    );
    bad = original.clone();
    bad["draft"]["groups"][0]["parent"] = json!(folder);
    fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(
        Project::load(&p.root)
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
}

#[test]
fn e57_with_an_invalid_scanner_pose_does_not_publish_an_import() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("invalid-pose.e57");
    let mut writer = e57::E57Writer::from_file(&input, "fixture").unwrap();
    let schema = [
        e57::RecordName::CartesianX,
        e57::RecordName::CartesianY,
        e57::RecordName::CartesianZ,
    ]
    .map(|name| e57::Record {
        name,
        data_type: e57::RecordDataType::F64,
    })
    .to_vec();
    let mut scan = writer.add_pointcloud("scan", schema).unwrap();
    scan.set_transform(Some(
        Pose {
            rotation_xyzw: [0.; 4],
            ..Default::default()
        }
        .to_e57(),
    ));
    scan.add_point(vec![e57::RecordValue::Double(1.); 3])
        .unwrap();
    scan.finalize().unwrap();
    writer.finalize().unwrap();
    drop(writer);
    let mut p = Project::create(&dir.path().join("project"), "Safety").unwrap();
    let before = state(&p);
    let saved = fs::read(p.root.join("project.json")).unwrap();
    let error = p
        .import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap_err();
    assert!(format!("{error:#}").contains("Invalid pose"));
    assert_unchanged(&p, &before, &saved);
    assert_eq!(fs::read_dir(p.root.join("data")).unwrap().count(), 0);
}

fn cancelling_job(stage: Stage) -> (JobControl, Arc<AtomicBool>) {
    let mut job = JobControl::default();
    let cancel = job.cancel.clone();
    let reached = Arc::new(AtomicBool::new(false));
    let observed = reached.clone();
    job.progress = Arc::new(move |current, done, _| {
        if current == stage && done >= 2 {
            observed.store(true, Ordering::Relaxed);
            cancel.store(true, Ordering::Relaxed);
        }
    });
    (job, reached)
}

#[test]
fn cancelling_filters_after_processed_chunks_preserves_points_and_reopened_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    p.filter_options.worker_threads = 1;
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    let before = state(&p);
    let saved = fs::read(p.root.join("project.json")).unwrap();
    for stage in [Stage::NoiseFilter, Stage::Subsampling] {
        let (job, reached) = cancelling_job(stage);
        let target = p.layer_named("Cancelled");
        let error = if stage == Stage::NoiseFilter {
            p.remove_noise(0.01, 1, &ids, &target, &job)
        } else {
            p.subsample(10., &ids, &target, &job)
        }
        .unwrap_err();
        assert!(reached.load(Ordering::Relaxed));
        assert_eq!(CoreError::find(&error), Some(&CoreError::Cancelled));
        assert_unchanged(&p, &before, &saved);
        // Interrupted label writes are reclaimed by cleanup without removing live data.
        p.cleanup().unwrap();
        assert_eq!(fs::read_dir(p.root.join("staging")).unwrap().count(), 0);
        let scan = p.scans().next().unwrap();
        for chunk in 0..scan.chunks.len() as u32 {
            assert!(
                p.labels(scan, chunk)
                    .unwrap()
                    .iter()
                    .all(|&label| label == DEFAULT_LAYER)
            );
            p.read_chunk(scan, chunk).unwrap();
        }
    }
}

#[test]
fn cancelled_exports_remove_partial_output_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let p = project(dir.path());
    let before = state(&p);
    let saved = fs::read(p.root.join("project.json")).unwrap();
    let output_dir = dir.path().join("exports");
    fs::create_dir(&output_dir).unwrap();
    for extension in ["e57", "las", "laz"] {
        let output = output_dir.join(format!("cloud.{extension}"));
        let stage = if extension == "e57" {
            Stage::WritingE57
        } else {
            Stage::WritingLas
        };
        let (job, reached) = cancelling_job(stage);
        let export = |job: &JobControl| -> anyhow::Result<()> {
            if extension == "e57" {
                p.export_e57(&output, job)
            } else {
                p.export_las(&output, job).map(|_| ())
            }
        };
        let error = export(&job).unwrap_err();
        assert!(reached.load(Ordering::Relaxed));
        assert_eq!(CoreError::find(&error), Some(&CoreError::Cancelled));
        assert_eq!(fs::read_dir(&output_dir).unwrap().count(), 0);
        assert_unchanged(&p, &before, &saved);
        export(&JobControl::default()).unwrap();
        assert!(output.is_file());
        if extension == "e57" {
            let reader = e57::E57Reader::from_file(&output).unwrap();
            assert_eq!(reader.pointclouds()[0].records, 128);
        } else {
            let mut reader = las::Reader::from_path(&output).unwrap();
            let points: Vec<_> = reader
                .read_all()
                .unwrap()
                .points()
                .map(Result::unwrap)
                .collect();
            assert_eq!(points.len(), 128);
        }
        let completed = fs::read(&output).unwrap();
        assert!(matches!(
            CoreError::find(&export(&JobControl::default()).unwrap_err()),
            Some(CoreError::OutputExists(_))
        ));
        assert_eq!(fs::read(&output).unwrap(), completed);
        fs::remove_file(output).unwrap();
    }
}

#[test]
fn truncated_point_and_view_files_return_errors() {
    let dir = tempfile::tempdir().unwrap();
    let p = project(dir.path());
    let scan = p.scans().next().unwrap();
    for (file, view) in [(&scan.points_file, false), (&scan.view_file, true)] {
        let path = p.path(file).unwrap();
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, []).unwrap();
        let error = if view {
            p.read_view(scan, 0).map(|_| ())
        } else {
            p.read_chunk(scan, 0).map(|_| ())
        }
        .unwrap_err();
        assert!(error.to_string().contains("Truncated"));
        fs::write(path, bytes).unwrap();
    }
}

#[test]
fn invalid_label_offsets_and_counts_return_errors_without_large_allocations() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path());
    let unlabelled = p.manifest.draft.clone();
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    p.remove_noise(
        0.01,
        1,
        &ids,
        &p.layer_named("Noise"),
        &JobControl::default(),
    )
    .unwrap();
    let manifest = read_json(&p.root.join("project.json"));
    let path = p.root.join(manifest["labels"][0].as_str().unwrap());
    let mut patch = read_json(&path);
    patch["blocks"][0]["offset"] = json!(u64::MAX);
    fs::write(path, serde_json::to_vec(&patch).unwrap()).unwrap();
    let loaded = Project::load(&p.root).unwrap();
    let scan = loaded.scans().next().unwrap();
    let chunk = patch["blocks"][0]["chunk"].as_u64().unwrap() as u32;
    assert!(
        loaded
            .labels(scan, chunk)
            .unwrap_err()
            .to_string()
            .contains("Truncated labels")
    );
    // Unlabelled chunks must enforce the allocation budget as well.
    p.restore_working_state(unlabelled).unwrap();
    let path = scan_file(&p);
    let mut scan = read_json(&path);
    scan["chunks"][0]["count"] = json!(u32::MAX);
    fs::write(path, serde_json::to_vec(&scan).unwrap()).unwrap();
    let loaded = Project::load(&p.root).unwrap();
    assert!(loaded.current().labels.is_empty());
    assert!(
        loaded
            .labels(loaded.scans().next().unwrap(), 0)
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
}

#[test]
fn export_read_failures_remove_partial_output_and_preserve_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let p = project(dir.path());
    let before = state(&p);
    let saved = fs::read(p.root.join("project.json")).unwrap();
    let output_dir = dir.path().join("exports");
    fs::create_dir(&output_dir).unwrap();
    fs::write(p.path(&p.scans().next().unwrap().points_file).unwrap(), []).unwrap();
    for extension in ["e57", "las", "laz"] {
        let output = output_dir.join(format!("cloud.{extension}"));
        let error = if extension == "e57" {
            p.export_e57(&output, &JobControl::default())
        } else {
            p.export_las(&output, &JobControl::default()).map(|_| ())
        }
        .unwrap_err();
        assert!(format!("{error:#}").contains("Truncated point data"));
        assert_eq!(fs::read_dir(&output_dir).unwrap().count(), 0);
        assert_unchanged(&p, &before, &saved);
    }
}

#[cfg(windows)]
#[test]
fn failed_manifest_replacement_keeps_the_previous_project_and_removes_the_temporary_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::create(&dir.path().join("project"), "Safety").unwrap();
    let before = state(&p);
    let path = p.root.join("project.json");
    let saved = fs::read(&path).unwrap();
    // Permit reads/writes but deny replacement while this handle is open.
    let blocker = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(p.create_group("Cannot save".into(), None).is_err());
    drop(blocker);
    assert_unchanged(&p, &before, &saved);
    assert!(!fs::read_dir(&p.root).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|e| e == "tmp")
    }));
    p.create_group("Retry".into(), None).unwrap();
    assert_eq!(Project::load(&p.root).unwrap().groups().len(), 1);
}
