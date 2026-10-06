//! Moving object removal. A point another scan's laser passed through was
//! not there when that scan was taken: a person walking by, a door that
//! moved. Each scan with a scanner position keeps a range image, the nearest
//! range it measured in each direction, and the points of the other scans
//! that lie well in front of enough of them are taken. Asking for more than
//! one scan keeps points that a single scan's ghosts seem to pass, such as
//! the mirror image a polished floor shows below itself.
use crate::coords::valid;
use crate::filter::{ChunkCache, Progress, each_chunk, filter_workers};
use crate::layers::LabelWriter;
use crate::layers::is_set;
use crate::parallel::for_each_unordered;
use crate::{CoreError, JobControl, LayerTarget, Project, Scan, Stage};
use anyhow::{Result, ensure};
use glam::{DMat4, DVec3};
use std::{
    collections::{BTreeMap, HashMap},
    f64::consts::{FRAC_PI_2, PI, TAU},
};
use uuid::Uuid;

/// Parameters of [`Project::remove_moving`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovingOptions {
    /// Side of a range image cell, in degrees.
    pub cell_degrees: f64,
    /// How far, in metres, a scan must have measured past a point to take it,
    /// on top of what a cell spans at the point's range.
    pub tolerance: f64,
    /// How many other scans must have seen through a point to take it.
    pub min_scans: u32,
}
impl Default for MovingOptions {
    fn default() -> Self {
        Self {
            cell_degrees: 0.1,
            tolerance: 0.05,
            min_scans: 2,
        }
    }
}

/// The nearest range a scan measured in each direction from its scanner, by
/// azimuth and elevation in its own coordinates, over each cell and its
/// eight neighbours: looking past a point there means looking past it in
/// every direction near it, so edges and surfaces seen at a grazing angle do
/// not count as passed. Infinity where nothing was measured.
struct RangeImage {
    /// The scan; its points are not judged against their own image.
    scan: usize,
    /// The project frame to the scan's coordinates.
    from_project: DMat4,
    sphere: Sphere,
    ranges: Vec<f32>,
}
/// Directions from a scanner in cells of `cell` radians: columns of azimuth
/// around the full turn, rows of elevation from straight down to up.
#[derive(Clone, Copy)]
struct Sphere {
    columns: usize,
    rows: usize,
    cell: f64,
}
impl Sphere {
    fn new(cell: f64) -> Self {
        Self {
            columns: (TAU / cell).ceil() as usize,
            rows: (PI / cell).ceil() as usize,
            cell,
        }
    }
    fn cells(&self) -> usize {
        self.columns * self.rows
    }
    /// The cell holding the direction of `q` (scan coordinates), and its range.
    fn locate(&self, q: DVec3) -> Option<(usize, f64)> {
        let range = q.length();
        if range < 1e-6 {
            return None;
        }
        let azimuth = q.y.atan2(q.x) + PI;
        let elevation = (q.z / range).clamp(-1., 1.).asin() + FRAC_PI_2;
        let column = ((azimuth / self.cell) as usize).min(self.columns - 1);
        let row = ((elevation / self.cell) as usize).min(self.rows - 1);
        Some((row * self.columns + column, range))
    }
}
impl RangeImage {
    fn new(scan: usize, from_project: DMat4, sphere: Sphere) -> Self {
        Self {
            scan,
            from_project,
            sphere,
            ranges: vec![f32::INFINITY; sphere.cells()],
        }
    }
    /// Keeps each cell's nearest range over it and its eight neighbours,
    /// around the full turn of azimuth.
    fn erode(&mut self) {
        let Sphere { columns, rows, .. } = self.sphere;
        let mut across = vec![f32::INFINITY; self.ranges.len()];
        for row in 0..rows {
            let line = &self.ranges[row * columns..(row + 1) * columns];
            for column in 0..columns {
                let left = line[(column + columns - 1) % columns];
                let right = line[(column + 1) % columns];
                across[row * columns + column] = line[column].min(left).min(right);
            }
        }
        for row in 0..rows {
            for column in 0..columns {
                let mut nearest = across[row * columns + column];
                if row > 0 {
                    nearest = nearest.min(across[(row - 1) * columns + column]);
                }
                if row + 1 < rows {
                    nearest = nearest.min(across[(row + 1) * columns + column]);
                }
                self.ranges[row * columns + column] = nearest;
            }
        }
    }
    /// Whether the scan measured well past `p` (project frame) in its
    /// direction.
    fn passes(&self, p: DVec3, tolerance: f64) -> bool {
        let q = self.from_project.transform_point3(p);
        let Some((cell, range)) = self.sphere.locate(q) else {
            return false;
        };
        let measured = self.ranges[cell] as f64;
        measured.is_finite() && measured > range + tolerance + range * self.sphere.cell
    }
}

