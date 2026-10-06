//! Layers: every point belongs to exactly one, moves between them are undone
//! by swapping states, and only visible layers take part in the work.
use geemil_core::{
    Camera, DEFAULT_LAYER, ImportOptions, JobControl, LayerTarget, Project, Selection,
    SelectionMode, ViewCache, interchange,
};
use std::{path::Path, sync::Arc};

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
    // Applied to every scan of the state, the record does not list them.
    assert_eq!(op["scans"], "all");

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

/// Points a view of everything shows with `cache`, kept across states as the
/// app's view loader keeps it.
fn shown(p: &Project, cache: &mut ViewCache) -> u64 {
    let camera = half(p, true).camera;
    let job = JobControl::default();
    let p = Arc::new(p.clone());
    let picks = p
        .select_view_cached(&camera, u32::MAX as usize, &ids(&p), &job, cache)
        .unwrap();
    let mut points = 0;
    p.load_view_nodes(&picks, cache, &job, 1, |node| {
        points += node.points.len() as u64;
        Ok(())
    })
    .unwrap();
    points
}

#[test]
fn cached_view_estimates_follow_moves_undo_and_layer_visibility() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let job = JobControl::default();
    let mut cache = ViewCache::new(0);
    cache.set_point_limit(1 << 24);
    let total = records(&p);
    assert_eq!(shown(&p, &mut cache), total);
    let before = p.manifest.draft.clone();

    // Two overlapping moves: the newer patch decides each chunk's labels.
    let left = p
        .move_selection(&half(&p, true), &ids(&p), &p.layer_named("Deleted"), &job)
        .unwrap();
    assert_eq!(shown(&p, &mut cache), total - left);
    let deleted = p.current().layers.last().unwrap().code;
    p.set_layer_visible(deleted, true).unwrap();
    let back = p
        .move_selection(
            &half(&p, true),
            &ids(&p),
            &LayerTarget::Existing(DEFAULT_LAYER),
            &job,
        )
        .unwrap();
    assert_eq!(back, left);
    p.set_layer_visible(deleted, false).unwrap();
    assert_eq!(shown(&p, &mut cache), total);
    let right = p
        .move_selection(
            &half(&p, false),
            &ids(&p),
            &LayerTarget::Existing(deleted),
            &job,
        )
        .unwrap();
    assert_eq!(shown(&p, &mut cache), total - right);
    assert_eq!(shown(&p, &mut cache), visible(&p));
    p.set_layer_visible(deleted, true).unwrap();
    assert_eq!(shown(&p, &mut cache), total);

    // Undo returns to an earlier state, which the cache must not mistake
    // for the current one.
    p.restore_working_state(before).unwrap();
    assert_eq!(shown(&p, &mut cache), total);
}

#[test]
fn edit_records_name_the_shorter_of_the_scans_used_or_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    p.import_file(
        &dir.path().join("demo.e57"),
        ImportOptions::default(),
        &JobControl::default(),
    )
    .unwrap();
    let all = ids(&p);
    assert_eq!(all.len(), 4);
    // Each move takes points not moved yet, so it records an edit.
    let record = |p: &mut Project, scans: &[uuid::Uuid], left: bool| {
        let target = p.layer_named("Deleted");
        p.move_selection(&half(p, left), scans, &target, &JobControl::default())
            .unwrap();
        p.current().operation["operations"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["scans"]
            .clone()
    };
    assert_eq!(record(&mut p, &all[3..], true), serde_json::json!([all[3]]));
    assert_eq!(
        record(&mut p, &all[..3], true),
        serde_json::json!({ "except": [all[3]] })
    );
    assert_eq!(record(&mut p, &all, false), "all");
}

/// Bounds of the visible points, from the points themselves.
fn exact_bounds(p: &Project, scan: &geemil_core::Scan) -> geemil_core::Bounds {
    let mut points = (0..scan.chunks.len() as u32).flat_map(|c| p.points(scan, c).unwrap());
    let mut bounds = geemil_core::Bounds::at(points.next().unwrap().position);
    for s in points {
        bounds.include(s.position);
    }
    bounds
}

