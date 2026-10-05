//! Rigid registration: a least-squares fit to picked point pairs, and ICP
//! (point-to-plane, with a shrinking correspondence distance) on processing
//! samples drawn from the LOD, so neither needs the whole scan in memory.
//!
//! Both produce a motion in the common project frame. [`Project::moved_pose`]
//! turns it into the own transform of the moved scan or folder.
use crate::layers::is_set;
use crate::{Bounds, JobControl, Pose, Project, Scan, Stage};
use anyhow::{Result, bail, ensure};
use glam::{DMat4, DQuat, DVec3};
use std::collections::HashMap;
use uuid::Uuid;

/// Eigenvalues (ascending) and the matching unit eigenvectors (columns of the
/// returned matrix, `v[k][i]` is component `i` of vector `k`) of a symmetric
/// matrix, by cyclic Jacobi rotations.
fn symmetric_eigen<const N: usize>(mut a: [[f64; N]; N]) -> ([f64; N], [[f64; N]; N]) {
    let mut v = [[0.; N]; N];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.;
    }
    for _ in 0..64 {
        let off: f64 = (0..N)
            .flat_map(|i| (0..N).filter(move |&j| j != i).map(move |j| (i, j)))
            .map(|(i, j)| a[i][j] * a[i][j])
            .sum();
        if off < 1e-30 {
            break;
        }
        for p in 0..N {
            for q in p + 1..N {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2. * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.).sqrt());
                let t = if theta == 0. { 1. } else { t };
                let c = 1. / (t * t + 1.).sqrt();
                let s = t * c;
                for row in a.iter_mut() {
                    let (akp, akq) = (row[p], row[q]);
                    row[p] = c * akp - s * akq;
                    row[q] = s * akp + c * akq;
                }
                let (rp, rq) = (a[p], a[q]);
                for k in 0..N {
                    a[p][k] = c * rp[k] - s * rq[k];
                    a[q][k] = s * rp[k] + c * rq[k];
                }
                for row in v.iter_mut() {
                    let (vp, vq) = (row[p], row[q]);
                    row[p] = c * vp - s * vq;
                    row[q] = s * vp + c * vq;
                }
            }
        }
    }
    let mut order: [usize; N] = std::array::from_fn(|i| i);
    order.sort_by(|&i, &j| a[i][i].total_cmp(&a[j][j]));
    (
        order.map(|i| a[i][i]),
        order.map(|k| std::array::from_fn(|i| v[i][k])),
    )
}

/// The rigid motion that best maps `moving[i]` onto `reference[i]` in the
/// least-squares sense (Horn's quaternion method). Needs three or more pairs
/// that are not on one line.
pub fn rigid_fit(moving: &[DVec3], reference: &[DVec3]) -> Result<DMat4> {
    ensure!(
        moving.len() == reference.len() && moving.len() >= 3,
        "At least three point pairs are needed"
    );
    let n = moving.len() as f64;
    let cm = moving.iter().sum::<DVec3>() / n;
    let cr = reference.iter().sum::<DVec3>() / n;
    let mut s = [[0.; 3]; 3];
    let mut spread = [[0.; 3]; 3];
    for (m, r) in moving.iter().zip(reference) {
        let (m, r) = ((*m - cm).to_array(), (*r - cr).to_array());
        for i in 0..3 {
            for j in 0..3 {
                s[i][j] += m[i] * r[j];
                spread[i][j] += m[i] * m[j];
            }
        }
    }
    // Collinear points leave the rotation about their line undetermined.
    let (shape, _) = symmetric_eigen(spread);
    ensure!(
        shape[1] > 1e-10 * shape[2].max(1e-300),
        "Point pairs are on one line"
    );
    let [[xx, xy, xz], [yx, yy, yz], [zx, zy, zz]] = s;
    let k = [
        [xx + yy + zz, yz - zy, zx - xz, xy - yx],
        [yz - zy, xx - yy - zz, xy + yx, zx + xz],
        [zx - xz, xy + yx, -xx + yy - zz, yz + zy],
        [xy - yx, zx + xz, yz + zy, -xx - yy + zz],
    ];
    let (_, vectors) = symmetric_eigen(k);
    let [w, x, y, z] = vectors[3];
    let rotation = DQuat::from_xyzw(x, y, z, w).normalize();
    let translation = cr - rotation * cm;
    Ok(DMat4::from_rotation_translation(rotation, translation))
}