impl Project {
    /// Moves points of `scan_ids` in visible layers that at least
    /// `min_scans` others of those scans saw through to `target`: in the
    /// direction of the point from such a scan's scanner, every range it
    /// measured near there is farther than the point by more than `tolerance`
    /// and what a cell spans at the point's range. Only scans with a scanner
    /// position (see [`Project::scanner_position`]) see through others; the
    /// points of every scan are judged. Range images, too, count only points
    /// in visible layers, so ghosts moved to a hidden layer first see through
    /// nothing. They are made as many at a time as half the filter memory
    /// holds, and all points are judged against each batch.
    pub fn remove_moving(
        &mut self,
        options: MovingOptions,
        scan_ids: &[Uuid],
        target: &LayerTarget,
        job: &JobControl,
    ) -> Result<u64> {
        let MovingOptions {
            cell_degrees,
            tolerance,
            min_scans,
        } = options;
        ensure!((1..=255).contains(&min_scans), "Invalid scan count");
        ensure!(
            cell_degrees.is_finite() && (0.01..=5.).contains(&cell_degrees),
            "Invalid cell size"
        );
        ensure!(
            tolerance.is_finite() && tolerance >= 0.,
            "Invalid tolerance"
        );
        let cell = cell_degrees.to_radians();
        let scans: Vec<&Scan> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let worlds: Vec<DMat4> = scans.iter().map(|s| self.world_matrix(s)).collect();
        let seeing: Vec<usize> = (0..scans.len())
            .filter(|&i| self.scanner_position(scans[i]).is_some())
            .collect();
        ensure!(!seeing.is_empty(), CoreError::NoScannerPositions);
        let sphere = Sphere::new(cell);
        let per_batch = self.filter_options.memory_bytes / 2 / (sphere.cells() * 4);
        ensure!(per_batch > 0, CoreError::FilterMemoryBudgetTooSmall);
        let batches: Vec<&[usize]> = seeing.chunks(per_batch).collect();
        let seeing_scans: Vec<&Scan> = seeing.iter().map(|&i| scans[i]).collect();
        let imaging = Progress::new(Stage::RangeImages, &seeing_scans, 1);
        let judging = Progress::new(Stage::MovingObjects, &scans, batches.len() as u64);
        // How many scans saw through each point so far, by scan and chunk,
        // over all batches; only chunks with such points.
        let mut passed: BTreeMap<(usize, u32), Vec<u8>> = BTreeMap::new();
        for batch in &batches {
            let mut images = vec![];
            for &si in *batch {
                let image =
                    self.range_image(scans[si], si, worlds[si].inverse(), sphere, job, &imaging)?;
                images.push(image);
            }
            let caches = ChunkCache::for_scans(self, &scans, true);
            each_chunk(
                &caches,
                job,
                &judging,
                &|si, chunk, job| {
                    let own = caches[si].get(chunk, job)?;
                    let others: Vec<&RangeImage> =
                        images.iter().filter(|image| image.scan != si).collect();
                    // Points and how many of these scans saw through them.
                    let mut found: Vec<(u32, u8)> = vec![];
                    if others.is_empty() {
                        return Ok(found);
                    }
                    for (n, (index, p)) in own.indices.iter().zip(&own.positions).enumerate() {
                        if n % 8192 == 0 {
                            job.check()?;
                        }
                        let p = worlds[si].transform_point3(*p);
                        let count = others.iter().filter(|i| i.passes(p, tolerance)).count();
                        if count > 0 {
                            found.push((*index, count.min(255) as u8));
                        }
                    }
                    Ok(found)
                },
                |si, chunk, found| {
                    if !found.is_empty() {
                        let points = scans[si].chunks[chunk as usize].count as usize;
                        let counts = passed.entry((si, chunk)).or_insert_with(|| vec![0; points]);
                        for (index, count) in found {
                            let slot = &mut counts[index as usize];
                            *slot = slot.saturating_add(count);
                        }
                    }
                    Ok(())
                },
            )?;
        }
        let masks: HashMap<(Uuid, u32), (Vec<u8>, u64)> = passed
            .into_iter()
            .map(|((si, chunk), counts)| {
                let mut mask = vec![0u8; counts.len().div_ceil(8)];
                let mut taken = 0;
                for (index, &count) in counts.iter().enumerate() {
                    if count as u32 >= min_scans {
                        mask[index / 8] |= 1 << (index % 8);
                        taken += 1;
                    }
                }
                ((scans[si].id, chunk), (mask, taken))
            })
            .collect();
        let mut chunks: Vec<(&Scan, u32)> = scans
            .iter()
            .flat_map(|s| (0..s.chunks.len() as u32).map(move |c| (*s, c)))
            .filter(|(s, c)| masks.get(&(s.id, *c)).is_some_and(|(_, n)| *n > 0))
            .collect();
        chunks.sort_by_key(|(s, c)| (s.id, *c));
        let mut labels = LabelWriter::new(self, target)?;
        labels.push_parallel(self, &chunks, Stage::MovingObjects, job, |scan, chunk| {
            Ok(masks[&(scan.id, chunk)].clone())
        })?;
        job.check()?;
        labels.commit(
            self,
            serde_json::json!({"kind": "moving_objects", "cell_degrees": cell_degrees,
                "tolerance": tolerance, "min_scans": min_scans,
                "scans": self.scans_record(scan_ids)}),
            |_| Ok(()),
            false,
        )
    }
    /// The range image of `scan` from its valid points in visible layers.
    fn range_image(
        &self,
        scan: &Scan,
        index: usize,
        from_project: DMat4,
        sphere: Sphere,
        job: &JobControl,
        progress: &Progress,
    ) -> Result<RangeImage> {
        let mut image = RangeImage::new(index, from_project, sphere);
        let chunks: Vec<u32> = (0..scan.chunks.len() as u32).collect();
        for_each_unordered(
            &chunks,
            filter_workers(self)?,
            job,
            |&chunk| {
                let data = self.read_chunk(scan, chunk)?;
                let hidden = self.hidden_mask(scan, chunk)?;
                let cells: Vec<(u32, f32)> = data
                    .chunks_exact(scan.stride)
                    .enumerate()
                    .filter(|(i, p)| valid(p) && !is_set(&hidden, *i))
                    .filter_map(|(_, p)| sphere.locate(DVec3::from(scan.coordinates.position(p))))
                    .map(|(cell, range)| (cell as u32, range as f32))
                    .collect();
                progress.step(job);
                Ok(cells)
            },
            |cells| {
                for (cell, range) in cells {
                    let slot = &mut image.ranges[cell as usize];
                    *slot = slot.min(range);
                }
                Ok(())
            },
        )?;
        image.erode();
        Ok(image)
    }
}
