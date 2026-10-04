use geemil_core::{
    CoreError, ImportOptions, JobControl, LasExportPolicy, Pose, Project, interchange,
};
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
            view_grid: 4,
            view_leaf_points: 32,
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
    let target = p.layer_named("Subsampled");
    let moved = p
        .subsample(0.2, &[id], &target, &JobControl::default())
        .unwrap();
    assert_eq!(moved, 300);
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

#[test]
fn las_attributes_extra_bytes_and_vlrs_survive_edit_and_laz_roundtrip() {
    for format in [3, 8] {
        for extension in ["las", "laz"] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join(format!("source.{extension}"));
            let mut builder = las::Builder::from((1, 4));
            builder.point_format = las::point::Format::new(format).unwrap();
            builder.point_format.extra_bytes = 2;
            builder.gps_time_type = las::GpsTimeType::Standard;
            builder.file_source_id = 42;
            // One unsigned-short Extra Bytes descriptor.
            let mut descriptor = vec![0u8; 192];
            descriptor[2] = 3;
            descriptor[4..11].copy_from_slice(b"Quality");
            builder.vlrs.push(las::Vlr {
                user_id: "LASF_Spec".into(),
                record_id: 4,
                description: "Extra Bytes".into(),
                data: descriptor,
            });
            builder.vlrs.push(las::Vlr {
                user_id: "LASF_Projection".into(),
                record_id: 34735,
                description: "GeoTIFF".into(),
                data: vec![1, 0, 1, 0, 0, 0, 0, 0],
            });
            builder.evlrs.push(las::Vlr {
                user_id: "test".into(),
                record_id: 99,
                description: "application metadata".into(),
                data: vec![7; 70_000],
            });
            let mut writer =
                las::Writer::from_path(&input, builder.into_header().unwrap()).unwrap();
            for i in 0..24u16 {
                writer
                    .write_point(las::Point {
                        x: i as f64 * 0.1,
                        y: 0.,
                        z: 0.,
                        intensity: i,
                        return_number: 2,
                        number_of_returns: 3,
                        classification: las::point::Classification::new(if format == 8 {
                            200
                        } else {
                            7
                        })
                        .unwrap(),
                        is_synthetic: true,
                        is_key_point: true,
                        is_withheld: true,
                        is_overlap: format == 8,
                        scanner_channel: if format == 8 { 2 } else { 0 },
                        scan_direction: las::point::ScanDirection::LeftToRight,
                        is_edge_of_flight_line: true,
                        scan_angle: if format == 8 { 12.006 } else { 12. },
                        user_data: 19,
                        point_source_id: 43,
                        gps_time: Some(123456. + i as f64 * 0.25),
                        color: Some(las::Color::new(i, i * 100, 65535)),
                        nir: (format == 8).then_some(1234 + i),
                        extra_bytes: (1000 + i).to_le_bytes().to_vec(),
                        ..Default::default()
                    })
                    .unwrap();
            }
            writer.close().unwrap();
            let (source_header, source_points) = read(&input);
            let root = dir.path().join("p");
            let mut project = Project::create(&root, "Attributes").unwrap();
            project
                .import_file(
                    &input,
                    ImportOptions {
                        chunk_points: 4,
                        ..Default::default()
                    },
                    &JobControl::default(),
                )
                .unwrap();
            let id = project.scans().next().unwrap().id;
            project
                .set_transform(
                    id,
                    Pose {
                        translation: [1., 2., 3.],
                        ..Default::default()
                    },
                )
                .unwrap();
            let target = project.layer_named("Thinned");
            project
                .subsample(0.2, &[id], &target, &JobControl::default())
                .unwrap();
            drop(project);
            let project = Project::load(&root).unwrap();
            assert!(
                project
                    .scans()
                    .next()
                    .unwrap()
                    .omitted_attributes
                    .is_empty()
            );
            for output_extension in ["las", "laz"] {
                let output = dir.path().join(format!("out.{output_extension}"));
                project.export_las(&output, &JobControl::default()).unwrap();
                let (header, points) = read(&output);
                assert!(!points.is_empty() && points.len() < source_points.len());
                let actual = las::Builder::from(header.clone());
                let expected = las::Builder::from(source_header.clone());
                assert_eq!(actual.gps_time_type, expected.gps_time_type);
                assert_eq!(actual.file_source_id, 42);
                assert_eq!(header.point_format().extra_bytes, 2);
                let vlrs = |h: &las::Header| {
                    h.vlrs()
                        .iter()
                        .filter(|v| v.user_id != "laszip encoded")
                        .cloned()
                        .collect::<Vec<_>>()
                };
                assert_eq!(vlrs(&header), vlrs(&source_header));
                assert_eq!(header.evlrs(), source_header.evlrs());
                for point in points {
                    let mut expected = source_points[point.intensity as usize].clone();
                    expected.x += 1.;
                    expected.y += 2.;
                    expected.z += 3.;
                    assert!((point.x - expected.x).abs() < 1e-4);
                    expected.x = point.x;
                    assert_eq!(point, expected);
                }
            }
            // The additional LAS payload must not break E57's numeric schema.
            project
                .export_e57(&dir.path().join("out.e57"), &JobControl::default())
                .unwrap();
        }
    }
}