#[derive(Clone, Copy, Debug)]
pub struct IcpOptions {
    /// Initial correspondence distance in metres; it halves each time ICP
    /// settles, so this should cover the remaining misalignment.
    pub max_distance: f64,
    /// The distance it narrows to, in metres; at most `max_distance`.
    pub min_distance: f64,
    /// Iterations allowed at each distance.
    pub iterations: u32,
    /// Processing samples of the moved scans; the reference gets four times as
    /// many within reach of them.
    pub samples: usize,
}
impl Default for IcpOptions {
    fn default() -> Self {
        Self {
            max_distance: 0.5,
            min_distance: 0.02,
            iterations: 50,
            samples: 60_000,
        }
    }
}
/// The fit at one correspondence distance.
#[derive(Clone, Copy, Debug)]
pub struct IcpStep {
    pub distance: f64,
    /// RMS distance of the correspondences to the reference surface, in metres.
    pub rms: f64,
    /// Fraction of moving samples with a correspondence at this distance.
    pub overlap: f64,
    pub iterations: u32,
}
#[derive(Clone, Debug)]
pub struct IcpResult {
    /// The own transform of the moved item after alignment.
    pub pose: Pose,
    /// RMS of the narrowest distance reached, in metres.
    pub rms: f64,
    /// Fraction of moving samples within the initial distance of the
    /// reference after alignment: how much of the item overlaps it.
    pub overlap: f64,
    /// Iterations over all distances.
    pub iterations: u32,
    /// The distances from the initial one down, as far as they held.
    pub steps: Vec<IcpStep>,
    /// A narrower distance that fitted too little to trust; the result is that
    /// of the one before it.
    pub stopped_at: Option<f64>,
}

/// A uniform grid over points for fixed-radius neighbour queries.
struct Grid {
    cell: f64,
    cells: HashMap<[i64; 3], Vec<u32>>,
}
impl Grid {
    fn new(points: &[DVec3], cell: f64) -> Self {
        let mut cells: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            cells.entry(Self::key(*p, cell)).or_default().push(i as u32);
        }
        Self { cell, cells }
    }
    fn key(p: DVec3, cell: f64) -> [i64; 3] {
        (p / cell).floor().as_i64vec3().to_array()
    }
    /// Points within `radius` (at most the cell size) of `p`.
    fn near<'a>(
        &'a self,
        points: &'a [DVec3],
        p: DVec3,
        radius: f64,
    ) -> impl Iterator<Item = (u32, f64)> + 'a {
        let [x, y, z] = Self::key(p, self.cell);
        let r2 = radius * radius;
        (-1..=1)
            .flat_map(move |dx| (-1..=1).flat_map(move |dy| (-1..=1).map(move |dz| [dx, dy, dz])))
            .filter_map(move |[dx, dy, dz]| self.cells.get(&[x + dx, y + dy, z + dz]))
            .flatten()
            .filter_map(move |&i| {
                let d2 = points[i as usize].distance_squared(p);
                (d2 <= r2).then_some((i, d2))
            })
    }
}

/// Splits `0..n` over worker threads and concatenates their results in order.
fn parallel<T: Send>(n: usize, work: impl Fn(std::ops::Range<usize>) -> Vec<T> + Sync) -> Vec<T> {
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    let size = n.div_ceil(workers).max(1024);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .step_by(size)
            .map(|start| {
                let work = &work;
                scope.spawn(move || work(start..(start + size).min(n)))
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("worker panicked"))
            .collect()
    })
}

