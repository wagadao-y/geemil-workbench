use crate::layers::{LabelWriter, is_set};
use crate::storage::{position, valid};
use crate::{Bounds, JobControl, LayerTarget, Pose, Project, Sample, Scan, Stage, ViewCache};
use anyhow::{Result, ensure};
use glam::{DMat4, DVec3, DVec4};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, BinaryHeap};
use uuid::Uuid;

/// Samples of a view in the common project frame, with the transforms they
/// were placed with so a renderer can move them before the next load.
#[derive(Clone, Debug, Default)]
pub struct LoadedView {
    pub samples: Vec<Sample>,
    pub segments: Vec<ViewSegment>,
}
/// The samples of one scan within a [`LoadedView`].
#[derive(Clone, Debug)]
pub struct ViewSegment {
    pub scan: Uuid,
    pub range: std::ops::Range<usize>,
    /// [`Project::world_matrix`] of the scan when the view was loaded.
    pub world: DMat4,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Camera {
    pub target: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
    pub aspect: f64,
    pub fov: f64,
    /// Parallel projection. The visible height then stays what perspective
    /// shows at `target`, so `distance` still zooms.
    #[serde(default)]
    pub ortho: bool,
}
impl Default for Camera {
    fn default() -> Self {
        Self {
            target: [0.; 3],
            yaw: -1.2,
            pitch: 0.65,
            distance: 10.,
            aspect: 1.,
            fov: std::f64::consts::FRAC_PI_4,
            ortho: false,
        }
    }
}
impl Camera {
    pub fn eye(&self) -> DVec3 {
        DVec3::from(self.target)
            + DVec3::new(
                self.yaw.cos() * self.pitch.cos(),
                self.yaw.sin() * self.pitch.cos(),
                self.pitch.sin(),
            ) * self.distance
    }
    pub fn matrix(&self) -> DMat4 {
        self.relative_matrix() * DMat4::from_translation(-DVec3::from(self.target))
    }
    pub fn relative_matrix(&self) -> DMat4 {
        self.projection() * self.relative_view()
    }
    /// Right-handed with a 0..1 depth range, as wgpu expects.
    fn projection(&self) -> DMat4 {
        if self.ortho {
            let h = self.half_height();
            let w = h * self.aspect;
            // Parallel rays see behind the eye too, so the near plane is there.
            let reach = (self.distance * 1000.).max(100.);
            glam::dcamera::rh::proj::directx::orthographic(-w, w, -h, h, -reach, reach)
        } else {
            glam::dcamera::rh::proj::directx::perspective(
                self.fov,
                self.aspect,
                (self.distance * 1e-5).max(0.0001),
                (self.distance * 1000.).max(100.),
            )
        }
    }
    /// Half the visible height at `target`, in metres.
    pub fn half_height(&self) -> f64 {
        self.distance * (self.fov * 0.5).tan()
    }
    /// The view transform for coordinates relative to `target`.
    fn relative_view(&self) -> DMat4 {
        glam::dcamera::rh::view::look_at_mat4(
            self.eye() - DVec3::from(self.target),
            DVec3::ZERO,
            DVec3::Z,
        )
    }
    /// Coefficients giving the view depth (distance in front of the eye along
    /// the view axis) of a world point `p` as `row.dot(p.extend(1))`. Behind
    /// the eye it is negative, which only a parallel projection shows.
    pub fn depth_row(&self) -> DVec4 {
        let view = self.relative_view() * DMat4::from_translation(-DVec3::from(self.target));
        -view.row(2)
    }
    /// Projects to normalized viewport coordinates (origin top left) and view depth.
    pub fn project(&self, p: DVec3) -> Option<([f64; 2], f64)> {
        self.projector().project(p)
    }
    /// `project` with the matrices computed once, for many points.
    pub fn projector(&self) -> Projector {
        Projector {
            matrix: self.matrix(),
            depth: self.depth_row(),
        }
    }
    /// The same eye position, looking at and orbiting around `target`.
    pub fn looking_at(&self, target: DVec3) -> Self {
        // In parallel projection the distance is the zoom; keep it.
        if self.ortho {
            return Self {
                target: target.to_array(),
                ..*self
            };
        }
        let offset = self.eye() - target;
        let distance = offset.length();
        if distance <= f64::EPSILON {
            return *self;
        }
        Self {
            target: target.to_array(),
            yaw: offset.y.atan2(offset.x),
            pitch: (offset.z / distance).asin().clamp(-1.5, 1.5),
            distance,
            ..*self
        }
    }
    pub fn sees(&self, scan: &Scan, node: u32, world: DMat4) -> bool {
        let points: Vec<_> = scan.nodes[node as usize]
            .bounds
            .corners()
            .map(|p| self.matrix() * world * p.extend(1.))
            .collect();
        !(0..6).any(|plane| {
            points.iter().all(|p| match plane {
                0 => p.x < -p.w,
                1 => p.x > p.w,
                2 => p.y < -p.w,
                3 => p.y > p.w,
                4 => p.z < 0.,
                _ => p.z > p.w,
            })
        })
    }
}
pub struct Projector {
    matrix: DMat4,
    depth: DVec4,
}
impl Projector {
    /// Normalized viewport coordinates and view depth (see `Camera::depth_row`)
    /// of a point within the view volume.
    pub fn project(&self, p: DVec3) -> Option<([f64; 2], f64)> {
        let clip = self.matrix * p.extend(1.);
        if clip.w <= 0. || clip.z < 0. || clip.z > clip.w {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some((
            [(ndc.x + 1.) * 0.5, (1. - ndc.y) * 0.5],
            self.depth.dot(p.extend(1.)),
        ))
    }
}
/// Which side of the selection a range move takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    /// Points seen through the polygon, optionally only from the nearest one to
    /// `depth_meters` behind it: clearing noise in front of the camera while
    /// moving through a cloud.
    #[default]
    ExcludeInside,
    /// Everything not seen through the polygon, at any depth: cropping.
    ExcludeOutside,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    pub camera: Camera,
    pub polygon: Vec<[f64; 2]>,
    /// `ExcludeInside` only: the thickness behind the nearest selected point, or
    /// None for any depth (like CloudCompare's segmentation).
    pub depth_meters: Option<f64>,
    #[serde(default)]
    pub mode: SelectionMode,
}
impl Selection {
    /// View depth of `p` if it projects inside the polygon.
    pub fn contains(&self, p: DVec3) -> Option<f64> {
        self.prepare().contains(p)
    }
    /// Computes the camera matrices once, for testing many points.
    pub fn prepare(&self) -> PreparedSelection<'_> {
        let mut min = [f64::INFINITY; 2];
        let mut max = [f64::NEG_INFINITY; 2];
        for v in &self.polygon {
            for axis in 0..2 {
                min[axis] = min[axis].min(v[axis]);
                max[axis] = max[axis].max(v[axis]);
            }
        }
        PreparedSelection {
            polygon: &self.polygon,
            mode: self.mode,
            depth: self.depth_meters,
            matrix: self.camera.matrix(),
            depth_row: self.camera.depth_row(),
            ortho: self.camera.ortho,
            projector: self.camera.projector(),
            min,
            max,
        }
    }
}
pub struct PreparedSelection<'a> {
    polygon: &'a [[f64; 2]],
    mode: SelectionMode,
    depth: Option<f64>,
    matrix: DMat4,
    depth_row: DVec4,
    ortho: bool,
    projector: Projector,
    /// Polygon bounding box in normalized viewport coordinates.
    min: [f64; 2],
    max: [f64; 2],
}
impl PreparedSelection<'_> {
    /// Same result as `Selection::contains`.
    pub fn contains(&self, p: DVec3) -> Option<f64> {
        let (uv, depth) = self.projector.project(p)?;
        self.inside_polygon(uv).then_some(depth)
    }
    /// Whether `p` is seen through the polygon, without the near and far
    /// planes that limit `contains`: in front of the eye in perspective, at any
    /// depth in parallel projection (where w is always 1).
    pub fn covers(&self, p: DVec3) -> bool {
        let clip = self.matrix * p.extend(1.);
        if clip.w <= 0. {
            return false;
        }
        let ndc = clip.truncate() / clip.w;
        self.inside_polygon([(ndc.x + 1.) * 0.5, (1. - ndc.y) * 0.5])
    }
    /// Whether this selection needs the nearest selected depth as its base.
    pub fn depth_limited(&self) -> bool {
        self.mode == SelectionMode::ExcludeInside && self.depth.is_some()
    }
    /// Whether the selection takes `p`. When `depth_limited`, `limit` is the
    /// nearest selected depth plus `depth_meters` (see `Project::selection_nearest`);
    /// otherwise it is ignored.
    pub fn excludes(&self, p: DVec3, limit: f64) -> bool {
        match (self.mode, self.depth) {
            (SelectionMode::ExcludeInside, Some(_)) => {
                self.contains(p).is_some_and(|depth| depth <= limit)
            }
            (SelectionMode::ExcludeInside, None) => self.covers(p),
            (SelectionMode::ExcludeOutside, _) => !self.covers(p),
        }
    }
    fn inside_polygon(&self, uv: [f64; 2]) -> bool {
        let mut inside = false;
        for i in 0..self.polygon.len() {
            let a = self.polygon[i];
            let b = self.polygon[(i + 1) % self.polygon.len()];
            if (a[1] > uv[1]) != (b[1] > uv[1])
                && uv[0] < (b[0] - a[0]) * (uv[1] - a[1]) / (b[1] - a[1]) + a[0]
            {
                inside = !inside;
            }
        }
        inside
    }
    /// False only when no point in `bounds` (local coordinates, placed by `world`)
    /// can be selected by `contains` at a view depth of at most `max_depth`.
    pub fn may_contain(&self, bounds: &Bounds, world: DMat4, max_depth: f64) -> bool {
        self.may_hit(bounds, world, Some(max_depth))
    }
    /// False only when no point in `bounds` is covered (see `covers`).
    pub fn may_cover(&self, bounds: &Bounds, world: DMat4) -> bool {
        self.may_hit(bounds, world, None)
    }
    /// Conservative: the box is culled only if all corners lie outside one
    /// clip-space half-space. `depth` adds the near/far planes and a depth limit.
    fn may_hit(&self, bounds: &Bounds, world: DMat4, depth: Option<f64>) -> bool {
        // Keep rounding in the corner transforms from culling points on the faces.
        let margin = DVec3::splat(bounds.radius() * 1e-6 + 1e-9);
        let padded = Bounds {
            min: (DVec3::from(bounds.min) - margin).to_array(),
            max: (DVec3::from(bounds.max) + margin).to_array(),
        };
        let m = self.matrix * world;
        let depth_row = world.transpose() * self.depth_row;
        let corners: Vec<_> = padded
            .corners()
            .map(|p| (m * p.extend(1.), depth_row.dot(p.extend(1.))))
            .collect();
        // Viewport u = (x/w + 1) / 2 and v = (1 - y/w) / 2, so for w > 0 the
        // polygon box is k_min*w <= x <= k_max*w and l_min*w <= y <= l_max*w.
        let x = [2. * self.min[0] - 1., 2. * self.max[0] - 1.];
        let y = [1. - 2. * self.max[1], 1. - 2. * self.min[1]];
        let outside = |test: &dyn Fn(DVec4) -> bool| corners.iter().all(|(c, _)| test(*c));
        let beyond_depth = depth.is_some_and(|max_depth| {
            outside(&|c| c.z < 0.)
                || outside(&|c| c.z > c.w)
                || corners.iter().all(|(_, d)| *d > max_depth)
        });
        // In parallel projection w is 1 everywhere.
        !(beyond_depth
            || (!self.ortho && outside(&|c| c.w <= 0.))
            || outside(&|c| c.x < x[0] * c.w)
            || outside(&|c| c.x > x[1] * c.w)
            || outside(&|c| c.y < y[0] * c.w)
            || outside(&|c| c.y > y[1] * c.w))
    }
}
fn validate(selection: &Selection) -> Result<()> {
    ensure!(
        selection.polygon.len() >= 3 && selection.polygon.iter().flatten().all(|v| v.is_finite()),
        "Invalid selection polygon"
    );
    ensure!(
        selection
            .depth_meters
            .is_none_or(|depth| depth.is_finite() && depth > 0.),
        "Depth must be positive"
    );
    Ok(())
}
impl Project {
    /// The valid original points of a chunk in visible layers, in scan coordinates.
    pub fn points(&self, scan: &Scan, chunk: u32) -> Result<Vec<Sample>> {
        let data = self.read_chunk(scan, chunk)?;
        let hidden = self.hidden_mask(scan, chunk)?;
        Ok(data
            .chunks_exact(scan.stride)
            .enumerate()
            .filter(|(i, p)| valid(p) && !is_set(&hidden, *i))
            .map(|(i, p)| Sample {
                chunk,
                index: i as u32,
                position: position(p),
                color: crate::storage::point_color(p),
            })
            .collect())
    }
    /// Depth of the nearest visible original point inside the polygon among
    /// `scan_ids`, which `ExcludeInside` measures `depth_meters` from.
    pub fn selection_nearest(
        &self,
        selection: &Selection,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<Option<f64>> {
        validate(selection)?;
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let nearest = self
            .chunk_nearest(&selection.prepare(), &scans, job)?
            .into_iter()
            .flatten()
            .fold(f64::INFINITY, f64::min);
        Ok(nearest.is_finite().then_some(nearest))
    }
    /// Nearest selected depth per chunk; infinite where nothing is selected.
    fn chunk_nearest(
        &self,
        test: &PreparedSelection,
        scans: &[&Scan],
        job: &JobControl,
    ) -> Result<Vec<Vec<f64>>> {
        let mut result = vec![];
        for scan in scans {
            let world = self.world_matrix(scan);
            let mut depths = vec![f64::INFINITY; scan.chunks.len()];
            for (id, c) in scan.chunks.iter().enumerate() {
                job.check()?;
                job.report(
                    Stage::SelectionNearestDepth,
                    id as u64,
                    scan.chunks.len() as u64,
                );
                if !test.may_contain(&c.bounds, world, f64::INFINITY) {
                    continue;
                }
                let data = self.read_chunk(scan, id as u32)?;
                let hidden = self.hidden_mask(scan, id as u32)?;
                for (i, p) in data.chunks_exact(scan.stride).enumerate() {
                    if i % 8192 == 0 {
                        job.check()?;
                    }
                    if valid(p)
                        && !is_set(&hidden, i)
                        && let Some(depth) =
                            test.contains(world.transform_point3(DVec3::from(position(p))))
                    {
                        depths[id] = depths[id].min(depth);
                    }
                }
            }
            result.push(depths);
        }
        Ok(result)
    }
    /// Moves the visible original points of `scan_ids` the selection takes to
    /// `target`. Returns the number of moved points.
    pub fn move_selection(
        &mut self,
        selection: &Selection,
        scan_ids: &[Uuid],
        target: &LayerTarget,
        job: &JobControl,
    ) -> Result<u64> {
        validate(selection)?;
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let test = selection.prepare();
        // A depth-limited selection first finds the nearest visible ORIGINAL
        // point, independent of LOD. Chunks keep their own nearest depth so the
        // second pass can skip them. Without a depth there is no first pass.
        let (nearest, limit, chunk_nearest) = match (selection.mode, selection.depth_meters) {
            (SelectionMode::ExcludeInside, Some(depth_meters)) => {
                let depths = self.chunk_nearest(&test, &scans, job)?;
                let nearest = depths
                    .iter()
                    .flatten()
                    .copied()
                    .fold(f64::INFINITY, f64::min);
                if !nearest.is_finite() {
                    return Ok(0);
                }
                (Some(nearest), nearest + depth_meters, Some(depths))
            }
            _ => (None, f64::INFINITY, None),
        };
        let mut labels = LabelWriter::new(self, target)?;
        for (index, scan) in scans.iter().enumerate() {
            let world = self.world_matrix(scan);
            for (chunk, c) in scan.chunks.iter().enumerate() {
                job.check()?;
                job.report(Stage::SelectionMove, chunk as u64, scan.chunks.len() as u64);
                let skip = match (&chunk_nearest, selection.mode) {
                    (Some(depths), _) => depths[index][chunk] > limit,
                    (None, SelectionMode::ExcludeInside) => !test.may_cover(&c.bounds, world),
                    // A box inside a non-convex polygon may still have outside points.
                    (None, SelectionMode::ExcludeOutside) => false,
                };
                if skip {
                    continue;
                }
                let data = self.read_chunk(scan, chunk as u32)?;
                let hidden = self.hidden_mask(scan, chunk as u32)?;
                let mut mask = vec![0; (c.count as usize).div_ceil(8)];
                let mut count = 0;
                for (i, p) in data.chunks_exact(scan.stride).enumerate() {
                    if i % 8192 == 0 {
                        job.check()?;
                    }
                    if valid(p)
                        && !is_set(&hidden, i)
                        && test.excludes(world.transform_point3(DVec3::from(position(p))), limit)
                    {
                        mask[i / 8] |= 1 << (i % 8);
                        count += 1;
                    }
                }
                labels.push(self, scan, chunk as u32, &mask, count)?;
            }
        }
        job.check()?;
        labels.commit(
            self,
            serde_json::json!({"kind": "selection", "selection": selection, "scans": scan_ids,
                "nearest": nearest}),
            |_| Ok(()),
            false,
        )
    }
    /// Sets the additional transform of a scan or folder, relative to its folder.
    pub fn set_transform(&mut self, id: Uuid, pose: Pose) -> Result<()> {
        self.edit(
            serde_json::json!({"kind": "transform", "id": id, "pose": pose}),
            |s| {
                ensure!(
                    s.scans.contains(&id) || s.groups.iter().any(|g| g.id == id),
                    "Missing scan or folder"
                );
                if pose == Pose::default() {
                    s.transforms.remove(&id);
                } else {
                    s.transforms.insert(id, pose);
                }
                Ok(())
            },
        )
    }

    /// The fraction of each node's points (by index) in visible layers, from
    /// the per-chunk layer counts.
    fn surviving_fractions(&self, scan: &Scan) -> Vec<f64> {
        let excluded: Vec<_> = (0..scan.chunks.len() as u32)
            .map(|chunk| self.hidden_count(scan, chunk))
            .collect();
        // (surviving, total) points below each node.
        fn count(scan: &Scan, excluded: &[u64], node: usize, out: &mut [(f64, f64)]) -> (f64, f64) {
            let n = &scan.nodes[node];
            let mut sums = (0., 0.);
            if let Some(chunk) = n.chunk {
                let total = scan.chunks[chunk as usize].count as f64;
                let gone = (excluded[chunk as usize] as f64).min(total);
                sums = (total - gone, total);
            }
            for &child in &n.children {
                let (a, b) = count(scan, excluded, child as usize, out);
                sums.0 += a;
                sums.1 += b;
            }
            out[node] = sums;
            sums
        }
        let mut sums = vec![(0., 0.); scan.nodes.len()];
        if !scan.nodes.is_empty() {
            count(scan, &excluded, 0, &mut sums);
        }
        sums.iter()
            .map(|(s, t)| if *t > 0. { s / t } else { 1. })
            .collect()
    }
    pub fn load_view(
        &self,
        camera: &Camera,
        budget: usize,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<Vec<Sample>> {
        self.load_view_cached(camera, budget, scan_ids, job, &mut ViewCache::new(0))
            .map(|view| view.samples)
    }
    pub fn load_view_cached(
        &self,
        camera: &Camera,
        budget: usize,
        scan_ids: &[Uuid],
        job: &JobControl,
        cache: &mut ViewCache,
    ) -> Result<LoadedView> {
        cache.prepare(self);
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let worlds: Vec<_> = scans.iter().map(|s| self.world_matrix(s)).collect();
        // Budget only the samples in visible layers, so a heavily cropped or
        // thinned scan is shown from deeper, denser levels.
        let surviving: Vec<_> = scans.iter().map(|s| self.surviving_fractions(s)).collect();
        let lod = |si: usize, ni: u32| {
            let node = &scans[si].nodes[ni as usize];
            (node.lod_count as f64 * surviving[si][ni as usize]).ceil() as usize
        };
        let shown = |si: usize, ni: u32| surviving[si][ni as usize] > 0.;
        let eye = camera.eye();
        let score = |si: usize, ni: u32| {
            let node = &scans[si].nodes[ni as usize];
            let center = worlds[si].transform_point3(node.bounds.center());
            // On screen, a node's size goes with its distance in perspective and
            // with the fixed visible height in parallel projection.
            let distance = if camera.ortho {
                camera.distance
            } else {
                eye.distance(center)
            };
            let score = node.bounds.radius() / distance.max(0.0001);
            (!node.children.is_empty() && score > 0.015).then_some((score.to_bits(), si, ni))
        };
        let mut cut = BTreeSet::new();
        let mut queue = BinaryHeap::new();
        let mut cost = 0usize;
        for (si, scan) in scans.iter().enumerate() {
            if !scan.nodes.is_empty() && shown(si, 0) && camera.sees(scan, 0, worlds[si]) {
                cut.insert((si, 0u32));
                cost += lod(si, 0);
                if let Some(candidate) = score(si, 0) {
                    queue.push(candidate);
                }
            }
        }
        while let Some((_, si, ni)) = queue.pop() {
            job.report(Stage::ViewLod, 0, budget as u64);
            job.check()?;
            let scan = scans[si];
            let children: Vec<_> = scan.nodes[ni as usize]
                .children
                .iter()
                .copied()
                .filter(|id| shown(si, *id) && camera.sees(scan, *id, worlds[si]))
                .map(|id| (si, id))
                .collect();
            let next_cost =
                cost - lod(si, ni) + children.iter().map(|(si, ni)| lod(*si, *ni)).sum::<usize>();
            if next_cost > budget {
                continue;
            }
            cost = next_cost;
            cut.remove(&(si, ni));
            for (si, ni) in children {
                cut.insert((si, ni));
                if let Some(candidate) = score(si, ni) {
                    queue.push(candidate);
                }
            }
        }
        let mut result = Vec::with_capacity(budget.min(2_000_000));
        let mut segments: Vec<ViewSegment> = vec![];
        let mut remaining_lod: usize = cut.iter().map(|(si, ni)| lod(*si, *ni)).sum();
        for (si, ni) in cut {
            job.report(Stage::ViewPoints, result.len() as u64, budget as u64);
            job.check()?;
            let scan = scans[si];
            let node = &scan.nodes[ni as usize];
            let node_lod = lod(si, ni);
            remaining_lod = remaining_lod.saturating_sub(node_lod);
            // Reserve representative points for the rest of the view. Expanding one
            // nearby leaf must not consume the budget of other nodes/scans.
            let available = budget.saturating_sub(result.len());
            let quota = if remaining_lod + node_lod > available {
                available.saturating_mul(node_lod) / (remaining_lod + node_lod).max(1)
            } else {
                available.saturating_sub(remaining_lod)
            };
            let survivors = node.point_count as f64 * surviving[si][ni as usize];
            let full = node.chunk.is_some() && survivors <= quota as f64;
            let samples = cache.samples(self, scan, ni, full, job)?;
            let sample_count = samples.len();
            // `cut` is ordered by scan, so each scan's samples are contiguous.
            if segments.last().is_none_or(|s| s.scan != scan.id) {
                segments.push(ViewSegment {
                    scan: scan.id,
                    range: result.len()..result.len(),
                    world: worlds[si],
                });
            }
            let world = worlds[si];
            let to_world = |s: &Sample| Sample {
                position: world.transform_point3(DVec3::from(s.position)).to_array(),
                ..*s
            };
            if quota >= sample_count {
                result.extend(samples.iter().map(to_world));
            } else {
                for (i, s) in samples.iter().enumerate() {
                    // Spread a reduced quota over the whole node instead of
                    // cropping a prefix, which would introduce a spatial bias.
                    if (i + 1) * quota / sample_count != i * quota / sample_count {
                        result.push(to_world(s));
                    }
                }
            }
            segments.last_mut().unwrap().range.end = result.len();
        }
        Ok(LoadedView {
            samples: result,
            segments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Camera, Selection, SelectionMode};
    use crate::Bounds;
    use glam::{DMat4, DQuat, DVec3};

    #[test]
    fn looking_at_keeps_the_eye_and_centres_the_target() {
        let camera = Camera {
            target: [10., -4., 2.],
            yaw: 0.7,
            pitch: 0.3,
            distance: 25.,
            ..Camera::default()
        };
        let target = DVec3::new(3., 5., -1.);
        let moved = camera.looking_at(target);
        assert!(moved.eye().distance(camera.eye()) < 1e-9);
        assert!((moved.distance - camera.eye().distance(target)).abs() < 1e-9);
        let (uv, depth) = moved.project(target).unwrap();
        assert!((uv[0] - 0.5).abs() < 1e-9 && (uv[1] - 0.5).abs() < 1e-9);
        assert!((depth - moved.distance).abs() < 1e-6);
        // Looking at the eye itself has no direction; keep the camera unchanged.
        assert_eq!(camera.looking_at(camera.eye()), camera);
    }

    #[test]
    fn parallel_projection_keeps_size_and_sees_behind_the_eye() {
        let camera = Camera {
            target: [10., 20., 5.],
            yaw: 0.3,
            pitch: 0.4,
            distance: 30.,
            aspect: 1.5,
            ortho: true,
            ..Camera::default()
        };
        let target = DVec3::from(camera.target);
        let toward_eye = (camera.eye() - target).normalize();
        // Points along the view axis land on the centre, whatever their depth,
        // with depth measured from the eye.
        for t in [-100., 0., 29., 45.] {
            let (uv, depth) = camera.project(target + toward_eye * t).unwrap();
            assert!((uv[0] - 0.5).abs() < 1e-9 && (uv[1] - 0.5).abs() < 1e-9);
            assert!((depth - (camera.distance - t)).abs() < 1e-6);
        }
        // The visible height at any depth is what perspective shows at the target.
        let up = toward_eye.cross(DVec3::Z).cross(toward_eye).normalize();
        let perspective = Camera {
            ortho: false,
            ..camera
        };
        for t in [-50., 0., 20.] {
            let p = target + toward_eye * t + up * camera.half_height() * 0.5;
            let (uv, _) = camera.project(p).unwrap();
            assert!((uv[1] - 0.25).abs() < 1e-9, "{uv:?}");
        }
        let (uv, _) = perspective
            .project(target + up * camera.half_height() * 0.5)
            .unwrap();
        assert!((uv[1] - 0.25).abs() < 1e-9);
        // Retargeting keeps the zoom.
        let moved = camera.looking_at(DVec3::new(0., 0., 0.));
        assert_eq!(moved.distance, camera.distance);
    }

    #[test]
    fn chunk_culling_never_skips_a_selectable_or_covered_point() {
        for ortho in [false, true] {
            culling_never_skips(ortho);
        }
    }

    fn culling_never_skips(ortho: bool) {
        // Deterministic pseudo-random values in [0, 1).
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let selection = Selection {
            camera: Camera {
                target: [1., 2., 0.5],
                yaw: 0.4,
                pitch: 0.5,
                distance: 12.,
                aspect: 1.5,
                ortho,
                ..Camera::default()
            },
            polygon: vec![[0.42, 0.40], [0.61, 0.44], [0.55, 0.63], [0.40, 0.58]],
            depth_meters: Some(1.),
            mode: SelectionMode::ExcludeInside,
        };
        let test = selection.prepare();
        let world = DMat4::from_rotation_translation(
            DQuat::from_rotation_z(0.3),
            DVec3::new(-2., 1., 0.25),
        );
        let (mut kept, mut culled, mut selectable, mut covered_samples) = (0, 0, 0, 0);
        for _ in 0..4000 {
            let center = DVec3::new(next(), next(), next()) * 16. - 8.;
            let half = DVec3::new(next(), next(), next()) * 1.5 + DVec3::splat(0.01);
            let bounds = Bounds {
                min: (center - half).to_array(),
                max: (center + half).to_array(),
            };
            let max_depth = 6. + next() * 10.;
            let may_contain = test.may_contain(&bounds, world, max_depth);
            let may_cover = test.may_cover(&bounds, world);
            if may_contain {
                kept += 1;
            } else {
                culled += 1;
            }
            // Corners, face points and interior samples of every box.
            for _ in 0..64 {
                let t = DVec3::new(next(), next(), next()).map(|v| (v * 3.).floor() / 2.);
                let r = DVec3::new(next(), next(), next());
                for f in [t, r] {
                    let local = center - half + 2. * half * f;
                    let selected = test
                        .contains(world.transform_point3(local))
                        .is_some_and(|depth| depth <= max_depth);
                    assert!(!selected || may_contain, "culled a selectable point");
                    selectable += selected as u32;
                    let covered = test.covers(world.transform_point3(local));
                    assert!(!covered || may_cover, "culled a covered point");
                    covered_samples += covered as u32;
                }
            }
        }
        assert!(selectable > 100, "only {selectable} selectable samples");
        assert!(covered_samples > selectable, "{covered_samples} covered");
        // The cull must actually skip most boxes away from the small polygon.
        assert!(culled > kept, "kept {kept}, culled {culled}");
    }
}