#[test]
fn incompatible_metadata_requires_an_explicit_export_choice() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.las");
    write_source(&input);
    let mut project = Project::create(&dir.path().join("p"), "Mismatch").unwrap();
    for _ in 0..2 {
        project
            .import_file(&input, ImportOptions::default(), &JobControl::default())
            .unwrap();
    }
    project.manifest.scans[1]
        .las
        .as_mut()
        .unwrap()
        .vlrs
        .push(geemil_core::LasVlr {
            user_id: "test".into(),
            record_id: 1,
            description: "different definition".into(),
            data: vec![1],
        });
    let output = dir.path().join("out.laz");
    assert_eq!(
        CoreError::find(
            &project
                .export_las(&output, &JobControl::default())
                .unwrap_err()
        ),
        Some(&CoreError::LasMetadataMismatch)
    );
    assert!(!output.exists());
    let report = project.las_export_compatibility().unwrap();
    assert_eq!(report.omitted_metadata, vec![("test".into(), 1)]);
    assert!(!report.omit_extra_bytes && !report.omit_gps_time && !report.omit_crs);
    project
        .export_las_with_policy(
            &output,
            LasExportPolicy::OmitIncompatible,
            &JobControl::default(),
        )
        .unwrap();
    let (header, points) = read(&output);
    assert_eq!(points.len(), 800);
    assert_eq!(header.get_wkt_crs_bytes(), Some(WKT.as_bytes()));
    assert!(
        points
            .iter()
            .all(|p| p.gps_time == Some(0.) && p.color.is_some())
    );
    assert!(!header.vlrs().iter().any(|v| v.user_id == "test"));
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .count(),
        0
    );
}

fn write_attribute_fixture(path: &Path, extra: u16, schema: u8, standard: bool, wkt: &str) {
    let mut builder = las::Builder::from((1, 4));
    builder.point_format = las::point::Format::new(8).unwrap();
    builder.point_format.extra_bytes = extra;
    builder.gps_time_type = if standard {
        las::GpsTimeType::Standard
    } else {
        las::GpsTimeType::Week
    };
    builder.vlrs.push(las::Vlr {
        user_id: "shared".into(),
        record_id: 1,
        description: "shared metadata".into(),
        data: vec![7],
    });
    if extra > 0 {
        builder.vlrs.push(las::Vlr {
            user_id: "LASF_Spec".into(),
            record_id: 4,
            description: "extra definition".into(),
            data: vec![schema; 192],
        });
    }
    let mut header = builder.into_header().unwrap();
    header.set_wkt_crs(wkt.as_bytes().to_vec()).unwrap();
    let mut writer = las::Writer::from_path(path, header).unwrap();
    writer
        .write_point(las::Point {
            x: 1.,
            y: 2.,
            z: 3.,
            intensity: 2345,
            classification: las::point::Classification::new(200).unwrap(),
            return_number: 2,
            number_of_returns: 3,
            is_withheld: true,
            is_overlap: true,
            scanner_channel: 2,
            scan_angle: 12.006,
            point_source_id: 42,
            user_data: 13,
            gps_time: Some(12345.),
            color: Some(las::Color::new(123, 456, 789)),
            nir: Some(3456),
            extra_bytes: vec![99; extra as usize],
            ..Default::default()
        })
        .unwrap();
    writer.close().unwrap();
}