#[test]
fn a_scans_visible_bounds_leave_out_points_in_hidden_layers() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("cloud.las");
    let mut writer = las::Writer::from_path(&source, las::Header::default()).unwrap();
    for i in 0..2000 {
        let (x, y) = ((i % 40) as f64 * 0.05, (i / 40) as f64 * 0.05);
        writer
            .write_point(las::Point {
                x,
                y,
                z: (x * 7. + y * 3.) % 0.2,
                ..Default::default()
            })
            .unwrap();
    }
    // Distant noise, and a stray point just beyond one edge.
    for (x, y, z) in [(80., 60., 9.), (2.3, 1., 0.1)] {
        writer
            .write_point(las::Point {
                x,
                y,
                z,
                ..Default::default()
            })
            .unwrap();
    }
    writer.close().unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Bounds").unwrap();
    let job = JobControl::default();
    let options = ImportOptions {
        chunk_points: 64,
        ..Default::default()
    };
    p.import_file(&source, options, &job).unwrap();
    let scan = p.scans().next().unwrap().clone();
    let all = p.visible_bounds(&scan).unwrap().unwrap();
    assert!(all.max[0] - all.min[0] > 70.);
    let world = p.world_matrix(&scan);
    let at = |x, y, z| world.transform_point3(glam::DVec3::new(x, y, z)).to_array();
    // The distant point alone in a hidden layer, then the strip with the
    // stray point, which cuts through chunks with visible points.
    let crop = |center, size| geemil_core::CropBox {
        center,
        size,
        rotation: [0., 0., 0., 1.],
    };
    p.move_box(
        &crop(at(80., 60., 9.), [4., 4., 4.]),
        true,
        &ids(&p),
        &p.layer_named("Noise"),
        &job,
    )
    .unwrap();
    let noise = p.visible_bounds(&scan).unwrap().unwrap();
    assert!(noise.max[0] - noise.min[0] < 3., "{noise:?}");
    assert_eq!(
        format!("{noise:?}"),
        format!("{:?}", exact_bounds(&p, &scan))
    );
    p.move_box(
        &crop(at(2.2, 1., 0.1), [0.4, 4., 4.]),
        true,
        &ids(&p),
        &p.layer_named("Noise"),
        &job,
    )
    .unwrap();
    let strip = p.visible_bounds(&scan).unwrap().unwrap();
    assert!(strip.max[0] - strip.min[0] < 2.1, "{strip:?}");
    assert_eq!(
        format!("{strip:?}"),
        format!("{:?}", exact_bounds(&p, &scan))
    );
    // Showing the layer again brings the points back into the box.
    let code = p
        .current()
        .layers
        .iter()
        .find(|l| l.name == "Noise")
        .unwrap()
        .code;
    p.set_layer_visible(code, true).unwrap();
    let shown = p.visible_bounds(&scan).unwrap().unwrap();
    assert_eq!(format!("{shown:?}"), format!("{all:?}"));
    // The patches keep each layer's bounds, so the moved points alone come
    // out of them too.
    assert!(
        p.manifest
            .patches
            .iter()
            .flat_map(|patch| &patch.blocks)
            .all(|b| b.bounds.is_some())
    );
    p.set_layer_visible(DEFAULT_LAYER, false).unwrap();
    let moved = p.visible_bounds(&scan).unwrap().unwrap();
    assert_eq!(
        format!("{moved:?}"),
        format!("{:?}", exact_bounds(&p, &scan))
    );
    // Patches written without bounds are read instead.
    p.set_layer_visible(DEFAULT_LAYER, true).unwrap();
    p.set_layer_visible(code, false).unwrap();
    let mut old = p.clone();
    for block in old.manifest.patches.iter_mut().flat_map(|p| &mut p.blocks) {
        block.bounds = None;
    }
    old.set_layer_visible(code, true).unwrap();
    old.set_layer_visible(code, false).unwrap();
    assert_eq!(
        format!("{:?}", old.visible_bounds(&scan).unwrap().unwrap()),
        format!("{strip:?}")
    );
    // Deleting the layer joins its bounds to the default layer's.
    p.delete_layer(code, &job).unwrap();
    let back = p.visible_bounds(&scan).unwrap().unwrap();
    assert_eq!(format!("{back:?}"), format!("{all:?}"));
}
