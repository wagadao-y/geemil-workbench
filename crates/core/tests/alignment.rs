use geemil_core::{IcpOptions, ImportOptions, JobControl, Pose, Project, rigid_fit};
use glam::{DMat4, DQuat, DVec3};
use std::path::Path;

/// Deterministic pseudo-random values in [0, 1).
fn rng(mut state: u64) -> impl FnMut() -> f64 {
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A room corner (floor and two walls) with a box on the floor, sampled at
/// random with 2 mm noise, in true coordinates.
fn room(seed: u64) -> Vec<DVec3> {
    let mut next = rng(seed);
    let mut points = vec![];
    let noise = |next: &mut dyn FnMut() -> f64| (next() - 0.5) * 0.004;
    for _ in 0..30_000 {
        let (u, v) = (next() * 8., next() * 8.);
        points.push(DVec3::new(u, v, noise(&mut next)));
    }
    for _ in 0..12_000 {
        let (u, h) = (next() * 8., next() * 3.);
        points.push(DVec3::new(noise(&mut next), u, h));
        let (u, h) = (next() * 8., next() * 3.);
        points.push(DVec3::new(u, noise(&mut next), h));
    }
    // Box from (4, 3, 0) to (5.5, 4, 1.2): top and two sides.
    for _ in 0..6_000 {
        let (u, v, h) = (next(), next(), next() * 1.2);
        points.push(DVec3::new(4. + u * 1.5, 3. + v, 1.2 + noise(&mut next)));
        points.push(DVec3::new(4. + u * 1.5, 3. + noise(&mut next), h));
        points.push(DVec3::new(5.5 + noise(&mut next), 3. + v, h));
    }
    points
}

fn write_las(path: &Path, points: &[DVec3]) {
    let mut builder = las::Builder::from((1, 2));
    builder.point_format = las::point::Format::new(0).unwrap();
    builder.transforms.x.scale = 0.0001;
    builder.transforms.y.scale = 0.0001;
    builder.transforms.z.scale = 0.0001;
    let mut writer = las::Writer::from_path(path, builder.into_header().unwrap()).unwrap();
    for p in points {
        writer
            .write_point(las::Point {
                x: p.x,
                y: p.y,
                z: p.z,
                ..Default::default()
            })
            .unwrap();
    }
    writer.close().unwrap();
}

/// The misalignment to recover: about 3.4 degrees and 19 cm.
fn truth() -> DMat4 {
    DMat4::from_rotation_translation(
        DQuat::from_euler(glam::EulerRot::XYZ, 0.012, -0.008, 0.055),
        DVec3::new(0.15, -0.1, 0.05),
    )
}

/// A project with the reference room and a second scan of it whose local
/// coordinates are `truth()` away from where they belong.
fn project(dir: &Path) -> (Project, uuid::Uuid, uuid::Uuid) {
    let reference = dir.join("reference.las");
    write_las(&reference, &room(1));
    let inverse = truth().inverse();
    let moving: Vec<_> = room(2)
        .into_iter()
        .map(|p| inverse.transform_point3(p))
        .collect();
    let moved = dir.join("moving.las");
    write_las(&moved, &moving);
    let mut p = Project::create(&dir.join("p"), "Align").unwrap();
    let options = ImportOptions {
        chunk_points: 4096,
        ..Default::default()
    };
    p.import_file(&reference, options, &JobControl::default())
        .unwrap();
    p.import_file(&moved, options, &JobControl::default())
        .unwrap();
    let id = |name: &str| p.scans().find(|s| s.name == name).unwrap().id;
    let (r, m) = (id("reference"), id("moving"));
    (p, r, m)
}

fn assert_close(actual: DMat4, expected: DMat4) {
    assert_within(actual, expected, 0.003);
}

fn assert_within(actual: DMat4, expected: DMat4, tolerance: f64) {
    let probe = [
        DVec3::ZERO,
        DVec3::new(8., 0., 0.),
        DVec3::new(0., 8., 0.),
        DVec3::new(0., 0., 3.),
    ];
    for q in probe {
        let error = actual
            .transform_point3(q)
            .distance(expected.transform_point3(q));
        assert!(error < tolerance, "{error} m off at {q}");
    }
}

#[test]
fn icp_recovers_a_scan_and_a_folder_misalignment() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, reference, moving) = project(dir.path());
    let options = IcpOptions {
        max_distance: 0.5,
        ..Default::default()
    };
    let result = p
        .icp(
            moving,
            Pose::default(),
            &[reference],
            &options,
            &JobControl::default(),
        )
        .unwrap();
    assert_close(result.pose.matrix(), truth());
    assert!(result.rms < 0.006, "rms {}", result.rms);
    assert!(result.overlap > 0.9, "overlap {}", result.overlap);

    // Inside a turned folder the scan's own transform compensates the folder.
    let folder = p.create_group("Floor 1".into(), None).unwrap();
    p.move_to_group(&[moving], Some(folder)).unwrap();
    let turn = Pose::from_matrix(DMat4::from_rotation_translation(
        DQuat::from_rotation_z(0.02),
        DVec3::new(0.05, 0., 0.),
    ));
    p.set_transform(folder, turn).unwrap();
    let own = p
        .current()
        .transforms
        .get(&moving)
        .copied()
        .unwrap_or_default();
    let result = p
        .icp(moving, own, &[reference], &options, &JobControl::default())
        .unwrap();
    p.set_transform(moving, result.pose).unwrap();
    let scan = p.scans().find(|s| s.id == moving).unwrap().clone();
    assert_close(p.world_matrix(&scan), truth());

    // Aligning the folder moves the scan inside with it.
    let result = p
        .icp(folder, turn, &[reference], &options, &JobControl::default())
        .unwrap();
    p.set_transform(folder, result.pose).unwrap();
    assert_close(p.world_matrix(&scan), truth());
}

