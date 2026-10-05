//! Global registration: every scan adjusted at once so all their overlaps
//! agree. Aligning scans one by one to those before leaves each small error
//! in place for the next to build on, and a loop of scans comes back with a
//! gap. Like ICP this minimises point-to-plane distances, but between every
//! pair of overlapping scans together, with some scans held still, so the
//! gap spreads over the loop.
use crate::align::{Surface, narrowing, parallel, typical_spacing};
use crate::{CoreError, JobControl, Pose, Project, Stage};
use anyhow::{Result, bail, ensure};
use glam::{DMat4, DQuat, DVec3};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub struct GlobalOptions {
    /// Initial correspondence distance in metres: it should cover what the
    /// scans aligned one by one still disagree by.
    pub max_distance: f64,
    /// The distance it narrows to, in metres; at most `max_distance`.
    pub min_distance: f64,
    /// Iterations allowed at each distance.
    pub iterations: u32,
    /// Processing samples of each scan.
    pub samples: usize,
}
impl Default for GlobalOptions {
    fn default() -> Self {
        Self {
            max_distance: 0.1,
            min_distance: 0.02,
            iterations: 30,
            samples: 50_000,
        }
    }
}
/// How well two overlapping scans agree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairFit {
    pub a: Uuid,
    pub b: Uuid,
    /// The larger of the fractions of either scan's samples with a
    /// correspondence in the other within the initial distance.
    pub overlap: f64,
    /// RMS distance of the correspondences within the end distance to the
    /// other scan's surface, in metres; none when there are none.
    pub rms: Option<f64>,
}
/// How well a scan agrees with all the scans it overlaps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanFit {
    pub scan: Uuid,
    /// As [`PairFit::rms`], against whichever scan is nearest.
    pub rms: Option<f64>,
    /// Fraction of its samples with a correspondence in any other scan
    /// within the initial distance.
    pub overlap: f64,
    /// How many scans it overlaps.
    pub neighbours: usize,
}
/// The fit at one correspondence distance.
#[derive(Clone, Copy, Debug)]
pub struct GlobalStep {
    pub distance: f64,
    pub rms: f64,
    pub iterations: u32,
}
#[derive(Clone, Debug)]
pub struct GlobalResult {
    /// The new own transform of each scan that moved.
    pub poses: Vec<(Uuid, Pose)>,
    /// The overlapping pairs at the end distance, before and after.
    pub before: Vec<PairFit>,
    pub after: Vec<PairFit>,
    /// Each scan against the others, before and after, in the order asked.
    pub scans_before: Vec<ScanFit>,
    pub scans_after: Vec<ScanFit>,
    /// The scans held still: those asked for, and one per group of scans
    /// overlapping none of them.
    pub fixed: Vec<Uuid>,
    /// Scans overlapping no other, left where they were.
    pub isolated: Vec<Uuid>,
    pub steps: Vec<GlobalStep>,
    /// A narrower distance that fitted too little to trust; the result is
    /// that of the one before it.
    pub stopped_at: Option<f64>,
}

/// A scan's samples in its own coordinates, with their bounds.
struct Member {
    id: Uuid,
    surface: Surface,
    lo: DVec3,
    hi: DVec3,
}
impl Member {
    fn corners(&self) -> [DVec3; 8] {
        std::array::from_fn(|i| {
            DVec3::new(
                if i & 1 == 0 { self.lo.x } else { self.hi.x },
                if i & 2 == 0 { self.lo.y } else { self.hi.y },
                if i & 4 == 0 { self.lo.z } else { self.hi.z },
            )
        })
    }
    /// Its bounds placed by `world`, as an axis-aligned box.
    fn world_box(&self, world: DMat4) -> (DVec3, DVec3) {
        self.corners()
            .iter()
            .fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(lo, hi), c| {
                let p = world.transform_point3(*c);
                (lo.min(p), hi.max(p))
            })
    }
}

