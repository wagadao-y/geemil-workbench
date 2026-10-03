use geemil_core::{ImportOptions, JobControl, LayerKind, Project};
use std::collections::{BTreeMap, BTreeSet};
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

/// Dense patches on a plane, scattered isolated points around them, and a few
/// exact duplicates, at georeferenced coordinates.
fn write_cloud(path: &Path) {
    let mut builder = las::Builder::from((1, 2));
    builder.point_format = las::point::Format::new(2).unwrap();
    builder.transforms.x.offset = 500000.;
    builder.transforms.y.offset = 4000000.;
    let mut writer = las::Writer::from_path(path, builder.into_header().unwrap()).unwrap();
    let mut next = rng(0x9e37_79b9_7f4a_7c15);
    let mut point = |x: f64, y: f64, z: f64| {
        writer
            .write_point(las::Point {
                x: 500000. + x,
                y: 4000000. + y,
                z,
                color: Some(las::Color::new(65535, 0, 0)),
                ..Default::default()
            })
            .unwrap();
    };
    for patch in 0..4 {
        let (cx, cy) = (patch as f64 * 3., (patch % 2) as f64 * 2.);
        for _ in 0..1500 {
            point(cx + next() * 2., cy + next() * 2., next() * 0.05);
        }
    }
    for _ in 0..60 {
        point(next() * 12. - 1., next() * 6. - 2., 1. + next() * 3.);
    }
    for _ in 0..3 {
        point(1., 1., 0.02);
    }
    writer.close().unwrap();
}

fn project(dir: &Path, name: &str, chunk_points: u32) -> Project {
    let source = dir.join("cloud.las");
    if !source.exists() {
        write_cloud(&source);
    }
    let mut p = Project::create(&dir.join(name), name).unwrap();
    p.import_file(
        &source,
        ImportOptions {
            chunk_points,
            lod_points: 16,
            ..Default::default()
        },
        &JobControl::default(),
    )
    .unwrap();
    p
}

/// Surviving points of every scan in scan coordinates.
fn surviving(p: &Project) -> Vec<[f64; 3]> {
    let mut result = vec![];
    for scan in p.scans() {
        for chunk in 0..scan.chunks.len() {
            result.extend(
                p.points(scan, chunk as u32)
                    .unwrap()
                    .iter()
                    .map(|s| s.position),
            );
        }
    }
    result
}

fn sorted_bits(points: &[[f64; 3]]) -> Vec<[u64; 3]> {
    let mut bits: Vec<_> = points.iter().map(|p| p.map(f64::to_bits)).collect();
    bits.sort();
    bits
}

fn voxel(p: [f64; 3], size: f64) -> [i64; 3] {
    p.map(|v| (v / size).floor() as i64)
}

fn ids(p: &Project) -> Vec<uuid::Uuid> {
    p.scans().map(|s| s.id).collect()
}

