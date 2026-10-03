//! The working state, saved revisions, undo snapshots, the scan tree and cleanup.
use geemil_core::{
    Camera, CoreError, ImportOptions, JobControl, Pose, Project, Selection, SelectionMode,
    interchange,
};
use glam::{DMat4, DVec3};
use std::path::Path;

fn imported(dir: &Path) -> Project {
    let source = dir.join("demo.e57");
    interchange::create_demo(&source).unwrap();
    let mut p = Project::create(&dir.join("p"), "Test").unwrap();
    p.import_file(&source, ImportOptions::default(), &JobControl::default())
        .unwrap();
    p
}

fn shift(x: f64) -> Pose {
    Pose {
        translation: [x, 0., 0.],
        ..Pose::default()
    }
}

/// Moves every point to a new, hidden "Deleted" layer.
fn exclude_everything(p: &mut Project) -> u64 {
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    let selection = Selection {
        camera: Camera {
            target: p.bounds().center().to_array(),
            distance: 30.,
            ..Camera::default()
        },
        polygon: vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
        depth_meters: None,
        mode: SelectionMode::ExcludeInside,
    };
    let target = p.layer_named("Deleted");
    p.move_selection(&selection, &ids, &target, &JobControl::default())
        .unwrap()
}

#[test]
fn edits_stay_unsaved_until_saved_and_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let created = p.manifest.current;
    assert!(p.has_unsaved_changes());
    assert_eq!(p.manifest.revisions.len(), 1);
    // The working state is on disk, so a crash or restart keeps it.
    let reopened = Project::load(&p.root).unwrap();
    assert_eq!(reopened.scans().count(), 2);
    assert!(reopened.has_unsaved_changes());

    let v1 = p.save_revision("Imported".into()).unwrap();
    assert!(!p.has_unsaved_changes());
    assert_eq!(p.manifest.current, v1);
    let saved = p.manifest.revisions.iter().find(|r| r.id == v1).unwrap();
    assert_eq!(saved.parent, Some(created));
    assert!(saved.saved_at.is_some());
    assert_eq!(saved.operation["operations"][0]["kind"], "import");

    // Showing and hiding a layer adds no revisions.
    exclude_everything(&mut p);
    let layer = p.current().layers.last().unwrap().code;
    for visible in [true, false, true] {
        p.set_layer_visible(layer, visible).unwrap();
    }
    assert_eq!(p.manifest.revisions.len(), 2);
    assert!(p.layer(layer).unwrap().visible);
    let ops = p.current().operation["operations"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(ops, 4);

    p.discard_changes().unwrap();
    assert!(!p.has_unsaved_changes());
    assert_eq!(p.current().id, v1);
    assert!(p.save_revision("Nothing".into()).is_err());
}

#[test]
fn undo_snapshots_restore_states_with_their_identity() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    p.save_revision("Imported".into()).unwrap();
    let scan = p.scans().next().unwrap().id;
    let mut snapshots = vec![p.manifest.draft.clone()];
    p.set_transform(scan, shift(1.)).unwrap();
    snapshots.push(p.manifest.draft.clone());
    let moved = p.current().id;
    exclude_everything(&mut p);
    snapshots.push(p.manifest.draft.clone());
    assert_ne!(p.current().id, moved, "every edit gets a new identity");

    // Undo twice, then redo once.
    p.restore_working_state(snapshots[1].clone()).unwrap();
    assert_eq!(p.current().id, moved);
    assert!(p.current().labels.is_empty());
    assert_eq!(p.current().layers.len(), 1);
    assert_eq!(p.current().transforms[&scan], shift(1.));
    p.restore_working_state(snapshots[0].clone()).unwrap();
    assert!(!p.has_unsaved_changes());
    assert!(p.current().transforms.is_empty());
    p.restore_working_state(snapshots[1].clone()).unwrap();
    assert_eq!(Project::load(&p.root).unwrap().current().id, moved);
}

#[test]
fn multi_scan_import_creates_a_folder_and_folder_transforms_compose() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let (groups, top_scans) = p.children(None);
    assert_eq!(groups.len(), 1);
    assert!(top_scans.is_empty());
    let folder = groups[0].id;
    assert_eq!(groups[0].name, "demo");
    let scans: Vec<_> = p.children(Some(folder)).1.iter().map(|s| s.id).collect();
    assert_eq!(scans.len(), 2);
    assert_eq!(p.scans_within(folder), scans);

    let world = |p: &Project, id| {
        let scan = p.scans().find(|s| s.id == id).unwrap();
        p.world_matrix(scan)
    };
    let before: Vec<_> = scans.iter().map(|id| world(&p, *id)).collect();
    p.set_transform(scans[0], shift(1.)).unwrap();
    p.set_transform(folder, shift(10.)).unwrap();
    for (i, id) in scans.iter().enumerate() {
        let expected =
            DMat4::from_translation(DVec3::new(10. + (i == 0) as u8 as f64, 0., 0.)) * before[i];
        assert!(world(&p, *id).abs_diff_eq(expected, 1e-9));
    }

    // Nested folders: moving keeps points in place; folders cannot nest in themselves.
    let outer = p.create_group("Building".into(), None).unwrap();
    p.set_transform(outer, shift(-3.)).unwrap();
    let placed: Vec<_> = scans.iter().map(|id| world(&p, *id)).collect();
    p.move_to_group(&[folder], Some(outer)).unwrap();
    p.move_to_group(&[scans[1]], None).unwrap();
    for (i, id) in scans.iter().enumerate() {
        assert!(world(&p, *id).abs_diff_eq(placed[i], 1e-9));
    }
    assert!(p.move_to_group(&[outer], Some(folder)).is_err());
    assert!(p.move_to_group(&[outer], Some(outer)).is_err());
    assert_eq!(p.parent_of(folder), Some(outer));

    // Dissolving a folder keeps its contents in place too.
    p.ungroup(outer).unwrap();
    assert_eq!(p.parent_of(folder), None);
    for (i, id) in scans.iter().enumerate() {
        assert!(world(&p, *id).abs_diff_eq(placed[i], 1e-9));
    }

    // Export applies the composed transform.
    let output = dir.path().join("out.e57");
    p.export_e57(&output, &JobControl::default()).unwrap();
    let reader = e57::E57Reader::from_file(&output).unwrap();
    let exported = reader.pointclouds()[0].transform.clone().unwrap();
    let expected = Pose::from_matrix(world(&p, scans[0]));
    assert!((exported.translation.x - expected.translation[0]).abs() < 1e-9);

    // Removing scans is an edit like any other.
    p.remove_scans(&[scans[1]]).unwrap();
    assert_eq!(p.scans().count(), 1);
    assert!(Project::load(&p.root).is_ok());
}