#[test]
fn incompatible_fields_can_be_omitted_without_losing_compatible_point_attributes() {
    // Same length/different schema, different lengths, different GPS conventions,
    // different CRS, and all three at once. Both compressed/uncompressed output.
    for (extra, schema, standard, wkt, expected) in [
        (2, 2, true, WKT, (true, false, false)),
        (1, 1, true, WKT, (true, false, false)),
        (2, 1, false, WKT, (false, true, false)),
        (2, 1, true, "LOCAL_CS[\"other\"]", (false, false, true)),
        (1, 2, false, "LOCAL_CS[\"other\"]", (true, true, true)),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.las");
        let b = dir.path().join("b.laz");
        write_attribute_fixture(&a, 2, 1, true, WKT);
        write_attribute_fixture(&b, extra, schema, standard, wkt);
        let mut project = Project::create(&dir.path().join("p"), "Conflicts").unwrap();
        for input in [&a, &b] {
            project
                .import_file(input, ImportOptions::default(), &JobControl::default())
                .unwrap();
        }
        let before = serde_json::to_vec(&project.manifest).unwrap();
        let report = project.las_export_compatibility().unwrap();
        assert_eq!(
            (
                report.omit_extra_bytes,
                report.omit_gps_time,
                report.omit_crs
            ),
            expected
        );
        assert!(
            project
                .export_las(&dir.path().join("strict.las"), &JobControl::default())
                .is_err()
        );
        for ext in ["las", "laz"] {
            let out = dir.path().join(format!("merged.{ext}"));
            assert_eq!(
                project
                    .export_las_with_policy(
                        &out,
                        LasExportPolicy::OmitIncompatible,
                        &JobControl::default()
                    )
                    .unwrap(),
                2
            );
            let (header, points) = read(&out);
            assert_eq!(
                header.point_format().extra_bytes,
                if expected.0 { 0 } else { 2 }
            );
            assert_eq!(
                header.get_wkt_crs_bytes(),
                if expected.2 {
                    None
                } else {
                    Some(WKT.as_bytes())
                }
            );
            assert!(header.vlrs().iter().any(|v| v.user_id == "shared"));
            assert_eq!(
                header
                    .vlrs()
                    .iter()
                    .any(|v| v.user_id == "LASF_Spec" && v.record_id == 4),
                !expected.0
            );
            let (_, original) = read(&a);
            for point in points {
                let mut expected_point = original[0].clone();
                if expected.0 {
                    expected_point.extra_bytes.clear();
                }
                if expected.1 {
                    expected_point.gps_time = Some(0.);
                }
                assert_eq!(point, expected_point);
            }
        }
        assert_eq!(serde_json::to_vec(&project.manifest).unwrap(), before);
        let split = dir.path().join("split");
        std::fs::create_dir(&split).unwrap();
        let paths = project
            .export_las_per_scan(&split, true, &JobControl::default())
            .unwrap();
        assert_eq!(read(&paths[0]).1, read(&a).1);
        assert_eq!(read(&paths[1]).1, read(&b).1);
    }
}

#[test]
fn metadata_order_and_description_differences_do_not_block_preserving_export() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.las");
    write_attribute_fixture(&input, 2, 1, true, WKT);
    let mut project = Project::create(&dir.path().join("p"), "Metadata").unwrap();
    for _ in 0..2 {
        project
            .import_file(&input, ImportOptions::default(), &JobControl::default())
            .unwrap();
    }
    let meta = project.manifest.scans[1].las.as_mut().unwrap();
    meta.vlrs.reverse();
    for v in &mut meta.vlrs {
        v.description = "other description".into();
    }
    assert!(!project.las_export_compatibility().unwrap().has_conflicts());
    let output = dir.path().join("merged.laz");
    assert_eq!(
        project.export_las(&output, &JobControl::default()).unwrap(),
        2
    );
    assert!(
        read(&output)
            .1
            .iter()
            .all(|p| p.extra_bytes == vec![99, 99])
    );
}

