use crate::storage::{position, valid};
use crate::{Bounds, ChunkMask, JobControl, Layer, Pose, Project, Sample, Scan, Stage, ViewCache};
use anyhow::{Result, ensure};
use glam::{DMat4, DVec3, DVec4};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, BinaryHeap},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Camera {
    pub target: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
    pub aspect: f64,
    pub fov: f64,
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
        DMat4::perspective_rh(
            self.fov,
            self.aspect,
            (self.distance * 1e-5).max(0.0001),
            (self.distance * 1000.).max(100.),
        ) * DMat4::look_at_rh(self.eye() - DVec3::from(self.target), DVec3::ZERO, DVec3::Z)
    }
    /// Projects to normalized viewport coordinates (origin top left) and view depth.
    pub fn project(&self, p: DVec3) -> Option<([f64; 2], f64)> {
        self.projector().project(p)
    }
    /// `project` with the matrices computed once, for many points.
    pub fn projector(&self) -> Projector {
        Projector {
            matrix: self.matrix(),
        }
    }
    /// The same eye position, looking at and orbiting around `target`.
    pub fn looking_at(&self, target: DVec3) -> Self {
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
}
impl Projector {
    pub fn project(&self, p: DVec3) -> Option<([f64; 2], f64)> {
        let clip = self.matrix * p.extend(1.);
        if clip.w <= 0. || clip.z < 0. || clip.z > clip.w {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(([(ndc.x + 1.) * 0.5, (1. - ndc.y) * 0.5], clip.w))
    }
}
/// Which side of the selection a manual exclusion removes.
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
    /// Whether `p` is in front of the camera and seen through the polygon,
    /// without the near and far planes that limit `contains`.
    pub fn covers(&self, p: DVec3) -> bool {
        let clip = self.matrix * p.extend(1.);
        if clip.w <= 0. {
            return false;
        }
        let ndc = clip.truncate() / clip.w;
        self.inside_polygon([(ndc.x + 1.) * 0.5, (1. - ndc.y) * 0.5])
    }
    /// Whether this exclusion needs the nearest selected depth as its base.
    pub fn depth_limited(&self) -> bool {
        self.mode == SelectionMode::ExcludeInside && self.depth.is_some()
    }
    /// Whether an exclusion removes `p`. When `depth_limited`, `limit` is the
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
        let corners: Vec<_> = padded.corners().map(|p| m * p.extend(1.)).collect();
        // Viewport u = (x/w + 1) / 2 and v = (1 - y/w) / 2, so for w > 0 the
        // polygon box is k_min*w <= x <= k_max*w and l_min*w <= y <= l_max*w.
        let x = [2. * self.min[0] - 1., 2. * self.max[0] - 1.];
        let y = [1. - 2. * self.max[1], 1. - 2. * self.min[1]];
        let outside = |test: &dyn Fn(DVec4) -> bool| corners.iter().all(|c| test(*c));
        let beyond_depth = depth.is_some_and(|max_depth| {
            outside(&|c| c.z < 0.) || outside(&|c| c.z > c.w) || outside(&|c| c.w > max_depth)
        });
        !(beyond_depth
            || outside(&|c| c.w <= 0.)
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
pub(crate) fn is_excluded(mask: &[u8], i: usize) -> bool {
    mask[i / 8] & (1 << (i % 8)) != 0
}

impl Project {
    pub fn exclusion_mask(&self, scan: &Scan, chunk: u32) -> Result<Vec<u8>> {
        let count = scan
            .chunks
            .get(chunk as usize)
            .ok_or_else(|| anyhow::anyhow!("Invalid chunk"))?
            .count as usize;
        let mut mask = vec![0; count.div_ceil(8)];
        for layer in self
            .manifest
            .layers
            .iter()
            .filter(|l| self.current().layers.contains(&l.id))
        {
            for entry in layer
                .masks
                .iter()
                .filter(|m| m.scan == scan.id && m.chunk == chunk)
            {
                ensure!(entry.bytes as usize == mask.len(), "Invalid mask length");
                let mut f = File::open(self.path(&layer.mask_file)?)?;
                ensure!(
                    entry.offset + entry.bytes as u64 <= f.metadata()?.len(),
                    "Truncated mask"
                );
                f.seek(SeekFrom::Start(entry.offset))?;
                let mut bytes = vec![0; mask.len()];
                f.read_exact(&mut bytes)?;
                for (a, b) in mask.iter_mut().zip(bytes) {
                    *a |= b;
                }
            }
        }
        Ok(mask)
    }
    /// Depth of the nearest surviving original point inside the polygon among
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
                let mask = self.exclusion_mask(scan, id as u32)?;
                for (i, p) in data.chunks_exact(scan.stride).enumerate() {
                    if i % 8192 == 0 {
                        job.check()?;
                    }
                    if valid(p)
                        && !is_excluded(&mask, i)
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
    pub fn delete_selection(
        &mut self,
        selection: &Selection,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<u64> {
        validate(selection)?;
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let test = selection.prepare();
        // A depth-limited exclusion first finds the nearest surviving ORIGINAL
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
        let id = Uuid::new_v4();
        let relative = format!("layers/{id}.mask");
        let tmp = self.root.join("staging").join(format!("{id}.mask"));
        let mut file = File::create(&tmp)?;
        let mut masks = vec![];
        let mut offset = 0;
        let mut total = 0;
        for (index, scan) in scans.iter().enumerate() {
            let world = self.world_matrix(scan);
            for (chunk, c) in scan.chunks.iter().enumerate() {
                job.check()?;
                job.report(
                    Stage::SelectionExclusionMask,
                    chunk as u64,
                    scan.chunks.len() as u64,
                );
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
                let old = self.exclusion_mask(scan, chunk as u32)?;
                let mut mask = vec![0; (c.count as usize).div_ceil(8)];
                let mut count = 0;
                for (i, p) in data.chunks_exact(scan.stride).enumerate() {
                    if i % 8192 == 0 {
                        job.check()?;
                    }
                    if valid(p)
                        && !is_excluded(&old, i)
                        && test.excludes(world.transform_point3(DVec3::from(position(p))), limit)
                    {
                        mask[i / 8] |= 1 << (i % 8);
                        count += 1;
                    }
                }
                if count > 0 {
                    file.write_all(&mask)?;
                    masks.push(ChunkMask {
                        scan: scan.id,
                        chunk: chunk as u32,
                        offset,
                        bytes: mask.len() as u32,
                        excluded: count,
                    });
                    offset += mask.len() as u64;
                    total += count;
                }
            }
        }
        file.sync_all()?;
        drop(file);
        job.check()?;
        if total == 0 {
            fs::remove_file(tmp)?;
            return Ok(0);
        }
        fs::rename(tmp, self.path(&relative)?)?;
        let mut next = self.clone();
        next.manifest.layers.push(Layer {
            id,
            name: format!("Manual exclusion ({total} points)"),
            mask_file: relative,
            masks,
            excluded: total,
        });
        let mut layers = next.current().layers.clone();
        layers.push(id);
        let transforms = next.current().transforms.clone();
        next.commit(format!("Exclude {total} points"),serde_json::json!({"kind":"selection","selection":selection,"scans":scan_ids,"nearest":nearest}),layers,transforms)?;
        *self = next;
        Ok(total)
    }
    pub fn set_layer_enabled(&mut self, id: Uuid, enabled: bool) -> Result<()> {
        ensure!(
            self.manifest.layers.iter().any(|l| l.id == id),
            "Missing layer"
        );
        let mut layers = self.current().layers.clone();
        layers.retain(|v| *v != id);
        if enabled {
            layers.push(id);
        }
        let transforms = self.current().transforms.clone();
        self.commit(
            if enabled {
                "Enable layer"
            } else {
                "Disable layer"
            }
            .into(),
            serde_json::json!({"kind":"layer","id":id,"enabled":enabled}),
            layers,
            transforms,
        )?;
        Ok(())
    }
    pub fn set_transform(&mut self, id: Uuid, pose: Pose) -> Result<()> {
        ensure!(self.current().scans.contains(&id), "Missing scan");
        let mut transforms = self.current().transforms.clone();
        transforms.insert(id, pose);
        self.commit(
            "Manual transform".into(),
            serde_json::json!({"kind":"transform","scan":id,"pose":pose}),
            self.current().layers.clone(),
            transforms,
        )?;
        Ok(())
    }
    pub fn fork(&mut self, name: String) -> Result<()> {
        self.commit(
            name,
            serde_json::json!({"kind":"fork"}),
            self.current().layers.clone(),
            self.current().transforms.clone(),
        )?;
        Ok(())
    }
    /// Keep other branches and layers; compact only the navigation ancestry of a new checkpoint.
    pub fn checkpoint(&mut self, name: String) -> Result<()> {
        let mut r = self.current().clone();
        r.id = Uuid::new_v4();
        r.parent = None;
        r.name = name;
        r.operation = serde_json::json!({"kind":"checkpoint","from":self.manifest.current});
        self.commit_snapshot(r)?;
        Ok(())
    }

    pub fn load_view(
        &self,
        camera: &Camera,
        budget: usize,
        scan_ids: &[Uuid],
        job: &JobControl,
    ) -> Result<Vec<Sample>> {
        self.load_view_cached(camera, budget, scan_ids, job, &mut ViewCache::new(0))
    }
    pub fn load_view_cached(
        &self,
        camera: &Camera,
        budget: usize,
        scan_ids: &[Uuid],
        job: &JobControl,
        cache: &mut ViewCache,
    ) -> Result<Vec<Sample>> {
        cache.prepare(self);
        let scans: Vec<_> = self.scans().filter(|s| scan_ids.contains(&s.id)).collect();
        let worlds: Vec<_> = scans.iter().map(|s| self.world_matrix(s)).collect();
        let eye = camera.eye();
        let score = |si: usize, ni: u32| {
            let node = &scans[si].nodes[ni as usize];
            let center = worlds[si].transform_point3(node.bounds.center());
            let score = node.bounds.radius() / eye.distance(center).max(0.0001);
            (!node.children.is_empty() && score > 0.015).then_some((score.to_bits(), si, ni))
        };
        let mut cut = BTreeSet::new();
        let mut queue = BinaryHeap::new();
        let mut cost = 0usize;
        for (si, scan) in scans.iter().enumerate() {
            if !scan.nodes.is_empty() && camera.sees(scan, 0, worlds[si]) {
                cut.insert((si, 0u32));
                cost += scan.nodes[0].lod_count as usize;
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
                .filter(|id| camera.sees(scan, *id, worlds[si]))
                .map(|id| (si, id))
                .collect();
            let next_cost = cost - scan.nodes[ni as usize].lod_count as usize
                + children
                    .iter()
                    .map(|(_, ni)| scan.nodes[*ni as usize].lod_count as usize)
                    .sum::<usize>();
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
        let mut remaining_lod: usize = cut
            .iter()
            .map(|(si, ni)| scans[*si].nodes[*ni as usize].lod_count as usize)
            .sum();
        for (si, ni) in cut {
            job.report(Stage::ViewPoints, result.len() as u64, budget as u64);
            job.check()?;
            let scan = scans[si];
            let node = &scan.nodes[ni as usize];
            remaining_lod = remaining_lod.saturating_sub(node.lod_count as usize);
            // Reserve representative points for the rest of the view. Expanding one
            // nearby leaf must not consume the budget of other nodes/scans.
            let available = budget.saturating_sub(result.len());
            let quota = if remaining_lod + node.lod_count as usize > available {
                available.saturating_mul(node.lod_count as usize)
                    / (remaining_lod + node.lod_count as usize).max(1)
            } else {
                available.saturating_sub(remaining_lod)
            };
            let full = node.chunk.is_some() && node.point_count as usize <= quota;
            let samples = cache.samples(self, scan, ni, full, job)?;
            let sample_count = samples.len();
            if quota >= sample_count {
                result.extend_from_slice(&samples);
                continue;
            }
            for (i, s) in samples.iter().enumerate() {
                // Spread a reduced quota over the whole node instead of cropping
                // a prefix, which would introduce a spatial bias.
                if quota < sample_count
                    && (i + 1) * quota / sample_count == i * quota / sample_count
                {
                    continue;
                }
                result.push(s.clone());
            }
        }
        Ok(result)
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
    fn chunk_culling_never_skips_a_selectable_or_covered_point() {
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
