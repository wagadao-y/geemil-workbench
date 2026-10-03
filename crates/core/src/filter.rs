//! Filters that judge each original point by its neighbours: voxel
//! subsampling and isolated point removal. Both run per scan in scan
//! coordinates, chunk by chunk, reading neighbouring chunks as far as the filter
//! looks, so the result does not depend on where chunks split the scan. They
//! add an exclusion layer instead of rewriting points.
use crate::edit::{LayerWriter, is_excluded};
use crate::storage::{position, valid};
use crate::{Bounds, CropBox, JobControl, LayerKind, Project, Scan, Stage};
use anyhow::{Result, ensure};
use glam::{DMat4, DVec3};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use uuid::Uuid;

/// Decoded neighbour chunks kept per scan while filtering.
const CACHE_BYTES: usize = 512 * 1024 * 1024;

/// The points of a chunk that are valid and not excluded, in scan coordinates.
struct ChunkPoints {
    indices: Vec<u32>,
    positions: Vec<DVec3>,
}
impl ChunkPoints {
    fn bytes(&self) -> usize {
        self.indices.len() * (4 + 24) + 64
    }
}

/// A small LRU of decoded chunks shared by the workers filtering one scan.
struct ChunkCache<'a> {
    project: &'a Project,
    scan: &'a Scan,
    state: Mutex<CacheState>,
    limit: usize,
}
#[derive(Default)]
struct CacheState {
    entries: HashMap<u32, (Arc<ChunkPoints>, u64)>,
    clock: u64,
    bytes: usize,
}
impl<'a> ChunkCache<'a> {
    fn new(project: &'a Project, scan: &'a Scan) -> Self {
        Self::with_limit(project, scan, CACHE_BYTES)
    }
    fn with_limit(project: &'a Project, scan: &'a Scan, limit: usize) -> Self {
        Self {
            project,
            scan,
            state: Mutex::default(),
            limit,
        }
    }
    fn get(&self, chunk: u32, job: &JobControl) -> Result<Arc<ChunkPoints>> {
        {
            let mut state = self.state.lock().unwrap();
            state.clock += 1;
            let clock = state.clock;
            if let Some((points, used)) = state.entries.get_mut(&chunk) {
                *used = clock;
                return Ok(points.clone());
            }
        }
        // Decode outside the lock; two workers may decode the same chunk once.
        let data = self.project.read_chunk(self.scan, chunk)?;
        let mask = self.project.exclusion_mask(self.scan, chunk)?;
        let mut points = ChunkPoints {
            indices: vec![],
            positions: vec![],
        };
        for (i, p) in data.chunks_exact(self.scan.stride).enumerate() {
            if i % 8192 == 0 {
                job.check()?;
            }
            if valid(p) && !is_excluded(&mask, i) {
                points.indices.push(i as u32);
                points.positions.push(DVec3::from(position(p)));
            }
        }
        let points = Arc::new(points);
        let bytes = points.bytes();
        let mut state = self.state.lock().unwrap();
        while state.bytes + bytes > self.limit {
            let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            let (old, _) = state.entries.remove(&oldest).unwrap();
            state.bytes -= old.bytes();
        }
        let clock = state.clock;
        if state
            .entries
            .insert(chunk, (points.clone(), clock))
            .is_none()
        {
            state.bytes += bytes;
        }
        Ok(points)
    }
    /// Surviving points of the other chunks that lie in the box `[lo, hi]`.
    fn neighbours(
        &self,
        own: u32,
        lo: DVec3,
        hi: DVec3,
        job: &JobControl,
    ) -> Result<Vec<(DVec3, u32, u32)>> {
        let mut result = vec![];
        for (id, chunk) in self.scan.chunks.iter().enumerate() {
            let id = id as u32;
            if id == own || !overlaps(&chunk.bounds, lo, hi) {
                continue;
            }
            let points = self.get(id, job)?;
            for (p, index) in points.positions.iter().zip(&points.indices) {
                if p.cmpge(lo).all() && p.cmple(hi).all() {
                    result.push((*p, id, *index));
                }
            }
        }
        Ok(result)
    }
}

fn overlaps(bounds: &Bounds, lo: DVec3, hi: DVec3) -> bool {
    DVec3::from(bounds.min).cmple(hi).all() && DVec3::from(bounds.max).cmpge(lo).all()
}

fn cell(p: DVec3, size: f64) -> [i64; 3] {
    (p / size).floor().as_i64vec3().to_array()
}