#[test]
fn deleting_revisions_reparents_children_and_keeps_the_current_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let root = p.manifest.current;
    let a = p.save_revision("A".into()).unwrap();
    let scan = p.scans().next().unwrap().id;
    p.set_transform(scan, shift(1.)).unwrap();
    let b = p.save_revision("B".into()).unwrap();
    assert!(p.delete_revision(b).is_err());
    p.delete_revision(a).unwrap();
    let b = p.manifest.revisions.iter().find(|r| r.id == b).unwrap();
    assert_eq!(b.parent, Some(root));
    p.rename_revision(root, "Start".into()).unwrap();
    assert_eq!(
        Project::load(&p.root).unwrap().manifest.revisions[0].name,
        "Start"
    );
}

#[test]
fn cleanup_removes_only_data_no_state_refers_to() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let kept = p.save_revision("Imported".into()).unwrap();
    // A second import, moved points and a staging leftover, all abandoned.
    let source = dir.path().join("demo.e57");
    p.import_file(&source, ImportOptions::default(), &JobControl::default())
        .unwrap();
    assert_eq!(p.scans().count(), 4);
    exclude_everything(&mut p);
    std::fs::create_dir(p.root.join("staging/leftover")).unwrap();
    std::fs::write(p.root.join("staging/leftover/x"), b"x").unwrap();
    let dirs = |p: &Project| std::fs::read_dir(p.root.join("data")).unwrap().count();
    assert_eq!(dirs(&p), 2);
    // The unsaved state still refers to everything: nothing scan-related goes.
    let report = p.cleanup().unwrap();
    assert_eq!((report.scans, report.labels), (0, 0));
    assert_eq!(report.files, 1);
    assert_eq!(dirs(&p), 2);

    p.switch(kept).unwrap();
    let report = p.cleanup().unwrap();
    assert_eq!((report.scans, report.labels), (2, 1));
    assert!(report.files > 3 && report.bytes > 0);
    assert_eq!(dirs(&p), 1);
    assert_eq!(std::fs::read_dir(p.root.join("labels")).unwrap().count(), 0);
    // The kept revision still reads all of its points.
    let p = Project::load(&p.root).unwrap();
    for scan in p.scans() {
        for chunk in 0..scan.chunks.len() {
            p.read_chunk(scan, chunk as u32).unwrap();
        }
    }
    assert_eq!(p.scans().count(), 2);
}

#[test]
fn edits_rewrite_only_the_history_and_other_formats_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let root = p.root.clone();
    let manifest = std::fs::read_to_string(root.join("project.json")).unwrap();
    assert!(
        !manifest.contains("\"chunks\""),
        "chunk metadata in project.json"
    );
    let scan_files: Vec<_> = p
        .scans()
        .map(|s| root.join(s.points_file.replace(".points", ".scan.json")))
        .collect();
    let stamps: Vec<_> = scan_files
        .iter()
        .map(|f| std::fs::metadata(f).unwrap().modified().unwrap())
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let id = p.scans().next().unwrap().id;
    p.set_transform(id, shift(0.5)).unwrap();
    exclude_everything(&mut p);
    let patch = p.manifest.patches[0].clone();
    let patch_file = root.join(format!("labels/{}.json", patch.id));
    assert!(patch_file.is_file());
    assert!(!manifest.contains("\"blocks\""));
    for (file, stamp) in scan_files.iter().zip(&stamps) {
        assert_eq!(std::fs::metadata(file).unwrap().modified().unwrap(), *stamp);
    }
    let reopened = Project::load(&root).unwrap();
    assert_eq!(
        serde_json::to_value(&reopened.manifest).unwrap(),
        serde_json::to_value(&p.manifest).unwrap()
    );

    // Cleanup removes the metadata of data nothing uses any more.
    p.discard_changes().unwrap();
    p.cleanup().unwrap();
    assert!(!patch_file.is_file());
    assert!(Project::load(&root).is_ok());

    // Only the current format opens.
    let mut other: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("project.json")).unwrap()).unwrap();
    other["format_version"] = 2.into();
    std::fs::write(root.join("project.json"), other.to_string()).unwrap();
    let error = Project::load(&root).unwrap_err();
    assert_eq!(
        CoreError::find(&error),
        Some(&CoreError::UnsupportedProjectFormat(2))
    );
}
