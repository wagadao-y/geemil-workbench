//! Filters that judge each original point by its neighbours: voxel
//! subsampling and isolated point removal. Both run per scan in scan
//! coordinates, chunk by chunk, reading neighbouring chunks as far as the filter
//! looks, so the result does not depend on where chunks split the scan. They
//! judge the points of visible layers and move the points they pick to
//! another layer instead of rewriting points.
use crate::layers::{LabelWriter, is_set};
use crate::storage::{position, valid};
use crate::{Bounds, CropBox, JobControl, LayerTarget, Project, Scan, Stage};
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
const SCRATCH_BYTES: usize = 32 * 1024 * 1024;

/// Runtime filter resources; not part of the saved project format.
#[derive(Clone, Copy, Debug)]
pub struct FilterOptions {
    pub memory_bytes: usize,
    /// Zero chooses available CPUs, bounded by memory and at most 16 workers.
    pub worker_threads: usize,
}
impl Default for FilterOptions {
    fn default() -> Self {
        Self {
            memory_bytes: 768 * 1024 * 1024,
            worker_threads: 0,
        }
    }
}

/// The valid points of a chunk in visible layers, in scan coordinates.
struct ChunkPoints {
    indices: Vec<u32>,
    positions: Vec<DVec3>,
}
impl ChunkPoints {
    fn bytes(&self) -> usize {
        self.indices.capacity() * 4 + self.positions.capacity() * 24 + 128
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
        Self::with_limit(project, scan, project.filter_options.memory_bytes / 4)
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
        let hidden = self.project.hidden_mask(self.scan, chunk)?;
        let mut points = ChunkPoints {
            indices: vec![],
            positions: vec![],
        };
        for (i, p) in data.chunks_exact(self.scan.stride).enumerate() {
            if i % 8192 == 0 {
                job.check()?;
            }
            if valid(p) && !is_set(&hidden, i) {
                points.indices.push(i as u32);
                points.positions.push(DVec3::from(position(p)));
            }
        }
        // Drop decode buffers before installing the reusable point array.
        drop(data);
        drop(hidden);
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
        if bytes <= self.limit {
            if let Some((old, _)) = state.entries.insert(chunk, (points.clone(), clock)) {
                state.bytes -= old.bytes();
            }
            state.bytes += bytes;
        }
        Ok(points)
    }
    /// Visible points of the other chunks that lie in the box `[lo, hi]`.
    fn visit_neighbours(
        &self,
        own: u32,
        lo: DVec3,
        hi: DVec3,
        job: &JobControl,
        mut visit: impl FnMut(DVec3, u32, u32),
    ) -> Result<()> {
        for (id, chunk) in self.scan.chunks.iter().enumerate() {
            let id = id as u32;
            if id == own || !overlaps(&chunk.bounds, lo, hi) {
                continue;
            }
            let points = self.get(id, job)?;
            for (i, (p, index)) in points.positions.iter().zip(&points.indices).enumerate() {
                if i % 8192 == 0 {
                    job.check()?;
                }
                if p.cmpge(lo).all() && p.cmple(hi).all() {
                    visit(*p, id, *index);
                }
            }
        }
        Ok(())
    }
    fn neighbour_bound(&self, lo: DVec3, hi: DVec3) -> u64 {
        self.scan
            .chunks
            .iter()
            .filter(|c| overlaps(&c.bounds, lo, hi))
            .map(|c| c.count as u64)
            .sum()
    }
    fn neighbour_points(
        &self,
        own: u32,
        lo: DVec3,
        hi: DVec3,
        job: &JobControl,
    ) -> Result<Vec<DVec3>> {
        let mut result = Vec::new();
        self.visit_neighbours(own, lo, hi, job, |p, _, _| result.push(p))?;
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
        self.nearest_into(p, radius, best);
    }
    /// Merge this tree's neighbours into an existing sorted distance list.
    fn nearest_into(&self, p: DVec3, radius: f64, best: &mut [f64]) {
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
    fn count_within(&self, p: DVec3, radius: f64, cap: usize) -> usize {
        fn visit(
            tree: &KdTree,
            range: std::ops::Range<usize>,
            p: DVec3,
            r2: f64,
            cap: usize,
        ) -> usize {
            if range.is_empty() || cap == 0 {
                return 0;
            }
            let mid = range.start + range.len() / 2;
            let q = tree.points[mid];
            let mut count = usize::from(q.distance_squared(p) <= r2);
            let diff = p[tree.axes[mid] as usize] - q[tree.axes[mid] as usize];
            let (near, far) = if diff < 0. {
                (range.start..mid, mid + 1..range.end)
            } else {
                (mid + 1..range.end, range.start..mid)
            };
            count += visit(tree, near, p, r2, cap.saturating_sub(count));
            if diff * diff <= r2 {
                count += visit(tree, far, p, r2, cap.saturating_sub(count));
            }
            count
        }
        visit(self, 0..self.points.len(), p, radius * radius, cap)
    }
}

/// Mean distance from each visible point of `chunk` to its `k` nearest
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
    if cache.neighbour_bound(lo, hi) > (SCRATCH_BYTES / 256) as u64 {
        return mean_neighbour_distances_streamed(cache, chunk, k, radius, job);
    }
    let mut points = own.positions.clone();
    points.extend(cache.neighbour_points(chunk, lo, hi, job)?);
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

fn mean_neighbour_distances_streamed(
    cache: &ChunkCache,
    chunk: u32,
    k: usize,
    radius: f64,
    job: &JobControl,
) -> Result<(Arc<ChunkPoints>, Vec<Option<f64>>)> {
    let own = cache.get(chunk, job)?;
    let mut result = Vec::with_capacity(own.positions.len());
    // At most 2 MiB of distance slots, independently of scan size/radius.
    for batch in own.positions.chunks(1024) {
        job.check()?;
        let mut best = vec![f64::INFINITY; batch.len() * (k + 1)];
        let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
        for p in batch {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        lo -= DVec3::splat(radius);
        hi += DVec3::splat(radius);
        for (id, info) in cache.scan.chunks.iter().enumerate() {
            if !overlaps(&info.bounds, lo, hi) {
                continue;
            }
            job.check()?;
            let points = cache.get(id as u32, job)?;
            let tree = KdTree::new(points.positions.clone());
            for (i, p) in batch.iter().enumerate() {
                if i % 64 == 0 {
                    job.check()?;
                }
                tree.nearest_into(*p, radius, &mut best[i * (k + 1)..(i + 1) * (k + 1)]);
            }
        }
        result.extend(best.chunks_exact(k + 1).map(|b| {
            b[k].is_finite()
                .then(|| b[1..].iter().map(|d| d.sqrt()).sum::<f64>() / k as f64)
        }));
    }
    Ok((own, result))
}

/// The points of a chunk to move, one bit per point, and their number.
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
    cache.visit_neighbours(chunk, lo, hi, job, |p, other, index| {
        let (key, score) = rank(p, other, index);
        // Only voxels this chunk has points in decide anything here.
        if let Some(entry) = best.get_mut(&key)
            && better(&score, entry)
        {
            *entry = score;
        }
    })?;
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
/// `min_neighbours` other points within `radius` is moved.
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
    if cache.neighbour_bound(lo, hi) > (SCRATCH_BYTES / 256) as u64 {
        return noise_chunk_streamed(cache, chunk, radius, min_neighbours, job);
    }
    let mut points = own.positions.clone();
    points.extend(cache.neighbour_points(chunk, lo, hi, job)?);
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

fn noise_chunk_streamed(
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
    let mut counts = vec![0u32; own.positions.len()];
    for (id, info) in cache.scan.chunks.iter().enumerate() {
        if !overlaps(&info.bounds, lo, hi) {
            continue;
        }
        job.check()?;
        let points = cache.get(id as u32, job)?;
        let tree = KdTree::new(points.positions.clone());
        for (i, p) in own.positions.iter().enumerate() {
            if i % 1024 == 0 {
                job.check()?;
            }
            if counts[i] >= min_neighbours {
                continue;
            }
            let self_point = usize::from(id as u32 == chunk);
            let cap = (min_neighbours - counts[i]) as usize + self_point;
            counts[i] += tree
                .count_within(*p, radius, cap)
                .saturating_sub(self_point) as u32;
        }
        if counts.iter().all(|n| *n >= min_neighbours) {
            break;
        }
    }
    let mut mask = vec![0u8; (cache.scan.chunks[chunk as usize].count as usize).div_ceil(8)];
    let mut count = 0;
    for (index, neighbours) in own.indices.iter().zip(counts) {
        if neighbours < min_neighbours {
            mask[*index as usize / 8] |= 1 << (index % 8);
            count += 1;
        }
    }
    Ok((mask, count))
}

impl Project {
    /// Keeps one original point per voxel of `size` metres in each of
    /// `scan_ids`, in scan coordinates, and moves the rest to `target`.
    /// Returns the number of moved points.
    pub fn subsample(
        &mut self,
        size: f64,
        scan_ids: &[Uuid],
        target: &LayerTarget,
        job: &JobControl,
    ) -> Result<u64> {
        ensure!(size.is_finite() && size > 0., "Invalid voxel size");
        self.filter(
            scan_ids,
            Stage::Subsampling,
            target,
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
        target: &LayerTarget,
        job: &JobControl,
    ) -> Result<u64> {
        ensure!(size.is_finite() && size > 0., "Invalid voxel size");
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let limit = self.filter_options.memory_bytes / 4 / scans.len().max(1);
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
        let mut labels = LabelWriter::new(self, target)?;
        for (si, scan) in scans.iter().enumerate() {
            each_chunk(
                &caches[si],
                job,
                &progress,
                &|_, chunk, job| {
                    merged_subsample_chunk(&caches, &worlds, &boxes, si, chunk, size, job)
                },
                |chunk, (mask, count)| labels.push(self, scan, chunk, &mask, count),
            )?;
        }
        job.check()?;
        labels.commit(
            self,
            serde_json::json!({"kind": "subsample", "size": size, "merged": true,
                "scans": scan_ids}),
            |_| Ok(()),
            false,
        )
    }
    /// Moves points of `scan_ids` with fewer than `min_neighbours` other
    /// points of the same scan within `radius` metres to `target`.
    pub fn remove_noise(
        &mut self,
        radius: f64,
        min_neighbours: u32,
        scan_ids: &[Uuid],
        target: &LayerTarget,
        job: &JobControl,
    ) -> Result<u64> {
        ensure!(radius.is_finite() && radius > 0., "Invalid search radius");
        ensure!(min_neighbours > 0, "Invalid neighbour count");
        self.filter(
            scan_ids,
            Stage::NoiseFilter,
            target,
            serde_json::json!({"kind": "noise_filter", "radius": radius,
                "min_neighbours": min_neighbours, "scans": scan_ids}),
            job,
            |cache, chunk, job| noise_chunk(cache, chunk, radius, min_neighbours, job),
        )
    }
    /// Moves the points of `scan_ids` inside `crop` (or outside it, for
    /// cropping to it) to `target`. Chunks entirely on the kept side are not
    /// read.
    pub fn move_box(
        &mut self,
        crop: &CropBox,
        inside: bool,
        scan_ids: &[Uuid],
        target: &LayerTarget,
        job: &JobControl,
    ) -> Result<u64> {
        ensure!(
            crop.size.iter().all(|s| s.is_finite() && *s > 0.)
                && crop
                    .center
                    .iter()
                    .chain(crop.rotation.iter())
                    .all(|v| v.is_finite())
                && (glam::DQuat::from_array(crop.rotation).length_squared() - 1.).abs() < 1e-6,
            "Invalid box"
        );
        let unit = crop.unit_matrix();
        self.filter(
            scan_ids,
            Stage::BoxCrop,
            target,
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
    /// Statistical outlier removal, per scan: a point moves to `target` when its mean
    /// distance to its `neighbours` nearest points is more than `deviations`
    /// standard deviations above the scan's mean, or when fewer than
    /// `neighbours` points are within `max_distance` (which bounds how far
    /// neighbouring chunks are read). Two passes: statistics, then the moves.
    pub fn remove_outliers(
        &mut self,
        neighbours: u32,
        deviations: f64,
        max_distance: f64,
        scan_ids: &[Uuid],
        target: &LayerTarget,
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
        let mut labels = LabelWriter::new(self, target)?;
        for scan in &scans {
            let cache = ChunkCache::new(self, scan);
            let (mut n, mut sum, mut squares) = (0u64, 0., 0.);
            each_chunk(
                &cache,
                job,
                &statistics,
                &|cache, chunk, job| {
                    let (_, distances) =
                        mean_neighbour_distances(cache, chunk, k, max_distance, job)?;
                    Ok(distances
                        .into_iter()
                        .flatten()
                        .fold((0u64, 0., 0.), |(n, s, q), d| (n + 1, s + d, q + d * d)))
                },
                |_, (a, b, c)| {
                    n += a;
                    sum += b;
                    squares += c;
                    Ok(())
                },
            )?;
            let mean = sum / n.max(1) as f64;
            let sigma = (squares / n.max(1) as f64 - mean * mean).max(0.).sqrt();
            let threshold = mean + deviations * sigma;
            each_chunk(
                &cache,
                job,
                &filtering,
                &|cache, chunk, job| {
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
                },
                |chunk, (mask, count)| labels.push(self, scan, chunk, &mask, count),
            )?;
        }
        job.check()?;
        labels.commit(
            self,
            serde_json::json!({"kind": "outlier_filter", "neighbours": neighbours,
                "deviations": deviations, "max_distance": max_distance, "scans": scan_ids}),
            |_| Ok(()),
            false,
        )
    }
    /// Runs `judge` on every chunk of the scans on worker threads and moves
    /// the points it picks to `target` in one edit.
    fn filter(
        &mut self,
        scan_ids: &[Uuid],
        stage: Stage,
        target: &LayerTarget,
        operation: serde_json::Value,
        job: &JobControl,
        judge: impl Fn(&ChunkCache, u32, &JobControl) -> Result<ChunkResult> + Sync,
    ) -> Result<u64> {
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let progress = Progress::new(stage, &scans, 1);
        let mut labels = LabelWriter::new(self, target)?;
        for scan in &scans {
            let cache = ChunkCache::new(self, scan);
            each_chunk(&cache, job, &progress, &judge, |chunk, (mask, count)| {
                labels.push(self, scan, chunk, &mask, count)
            })?;
        }
        job.check()?;
        labels.commit(self, operation, |_| Ok(()), false)
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
/// consumes bounded batches in chunk order; the first error stops the rest.
fn each_chunk<R: Send>(
    cache: &ChunkCache,
    job: &JobControl,
    progress: &Progress,
    work: &(impl Fn(&ChunkCache, u32, &JobControl) -> Result<R> + Sync),
    mut consume: impl FnMut(u32, R) -> Result<()>,
) -> Result<()> {
    let chunks = cache.scan.chunks.len();
    if chunks == 0 {
        return Ok(());
    }
    let options = cache.project.filter_options;
    ensure!(options.worker_threads <= 64, "Too many filter workers");
    // Count the largest possible decode (compressed input, shuffled and final
    // bytes), held neighbour/own arrays, search structures, labels and scratch.
    // Merged filtering can read a chunk from any current scan.
    let largest_raw = cache
        .project
        .scans()
        .flat_map(|s| s.chunks.iter().map(move |c| c.count as usize * s.stride))
        .max()
        .unwrap_or(0);
    let largest_points = cache
        .project
        .scans()
        .flat_map(|s| &s.chunks)
        .map(|c| c.count as usize)
        .max()
        .unwrap_or(0);
    let per_worker = largest_raw
        .saturating_mul(4)
        .saturating_add(largest_points.saturating_mul(512))
        .saturating_add(SCRATCH_BYTES);
    let available = options
        .memory_bytes
        .saturating_sub(options.memory_bytes / 4);
    let capacity = available / per_worker;
    ensure!(capacity > 0, crate::CoreError::FilterMemoryBudgetTooSmall);
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let workers = if options.worker_threads == 0 {
        cpus.min(16)
    } else {
        options.worker_threads
    }
    .min(capacity)
    .min(chunks);
    let width = workers * 2;
    for start in (0..chunks).step_by(width) {
        job.check()?;
        let length = width.min(chunks - start);
        let results: Vec<Mutex<Option<R>>> = (0..length).map(|_| Mutex::new(None)).collect();
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let error = Mutex::new(None);
        std::thread::scope(|scope| {
            for _ in 0..workers.min(length) {
                scope.spawn(|| {
                    while !failed.load(Ordering::Relaxed) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= length {
                            break;
                        }
                        match work(cache, (start + index) as u32, job) {
                            Ok(result) => *results[index].lock().unwrap() = Some(result),
                            Err(e) => {
                                failed.store(true, Ordering::Relaxed);
                                *error.lock().unwrap() = Some(e);
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
        for (index, result) in results.into_iter().enumerate() {
            job.check()?;
            consume(
                (start + index) as u32,
                result.into_inner().unwrap().expect("chunk processed"),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_search_matches_combined_search_with_duplicates_and_hidden_points() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("points.las");
        let mut writer = las::Writer::from_path(&input, las::Header::default()).unwrap();
        for i in 0..180 {
            writer
                .write_point(las::Point {
                    x: (i % 15) as f64 * 0.1,
                    y: (i / 15 % 3) as f64 * 0.1,
                    z: if i < 170 { 0. } else { 9. },
                    ..Default::default()
                })
                .unwrap();
        }
        writer.close().unwrap();
        let mut p = Project::create(&dir.path().join("p"), "Streaming").unwrap();
        let job = JobControl::default();
        p.import_file(
            &input,
            crate::ImportOptions {
                chunk_points: 16,
                ..Default::default()
            },
            &job,
        )
        .unwrap();
        let id = p.scans().next().unwrap().id;
        p.subsample(0.03, &[id], &p.layer_named("Hidden"), &job)
            .unwrap();
        let scan = p.scans().next().unwrap();
        let cache = ChunkCache::new(&p, scan);
        for chunk in 0..scan.chunks.len() as u32 {
            for radius in [0.15, 20.] {
                for count in [1, 8, 200] {
                    assert_eq!(
                        noise_chunk(&cache, chunk, radius, count, &job).unwrap(),
                        noise_chunk_streamed(&cache, chunk, radius, count, &job).unwrap()
                    );
                    let (_, combined) =
                        mean_neighbour_distances(&cache, chunk, count as usize, radius, &job)
                            .unwrap();
                    let (_, streamed) = mean_neighbour_distances_streamed(
                        &cache,
                        chunk,
                        count as usize,
                        radius,
                        &job,
                    )
                    .unwrap();
                    assert_eq!(combined, streamed);
                }
            }
        }
        job.cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            crate::CoreError::find(&noise_chunk_streamed(&cache, 0, 20., 8, &job).unwrap_err()),
            Some(&crate::CoreError::Cancelled)
        );
    }
}