/// Sums over the correspondences from one scan's samples to another's
/// surface: the normal equations of a point-to-plane step for both scans'
/// motions (the first scan's six unknowns, then the other's), and counts.
#[derive(Clone, Copy)]
struct Sums {
    h: [[f64; 12]; 12],
    g: [f64; 12],
    matched: usize,
    /// Correspondences on a plane of the other scan, and their squared
    /// distances to it.
    planar: usize,
    squares: f64,
}
impl Default for Sums {
    fn default() -> Self {
        Self {
            h: [[0.; 12]; 12],
            g: [0.; 12],
            matched: 0,
            planar: 0,
            squares: 0.,
        }
    }
}
impl Sums {
    fn add(&mut self, other: &Sums) {
        for i in 0..12 {
            for j in 0..12 {
                self.h[i][j] += other.h[i][j];
            }
            self.g[i] += other.g[i];
        }
        self.matched += other.matched;
        self.planar += other.planar;
        self.squares += other.squares;
    }
    /// A residual along `n` between `p` of the first scan and `q` of the
    /// other, both in the working frame. A small turn `w` and shift `t` of
    /// the first moves `p` by `w × p + t`, so the residual by `-(p × n)·w -
    /// n·t`; the other's moves it the opposite way about `q`.
    fn row(&mut self, p: DVec3, q: DVec3, n: DVec3, huber: f64) {
        let r = (q - p).dot(n);
        let (a, b) = (p.cross(n), q.cross(n));
        let row = [
            -a.x, -a.y, -a.z, -n.x, -n.y, -n.z, b.x, b.y, b.z, n.x, n.y, n.z,
        ];
        let w = if r.abs() <= huber {
            1.
        } else {
            huber / r.abs()
        };
        for i in 0..12 {
            for j in 0..12 {
                self.h[i][j] += w * row[i] * row[j];
            }
            self.g[i] += w * row[i] * r;
        }
    }
}

/// The correspondences from `a`'s samples, placed by `wa`, to `b`'s surface,
/// placed by `wb`, within `distance`.
fn pair_sums(a: &Member, wa: DMat4, b: &Member, wb: DMat4, distance: f64) -> Sums {
    let to_b = wb.inverse() * wa;
    let rotate_b = DMat4::from_quat(wb.to_scale_rotation_translation().1);
    let (lo, hi) = (b.lo - distance, b.hi + distance);
    // Robust weights as in ICP: full inside a third of the distance.
    let huber = distance / 3.;
    let points = &a.surface.points;
    let parts = parallel(points.len(), |range| {
        let mut sums = Sums::default();
        for i in range {
            let local = to_b.transform_point3(points[i]);
            if local.cmplt(lo).any() || local.cmpgt(hi).any() {
                continue;
            }
            let Some((k, _)) = b.surface.nearest(local, distance) else {
                continue;
            };
            let p = wa.transform_point3(points[i]);
            let q = wb.transform_point3(b.surface.points[k as usize]);
            sums.matched += 1;
            match b.surface.normals[k as usize] {
                Some(n) => {
                    let n = rotate_b.transform_vector3(n);
                    sums.planar += 1;
                    sums.squares += (q - p).dot(n).powi(2);
                    sums.row(p, q, n, huber);
                }
                // Point-to-point rows where the surface has no plane.
                None => {
                    for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
                        sums.row(p, q, axis, huber);
                    }
                }
            }
        }
        vec![sums]
    });
    let mut total = Sums::default();
    for part in &parts {
        total.add(part);
    }
    total
}

/// The pairs of members whose placed boxes come within `distance`.
fn overlapping(members: &[Member], worlds: &[DMat4], distance: f64) -> Vec<(usize, usize)> {
    let boxes: Vec<_> = members
        .iter()
        .zip(worlds)
        .map(|(m, w)| m.world_box(*w))
        .collect();
    let mut pairs = vec![];
    for i in 0..members.len() {
        for j in i + 1..members.len() {
            let ((alo, ahi), (blo, bhi)) = (boxes[i], boxes[j]);
            if (alo - distance).cmple(bhi).all() && (blo - distance).cmple(ahi).all() {
                pairs.push((i, j));
            }
        }
    }
    pairs
}

