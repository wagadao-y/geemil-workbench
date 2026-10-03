//! Filters that judge each original point by its neighbours: voxel
//! subsampling and isolated point removal. Both run per scan in scan
//! coordinates, chunk by chunk, reading neighbouring chunks as far as the filter
//! looks, so the result does not depend on where chunks split the scan. They
//! add an exclusion layer instead of rewriting points.
use crate::edit::{LayerWriter, is_excluded};
use crate::storage::{position, valid};
use crate::{Bounds, CropBox, JobControl, LayerKind, Project, Scan, Stage};
use anyhow::{Result, ensure};
use glam::DVec3;
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
}
#[derive(Default)]
struct CacheState {
    entries: HashMap<u32, (Arc<ChunkPoints>, u64)>,
    clock: u64,
    bytes: usize,
}
impl<'a> ChunkCache<'a> {
    fn new(project: &'a Project, scan: &'a Scan) -> Self {
        Self {
            project,
            scan,
            state: Mutex::default(),
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
        while state.bytes + bytes > CACHE_BYTES {
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
            LayerKind::Subsample { size },
            serde_json::json!({"kind": "subsample", "size": size, "scans": scan_ids}),
            job,
            |cache, chunk, job| subsample_chunk(cache, chunk, size, job),
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
        let total: u64 = scans.iter().map(|s| s.chunks.len() as u64).sum();
        let done = AtomicU64::new(0);
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
        let mut layer = LayerWriter::new(self)?;
        for scan in &scans {
            let cache = ChunkCache::new(self, scan);
            let results: Vec<Mutex<Option<ChunkResult>>> =
                scan.chunks.iter().map(|_| Mutex::new(None)).collect();
            let next = AtomicUsize::new(0);
            let failed = AtomicBool::new(false);
            let error = Mutex::new(None);
            std::thread::scope(|scope| {
                for _ in 0..workers.min(scan.chunks.len()) {
                    scope.spawn(|| {
                        while !failed.load(Ordering::Relaxed) {
                            let chunk = next.fetch_add(1, Ordering::Relaxed);
                            if chunk >= scan.chunks.len() {
                                break;
                            }
                            match judge(&cache, chunk as u32, job) {
                                Ok(result) => *results[chunk].lock().unwrap() = Some(result),
                                Err(e) => {
                                    failed.store(true, Ordering::Relaxed);
                                    error.lock().unwrap().get_or_insert(e);
                                    break;
                                }
                            }
                            job.report(stage, done.fetch_add(1, Ordering::Relaxed) + 1, total);
                        }
                    });
                }
            });
            if let Some(e) = error.into_inner().unwrap() {
                return Err(e);
            }
            for (chunk, result) in results.into_iter().enumerate() {
                let (mask, count) = result.into_inner().unwrap().expect("every chunk judged");
                layer.push(scan.id, chunk as u32, &mask, count)?;
            }
        }
        job.check()?;
        layer.finish(self, kind, operation)
    }
}