#[test]
fn icp_reports_no_overlap_and_point_pairs_set_the_initial_pose() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, reference, moving) = project(dir.path());
    // Moved 30 m away, nothing is within reach.
    let far = Pose {
        translation: [30., 0., 0.],
        ..Pose::default()
    };
    let error = p
        .icp(
            moving,
            far,
            &[reference],
            &IcpOptions::default(),
            &JobControl::default(),
        )
        .unwrap_err();
    assert_eq!(
        geemil_core::CoreError::find(&error),
        Some(&geemil_core::CoreError::NoOverlap)
    );

    // Picked pairs: corners as seen in the moving scan's current position and
    // in the reference, with a few millimetres of picking error.
    let inverse = truth().inverse();
    let corners = [
        DVec3::new(0., 0., 0.),
        DVec3::new(8., 0., 0.),
        DVec3::new(0., 8., 3.),
        DVec3::new(5.5, 3., 1.2),
    ];
    let errors = [
        DVec3::new(0.003, -0.002, 0.),
        DVec3::new(-0.002, 0.003, 0.001),
        DVec3::new(0., -0.003, 0.002),
        DVec3::new(0.002, 0.001, -0.003),
    ];
    let moving_points: Vec<_> = corners
        .iter()
        .zip(errors)
        .map(|(c, e)| inverse.transform_point3(*c) + e)
        .collect();
    let motion = rigid_fit(&moving_points, &corners).unwrap();
    let pose = p.moved_pose(moving, Pose::default(), motion);
    assert_within(pose.matrix(), truth(), 0.01);
    p.set_transform(moving, pose).unwrap();
    // ICP refines from there with a tight distance.
    let result = p
        .icp(
            moving,
            pose,
            &[reference],
            &IcpOptions {
                max_distance: 0.05,
                ..Default::default()
            },
            &JobControl::default(),
        )
        .unwrap();
    assert_close(result.pose.matrix(), truth());
}