#[test]
fn noise_filter_matches_brute_force_regardless_of_chunking() {
    let dir = tempfile::tempdir().unwrap();
    let (radius, min_neighbours) = (0.15, 3);
    let mut results = vec![];
    for (name, chunk_points) in [("small", 64), ("large", 65_536)] {
        let mut p = project(dir.path(), name, chunk_points);
        if chunk_points == 64 {
            assert!(p.scans().next().unwrap().chunks.len() > 50);
        }
        let before = surviving(&p);
        let removed = p
            .remove_noise(radius, min_neighbours, &ids(&p), &JobControl::default())
            .unwrap();
        let after = surviving(&p);
        let expected: Vec<_> = before
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                before
                    .iter()
                    .enumerate()
                    .filter(|(j, b)| {
                        i != j
                            && (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>() <= radius * radius
                    })
                    .count()
                    >= min_neighbours as usize
            })
            .map(|(_, a)| *a)
            .collect();
        assert_eq!(sorted_bits(&after), sorted_bits(&expected));
        assert_eq!(removed as usize, before.len() - after.len());
        assert!(removed > 30, "only {removed} isolated points removed");
        let layer = p.manifest.layers.last().unwrap();
        assert_eq!(
            layer.kind,
            LayerKind::Noise {
                radius,
                min_neighbours
            }
        );
        // The layer is an ordinary exclusion: turning it off restores everything.
        p.set_layer_enabled(layer.id, false).unwrap();
        assert_eq!(surviving(&p).len(), before.len());
        results.push(sorted_bits(&after));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn subsampling_keeps_the_point_nearest_each_voxel_centre_regardless_of_chunking() {
    let dir = tempfile::tempdir().unwrap();
    let size = 0.25;
    let mut results = vec![];
    for (name, chunk_points) in [("small", 64), ("large", 65_536)] {
        let mut p = project(dir.path(), name, chunk_points);
        let before = surviving(&p);
        p.subsample(size, &ids(&p), &JobControl::default()).unwrap();
        let after = surviving(&p);
        let centre_distance = |q: [f64; 3]| {
            let v = voxel(q, size);
            (0..3)
                .map(|k| (q[k] - (v[k] as f64 + 0.5) * size).powi(2))
                .sum::<f64>()
        };
        let mut nearest: BTreeMap<[i64; 3], f64> = BTreeMap::new();
        for q in &before {
            let d = centre_distance(*q);
            nearest
                .entry(voxel(*q, size))
                .and_modify(|best| *best = best.min(d))
                .or_insert(d);
        }
        let kept: BTreeSet<_> = after.iter().map(|q| voxel(*q, size)).collect();
        assert_eq!(kept.len(), after.len(), "a voxel kept two points");
        assert_eq!(kept.len(), nearest.len(), "a voxel lost all points");
        for q in &after {
            assert_eq!(centre_distance(*q), nearest[&voxel(*q, size)]);
        }
        assert_eq!(
            p.manifest.layers.last().unwrap().kind,
            LayerKind::Subsample {
                size,
                merged: false
            }
        );
        // Filtering what is left only considers surviving points.
        let removed = p
            .remove_noise(size * 0.1, 1, &ids(&p), &JobControl::default())
            .unwrap();
        assert_eq!(removed as usize, after.len() - surviving(&p).len());
        results.push(sorted_bits(&after));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn filters_reject_invalid_parameters_and_cancel_without_a_layer() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path(), "p", 64);
    let scans = ids(&p);
    let state = p.current().id;
    assert!(p.subsample(0., &scans, &JobControl::default()).is_err());
    assert!(
        p.subsample(f64::NAN, &scans, &JobControl::default())
            .is_err()
    );
    assert!(
        p.remove_noise(0.1, 0, &scans, &JobControl::default())
            .is_err()
    );
    let job = JobControl::default();
    job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(p.remove_noise(0.1, 2, &scans, &job).is_err());
    assert!(p.manifest.layers.is_empty());
    assert_eq!(p.current().id, state);
}

#[test]
fn box_crop_matches_brute_force_and_inside_complements_outside() {
    use geemil_core::{CropBox, Pose};
    use glam::{DQuat, DVec3};
    let dir = tempfile::tempdir().unwrap();
    let crop = CropBox {
        center: [500004.5, 4000001.2, 0.5],
        size: [3., 1.5, 2.],
        yaw: 0.4,
    };
    for (name, chunk_points) in [("small", 64), ("large", 65_536)] {
        let mut p = project(dir.path(), name, chunk_points);
        let scan = p.scans().next().unwrap().clone();
        // A transformed scan: the box applies in the project frame.
        let centre = DVec3::new(500005., 4000001., 0.);
        let pose = Pose::from_matrix(
            glam::DMat4::from_translation(centre + DVec3::new(0.3, -0.2, 0.))
                * glam::DMat4::from_quat(DQuat::from_rotation_z(0.05))
                * glam::DMat4::from_translation(-centre),
        );
        p.set_transform(scan.id, pose).unwrap();
        let world = p.world_matrix(&scan);
        let all = surviving(&p);
        let expected_inside = all
            .iter()
            .filter(|q| crop.contains(world.transform_point3(DVec3::from(**q))))
            .count() as u64;
        assert!(expected_inside > 100 && expected_inside < all.len() as u64 / 2);
        let state = p.manifest.draft.clone();
        let removed = p
            .exclude_box(&crop, true, &[scan.id], &JobControl::default())
            .unwrap();
        assert_eq!(removed, expected_inside);
        assert!(
            surviving(&p)
                .iter()
                .all(|q| !crop.contains(world.transform_point3(DVec3::from(*q))))
        );
        p.restore_working_state(state).unwrap();
        let removed = p
            .exclude_box(&crop, false, &[scan.id], &JobControl::default())
            .unwrap();
        assert_eq!(removed, all.len() as u64 - expected_inside);
        assert_eq!(
            p.manifest.layers.last().unwrap().kind,
            LayerKind::Box { inside: false }
        );
    }
}

#[test]
fn statistical_outliers_match_brute_force_regardless_of_chunking() {
    let dir = tempfile::tempdir().unwrap();
    let (k, deviations, reach) = (6usize, 1.0, 2.0);
    let mut results = vec![];
    for (name, chunk_points) in [("small", 64), ("large", 65_536)] {
        let mut p = project(dir.path(), name, chunk_points);
        let before = surviving(&p);
        // Mean distance to the k nearest other points; None if fewer in reach.
        let means: Vec<Option<f64>> = before
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let mut d: Vec<f64> = before
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, b)| (0..3).map(|c| (a[c] - b[c]).powi(2)).sum::<f64>().sqrt())
                    .filter(|d| *d <= reach)
                    .collect();
                d.sort_by(f64::total_cmp);
                (d.len() >= k).then(|| d[..k].iter().sum::<f64>() / k as f64)
            })
            .collect();
        let known: Vec<f64> = means.iter().flatten().copied().collect();
        let mean = known.iter().sum::<f64>() / known.len() as f64;
        let sigma =
            (known.iter().map(|d| d * d).sum::<f64>() / known.len() as f64 - mean * mean).sqrt();
        let threshold = mean + deviations * sigma;
        let expected: Vec<_> = before
            .iter()
            .zip(&means)
            .filter(|(_, m)| m.is_some_and(|m| m <= threshold))
            .map(|(p, _)| *p)
            .collect();
        let removed = p
            .remove_outliers(
                k as u32,
                deviations,
                reach,
                &ids(&p),
                &JobControl::default(),
            )
            .unwrap();
        let after = surviving(&p);
        assert_eq!(removed as usize, before.len() - after.len());
        assert_eq!(sorted_bits(&after), sorted_bits(&expected));
        assert!(removed > 30, "only {removed} removed");
        assert_eq!(
            p.manifest.layers.last().unwrap().kind,
            LayerKind::Statistical {
                neighbours: k as u32,
                deviations
            }
        );
        results.push(sorted_bits(&after));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn heavy_exclusions_still_fill_the_view_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project(dir.path(), "p", 64);
    let scans = ids(&p);
    let camera = geemil_core::Camera {
        target: p.bounds().center().to_array(),
        distance: p.bounds().radius() * 3.,
        ..Default::default()
    };
    let budget = 400;
    let before = p
        .load_view(&camera, budget, &scans, &JobControl::default())
        .unwrap()
        .len();
    assert!(before > budget * 3 / 4, "{before} of {budget}");
    // Keep about a tenth of the points.
    p.subsample(0.12, &scans, &JobControl::default()).unwrap();
    let survivors = surviving(&p).len();
    assert!(survivors > budget && survivors < 2000, "{survivors}");
    let after = p
        .load_view(&camera, budget, &scans, &JobControl::default())
        .unwrap()
        .len();
    // Excluded samples no longer use up the budget (before: about a quarter).
    assert!(after > budget * 9 / 10, "{after} of {budget}");
}