/// Surface normals by PCA of the neighbours within `radius`; `None` where too
/// few neighbours or no clear plane.
fn normals(points: &[DVec3], radius: f64) -> Vec<Option<DVec3>> {
    let grid = Grid::new(points, radius);
    parallel(points.len(), |range| {
        range
            .map(|i| {
                let p = points[i];
                let near: Vec<_> = grid.near(points, p, radius).collect();
                if near.len() < 5 {
                    return None;
                }
                let mean = near.iter().map(|(j, _)| points[*j as usize]).sum::<DVec3>()
                    / near.len() as f64;
                let mut c = [[0.; 3]; 3];
                for (j, _) in &near {
                    let d = (points[*j as usize] - mean).to_array();
                    for a in 0..3 {
                        for b in 0..3 {
                            c[a][b] += d[a] * d[b];
                        }
                    }
                }
                let (values, vectors) = symmetric_eigen(c);
                // A plane has one clearly smallest spread.
                (values[0] < 0.3 * values[1]).then(|| DVec3::from(vectors[0]))
            })
            .collect()
    })
}

/// Solves the symmetric positive system `a x = b` by Cholesky; `None` when
/// the motion is not determined (e.g. a single plane).
fn solve6(a: [[f64; 6]; 6], b: [f64; 6]) -> Option<[f64; 6]> {
    let mut l = [[0.; 6]; 6];
    let scale = (0..6).map(|i| a[i][i]).fold(0., f64::max).max(1e-300);
    for i in 0..6 {
        for j in 0..=i {
            let s = a[i][j] - (0..j).map(|k| l[i][k] * l[j][k]).sum::<f64>();
            if i == j {
                if s <= 1e-12 * scale {
                    return None;
                }
                l[i][i] = s.sqrt();
            } else {
                l[i][j] = s / l[j][j];
            }
        }
    }
    let mut y = [0.; 6];
    for i in 0..6 {
        y[i] = (b[i] - (0..i).map(|k| l[i][k] * y[k]).sum::<f64>()) / l[i][i];
    }
    let mut x = [0.; 6];
    for i in (0..6).rev() {
        x[i] = (y[i] - (i + 1..6).map(|k| l[k][i] * x[k]).sum::<f64>()) / l[i][i];
    }
    Some(x)
}

