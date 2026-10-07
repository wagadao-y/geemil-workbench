//! Panoramas: import, placement from correspondences, the tree, E57 export
//! and cleanup.
use geemil_core::{
    CoreError, ImportOptions, JobControl, PanoramaPair, Pose, Project, RegistrationMethod,
    interchange, solve_panorama,
};
use glam::{DMat4, DQuat, DVec3};
use std::path::{Path, PathBuf};

fn imported(dir: &Path) -> Project {
    let source = dir.join("demo.e57");
    interchange::create_demo(&source).unwrap();
    let mut p = Project::create(&dir.join("p"), "Test").unwrap();
    p.import_file(&source, ImportOptions::default(), &JobControl::default())
        .unwrap();
    p
}

fn photo(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
    let path = dir.join(name);
    image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 90]))
        .save(&path)
        .unwrap();
    path
}

#[test]
fn panoramas_are_placed_kept_in_folders_exported_and_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = imported(dir.path());
    let wide = photo(dir.path(), "wide.jpg", 96, 32);
    assert!(matches!(
        p.import_panorama(&wide)
            .map_err(|e| CoreError::find(&e).cloned()),
        Err(Some(CoreError::NotEquirectangular(96, 32)))
    ));
    let source = photo(dir.path(), "room.jpg", 128, 64);
    let id = p.import_panorama(&source).unwrap();
    let panorama = p.panorama(id).unwrap().clone();
    assert_eq!((panorama.width, panorama.height), (128, 64));
    assert_eq!(p.panorama_name(&panorama), "room");
    assert_eq!(p.panoramas_in(None).len(), 1);
    // It starts among the scans.
    let start = p.correction(id).transform_point3(DVec3::ZERO);
    assert!(start.distance(p.bounds().center()) < 1e-9);

    // Pixels where a panorama at `truth` sees points of the first scan.
    let truth =
        DMat4::from_rotation_translation(DQuat::from_rotation_z(2.3), DVec3::new(1.2, 0.4, 1.5));
    let scan = p.scans().next().unwrap().clone();
    let pairs: Vec<PanoramaPair> = [
        [-1.8, -1.7],
        [1.6, -1.2],
        [0.3, 1.9],
        [-1.1, 0.8],
        [1.9, 1.7],
    ]
    .into_iter()
    .map(|[x, y]| {
        let local = DVec3::new(x, y, (x * 2f64).sin() * (y * 2f64).cos() * 0.3);
        let world = p.world_matrix(&scan).transform_point3(local);
        PanoramaPair {
            pixel: panorama.pixel(truth.inverse().transform_point3(world)),
            scan: scan.id,
            local: local.to_array(),
        }
    })
    .collect();
    let (bearings, points) = p.panorama_rays(&panorama, &pairs);
    let solution = solve_panorama(&bearings, &points, true).unwrap();
    assert!(solution.world.abs_diff_eq(truth, 1e-6));
    let own = p.panorama_own_pose(id, solution.world);
    p.place_panorama(id, own, pairs.clone(), solution.rms)
        .unwrap();
    assert!(p.correction(id).abs_diff_eq(truth, 1e-6));
    assert_eq!(p.panorama_pairs(id), &pairs[..]);
    let registration = p.registration(id).unwrap();
    assert_eq!(
        registration.registration.fit.method,
        RegistrationMethod::Panorama
    );
    assert!(!registration.moved);

    // A folder carries it along, and moving into one keeps its place.
    let folder = p.create_group("Photos".into(), None).unwrap();
    p.move_to_group(&[id], Some(folder)).unwrap();
    assert!(p.correction(id).abs_diff_eq(truth, 1e-6));
    assert_eq!(p.panoramas_in(Some(folder)).len(), 1);
    let lift = Pose {
        translation: [0., 0., 2.],
        ..Pose::default()
    };
    p.set_transform(folder, lift).unwrap();
    let placed = lift.matrix() * truth;
    assert!(p.correction(id).abs_diff_eq(placed, 1e-6));
    // Renamed with scans in one edit, and back to the imported name.
    let first = p.scans().next().unwrap().id;
    p.rename_scans(&[(first, "North".into()), (id, "Hall".into())])
        .unwrap();
    assert_eq!(p.panorama_name(p.panorama(id).unwrap()), "Hall");
    assert_eq!(p.scan_name(p.scan(first).unwrap()), "North");
    p.rename_scans(&[(id, "room".into())]).unwrap();
    assert!(!p.current().scan_names.contains_key(&id));
    p.rename_panorama(id, " Living room ").unwrap();
    let reopened = Project::load(&p.root).unwrap();
    assert_eq!(
        reopened.panorama_name(reopened.panorama(id).unwrap()),
        "Living room"
    );
    assert_eq!(reopened.panorama_pairs(id).len(), 5);

    // Export: an unassociated spherical image with the photo as it was.
    let output = dir.path().join("out.e57");
    p.export_e57(&output, &JobControl::default()).unwrap();
    let mut reader = e57::E57Reader::from_file(&output).unwrap();
    let images = reader.images();
    let image = images
        .iter()
        .find(|i| i.name.as_deref() == Some("Living room"))
        .unwrap();
    assert!(image.pointcloud_guid.is_none());
    let pose = Pose::from_e57(image.transform.as_ref().unwrap());
    assert!(pose.matrix().abs_diff_eq(placed, 1e-6));
    let Some(e57::Projection::Spherical(spherical)) = &image.projection else {
        panic!("Not spherical");
    };
    assert_eq!(
        (spherical.properties.width, spherical.properties.height),
        (128, 64)
    );
    assert!((spherical.properties.pixel_width * 128. - std::f64::consts::TAU).abs() < 1e-12);
    let mut blob = vec![];
    reader.blob(&spherical.blob.data, &mut blob).unwrap();
    assert_eq!(blob, std::fs::read(&source).unwrap());

    // Taking it out with its folder drops what referred to it; cleanup then
    // deletes the copied photo, which no state uses.
    let file = p.path(&panorama.file).unwrap();
    p.remove_items(&[folder]).unwrap();
    assert_eq!(p.panoramas().count(), 0);
    assert!(p.current().panorama_pairs.is_empty());
    assert!(file.is_file());
    let report = p.cleanup().unwrap();
    assert_eq!(report.panoramas, 1);
    assert!(!file.exists());
    assert!(Project::load(&p.root).is_ok());
}