#[test]
fn mixed_e57_and_las_with_extra_bytes_can_be_merged_by_explicit_omission() {
    let dir = tempfile::tempdir().unwrap();
    let las = dir.path().join("source.las");
    let e57 = dir.path().join("source.e57");
    write_attribute_fixture(&las, 2, 1, true, WKT);
    interchange::create_demo(&e57).unwrap();
    let mut project = Project::create(&dir.path().join("p"), "Mixed").unwrap();
    for input in [&e57, &las] {
        project
            .import_file(input, ImportOptions::default(), &JobControl::default())
            .unwrap();
    }
    let expected: u64 = project.scans().map(|s| s.valid_points).sum();
    let report = project.las_export_compatibility().unwrap();
    assert!(report.omit_extra_bytes && report.omit_crs);
    assert!(!report.omit_gps_time);
    let out = dir.path().join("mixed.laz");
    assert_eq!(
        project
            .export_las_with_policy(
                &out,
                LasExportPolicy::OmitIncompatible,
                &JobControl::default()
            )
            .unwrap(),
        expected
    );
    let (header, points) = read(&out);
    assert_eq!(header.point_format().extra_bytes, 0);
    assert!(header.get_wkt_crs_bytes().is_none());
    assert!(points.iter().all(|p| p.extra_bytes.is_empty()));
    let las_point = points.iter().find(|p| p.point_source_id == 42).unwrap();
    assert_eq!(las_point.gps_time, Some(12345.));
    assert_eq!(las_point.nir, Some(3456));
    assert_eq!(
        las_point.classification,
        las::point::Classification::new(200).unwrap()
    );
}

#[test]
fn gps_time_type_is_taken_from_scans_that_actually_have_gps_and_can_be_removed_from_classic_formats()
 {
    let dir = tempfile::tempdir().unwrap();
    let mut project = Project::create(&dir.path().join("p"), "GPS").unwrap();
    for (i, format, time_type) in [
        (0, 0, las::GpsTimeType::Week),
        (1, 1, las::GpsTimeType::Standard),
        (2, 1, las::GpsTimeType::Week),
    ] {
        let input = dir.path().join(format!("source{i}.las"));
        let mut builder = las::Builder::from((1, 2));
        builder.point_format = las::point::Format::new(format).unwrap();
        builder.gps_time_type = time_type;
        let mut writer = las::Writer::from_path(&input, builder.into_header().unwrap()).unwrap();
        writer
            .write_point(las::Point {
                gps_time: (format == 1).then_some(12345.),
                ..Default::default()
            })
            .unwrap();
        writer.close().unwrap();
        project
            .import_file(&input, ImportOptions::default(), &JobControl::default())
            .unwrap();
        if i == 1 {
            assert!(!project.las_export_compatibility().unwrap().has_conflicts());
            let output = dir.path().join("preserved.laz");
            project.export_las(&output, &JobControl::default()).unwrap();
            let (header, points) = read(&output);
            assert_eq!(
                las::Builder::from(header).gps_time_type,
                las::GpsTimeType::Standard
            );
            assert_eq!(points[0].gps_time, Some(0.));
            assert_eq!(points[1].gps_time, Some(12345.));
        }
    }
    assert!(project.las_export_compatibility().unwrap().omit_gps_time);
    for ext in ["las", "laz"] {
        let output = dir.path().join(format!("omitted.{ext}"));
        project
            .export_las_with_policy(
                &output,
                LasExportPolicy::OmitIncompatible,
                &JobControl::default(),
            )
            .unwrap();
        let (header, points) = read(&output);
        assert!(!header.point_format().has_gps_time);
        assert_eq!(points.len(), 3);
        assert!(points.iter().all(|p| p.gps_time.is_none()));
    }
}