/// Samples of a surface with their normals, in a kd-tree for nearest-point
/// queries: each subslice is a subtree whose middle element splits it on the
/// axis stored with it.
struct Surface {
    points: Vec<DVec3>,
    normals: Vec<Option<DVec3>>,
    axes: Vec<u8>,
}
impl Surface {
    fn new(points: Vec<DVec3>, normal_radius: f64) -> Self {
        let normals = normals(&points, normal_radius);
        let mut items: Vec<(DVec3, Option<DVec3>)> = points.into_iter().zip(normals).collect();
        let mut axes = vec![0u8; items.len()];
        fn build(items: &mut [(DVec3, Option<DVec3>)], axes: &mut [u8]) {
            if items.len() <= 1 {
                return;
            }
            let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
            for (p, _) in items.iter() {
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
            let mid = items.len() / 2;
            items.select_nth_unstable_by(mid, |a, b| a.0[axis].total_cmp(&b.0[axis]));
            axes[mid] = axis as u8;
            let (left, right) = items.split_at_mut(mid);
            let (left_axes, right_axes) = axes.split_at_mut(mid);
            build(left, left_axes);
            build(&mut right[1..], &mut right_axes[1..]);
        }
        build(&mut items, &mut axes);
        let (points, normals) = items.into_iter().unzip();
        Self {
            points,
            normals,
            axes,
        }
    }
    /// The sample nearest `p` within `radius` and its squared distance.
    fn nearest(&self, p: DVec3, radius: f64) -> Option<(u32, f64)> {
        fn visit(tree: &Surface, range: std::ops::Range<usize>, p: DVec3, best: &mut (u32, f64)) {
            if range.is_empty() {
                return;
            }
            let mid = range.start + range.len() / 2;
            let q = tree.points[mid];
            let d2 = q.distance_squared(p);
            if d2 < best.1 {
                *best = (mid as u32, d2);
            }
            let axis = tree.axes[mid] as usize;
            let diff = p[axis] - q[axis];
            let (near, far) = if diff < 0. {
                (range.start..mid, mid + 1..range.end)
            } else {
                (mid + 1..range.end, range.start..mid)
            };
            visit(tree, near, p, best);
            if diff * diff < best.1 {
                visit(tree, far, p, best);
            }
        }
        // Only points strictly within reach of the limit replace it.
        let mut best = (u32::MAX, radius * radius * (1. + 1e-12));
        visit(self, 0..self.points.len(), p, &mut best);
        (best.0 != u32::MAX).then_some(best)
    }
    /// The sample nearest each moved point within `distance`, paired with the
    /// moved point.
    fn pairs(&self, moving: &[DVec3], motion: DMat4, distance: f64) -> Vec<(DVec3, u32)> {
        parallel(moving.len(), |range| {
            range
                .filter_map(|i| {
                    let p = motion.transform_point3(moving[i]);
                    let (j, _) = self.nearest(p, distance)?;
                    Some((p, j))
                })
                .collect()
        })
    }
    /// One point-to-plane step from `pairs`: the motion to apply after the
    /// current one.
    fn step(&self, pairs: &[(DVec3, u32)], distance: f64) -> Result<DMat4> {
        // Robust weights: full inside a third of the distance, then 1/r.
        let huber = distance / 3.;
        let mut a = [[0.; 6]; 6];
        let mut b = [0.; 6];
        let mut add = |row: [f64; 6], r: f64| {
            let w = if r.abs() <= huber {
                1.
            } else {
                huber / r.abs()
            };
            for i in 0..6 {
                for j in 0..6 {
                    a[i][j] += w * row[i] * row[j];
                }
                b[i] += w * row[i] * r;
            }
        };
        for (p, j) in pairs {
            let q = self.points[*j as usize];
            match self.normals[*j as usize] {
                Some(n) => {
                    let c = p.cross(n);
                    add([c.x, c.y, c.z, n.x, n.y, n.z], (q - *p).dot(n));
                }
                // Point-to-point rows where the reference has no plane.
                None => {
                    for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
                        let c = p.cross(axis);
                        add([c.x, c.y, c.z, axis.x, axis.y, axis.z], (q - *p).dot(axis));
                    }
                }
            }
        }
        let Some(x) = solve6(a, b) else {
            bail!(crate::CoreError::AlignmentUndetermined);
        };
        Ok(DMat4::from_rotation_translation(
            DQuat::from_scaled_axis(DVec3::new(x[0], x[1], x[2])),
            DVec3::new(x[3], x[4], x[5]),
        ))
    }
    /// The RMS to the reference surface, where it is a plane, of `pairs`. Point
    /// distances at edges would mostly measure the sample spacing.
    fn rms(&self, pairs: &[(DVec3, u32)]) -> f64 {
        let surface: Vec<f64> = pairs
            .iter()
            .filter_map(|(p, j)| {
                self.normals[*j as usize].map(|n| (self.points[*j as usize] - *p).dot(n).powi(2))
            })
            .collect();
        (surface.iter().sum::<f64>() / surface.len().max(1) as f64).sqrt()
    }
}

/// The median distance from some of `points` to their nearest other point
/// within `reach`; a tenth of `reach` when none has one.
fn typical_spacing(points: &[DVec3], reach: f64) -> f64 {
    let grid = Grid::new(points, reach);
    let mut gaps: Vec<f64> = (0..points.len())
        .step_by((points.len() / 2000).max(1))
        .filter_map(|i| {
            grid.near(points, points[i], reach)
                .filter(|(j, _)| *j as usize != i)
                .map(|(_, d2)| d2)
                .min_by(f64::total_cmp)
                .map(f64::sqrt)
        })
        .collect();
    gaps.sort_by(f64::total_cmp);
    gaps.get(gaps.len() / 2).copied().unwrap_or(reach / 10.)
}