/// A static kd-tree for nearest-neighbour queries: each subslice is a subtree
/// whose middle element splits it on the axis stored with it.
struct KdTree {
    points: Vec<DVec3>,
    axes: Vec<u8>,
}
impl KdTree {
    fn new(mut points: Vec<DVec3>) -> Self {
        let mut axes = vec![0u8; points.len()];
        fn build(points: &mut [DVec3], axes: &mut [u8]) {
            if points.len() <= 1 {
                return;
            }
            let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
            for p in points.iter() {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
            let extent = hi - lo;
            let axis = if extent.x >= extent.y && extent.x >= extent.z {
                0
            } else if extent.y >= extent.z {
                1
            } else {
                2
            };
            let mid = points.len() / 2;
            points.select_nth_unstable_by(mid, |a, b| a[axis].total_cmp(&b[axis]));
            axes[mid] = axis as u8;
            let (left, right) = points.split_at_mut(mid);
            let (left_axes, right_axes) = axes.split_at_mut(mid);
            build(left, left_axes);
            build(&mut right[1..], &mut right_axes[1..]);
        }
        build(&mut points, &mut axes);
        Self { points, axes }
    }
    /// The squared distances of the `best.len()` nearest points within
    /// `radius` of `p`, ascending; unfilled slots stay infinite.
    fn nearest(&self, p: DVec3, radius: f64, best: &mut [f64]) {
        best.fill(f64::INFINITY);
        let limit = radius * radius;
        fn visit(
            tree: &KdTree,
            range: std::ops::Range<usize>,
            p: DVec3,
            limit: f64,
            best: &mut [f64],
        ) {
            if range.is_empty() {
                return;
            }
            let mid = range.start + range.len() / 2;
            let q = tree.points[mid];
            let d2 = q.distance_squared(p);
            if d2 <= limit && d2 < best[best.len() - 1] {
                // Insert into the ascending list.
                let mut i = best.len() - 1;
                while i > 0 && best[i - 1] > d2 {
                    best[i] = best[i - 1];
                    i -= 1;
                }
                best[i] = d2;
            }
            let axis = tree.axes[mid] as usize;
            let diff = p[axis] - q[axis];
            let (near, far) = if diff < 0. {
                (range.start..mid, mid + 1..range.end)
            } else {
                (mid + 1..range.end, range.start..mid)
            };
            visit(tree, near, p, limit, best);
            if diff * diff <= limit.min(best[best.len() - 1]) {
                visit(tree, far, p, limit, best);
            }
        }
        visit(self, 0..self.points.len(), p, limit, best);
    }
}

/// Mean distance from each surviving point of `chunk` to its `k` nearest
/// other points of the scan within `radius`; `None` where fewer are in reach.
fn mean_neighbour_distances(
    cache: &ChunkCache,
    chunk: u32,
    k: usize,
    radius: f64,
    job: &JobControl,
) -> Result<(std::sync::Arc<ChunkPoints>, Vec<Option<f64>>)> {
    let own = cache.get(chunk, job)?;
    let bounds = &cache.scan.chunks[chunk as usize].bounds;
    let lo = DVec3::from(bounds.min) - radius;
    let hi = DVec3::from(bounds.max) + radius;
    let mut points = own.positions.clone();
    points.extend(
        cache
            .neighbours(chunk, lo, hi, job)?
            .into_iter()
            .map(|(p, ..)| p),
    );
    let tree = KdTree::new(points);
    // The point itself comes first at distance 0.
    let mut best = vec![0.; k + 1];
    let mut result = Vec::with_capacity(own.positions.len());
    for (i, p) in own.positions.iter().enumerate() {
        if i % 4096 == 0 {
            job.check()?;
        }
        tree.nearest(*p, radius, &mut best);
        result.push(
            best[k]
                .is_finite()
                .then(|| best[1..].iter().map(|d| d.sqrt()).sum::<f64>() / k as f64),
        );
    }
    Ok((own, result))
}

/// A chunk's exclusion mask and the number of points it excludes.
type ChunkResult = (Vec<u8>, u64);

/// Voxel subsampling of one chunk: within each voxel, keep the point closest
/// to the voxel centre, ties broken by chunk and index. Every chunk with
/// points in the chunk's voxels takes part, so each voxel keeps exactly one.
fn subsample_chunk(
    cache: &ChunkCache,
    chunk: u32,
    size: f64,
    job: &JobControl,
) -> Result<ChunkResult> {
    let own = cache.get(chunk, job)?;
    let bounds = &cache.scan.chunks[chunk as usize].bounds;
    let lo = (DVec3::from(bounds.min) / size).floor() * size;
    let hi = ((DVec3::from(bounds.max) / size).floor() + 1.) * size;
    let rank = |p: DVec3, chunk: u32, index: u32| {
        let key = cell(p, size);
        let centre = (DVec3::from(key.map(|v| v as f64)) + 0.5) * size;
        (key, (p.distance_squared(centre), chunk, index))
    };
    let mut best: HashMap<[i64; 3], (f64, u32, u32)> = HashMap::new();
    let better = |a: &(f64, u32, u32), b: &(f64, u32, u32)| {
        a.0.total_cmp(&b.0)
            .then((a.1, a.2).cmp(&(b.1, b.2)))
            .is_lt()
    };
    for (p, index) in own.positions.iter().zip(&own.indices) {
        let (key, score) = rank(*p, chunk, *index);
        let entry = best.entry(key).or_insert(score);
        if better(&score, entry) {
            *entry = score;
        }
    }
    for (p, other, index) in cache.neighbours(chunk, lo, hi, job)? {
        let (key, score) = rank(p, other, index);
        // Only voxels this chunk has points in decide anything here.
        if let Some(entry) = best.get_mut(&key)
            && better(&score, entry)
        {
            *entry = score;
        }
    }
    let mut mask = vec![0u8; (cache.scan.chunks[chunk as usize].count as usize).div_ceil(8)];
    let mut count = 0;
    for (p, index) in own.positions.iter().zip(&own.indices) {
        let (key, _) = rank(*p, chunk, *index);
        let (_, c, i) = best[&key];
        if (c, i) != (chunk, *index) {
            mask[*index as usize / 8] |= 1 << (index % 8);
            count += 1;
        }
    }
    Ok((mask, count))
}

/// Merged voxel subsampling of chunk `chunk` of scan `si`, in the project
/// frame; see `Project::subsample_merged`.
fn merged_subsample_chunk(
    caches: &[ChunkCache],
    worlds: &[DMat4],
    boxes: &[Vec<(DVec3, DVec3)>],
    si: usize,
    chunk: u32,
    size: f64,
    job: &JobControl,
) -> Result<ChunkResult> {
    type Score = (f64, u32, u32, u32);
    let own = caches[si].get(chunk, job)?;
    let (lo, hi) = boxes[si][chunk as usize];
    let lo = (lo / size).floor() * size;
    let hi = ((hi / size).floor() + 1.) * size;
    let rank = |p: DVec3, scan: usize, chunk: u32, index: u32| -> ([i64; 3], Score) {
        let key = cell(p, size);
        let centre = (DVec3::from(key.map(|v| v as f64)) + 0.5) * size;
        (key, (p.distance_squared(centre), scan as u32, chunk, index))
    };
    let better = |a: &Score, b: &Score| {
        a.0.total_cmp(&b.0)
            .then((a.1, a.2, a.3).cmp(&(b.1, b.2, b.3)))
            .is_lt()
    };
    let world = |scan: usize, p: &DVec3| worlds[scan].transform_point3(*p);
    let mut best: HashMap<[i64; 3], Score> = HashMap::new();
    for (p, index) in own.positions.iter().zip(&own.indices) {
        let (key, score) = rank(world(si, p), si, chunk, *index);
        let entry = best.entry(key).or_insert(score);
        if better(&score, entry) {
            *entry = score;
        }
    }
    for (sj, cache) in caches.iter().enumerate() {
        for (cj, (a, b)) in boxes[sj].iter().enumerate() {
            let cj = cj as u32;
            if (sj, cj) == (si, chunk) || a.cmpgt(hi).any() || b.cmplt(lo).any() {
                continue;
            }
            let points = cache.get(cj, job)?;
            for (p, index) in points.positions.iter().zip(&points.indices) {
                let p = world(sj, p);
                if !(p.cmpge(lo).all() && p.cmple(hi).all()) {
                    continue;
                }
                let (key, score) = rank(p, sj, cj, *index);
                if let Some(entry) = best.get_mut(&key)
                    && better(&score, entry)
                {
                    *entry = score;
                }
            }
        }
    }
    let count_bits = caches[si].scan.chunks[chunk as usize].count as usize;
    let mut mask = vec![0u8; count_bits.div_ceil(8)];
    let mut count = 0;
    for (p, index) in own.positions.iter().zip(&own.indices) {
        let (key, _) = rank(world(si, p), si, chunk, *index);
        let (_, s, c, i) = best[&key];
        if (s as usize, c, i) != (si, chunk, *index) {
            mask[*index as usize / 8] |= 1 << (index % 8);
            count += 1;
        }
    }
    Ok((mask, count))
}

/// Isolated point removal for one chunk: a point with fewer than
/// `min_neighbours` other points within `radius` is excluded.
fn noise_chunk(
    cache: &ChunkCache,
    chunk: u32,
    radius: f64,
    min_neighbours: u32,
    job: &JobControl,
) -> Result<ChunkResult> {
    let own = cache.get(chunk, job)?;
    let bounds = &cache.scan.chunks[chunk as usize].bounds;
    let lo = DVec3::from(bounds.min) - radius;
    let hi = DVec3::from(bounds.max) + radius;
    let mut points = own.positions.clone();
    points.extend(
        cache
            .neighbours(chunk, lo, hi, job)?
            .into_iter()
            .map(|(p, ..)| p),
    );
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    for (i, p) in points.iter().enumerate() {
        grid.entry(cell(*p, radius)).or_default().push(i as u32);
    }
    let r2 = radius * radius;
    let mut mask = vec![0u8; (cache.scan.chunks[chunk as usize].count as usize).div_ceil(8)];
    let mut count = 0;
    for (i, (p, index)) in own.positions.iter().zip(&own.indices).enumerate() {
        if i % 4096 == 0 {
            job.check()?;
        }
        let [x, y, z] = cell(*p, radius);
        let mut found = 0;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(cell) = grid.get(&[x + dx, y + dy, z + dz]) else {
                        continue;
                    };
                    for &j in cell {
                        if j as usize != i && points[j as usize].distance_squared(*p) <= r2 {
                            found += 1;
                            if found >= min_neighbours {
                                break 'search;
                            }
                        }
                    }
                }
            }
        }
        if found < min_neighbours {
            mask[*index as usize / 8] |= 1 << (index % 8);
            count += 1;
        }
    }
    Ok((mask, count))
}

