//! Layers: every point belongs to exactly one, moves between them are undone
//! by swapping states, and only visible layers take part in the work.
use geemil_core::{
    Camera, DEFAULT_LAYER, ImportOptions, JobControl, LayerTarget, Project, Selection,
    SelectionMode, interchange,
};
use std::path::Path;

fn imported(dir: &Path) -> Project {
    let source = dir.join("demo.e57");
    interchange::create_demo(&source).unwrap();
    let mut p = Project::create(&dir.join("p"), "Test").unwrap();
    let options = ImportOptions {
        chunk_points: 512,
        ..Default::default()
    };
    p.import_file(&source, options, &JobControl::default())
        .unwrap();
    p
}

/// Everything seen through the left or right half of the view.
fn half(p: &Project, left: bool) -> Selection {
    let (a, b) = if left { (0., 0.5) } else { (0.5, 1.) };
    Selection {
        camera: Camera {
            target: p.bounds().center().to_array(),
            distance: p.bounds().radius() * 3.,
            ..Camera::default()
        },
        polygon: vec![[a, 0.], [b, 0.], [b, 1.], [a, 1.]],
        depth_meters: None,
        mode: SelectionMode::ExcludeInside,
    }
}

fn ids(p: &Project) -> Vec<uuid::Uuid> {
    p.scans().map(|s| s.id).collect()
}

/// Points in visible layers.
fn visible(p: &Project) -> u64 {
    p.scans()
        .map(|s| {
            (0..s.chunks.len() as u32)
                .map(|c| p.points(s, c).unwrap().len() as u64)
                .sum::<u64>()
        })
        .sum()
}

fn records(p: &Project) -> u64 {
    p.scans().map(|s| s.records).sum()
}

#[test]
fn moved_points_belong_to_one_layer_and_come_back_in_part_or_whole() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let job = JobControl::default();
    let total = records(&p);
    assert_eq!(visible(&p), total);
    assert_eq!(p.layer_counts()[&DEFAULT_LAYER], total);

    // Moving to a new layer creates it hidden; the points leave the work.
    let left = p
        .move_selection(&half(&p, true), &ids(&p), &p.layer_named("Deleted"), &job)
        .unwrap();
    assert!(left > 0 && left < total);
    let deleted = p.current().layers.last().unwrap().clone();
    assert_eq!((deleted.name.as_str(), deleted.visible), ("Deleted", false));
    assert_eq!(visible(&p), total - left);
    let op = &p.current().operation["operations"].as_array().unwrap()[1];
    assert_eq!(
        (op["moved"].as_u64(), op["target"].as_u64()),
        (Some(left), Some(deleted.code as u64))
    );

    // A second move to the same layer reuses it; nothing is in two layers.
    let right = p
        .move_selection(&half(&p, false), &ids(&p), &p.layer_named("Deleted"), &job)
        .unwrap();
    assert_eq!(p.current().layers.len(), 2);
    let counts = p.layer_counts();
    assert_eq!(counts[&deleted.code], left + right);
    assert_eq!(counts.values().sum::<u64>(), total);

    // Showing the layer and moving part of it back restores just that part.
    p.set_layer_visible(deleted.code, true).unwrap();
    let back = p
        .move_selection(
            &half(&p, true),
            &ids(&p),
            &LayerTarget::Existing(DEFAULT_LAYER),
            &job,
        )
        .unwrap();
    assert_eq!(back, left);
    p.set_layer_visible(deleted.code, false).unwrap();
    assert_eq!(visible(&p), left);

    // Moving the whole layer back restores everything, whatever moved it.
    let all = p
        .move_layer_points(deleted.code, DEFAULT_LAYER, &job)
        .unwrap();
    assert_eq!(all, right);
    assert_eq!(visible(&p), total);
    assert_eq!(p.layer_counts().get(&deleted.code), Some(&0));
    // Labels are on disk: a reopened project sees the same.
    let reopened = Project::load(&p.root).unwrap();
    assert_eq!(reopened.layer_counts(), p.layer_counts());
}

#[test]
fn undo_swaps_states_and_deleting_a_layer_returns_its_points() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let job = JobControl::default();
    let total = records(&p);
    let before = p.manifest.draft.clone();
    let moved = p
        .move_selection(&half(&p, true), &ids(&p), &p.layer_named("Noise"), &job)
        .unwrap();
    let after = p.manifest.draft.clone();
    let patches = p.manifest.patches.len();

    // Undo and redo only swap the working state; nothing new is written.
    p.restore_working_state(before).unwrap();
    assert_eq!(visible(&p), total);
    assert_eq!(p.current().layers.len(), 1);
    p.restore_working_state(after).unwrap();
    assert_eq!(visible(&p), total - moved);
    assert_eq!(p.manifest.patches.len(), patches);

    // Layers can be made, renamed and deleted; the default one stays.
    let mine = p.create_layer("Mine".into()).unwrap();
    assert!(p.layer(mine).unwrap().visible);
    p.rename_layer(mine, "Ground".into()).unwrap();
    assert_eq!(p.layer(mine).unwrap().name, "Ground");
    assert!(p.rename_layer(DEFAULT_LAYER, "x".into()).is_err());
    assert!(p.delete_layer(DEFAULT_LAYER, &job).is_err());
    assert_eq!(p.delete_layer(mine, &job).unwrap(), 0);
    assert!(p.layer(mine).is_none());

    let noise = p.current().layers.last().unwrap().code;
    assert_eq!(p.delete_layer(noise, &job).unwrap(), moved);
    assert_eq!(p.current().layers.len(), 1);
    assert_eq!(visible(&p), total);
    // A freed code is used again by the next layer.
    assert_eq!(p.create_layer("Again".into()).unwrap(), noise.min(mine));
}

#[test]
fn hidden_layers_take_no_part_in_filters_or_exports() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let job = JobControl::default();
    let total = records(&p);
    let left = p
        .move_selection(&half(&p, true), &ids(&p), &p.layer_named("Deleted"), &job)
        .unwrap();
    // A box around everything moves only what is visible.
    let bounds = p.bounds();
    let size = (glam::DVec3::from(bounds.max) - glam::DVec3::from(bounds.min) + 1.).to_array();
    let crop = geemil_core::CropBox {
        center: bounds.center().to_array(),
        size,
        rotation: [0., 0., 0., 1.],
    };
    let into = p.create_layer("Kept".into()).unwrap();
    let moved = p
        .move_box(&crop, true, &ids(&p), &LayerTarget::Existing(into), &job)
        .unwrap();
    assert_eq!(moved, total - left);
    // "Kept" is visible, so the points stay in the work.
    assert_eq!(visible(&p), total - left);
    let output = dir.path().join("out.las");
    assert_eq!(p.export_las(&output, &job).unwrap(), total - left);
    p.set_layer_visible(into, false).unwrap();
    assert_eq!(visible(&p), 0);
}
