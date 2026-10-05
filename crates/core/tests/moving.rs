//! Moving object removal against scans that saw through the objects.
use e57::{Record, RecordDataType as T, RecordName as N, RecordValue as V};
use geemil_core::{
    Camera, CoreError, ImportOptions, JobControl, MovingOptions, Project, Selection, SelectionMode,
};
use glam::DVec3;
use std::path::Path;

/// Hits of a scanner's laser every 0.1 degrees on a wall 10 m ahead
/// (x = 10), a floor 1.5 m down and solid boxes: a pillar both scans see and
/// a person 5 m ahead of A. Scanner A stands at the origin; B stands 2 m to
/// the side when the person has gone.
fn scene(dir: &Path) -> Project {
    let input = dir.join("scans.e57");
    let mut writer = e57::E57Writer::from_file(&input, "moving").unwrap();
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
    ];
    let pillar = (DVec3::new(6.8, -2.2, -1.5), DVec3::new(7.2, -1.8, 1.5));
    let person = (DVec3::new(4.8, -0.2, -1.5), DVec3::new(5.2, 0.2, 0.2));
    for (name, scanner, with_person) in [("A", DVec3::ZERO, true), ("B", DVec3::Y * 2., false)] {
        let mut scan = writer.add_pointcloud(name, schema.clone()).unwrap();
        scan.set_name(Some(name.to_owned()));
        scan.set_transform(Some(e57::Transform {
            rotation: e57::Quaternion {
                w: 1.,
                x: 0.,
                y: 0.,
                z: 0.,
            },
            translation: e57::Translation {
                x: scanner.x,
                y: scanner.y,
                z: scanner.z,
            },
        }));
        let mut solids = vec![pillar];
        if with_person {
            solids.push(person);
        }
        for a in -400..=400 {
            for e in -350..=150 {
                let (azimuth, elevation) =
                    ((a as f64 * 0.1).to_radians(), (e as f64 * 0.1).to_radians());
                let ray = DVec3::new(
                    azimuth.cos() * elevation.cos(),
                    azimuth.sin() * elevation.cos(),
                    elevation.sin(),
                );
                let Some(t) = hit(scanner, ray, &solids) else {
                    continue;
                };
                let local = ray * t;
                scan.add_point(vec![
                    V::Double(local.x),
                    V::Double(local.y),
                    V::Double(local.z),
                ])
                .unwrap();
            }
        }
        scan.finalize().unwrap();
    }
    writer.finalize().unwrap();
    let mut p = Project::create(&dir.join("p"), "Moving").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    p
}

/// How far along `ray` from `from` it first meets the wall, the floor or
/// a box.
fn hit(from: DVec3, ray: DVec3, boxes: &[(DVec3, DVec3)]) -> Option<f64> {
    let mut nearest = f64::INFINITY;
    if ray.x > 1e-9 {
        nearest = nearest.min((10. - from.x) / ray.x);
    }
    if ray.z < -1e-9 {
        nearest = nearest.min((-1.5 - from.z) / ray.z);
    }
    for &(lo, hi) in boxes {
        // Slabs: the ray is inside the box between its last entry and first exit.
        let (a, b) = ((lo - from) / ray, (hi - from) / ray);
        let (enter, exit) = (a.min(b).max_element(), a.max(b).min_element());
        if enter <= exit && enter > 0. {
            nearest = nearest.min(enter);
        }
    }
    nearest.is_finite().then_some(nearest)
}

/// Each scan's points in visible layers in the project frame, with whether
/// they are in `layer`.
fn points_in(p: &Project, layer: u8) -> Vec<(String, DVec3, bool)> {
    let mut result = vec![];
    for scan in p.scans() {
        let world = p.world_matrix(scan);
        for chunk in 0..scan.chunks.len() as u32 {
            let labels = p.labels(scan, chunk).unwrap();
            for sample in p.points(scan, chunk).unwrap() {
                let at = world.transform_point3(DVec3::from(sample.position));
                let taken = labels[sample.index as usize] == layer;
                result.push((scan.name.clone(), at, taken));
            }
        }
    }
    result
}