impl Project {
    /// Keeps one original point per voxel of `size` metres in each of
    /// `scan_ids`, in scan coordinates, and excludes the rest as a new layer.
    /// Returns the number of excluded points.
    pub fn subsample(&mut self, size: f64, scan_ids: &[Uuid], job: &JobControl) -> Result<u64> {
        ensure!(size.is_finite() && size > 0., "Invalid voxel size");
        self.filter(
            scan_ids,
            Stage::Subsampling,
            LayerKind::Subsample {
                size,
                merged: false,
            },
            serde_json::json!({"kind": "subsample", "size": size, "scans": scan_ids}),
            job,
            |cache, chunk, job| subsample_chunk(cache, chunk, size, job),
        )
    }
    /// Keeps one original point per voxel of `size` metres over all of
    /// `scan_ids` together, on one grid in the project frame, so where scans
    /// overlap only one point of all of them remains in each voxel. Like the
    /// per-scan version, the point nearest the voxel centre wins (ties by scan
    /// order, chunk and index), and every chunk of every scan reaching into a
    /// chunk's voxels takes part.
    pub fn subsample_merged(
        &mut self,
        size: f64,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<u64> {
        ensure!(size.is_finite() && size > 0., "Invalid voxel size");
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let limit = (CACHE_BYTES / scans.len().max(1)).max(32 * 1024 * 1024);
        let caches: Vec<_> = scans
            .iter()
            .map(|s| ChunkCache::with_limit(self, s, limit))
            .collect();
        let worlds: Vec<_> = scans.iter().map(|s| self.world_matrix(s)).collect();
        // Each chunk's box in the project frame.
        let boxes: Vec<Vec<(DVec3, DVec3)>> = scans
            .iter()
            .zip(&worlds)
            .map(|(scan, world)| {
                scan.chunks
                    .iter()
                    .map(|c| {
                        c.bounds.corners().fold(
                            (DVec3::INFINITY, DVec3::NEG_INFINITY),
                            |(lo, hi), p| {
                                let p = world.transform_point3(p);
                                (lo.min(p), hi.max(p))
                            },
                        )
                    })
                    .collect()
            })
            .collect();
        let progress = Progress::new(Stage::Subsampling, &scans, 1);
        let mut layer = LayerWriter::new(self)?;
        for (si, scan) in scans.iter().enumerate() {
            let masks = each_chunk(&caches[si], job, &progress, &|_, chunk, job| {
                merged_subsample_chunk(&caches, &worlds, &boxes, si, chunk, size, job)
            })?;
            for (chunk, (mask, count)) in masks.into_iter().enumerate() {
                layer.push(scan.id, chunk as u32, &mask, count)?;
            }
        }
        job.check()?;
        layer.finish(
            self,
            LayerKind::Subsample { size, merged: true },
            serde_json::json!({"kind": "subsample", "size": size, "merged": true,
                "scans": scan_ids}),
        )
    }
    /// Excludes points of `scan_ids` with fewer than `min_neighbours` other
    /// points of the same scan within `radius` metres, as a new layer.
    pub fn remove_noise(
        &mut self,
        radius: f64,
        min_neighbours: u32,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<u64> {
        ensure!(radius.is_finite() && radius > 0., "Invalid search radius");
        ensure!(min_neighbours > 0, "Invalid neighbour count");
        self.filter(
            scan_ids,
            Stage::NoiseFilter,
            LayerKind::Noise {
                radius,
                min_neighbours,
            },
            serde_json::json!({"kind": "noise_filter", "radius": radius,
                "min_neighbours": min_neighbours, "scans": scan_ids}),
            job,
            |cache, chunk, job| noise_chunk(cache, chunk, radius, min_neighbours, job),
        )
    }
    /// Excludes the points of `scan_ids` inside `crop` (or outside it, for
    /// cropping to it) as a new layer. Chunks entirely on the kept side are
    /// not read.
    pub fn exclude_box(
        &mut self,
        crop: &CropBox,
        inside: bool,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<u64> {
        ensure!(
            crop.size.iter().all(|s| s.is_finite() && *s > 0.)
                && crop.center.iter().chain([&crop.yaw]).all(|v| v.is_finite()),
            "Invalid box"
        );
        let unit = crop.unit_matrix();
        self.filter(
            scan_ids,
            Stage::BoxCrop,
            LayerKind::Box { inside },
            serde_json::json!({"kind": "box", "box": crop, "inside": inside, "scans": scan_ids}),
            job,
            |cache, chunk, job| {
                let to_box = unit * cache.project.world_matrix(cache.scan);
                let info = &cache.scan.chunks[chunk as usize];
                let mut mask = vec![0u8; (info.count as usize).div_ceil(8)];
                // The chunk's bounds in box coordinates, conservatively.
                let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
                for c in info.bounds.corners() {
                    let q = to_box.transform_point3(c);
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
                let disjoint = lo.cmpgt(DVec3::ONE).any() || hi.cmplt(-DVec3::ONE).any();
                let within = lo.cmpge(-DVec3::ONE).all() && hi.cmple(DVec3::ONE).all();
                if (inside && disjoint) || (!inside && within) {
                    return Ok((mask, 0));
                }
                let points = cache.get(chunk, job)?;
                let mut count = 0;
                for (p, index) in points.positions.iter().zip(&points.indices) {
                    let q = to_box.transform_point3(*p);
                    if q.abs().cmple(DVec3::ONE).all() == inside {
                        mask[*index as usize / 8] |= 1 << (index % 8);
                        count += 1;
                    }
                }
                Ok((mask, count))
            },
        )
    }
    /// Statistical outlier removal, per scan: a point is excluded when its mean
    /// distance to its `neighbours` nearest points is more than `deviations`
    /// standard deviations above the scan's mean, or when fewer than
    /// `neighbours` points are within `max_distance` (which bounds how far
    /// neighbouring chunks are read). Two passes: statistics, then the mask.
    pub fn remove_outliers(
        &mut self,
        neighbours: u32,
        deviations: f64,
        max_distance: f64,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<u64> {
        ensure!((1..=256).contains(&neighbours), "Invalid neighbour count");
        ensure!(
            deviations.is_finite() && deviations >= 0.,
            "Invalid deviation"
        );
        ensure!(
            max_distance.is_finite() && max_distance > 0.,
            "Invalid search distance"
        );
        let k = neighbours as usize;
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let statistics = Progress::new(Stage::OutlierStatistics, &scans, 1);
        let filtering = Progress::new(Stage::OutlierFilter, &scans, 1);
        let mut layer = LayerWriter::new(self)?;
        for scan in &scans {
            let cache = ChunkCache::new(self, scan);
            let sums = each_chunk(&cache, job, &statistics, &|cache, chunk, job| {
                let (_, distances) = mean_neighbour_distances(cache, chunk, k, max_distance, job)?;
                Ok(distances
                    .into_iter()
                    .flatten()
                    .fold((0u64, 0., 0.), |(n, s, q), d| (n + 1, s + d, q + d * d)))
            })?;
            let (n, sum, squares) = sums
                .into_iter()
                .fold((0u64, 0., 0.), |(n, s, q), (a, b, c)| (n + a, s + b, q + c));
            let mean = sum / n.max(1) as f64;
            let sigma = (squares / n.max(1) as f64 - mean * mean).max(0.).sqrt();
            let threshold = mean + deviations * sigma;
            let masks = each_chunk(&cache, job, &filtering, &|cache, chunk, job| {
                let (own, distances) =
                    mean_neighbour_distances(cache, chunk, k, max_distance, job)?;
                let mut mask =
                    vec![0u8; (cache.scan.chunks[chunk as usize].count as usize).div_ceil(8)];
                let mut count = 0;
                for (index, d) in own.indices.iter().zip(distances) {
                    if d.is_none_or(|d| d > threshold) {
                        mask[*index as usize / 8] |= 1 << (index % 8);
                        count += 1;
                    }
                }
                Ok((mask, count))
            })?;
            for (chunk, (mask, count)) in masks.into_iter().enumerate() {
                layer.push(scan.id, chunk as u32, &mask, count)?;
            }
        }
        job.check()?;
        layer.finish(
            self,
            LayerKind::Statistical {
                neighbours,
                deviations,
            },
            serde_json::json!({"kind": "outlier_filter", "neighbours": neighbours,
                "deviations": deviations, "max_distance": max_distance, "scans": scan_ids}),
        )
    }
    /// Runs `judge` on every chunk of the scans on worker threads and writes
    /// the results, in chunk order, as one layer.
    fn filter(
        &mut self,
        scan_ids: &[Uuid],
        stage: Stage,
        kind: LayerKind,
        operation: serde_json::Value,
        job: &JobControl,
        judge: impl Fn(&ChunkCache, u32, &JobControl) -> Result<ChunkResult> + Sync,
    ) -> Result<u64> {
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let progress = Progress::new(stage, &scans, 1);
        let mut layer = LayerWriter::new(self)?;
        for scan in &scans {
            let cache = ChunkCache::new(self, scan);
            let results = each_chunk(&cache, job, &progress, &judge)?;
            for (chunk, (mask, count)) in results.into_iter().enumerate() {
                layer.push(scan.id, chunk as u32, &mask, count)?;
            }
        }
        job.check()?;
        layer.finish(self, kind, operation)
    }
}

/// Chunks done over all passes, for progress reports.
struct Progress {
    stage: Stage,
    done: AtomicU64,
    total: u64,
}
impl Progress {
    fn new(stage: Stage, scans: &[&Scan], passes: u64) -> Self {
        Self {
            stage,
            done: AtomicU64::new(0),
            total: scans.iter().map(|s| s.chunks.len() as u64).sum::<u64>() * passes,
        }
    }
    fn step(&self, job: &JobControl) {
        job.report(
            self.stage,
            self.done.fetch_add(1, Ordering::Relaxed) + 1,
            self.total,
        );
    }
}

/// Runs `work` on every chunk of the cache's scan on worker threads and
/// returns the results in chunk order; the first error stops the rest.
fn each_chunk<R: Send>(
    cache: &ChunkCache,
    job: &JobControl,
    progress: &Progress,
    work: &(impl Fn(&ChunkCache, u32, &JobControl) -> Result<R> + Sync),
) -> Result<Vec<R>> {
    let chunks = cache.scan.chunks.len();
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    let results: Vec<Mutex<Option<R>>> = (0..chunks).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let error = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..workers.min(chunks) {
            scope.spawn(|| {
                while !failed.load(Ordering::Relaxed) {
                    let chunk = next.fetch_add(1, Ordering::Relaxed);
                    if chunk >= chunks {
                        break;
                    }
                    match work(cache, chunk as u32, job) {
                        Ok(result) => *results[chunk].lock().unwrap() = Some(result),
                        Err(e) => {
                            failed.store(true, Ordering::Relaxed);
                            error.lock().unwrap().get_or_insert(e);
                            break;
                        }
                    }
                    progress.step(job);
                }
            });
        }
    });
    if let Some(e) = error.into_inner().unwrap() {
        return Err(e);
    }
    Ok(results
        .into_iter()
        .map(|r| r.into_inner().unwrap().expect("every chunk processed"))
        .collect())
}
