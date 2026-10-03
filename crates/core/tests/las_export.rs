use geemil_core::{CoreError, ImportOptions, JobControl, Pose, Project, interchange};
use glam::{DQuat, DVec3};
use las::Reader;
use std::path::Path;

const WKT: &str = r#"PROJCS["JGD2011 / Japan Plane Rectangular CS IX",GEOGCS["JGD2011"]]"#;

/// A georeferenced LAS 1.4 cloud whose point `i` has intensity `i * 50` and
/// colour `(i * 100, 65535 - i * 100, 7)`.
fn write_source(path: &Path) {
    let mut builder = las::Builder::from((1, 4));
    builder.point_format = las::point::Format::new(7).unwrap();
    builder.transforms.x.offset = 30000.;
    builder.transforms.y.offset = -50000.;
    let mut header = builder.into_header().unwrap();
    header.set_wkt_crs(WKT.as_bytes().to_vec()).unwrap();
    let mut writer = las::Writer::from_path(path, header).unwrap();
    for i in 0..400u16 {
        writer
            .write_point(las::Point {
                x: 30000. + (i % 20) as f64 * 0.1,
                y: -50000. + (i / 20) as f64 * 0.1,
                z: 12.5 + (i % 7) as f64 * 0.01,
                intensity: i * 50,
                color: Some(las::Color::new(i * 100, 65535 - i * 100, 7)),
                gps_time: Some(0.),
                ..Default::default()
            })
            .unwrap();
    }
    writer.close().unwrap();
}

fn read(path: &Path) -> (las::Header, Vec<las::Point>) {
    let mut reader = Reader::from_path(path).unwrap();
    let n = reader.header().number_of_points();
    let data = reader.read_points(n).unwrap();
    let points = data.points().map(Result::unwrap).collect();
    (reader.header().clone(), points)
}

#[test]
fn las_and_laz_keep_attributes_transforms_exclusions_and_crs() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.las");
    write_source(&source);
    let mut p = Project::create(&dir.path().join("p"), "LAS").unwrap();
    p.import_file(
        &source,
        ImportOptions {
            chunk_points: 32,
            lod_points: 8,
            ..Default::default()
        },
        &JobControl::default(),
    )
    .unwrap();
    let id = p.scans().next().unwrap().id;
    let pose = Pose {
        translation: [1.5, -2., 0.25],
        rotation_xyzw: DQuat::from_rotation_z(0.3).to_array(),
    };
    p.set_transform(id, pose).unwrap();
    // 10 cm grid, 20 cm voxels: one point of four survives.
    let excluded = p.subsample(0.2, &[id], &JobControl::default()).unwrap();
    assert_eq!(excluded, 300);
    for ext in ["las", "laz"] {
        let out = dir.path().join(format!("out.{ext}"));
        let written = p.export_las(&out, &JobControl::default()).unwrap();
        assert_eq!(written, 100);
        let (header, points) = read(&out);
        assert_eq!(header.number_of_points(), 100);
        assert_eq!(header.version(), las::Version::new(1, 4));
        assert_eq!(header.point_format().to_u8().unwrap(), 7);
        assert_eq!(header.point_format().is_compressed, ext == "laz");
        assert_eq!(header.get_wkt_crs_bytes(), Some(WKT.as_bytes()));
        let world = pose.matrix();
        for point in &points {
            // Recover the source index from the exact 16-bit intensity.
            let i = point.intensity / 50;
            assert_eq!(point.intensity, i * 50);
            assert_eq!(
                point.color,
                Some(las::Color::new(i * 100, 65535 - i * 100, 7))
            );
            let local = DVec3::new(
                30000. + (i % 20) as f64 * 0.1,
                -50000. + (i / 20) as f64 * 0.1,
                12.5 + (i % 7) as f64 * 0.01,
            );
            let expected = world.transform_point3(local);
            let actual = DVec3::new(point.x, point.y, point.z);
            assert!(expected.distance(actual) < 1e-4, "{expected} != {actual}");
        }
        assert!(matches!(
            CoreError::find(&p.export_las(&out, &JobControl::default()).unwrap_err()),
            Some(CoreError::OutputExists(_))
        ));
    }
}

#[test]
fn e57_scans_export_merged_or_per_scan_with_8_bit_colours_widened() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("demo.e57");
    interchange::create_demo(&input).unwrap();
    let mut p = Project::create(&dir.path().join("p"), "Demo").unwrap();
    p.import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let valid: u64 = p.scans().map(|s| s.valid_points).sum();
    let merged = dir.path().join("merged.las");
    assert_eq!(
        p.export_las(&merged, &JobControl::default()).unwrap(),
        valid
    );
    let (header, points) = read(&merged);
    assert_eq!(header.version(), las::Version::new(1, 2));
    assert_eq!(header.point_format().to_u8().unwrap(), 2);
    assert!(header.get_wkt_crs_bytes().is_none());
    // 8-bit colours become 16-bit by repeating the byte, so `c * 257`.
    assert!(points.iter().all(|p| {
        let c = p.color.unwrap();
        [c.red, c.green, c.blue].iter().all(|v| v % 257 == 0)
    }));

    let out = dir.path().join("scans");
    std::fs::create_dir(&out).unwrap();
    let files = p
        .export_las_per_scan(&out, true, &JobControl::default())
        .unwrap();
    assert_eq!(files.len(), p.scans().count());
    let per_scan: u64 = files.iter().map(|f| read(f).0.number_of_points()).sum();
    assert_eq!(per_scan, valid);
    assert!(files.iter().all(|f| f.extension().unwrap() == "laz"));
    // Nothing is replaced, and nothing is written when a name is taken.
    std::fs::remove_file(&files[1]).unwrap();
    assert!(
        p.export_las_per_scan(&out, true, &JobControl::default())
            .is_err()
    );
    assert!(!files[1].exists());
}