/// Solves the symmetric positive system `a x = b` (row-major, `n` by `n`) by
/// Cholesky; `None` when it is not positive definite.
fn solve(mut a: Vec<f64>, n: usize, b: &[f64]) -> Option<Vec<f64>> {
    let scale = (0..n).map(|i| a[i * n + i]).fold(0., f64::max).max(1e-300);
    for j in 0..n {
        let mut d = a[j * n + j];
        for k in 0..j {
            d -= a[j * n + k] * a[j * n + k];
        }
        if d <= 1e-12 * scale {
            return None;
        }
        let d = d.sqrt();
        a[j * n + j] = d;
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k];
            }
            a[i * n + j] = s / d;
        }
    }
    let mut y = vec![0.; n];
    for i in 0..n {
        let s: f64 = (0..i).map(|k| a[i * n + k] * y[k]).sum();
        y[i] = (b[i] - s) / a[i * n + i];
    }
    let mut x = vec![0.; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|k| a[k * n + i] * x[k]).sum();
        x[i] = (y[i] - s) / a[i * n + i];
    }
    Some(x)
}

/// Groups of members linked by `links`, each a list of member indices.
fn components(count: usize, links: &[(usize, usize)]) -> Vec<Vec<usize>> {
    let mut group: Vec<usize> = (0..count).collect();
    fn root(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    for &(a, b) in links {
        let (ra, rb) = (root(&mut group, a), root(&mut group, b));
        group[ra] = rb;
    }
    let mut result: Vec<Vec<usize>> = vec![];
    let mut index = std::collections::HashMap::new();
    for i in 0..count {
        let r = root(&mut group, i);
        let at = *index.entry(r).or_insert_with(|| {
            result.push(vec![]);
            result.len() - 1
        });
        result[at].push(i);
    }
    result
}

/// How well the members agree, placed by `worlds`: each overlapping pair,
/// and each member against all it overlaps. Overlap counts correspondences
/// within `reach`, the initial distance, as ICP's does; at the end distance
/// sparse samples of whole scans would match few points even where surfaces
/// meet. The RMS is of those within `distance`.
fn evaluate(
    members: &[Member],
    worlds: &[DMat4],
    pairs: &[(usize, usize)],
    reach: f64,
    distance: f64,
) -> (Vec<PairFit>, Vec<ScanFit>) {
    let rms = |squares: f64, count: usize| (count > 0).then(|| (squares / count as f64).sqrt());
    let mut pair_fits = vec![];
    let mut neighbours = vec![0; members.len()];
    for &(i, j) in pairs {
        let (a, b) = (&members[i], &members[j]);
        let (wa, wb) = (worlds[i], worlds[j]);
        let (ab, ba) = (
            pair_sums(a, wa, b, wb, reach),
            pair_sums(b, wb, a, wa, reach),
        );
        if ab.matched + ba.matched == 0 {
            continue;
        }
        neighbours[i] += 1;
        neighbours[j] += 1;
        let (abn, ban) = (
            pair_sums(a, wa, b, wb, distance),
            pair_sums(b, wb, a, wa, distance),
        );
        let share = |s: &Sums, m: &Member| s.matched as f64 / m.surface.points.len() as f64;
        pair_fits.push(PairFit {
            a: a.id,
            b: b.id,
            overlap: share(&ab, a).max(share(&ba, b)),
            rms: rms(abn.squares + ban.squares, abn.planar + ban.planar),
        });
    }
    let scan_fits = (0..members.len())
        .map(|i| {
            let others: Vec<usize> = pairs
                .iter()
                .filter_map(|&(a, b)| match (a == i, b == i) {
                    (true, _) => Some(b),
                    (_, true) => Some(a),
                    _ => None,
                })
                .collect();
            let m = &members[i];
            // Distances are the same in the other scan's frame.
            let into: Vec<DMat4> = others
                .iter()
                .map(|&j| worlds[j].inverse() * worlds[i])
                .collect();
            // The nearest correspondence over all the others within reach,
            // per sample, and its squared distance to the plane there when
            // it is within the end distance.
            let found: Vec<Option<f64>> = parallel(m.surface.points.len(), |range| {
                range
                    .filter_map(|s| {
                        let p = m.surface.points[s];
                        let mut best: Option<(f64, Option<f64>)> = None;
                        for (&j, to_b) in others.iter().zip(&into) {
                            let b = &members[j];
                            let local = to_b.transform_point3(p);
                            let Some((k, d2)) = b.surface.nearest(local, reach) else {
                                continue;
                            };
                            if best.is_some_and(|(d, _)| d <= d2) {
                                continue;
                            }
                            let plane = b.surface.normals[k as usize]
                                .filter(|_| d2 <= distance * distance)
                                .map(|n| (b.surface.points[k as usize] - local).dot(n).powi(2));
                            best = Some((d2, plane));
                        }
                        best.map(|(_, plane)| plane)
                    })
                    .collect()
            });
            let planes: Vec<f64> = found.iter().flatten().copied().collect();
            ScanFit {
                scan: m.id,
                rms: rms(planes.iter().sum(), planes.len()),
                overlap: found.len() as f64 / m.surface.points.len().max(1) as f64,
                neighbours: neighbours[i],
            }
        })
        .collect();
    (pair_fits, scan_fits)
}

impl Project {
    /// Adjusts the scans `scans` together so they agree where they overlap,
    /// holding `fixed` (some of them) still. Each scan moves by its own
    /// transform; folders stay. The scans should already be roughly aligned,
    /// within the initial distance.
    pub fn align_globally(
        &self,
        scans: &[Uuid],
        fixed: &[Uuid],
        options: &GlobalOptions,
        job: &JobControl,
    ) -> Result<GlobalResult> {
        let d0 = options.max_distance;
        ensure!(
            d0.is_finite() && d0 > 0. && options.min_distance > 0.,
            "Invalid correspondence distance"
        );
        ensure!(options.samples >= 100, "Too few samples");
        let wanted: HashSet<Uuid> = scans.iter().copied().collect();
        let chosen: Vec<_> = self.scans().filter(|s| wanted.contains(&s.id)).collect();
        ensure!(chosen.len() >= 2, "At least two scans are needed");
        ensure!(
            fixed.iter().any(|id| wanted.contains(id)),
            "At least one of the scans must stay fixed"
        );
        let mut members = vec![];
        let mut originals = vec![];
        for (k, scan) in chosen.iter().enumerate() {
            job.report(Stage::IcpSampling, k as u64, chosen.len() as u64);
            let samples =
                self.processing_samples(scan, DMat4::IDENTITY, options.samples, None, job)?;
            if samples.len() < 10 {
                continue;
            }
            let (lo, hi) = samples
                .iter()
                .fold((DVec3::INFINITY, DVec3::NEG_INFINITY), |(lo, hi), p| {
                    (lo.min(*p), hi.max(*p))
                });
            let spacing = typical_spacing(&samples, d0);
            members.push(Member {
                id: scan.id,
                surface: Surface::new(samples, (spacing * 4.).min(d0)),
                lo,
                hi,
            });
            originals.push((self.world_matrix(scan), spacing));
        }
        ensure!(members.len() >= 2, "Too few points to align");
        // Work near the origin for well-conditioned sums.
        let centre = originals
            .iter()
            .map(|(w, _)| w.transform_point3(DVec3::ZERO))
            .sum::<DVec3>()
            / originals.len() as f64;
        let start: Vec<DMat4> = originals
            .iter()
            .map(|(w, _)| DMat4::from_translation(-centre) * *w)
            .collect();
        let mut spacings: Vec<f64> = originals.iter().map(|(_, s)| *s).collect();
        spacings.sort_by(f64::total_cmp);
        let spacing = spacings[spacings.len() / 2];

        // Which scans overlap at all, and which hold still: those asked for,
        // and in each group overlapping none of them, its largest scan.
        let candidates = overlapping(&members, &start, d0);
        let mut links = vec![];
        for &(i, j) in &candidates {
            job.check()?;
            let ab = pair_sums(&members[i], start[i], &members[j], start[j], d0);
            let ba = pair_sums(&members[j], start[j], &members[i], start[i], d0);
            // Too few correspondences say nothing about a scan's place.
            if ab.matched + ba.matched >= 100 {
                links.push((i, j));
            }
        }
        let asked: HashSet<Uuid> = fixed.iter().copied().collect();
        let mut still = vec![false; members.len()];
        let mut isolated = vec![];
        for group in components(members.len(), &links) {
            if let [only] = group[..] {
                still[only] = true;
                if !asked.contains(&members[only].id) {
                    isolated.push(members[only].id);
                    continue;
                }
            }
            let held: Vec<usize> = group
                .iter()
                .copied()
                .filter(|i| asked.contains(&members[*i].id))
                .collect();
            if held.is_empty() {
                let largest = *group
                    .iter()
                    .max_by_key(|i| members[**i].surface.points.len())
                    .expect("groups are not empty");
                still[largest] = true;
            }
            for i in held {
                still[i] = true;
            }
        }
        let fixed_ids: Vec<Uuid> = (0..members.len())
            .filter(|i| still[*i] && !isolated.contains(&members[*i].id))
            .map(|i| members[i].id)
            .collect();
        // The unknowns: six per scan that moves.
        let mut slot = vec![None; members.len()];
        let mut unknowns = 0;
        for i in 0..members.len() {
            if !still[i] {
                slot[i] = Some(unknowns);
                unknowns += 6;
            }
        }

        let distances = narrowing(d0, options.min_distance);
        let mut worlds = start.clone();
        let mut steps: Vec<GlobalStep> = vec![];
        let mut stopped_at = None;
        let mut first_matched = None;
        for (level, &distance) in distances.iter().enumerate() {
            job.report(Stage::IcpIterations, level as u64, distances.len() as u64);
            let settled = if level + 1 == distances.len() {
                spacing.min(distance) * 0.05
            } else {
                distance * 0.005
            };
            let mut trial = worlds.clone();
            let mut iterations = 0;
            let mut fitted = || -> Result<()> {
                for _ in 0..options.iterations {
                    job.check()?;
                    iterations += 1;
                    let mut h = vec![0.; unknowns * unknowns];
                    let mut g = vec![0.; unknowns];
                    let mut matched = 0;
                    for &(i, j) in &links {
                        for (a, b) in [(i, j), (j, i)] {
                            let s =
                                pair_sums(&members[a], trial[a], &members[b], trial[b], distance);
                            matched += s.matched;
                            let at = [slot[a], slot[b]];
                            for (bi, oi) in at.iter().enumerate() {
                                let Some(oi) = oi else { continue };
                                for r in 0..6 {
                                    g[oi + r] += s.g[bi * 6 + r];
                                    for (bj, oj) in at.iter().enumerate() {
                                        let Some(oj) = oj else { continue };
                                        for c in 0..6 {
                                            h[(oi + r) * unknowns + oj + c] +=
                                                s.h[bi * 6 + r][bj * 6 + c];
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // A narrower distance keeping under a tenth of the first's
                    // correspondences fits too little to trust.
                    let least = first_matched.map_or(6, |m: usize| (m / 10).max(6));
                    if matched < least {
                        bail!(CoreError::NoOverlap);
                    }
                    first_matched.get_or_insert(matched);
                    if unknowns == 0 {
                        return Ok(());
                    }
                    // A touch of damping keeps scans that barely constrain a
                    // direction (a single plane, say) from running off along it.
                    let damping = 1e-9
                        * (0..unknowns)
                            .map(|k| h[k * unknowns + k])
                            .fold(0., f64::max);
                    for k in 0..unknowns {
                        h[k * unknowns + k] += damping;
                    }
                    let minus_g: Vec<f64> = g.iter().map(|v| -v).collect();
                    let Some(x) = solve(h, unknowns, &minus_g) else {
                        bail!(CoreError::AlignmentUndetermined);
                    };
                    let mut moved: f64 = 0.;
                    for i in 0..members.len() {
                        let Some(o) = slot[i] else { continue };
                        let step = DMat4::from_rotation_translation(
                            DQuat::from_scaled_axis(DVec3::new(x[o], x[o + 1], x[o + 2])),
                            DVec3::new(x[o + 3], x[o + 4], x[o + 5]),
                        );
                        let next = step * trial[i];
                        // Re-orthonormalise against drift.
                        let (_, r, t) = next.to_scale_rotation_translation();
                        let next = DMat4::from_rotation_translation(r.normalize(), t);
                        for c in members[i].corners() {
                            moved = moved.max(
                                next.transform_point3(c)
                                    .distance(trial[i].transform_point3(c)),
                            );
                        }
                        trial[i] = next;
                    }
                    if moved < settled {
                        break;
                    }
                }
                Ok(())
            };
            if let Err(e) = fitted() {
                if level == 0 || CoreError::find(&e) == Some(&CoreError::Cancelled) {
                    return Err(e);
                }
                stopped_at = Some(distance);
                break;
            }
            worlds = trial;
            let (mut squares, mut planar) = (0., 0);
            for &(i, j) in &links {
                for (a, b) in [(i, j), (j, i)] {
                    let s = pair_sums(&members[a], worlds[a], &members[b], worlds[b], distance);
                    squares += s.squares;
                    planar += s.planar;
                }
            }
            steps.push(GlobalStep {
                distance,
                rms: (squares / planar.max(1) as f64).sqrt(),
                iterations,
            });
        }

        let end = steps
            .last()
            .expect("the first distance fits or fails")
            .distance;
        let (before, scans_before) = evaluate(&members, &start, &candidates, d0, end);
        let (after, scans_after) = evaluate(&members, &worlds, &candidates, d0, end);
        let to_project = DMat4::from_translation(centre);
        let poses = (0..members.len())
            .filter(|i| slot[*i].is_some())
            .map(|i| {
                let id = members[i].id;
                let own = self
                    .current()
                    .transforms
                    .get(&id)
                    .copied()
                    .unwrap_or_default();
                let motion = to_project * worlds[i] * start[i].inverse() * to_project.inverse();
                (id, self.moved_pose(id, own, motion))
            })
            .collect();
        Ok(GlobalResult {
            poses,
            before,
            after,
            scans_before,
            scans_after,
            fixed: fixed_ids,
            isolated,
            steps,
            stopped_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{components, solve};

    #[test]
    fn solves_a_dense_system_and_rejects_a_singular_one() {
        let a = vec![4., 1., 0., 1., 3., 1., 0., 1., 2.];
        let x = [1., -2., 0.5];
        let b: Vec<f64> = (0..3)
            .map(|i| (0..3).map(|j| a[i * 3 + j] * x[j]).sum())
            .collect();
        let solved = solve(a, 3, &b).unwrap();
        assert!(solved.iter().zip(x).all(|(s, x)| (s - x).abs() < 1e-12));
        assert!(solve(vec![1., 1., 1., 1.], 2, &[1., 1.]).is_none());
    }

    #[test]
    fn groups_follow_links() {
        let mut groups = components(5, &[(0, 2), (3, 4), (2, 0)]);
        groups.sort();
        assert_eq!(groups, [vec![0, 2], vec![1], vec![3, 4]]);
    }
}