#[test]
fn points_another_scan_saw_through_move_and_the_rest_stay() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = scene(dir.path());
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    assert!(p.scans().all(|s| p.scanner_position(s).is_some()));
    let target = p.layer_named("Moving");
    // Coarser than the scans' 0.1 degrees, as cells must be; only B can see
    // through A's points.
    let options = MovingOptions {
        cell_degrees: 0.2,
        min_scans: 1,
        ..Default::default()
    };
    let moved = p
        .remove_moving(options, &ids, &target, &JobControl::default())
        .unwrap();
    let code = p
        .current()
        .layers
        .iter()
        .find(|l| l.name == "Moving")
        .unwrap()
        .code;
    // New layers start hidden.
    p.set_layer_visible(code, true).unwrap();
    let points = points_in(&p, code);
    let is_person = |at: DVec3| {
        (4.75..=5.25).contains(&at.x) && (-0.25..=0.25).contains(&at.y) && at.z > -1.5 + 1e-9
    };
    let taken: Vec<_> = points.iter().filter(|(.., taken)| *taken).collect();
    assert_eq!(moved, taken.len() as u64);
    // Only the person moves: walls, floor and the pillar seen by both stay,
    // as do B's points on the wall behind the person, which A could not see.
    for (scan, at, _) in &taken {
        assert!(scan == "A" && is_person(*at), "{scan} {at}");
    }
    // The person moves but for the feet, within the tolerance of the floor.
    let person: Vec<_> = points.iter().filter(|(_, at, _)| is_person(*at)).collect();
    let above = person.iter().filter(|(_, at, _)| at.z > -1.3).count();
    assert!(above > 100);
    assert!(taken.len() >= above, "{} of {above}", taken.len());
}

#[test]
fn points_fewer_scans_saw_through_and_hidden_points_do_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = scene(dir.path());
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    let target = p.layer_named("Moving");
    let options = MovingOptions {
        cell_degrees: 0.2,
        min_scans: 1,
        ..Default::default()
    };
    // Only B sees through the person, short of two scans.
    let two = MovingOptions {
        min_scans: 2,
        ..options
    };
    let mut q = p.clone();
    assert_eq!(
        q.remove_moving(two, &ids, &target, &JobControl::default())
            .unwrap(),
        0
    );
    // With all of B in a hidden layer, its range image is empty: nothing
    // to see through, as with ghosts moved out of the way first.
    let b = p.scans().find(|s| s.name == "B").unwrap().id;
    let everything = Selection {
        camera: Camera {
            target: [5., 0., 0.],
            distance: 1000.,
            ..Camera::default()
        },
        // A sliver of the view, so all but nothing lies outside it.
        polygon: vec![[0., 0.], [1e-6, 0.], [0., 1e-6]],
        depth_meters: None,
        mode: SelectionMode::ExcludeOutside,
    };
    let hidden = p.layer_named("Hidden");
    let all_of_b = p.scans().find(|s| s.id == b).unwrap().valid_points;
    let moved = p
        .move_selection(&everything, &[b], &hidden, &JobControl::default())
        .unwrap();
    assert_eq!(moved, all_of_b);
    let target = p.layer_named("Moving");
    assert_eq!(
        p.remove_moving(options, &ids, &target, &JobControl::default())
            .unwrap(),
        0
    );
}

#[test]
fn scans_without_scanner_positions_cannot_see_through_others() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("points.las");
    let mut writer = las::Writer::from_path(&input, las::Header::default()).unwrap();
    for i in 0..100 {
        writer
            .write_point(las::Point {
                x: i as f64,
                ..Default::default()
            })
            .unwrap();
    }
    writer.close().unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Las").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let ids: Vec<_> = p.scans().map(|s| s.id).collect();
    let target = p.layer_named("Moving");
    let error = p
        .remove_moving(
            MovingOptions::default(),
            &ids,
            &target,
            &JobControl::default(),
        )
        .unwrap_err();
    assert_eq!(
        CoreError::find(&error),
        Some(&CoreError::NoScannerPositions)
    );
}