/// How far `b` puts any of `points` from where `a` puts it.
fn farthest_shift(points: &[DVec3], a: DMat4, b: DMat4) -> f64 {
    points
        .iter()
        .map(|p| a.transform_point3(*p).distance(b.transform_point3(*p)))
        .fold(0., f64::max)
}

/// The correspondence distances from `start`, halving down to `end`.
fn narrowing(start: f64, end: f64) -> Vec<f64> {
    let end = end.min(start);
    let mut distances = vec![start];
    while let Some(&last) = distances.last()
        && last > end * (1. + 1e-9)
    {
        distances.push((last * 0.5).max(end));
    }
    distances
}

fn intersects(bounds: &Bounds, world: DMat4, region: Option<(DVec3, DVec3)>) -> bool {
    let Some((lo, hi)) = region else { return true };
    let (mut a, mut b) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
    for c in bounds.corners() {
        let p = world.transform_point3(c);
        a = a.min(p);
        b = b.max(p);
    }
    a.cmple(hi).all() && b.cmpge(lo).all()
}

impl Project {
    /// The own transform `item` (a scan or folder) needs, starting from `own`,
    /// for everything in it to move by `motion` in the project frame.
    pub fn moved_pose(&self, item: Uuid, own: Pose, motion: DMat4) -> Pose {
        let above = self
            .parent_of(item)
            .map_or(DMat4::IDENTITY, |parent| self.correction(parent));
        Pose::from_matrix(above.inverse() * motion * above * own.matrix())
    }
    /// About `budget` points of `scan` in visible layers, spread evenly over
    /// it and mapped by `world`: whole levels of the additive display octree,
    /// coarsest first, as far as the budget reaches. `region` limits them to
    /// a box in the mapped frame.
    fn processing_samples(
        &self,
        scan: &Scan,
        world: DMat4,
        budget: usize,
        region: Option<(DVec3, DVec3)>,
        job: &JobControl,
    ) -> Result<Vec<DVec3>> {
        if scan.nodes.is_empty() || !intersects(&scan.nodes[0].bounds, world, region) {
            return Ok(vec![]);
        }
        let keep = |p: DVec3| region.is_none_or(|(lo, hi)| p.cmpge(lo).all() && p.cmple(hi).all());
        let mut masks: HashMap<u32, Vec<u8>> = HashMap::new();
        let mut result = vec![];
        let mut level = vec![0u32];
        while !level.is_empty() && result.len() < budget {
            let mut next = vec![];
            for &node in &level {
                job.check()?;
                for sample in self.read_view(scan, node)? {
                    if let std::collections::hash_map::Entry::Vacant(e) = masks.entry(sample.chunk)
                    {
                        e.insert(self.hidden_mask(scan, sample.chunk)?);
                    }
                    if is_set(&masks[&sample.chunk], sample.index as usize) {
                        continue;
                    }
                    let p = world.transform_point3(DVec3::from(sample.position));
                    if keep(p) {
                        result.push(p);
                    }
                }
                next.extend(
                    scan.nodes[node as usize]
                        .children
                        .iter()
                        .copied()
                        .filter(|&c| intersects(&scan.nodes[c as usize].bounds, world, region)),
                );
            }
            level = next;
        }
        // The last level can overshoot; thin it evenly.
        if result.len() > budget * 3 / 2 {
            let step = result.len() as f64 / budget as f64;
            result = (0..budget)
                .map(|i| result[(i as f64 * step) as usize])
                .collect();
        }
        Ok(result)
    }
    /// Aligns `item` (a scan or folder), starting from `initial` as its own
    /// transform, to the scans `reference` by point-to-plane ICP: at the
    /// initial distance until it settles, then at half of it and so on down
    /// to the narrowest. The samples are read once for all distances.
    pub fn icp(
        &self,
        item: Uuid,
        initial: Pose,
        reference: &[Uuid],
        options: &IcpOptions,
        job: &JobControl,
    ) -> Result<IcpResult> {
        ensure!(
            options.max_distance.is_finite() && options.max_distance > 0.,
            "Invalid correspondence distance"
        );
        ensure!(options.samples >= 100, "Too few ICP samples");
        let moving_ids = self.scans_within(item);
        ensure!(!moving_ids.is_empty(), "Nothing to align");
        let moving_scans: Vec<_> = self
            .scans()
            .filter(|s| moving_ids.contains(&s.id))
            .collect();
        let reference_scans: Vec<_> = self
            .scans()
            .filter(|s| reference.contains(&s.id) && !moving_ids.contains(&s.id))
            .collect();
        ensure!(!reference_scans.is_empty(), "No reference scans");
        let share = |scans: &[&Scan], scan: &Scan, budget: usize| {
            let total: u64 = scans.iter().map(|s| s.valid_points).sum();
            ((budget as f64 * scan.valid_points as f64 / total.max(1) as f64) as usize).max(100)
        };
        let mut moving = vec![];
        for scan in &moving_scans {
            job.report(Stage::IcpSampling, 0, 1);
            let world = self.world_matrix_with(scan, item, initial);
            let budget = share(&moving_scans, scan, options.samples);
            moving.extend(self.processing_samples(scan, world, budget, None, job)?);
        }
        ensure!(moving.len() >= 10, "Too few points to align");
        let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
        for p in &moving {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let region = Some((lo - options.max_distance, hi + options.max_distance));
        let mut target = vec![];
        for scan in &reference_scans {
            let budget = share(&reference_scans, scan, options.samples * 4);
            target.extend(self.processing_samples(
                scan,
                self.world_matrix(scan),
                budget,
                region,
                job,
            )?);
        }
        if target.len() < 10 {
            bail!(crate::CoreError::NoOverlap);
        }
        // Work near the origin for well-conditioned sums.
        let centre = target.iter().sum::<DVec3>() / target.len() as f64;
        for p in moving.iter_mut().chain(target.iter_mut()) {
            *p -= centre;
        }
        let d0 = options.max_distance;
        // Typical spacing of the reference samples sets the normal radius and
        // how still the narrowest distance must settle.
        let spacing = typical_spacing(&target, d0);
        let reference = Surface::new(target, (spacing * 4.).min(d0));
        let distances = narrowing(d0, options.min_distance);
        let mut motion = DMat4::IDENTITY;
        let mut steps: Vec<IcpStep> = vec![];
        let mut stopped_at = None;
        for (level, &distance) in distances.iter().enumerate() {
            job.report(Stage::IcpIterations, level as u64, distances.len() as u64);
            // A narrower distance keeping under a tenth of the first's
            // correspondences fits too little of the item to trust.
            let least = steps
                .first()
                .map_or(6., |s| (s.overlap * moving.len() as f64 * 0.1).max(6.));
            // Each distance settles before the next drops far parts; the
            // narrowest well below the sample spacing.
            let settled = if level + 1 == distances.len() {
                spacing.min(distance) * 0.05
            } else {
                distance * 0.005
            };
            let mut trial = motion;
            let mut iterations = 0;
            let mut fitted = || -> Result<()> {
                for _ in 0..options.iterations {
                    job.check()?;
                    iterations += 1;
                    let pairs = reference.pairs(&moving, trial, distance);
                    if (pairs.len() as f64) < least {
                        bail!(crate::CoreError::NoOverlap);
                    }
                    let next = reference.step(&pairs, distance)? * trial;
                    // Re-orthonormalise against drift.
                    let (_, r, t) = next.to_scale_rotation_translation();
                    let next = DMat4::from_rotation_translation(r.normalize(), t);
                    let moved = farthest_shift(&moving, trial, next);
                    trial = next;
                    if moved < settled {
                        break;
                    }
                }
                Ok(())
            };
            if let Err(e) = fitted() {
                if level == 0 || crate::CoreError::find(&e) == Some(&crate::CoreError::Cancelled) {
                    return Err(e);
                }
                stopped_at = Some(distance);
                break;
            }
            motion = trial;
            let pairs = reference.pairs(&moving, motion, distance);
            steps.push(IcpStep {
                distance,
                rms: reference.rms(&pairs),
                overlap: pairs.len() as f64 / moving.len() as f64,
                iterations,
            });
        }
        let last = steps.last().expect("the first distance fits or fails");
        let world_motion =
            DMat4::from_translation(centre) * motion * DMat4::from_translation(-centre);
        Ok(IcpResult {
            pose: self.moved_pose(item, initial, world_motion),
            rms: last.rms,
            overlap: reference.pairs(&moving, motion, d0).len() as f64 / moving.len() as f64,
            iterations: steps.iter().map(|s| s.iterations).sum(),
            steps,
            stopped_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{narrowing, rigid_fit, solve6, symmetric_eigen};
    use glam::{DMat4, DQuat, DVec3};

    #[test]
    fn eigen_decomposition_reconstructs_the_matrix() {
        let a = [[4., 1., 0.5], [1., 3., -0.2], [0.5, -0.2, 1.]];
        let (values, vectors) = symmetric_eigen(a);
        assert!(values[0] <= values[1] && values[1] <= values[2]);
        for i in 0..3 {
            for j in 0..3 {
                let r: f64 = (0..3)
                    .map(|k| values[k] * vectors[k][i] * vectors[k][j])
                    .sum();
                assert!((r - a[i][j]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn rigid_fit_recovers_a_motion_and_rejects_collinear_pairs() {
        let motion = DMat4::from_rotation_translation(
            DQuat::from_euler(glam::EulerRot::XYZ, 0.2, -0.4, 1.1),
            DVec3::new(120.5, -30., 4.25),
        );
        let moving = [
            DVec3::new(0., 0., 0.),
            DVec3::new(5., 0., 0.),
            DVec3::new(0., 3., 0.),
            DVec3::new(1., 1., 2.),
        ];
        let reference: Vec<_> = moving.iter().map(|p| motion.transform_point3(*p)).collect();
        let fit = rigid_fit(&moving, &reference).unwrap();
        assert!(fit.abs_diff_eq(motion, 1e-9), "{fit} != {motion}");
        // Three pairs are enough.
        assert!(
            rigid_fit(&moving[..3], &reference[..3])
                .unwrap()
                .abs_diff_eq(motion, 1e-9)
        );
        let line = [DVec3::ZERO, DVec3::X, DVec3::X * 2.];
        assert!(rigid_fit(&line, &line).is_err());
        assert!(rigid_fit(&moving[..2], &reference[..2]).is_err());
    }

    #[test]
    fn solve6_solves_and_rejects_singular_systems() {
        let mut a = [[0.; 6]; 6];
        for i in 0..6 {
            a[i][i] = 2. + i as f64;
            if i > 0 {
                a[i][i - 1] = 0.5;
                a[i - 1][i] = 0.5;
            }
        }
        let x = [1., -2., 3., 0.5, 0., -1.];
        let b: [f64; 6] = std::array::from_fn(|i| (0..6).map(|j| a[i][j] * x[j]).sum());
        let solved = solve6(a, b).unwrap();
        assert!(solved.iter().zip(x).all(|(s, x)| (s - x).abs() < 1e-12));
        a[5] = [0.; 6];
        for row in a.iter_mut() {
            row[5] = 0.;
        }
        assert!(solve6(a, b).is_none());
    }

    #[test]
    fn distances_halve_down_to_the_end_and_never_widen() {
        assert_eq!(narrowing(0.4, 0.1), [0.4, 0.2, 0.1]);
        assert_eq!(narrowing(0.5, 0.2), [0.5, 0.25, 0.2]);
        assert_eq!(narrowing(0.3, 0.3), [0.3]);
        assert_eq!(narrowing(0.3, 1.), [0.3]);
    }
}