#[test]
fn conflicting_evlrs_are_reported_and_removed_while_common_evlrs_survive() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.las");
    write_source(&input);
    let mut project = Project::create(&dir.path().join("p"), "EVLR").unwrap();
    for i in 0..2 {
        project
            .import_file(&input, ImportOptions::default(), &JobControl::default())
            .unwrap();
        let meta = project.manifest.scans[i].las.as_mut().unwrap();
        for (id, value) in [(1, 7), (2, i as u8)] {
            meta.evlrs.push(geemil_core::LasVlr {
                user_id: "vendor".into(),
                record_id: id,
                description: "EVLR".into(),
                data: vec![value],
            });
        }
    }
    let report = project.las_export_compatibility().unwrap();
    assert_eq!(report.omitted_metadata, vec![("vendor".into(), 2)]);
    let output = dir.path().join("merged.laz");
    project
        .export_las_with_policy(
            &output,
            LasExportPolicy::OmitIncompatible,
            &JobControl::default(),
        )
        .unwrap();
    let (header, points) = read(&output);
    assert_eq!(points.len(), 800);
    assert_eq!(header.evlrs().len(), 1);
    assert_eq!(header.evlrs()[0].record_id, 1);
    assert_eq!(header.evlrs()[0].data, vec![7]);
}

#[test]
fn source_layout_pointers_are_not_copied_into_ordinary_laz() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.las");
    write_source(&input);
    let mut project = Project::create(&dir.path().join("p"), "Layout").unwrap();
    project
        .import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let meta = project.manifest.scans[0].las.as_mut().unwrap();
    meta.vlrs.push(geemil_core::LasVlr {
        user_id: "copc".into(),
        record_id: 1,
        description: "source hierarchy offsets".into(),
        data: vec![0; 160],
    });
    meta.evlrs.push(geemil_core::LasVlr {
        user_id: "copc".into(),
        record_id: 1000,
        description: "source chunk offsets".into(),
        data: vec![0; 32],
    });
    let output = dir.path().join("out.laz");
    project.export_las(&output, &JobControl::default()).unwrap();
    let (header, points) = read(&output);
    assert_eq!(points.len(), 400);
    assert!(
        header
            .vlrs()
            .iter()
            .chain(header.evlrs())
            .all(|v| v.user_id != "copc")
    );
    assert!(header.vlrs().iter().any(|v| v.user_id == "laszip encoded"));
}

#[test]
fn waveform_import_fails_before_publishing_a_scan() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("waveform.las");
    let mut builder = las::Builder::from((1, 4));
    builder.point_format = las::point::Format::new(4).unwrap();
    let mut writer = las::Writer::from_path(&input, builder.into_header().unwrap()).unwrap();
    writer
        .write_point(las::Point {
            gps_time: Some(0.),
            waveform: Some(Default::default()),
            ..Default::default()
        })
        .unwrap();
    writer.close().unwrap();
    let mut project = Project::create(&dir.path().join("p"), "Waveform").unwrap();
    let before = project.current().id;
    let error = project
        .import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap_err();
    assert_eq!(
        CoreError::find(&error),
        Some(&CoreError::UnsupportedLasWaveform)
    );
    assert_eq!(project.current().id, before);
    assert_eq!(project.scans().count(), 0);
}

#[test]
fn invalid_las_layout_is_an_error_instead_of_a_length_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.las");
    write_source(&input);
    let root = dir.path().join("p");
    let mut project = Project::create(&root, "Layout validation").unwrap();
    project
        .import_file(&input, ImportOptions::default(), &JobControl::default())
        .unwrap();
    let scan = project.scans().next().unwrap();
    let metadata_path =
        root.join(scan.points_file.trim_end_matches(".points").to_owned() + ".scan.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
    metadata["las"]["extra_bytes"] = u16::MAX.into();
    std::fs::write(metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(
        Project::load(&root)
            .unwrap_err()
            .to_string()
            .contains("LAS record length")
    );
}