#[test]
fn merged_subsampling_keeps_one_point_per_voxel_over_overlapping_scans() {
    use geemil_core::Pose;
    use glam::{DMat4, DQuat, DVec3};
    let dir = tempfile::tempdir().unwrap();
    let size = 0.25;
    let mut results = vec![];
    for (name, chunk_points) in [("small", 64), ("large", 65_536)] {
        let mut p = project(dir.path(), name, chunk_points);
        // The same cloud again, slightly turned and shifted: a second scan of
        // the same place.
        p.import_file(
            &dir.path().join("cloud.las"),
            ImportOptions {
                chunk_points,
                lod_points: 16,
                ..Default::default()
            },
            &JobControl::default(),
        )
        .unwrap();
        let scans = ids(&p);
        assert_eq!(scans.len(), 2);
        let centre = DVec3::new(500005., 4000001., 0.);
        let pose = Pose::from_matrix(
            DMat4::from_translation(centre + DVec3::new(0.013, -0.021, 0.004))
                * DMat4::from_quat(DQuat::from_rotation_z(0.01))
                * DMat4::from_translation(-centre),
        );
        p.set_transform(scans[1], pose).unwrap();
        let world_points = |p: &Project| -> Vec<[f64; 3]> {
            let mut out = vec![];
            for scan in p.scans() {
                let world = p.world_matrix(scan);
                for chunk in 0..scan.chunks.len() {
                    for s in p.points(scan, chunk as u32).unwrap() {
                        out.push(world.transform_point3(DVec3::from(s.position)).to_array());
                    }
                }
            }
            out
        };
        let before = world_points(&p);
        p.subsample_merged(size, &scans, &JobControl::default())
            .unwrap();
        let after = world_points(&p);
        let centre_distance = |q: [f64; 3]| {
            let v = voxel(q, size);
            (0..3)
                .map(|k| (q[k] - (v[k] as f64 + 0.5) * size).powi(2))
                .sum::<f64>()
        };
        let mut nearest: BTreeMap<[i64; 3], f64> = BTreeMap::new();
        for q in &before {
            let d = centre_distance(*q);
            nearest
                .entry(voxel(*q, size))
                .and_modify(|best| *best = best.min(d))
                .or_insert(d);
        }
        let kept: BTreeSet<_> = after.iter().map(|q| voxel(*q, size)).collect();
        assert_eq!(kept.len(), after.len(), "a voxel kept two points");
        assert_eq!(kept.len(), nearest.len(), "a voxel lost all points");
        for q in &after {
            assert_eq!(centre_distance(*q), nearest[&voxel(*q, size)]);
        }
        // Fewer than per-scan subsampling would keep: the overlap counts once.
        assert!(after.len() < before.len() / 2);
        assert_eq!(
            p.manifest.layers.last().unwrap().kind,
            LayerKind::Subsample { size, merged: true }
        );
        results.push(sorted_bits(&after));
    }
    assert_eq!(results[0], results[1]);
}
