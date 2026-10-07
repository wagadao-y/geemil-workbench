//! Equirectangular panoramas placed by correspondences: pixels of the photo
//! and the points seen there. The placement minimizes the angles between the
//! directions the pixels show and those towards the points.
use crate::{
    AlignmentFit, CoreError, Panorama, PanoramaFormat, PanoramaPair, Pose, Project, Registration,
    RegistrationMethod,
};
use anyhow::{Context, Result, ensure};
use glam::{DMat3, DMat4, DQuat, DVec3};
use serde_json::json;
use std::{f64::consts::PI, fs, path::Path};
use uuid::Uuid;

/// Correspondences a placement needs: fewer would hide a wrong one.
pub const MIN_PANORAMA_PAIRS: usize = 4;

impl Panorama {
    /// Radians per pixel across and down.
    fn pixel_size(&self) -> (f64, f64) {
        (2. * PI / self.width as f64, PI / self.height as f64)
    }
    /// The unit direction an image position shows, in the panorama's frame.
    /// As in E57 spherical images, x = width / 2 - azimuth / pixel width and
    /// y = height / 2 - elevation / pixel height.
    pub fn bearing(&self, pixel: [f64; 2]) -> DVec3 {
        let (pw, ph) = self.pixel_size();
        let azimuth = (self.width as f64 / 2. - pixel[0]) * pw;
        let elevation = ((self.height as f64 / 2. - pixel[1]) * ph).clamp(-PI / 2., PI / 2.);
        DVec3::new(
            elevation.cos() * azimuth.cos(),
            elevation.cos() * azimuth.sin(),
            elevation.sin(),
        )
    }
    /// The image position a direction in the panorama's frame falls on, x in
    /// `[0, width)`.
    pub fn pixel(&self, direction: DVec3) -> [f64; 2] {
        let (pw, ph) = self.pixel_size();
        let azimuth = direction.y.atan2(direction.x);
        let elevation = direction.z.atan2(direction.x.hypot(direction.y));
        let x = (self.width as f64 / 2. - azimuth / pw).rem_euclid(self.width as f64);
        [x, self.height as f64 / 2. - elevation / ph]
    }
    /// How far apart two image positions are in pixels, across the seam too.
    pub fn pixel_distance(&self, a: [f64; 2], b: [f64; 2]) -> f64 {
        let w = self.width as f64;
        let dx = (a[0] - b[0] + w / 2.).rem_euclid(w) - w / 2.;
        dx.hypot(a[1] - b[1])
    }
}

/// A placement found from correspondences.
#[derive(Clone, Debug)]
pub struct PanoramaSolution {
    /// The panorama's pose in the project frame.
    pub world: DMat4,
    /// The angle between each pixel's direction and its point's, in degrees.
    pub residuals: Vec<f64>,
    /// RMS of `residuals`.
    pub rms: f64,
}

/// Places a panorama so the directions `bearings` (in its frame) point at
/// `points` (project frame): the pose minimizing the squared angles. With
/// `level` it only turns about Z, for photos levelled by their camera.
/// Searches the heading all around, so it needs no initial guess.
pub fn solve_panorama(
    bearings: &[DVec3],
    points: &[DVec3],
    level: bool,
) -> Result<PanoramaSolution> {
    ensure!(
        bearings.len() == points.len() && bearings.len() >= MIN_PANORAMA_PAIRS,
        CoreError::TooFewPanoramaPairs(MIN_PANORAMA_PAIRS)
    );
    // Relative to the points' centre, so far-off coordinates keep precision.
    let centre = points.iter().sum::<DVec3>() / points.len() as f64;
    let points: Vec<DVec3> = points.iter().map(|p| *p - centre).collect();
    let bearings: Vec<DVec3> = bearings.iter().map(|b| b.normalize()).collect();
    let problem = Problem {
        bearings: &bearings,
        points: &points,
    };
    // Every heading, half a degree apart, with the position that best meets
    // the rays it gives; the best start levelled refinement.
    let mut best: Option<(f64, f64, DVec3)> = None;
    for step in 0..720 {
        let yaw = step as f64 * PI / 360.;
        let rotation = DMat3::from_rotation_z(yaw);
        let Some(position) = ray_meeting(&bearings, &points, rotation) else {
            continue;
        };
        let cost = problem.cost(rotation, position);
        if best.is_none_or(|(c, ..)| cost < c) {
            best = Some((cost, yaw, position));
        }
    }
    let (_, yaw, position) = best.context("The pixels' directions are all parallel")?;
    let levelled = levenberg_marquardt(&[position.x, position.y, position.z, yaw], |x| {
        problem.residuals(DMat3::from_rotation_z(x[3]), DVec3::new(x[0], x[1], x[2]))
    });
    let mut rotation = DMat3::from_rotation_z(levelled[3]);
    let mut position = DVec3::new(levelled[0], levelled[1], levelled[2]);
    if !level {
        // Tilt as a rotation vector applied after the levelled heading.
        let start = rotation;
        let x = levenberg_marquardt(&[position.x, position.y, position.z, 0., 0., 0.], |x| {
            let tilt = DVec3::new(x[3], x[4], x[5]);
            problem.residuals(start * rotation_vector(tilt), DVec3::new(x[0], x[1], x[2]))
        });
        rotation = start * rotation_vector(DVec3::new(x[3], x[4], x[5]));
        position = DVec3::new(x[0], x[1], x[2]);
    }
    let residuals: Vec<f64> = bearings
        .iter()
        .zip(&points)
        .map(|(b, p)| angle(*b, rotation.transpose() * (*p - position)).to_degrees())
        .collect();
    let rms = (residuals.iter().map(|r| r * r).sum::<f64>() / residuals.len() as f64).sqrt();
    let world = DMat4::from_rotation_translation(
        DQuat::from_mat3(&rotation).normalize(),
        position + centre,
    );
    Ok(PanoramaSolution {
        world,
        residuals,
        rms,
    })
}

struct Problem<'a> {
    bearings: &'a [DVec3],
    points: &'a [DVec3],
}
impl Problem<'_> {
    /// Per correspondence, the angle from the pixel's direction to the
    /// point's, split along two axes across the pixel's direction.
    fn residuals(&self, rotation: DMat3, position: DVec3) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.bearings.len() * 2);
        for (b, p) in self.bearings.iter().zip(self.points) {
            let seen = (rotation.transpose() * (*p - position)).normalize_or_zero();
            let across = seen - *b * seen.dot(*b);
            let (e1, e2) = b.any_orthonormal_pair();
            let sin = across.length();
            let scale = if sin > 1e-12 {
                sin.atan2(seen.dot(*b)) / sin
            } else {
                1.
            };
            out.push(across.dot(e1) * scale);
            out.push(across.dot(e2) * scale);
        }
        out
    }
    fn cost(&self, rotation: DMat3, position: DVec3) -> f64 {
        self.residuals(rotation, position)
            .iter()
            .map(|r| r * r)
            .sum()
    }
}

/// The angle between two directions, in radians.
fn angle(a: DVec3, b: DVec3) -> f64 {
    a.cross(b).length().atan2(a.dot(b))
}

fn rotation_vector(v: DVec3) -> DMat3 {
    let length = v.length();
    if length < 1e-15 {
        return DMat3::IDENTITY;
    }
    DMat3::from_axis_angle(v / length, length)
}

/// The position closest to every ray from it along `rotation * bearing`
/// through its point, in the least-squares sense.
fn ray_meeting(bearings: &[DVec3], points: &[DVec3], rotation: DMat3) -> Option<DVec3> {
    let mut a = DMat3::ZERO;
    let mut b = DVec3::ZERO;
    for (bearing, point) in bearings.iter().zip(points) {
        let w = rotation * *bearing;
        // Projects across the ray.
        let across = DMat3::IDENTITY - DMat3::from_cols(w * w.x, w * w.y, w * w.z);
        a += across;
        b += across * *point;
    }
    (a.determinant().abs() > 1e-12).then(|| a.inverse() * b)
}

/// Minimizes the sum of squared `residuals` from `start`, with numerical
/// derivatives.
fn levenberg_marquardt(start: &[f64], residuals: impl Fn(&[f64]) -> Vec<f64>) -> Vec<f64> {
    let n = start.len();
    let mut x = start.to_vec();
    let mut r = residuals(&x);
    let mut cost: f64 = r.iter().map(|v| v * v).sum();
    let mut lambda = 1e-3;
    for _ in 0..200 {
        let mut jacobian = vec![vec![0.; r.len()]; n];
        for (j, column) in jacobian.iter_mut().enumerate() {
            let h = 1e-7 * x[j].abs().max(1.);
            let (mut plus, mut minus) = (x.clone(), x.clone());
            plus[j] += h;
            minus[j] -= h;
            let (rp, rm) = (residuals(&plus), residuals(&minus));
            for (c, (p, m)) in column.iter_mut().zip(rp.iter().zip(&rm)) {
                *c = (p - m) / (2. * h);
            }
        }
        let jtj: Vec<Vec<f64>> = (0..n)
            .map(|a| {
                (0..n)
                    .map(|b| {
                        jacobian[a]
                            .iter()
                            .zip(&jacobian[b])
                            .map(|(p, q)| p * q)
                            .sum()
                    })
                    .collect()
            })
            .collect();
        let jtr: Vec<f64> = (0..n)
            .map(|a| jacobian[a].iter().zip(&r).map(|(p, q)| p * q).sum())
            .collect();
        let mut improved = false;
        while lambda < 1e12 {
            let mut m = jtj.clone();
            for (i, row) in m.iter_mut().enumerate() {
                row[i] += lambda * row[i].max(1e-12);
            }
            let Some(step) = solve(m, jtr.iter().map(|v| -v).collect()) else {
                lambda *= 10.;
                continue;
            };
            let next: Vec<f64> = x.iter().zip(&step).map(|(a, b)| a + b).collect();
            let next_r = residuals(&next);
            let next_cost: f64 = next_r.iter().map(|v| v * v).sum();
            if next_cost < cost {
                let done = cost - next_cost < 1e-15 * cost.max(1e-300);
                (x, r, cost) = (next, next_r, next_cost);
                lambda = (lambda * 0.3).max(1e-12);
                improved = !done;
                break;
            }
            lambda *= 10.;
        }
        if !improved {
            break;
        }
    }
    x
}

/// Solves `m x = b` by Gaussian elimination with partial pivoting.
fn solve(mut m: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))?;
        if m[pivot][col].abs() < 1e-300 {
            return None;
        }
        m.swap(col, pivot);
        b.swap(col, pivot);
        let (above, below) = m.split_at_mut(col + 1);
        let pivot_row = &above[col];
        for (i, row) in below.iter_mut().enumerate() {
            let f = row[col] / pivot_row[col];
            for (value, p) in row[col..].iter_mut().zip(&pivot_row[col..]) {
                *value -= f * p;
            }
            b[col + 1 + i] -= f * b[col];
        }
    }
    let mut x = vec![0.; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| m[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / m[row][row];
    }
    Some(x)
}

impl Project {
    /// The panoramas of the current state, in import order.
    pub fn panoramas(&self) -> impl Iterator<Item = &Panorama> {
        let current = &self.current().panoramas;
        self.manifest
            .panoramas
            .iter()
            .filter(move |p| current.contains(&p.id))
    }
    /// A panorama of the current state.
    pub fn panorama(&self, id: Uuid) -> Option<&Panorama> {
        self.panoramas().find(|p| p.id == id)
    }
    /// A panorama's name: the one it was renamed to, else its imported one.
    pub fn panorama_name<'a>(&'a self, panorama: &'a Panorama) -> &'a str {
        self.current()
            .scan_names
            .get(&panorama.id)
            .map_or(&panorama.name, |name| name)
    }
    /// The correspondences a panorama was last placed with.
    pub fn panorama_pairs(&self, id: Uuid) -> &[PanoramaPair] {
        self.current()
            .panorama_pairs
            .get(&id)
            .map_or(&[], Vec::as_slice)
    }
    /// Copies an equirectangular JPEG or PNG photo into the project as a
    /// panorama, near the middle of the scans until it is placed.
    pub fn import_panorama(&mut self, source: &Path) -> Result<Uuid> {
        let reader = image::ImageReader::open(source)
            .with_context(|| format!("Reading {}", source.display()))?
            .with_guessed_format()?;
        let format = match reader.format() {
            Some(image::ImageFormat::Jpeg) => PanoramaFormat::Jpeg,
            Some(image::ImageFormat::Png) => PanoramaFormat::Png,
            _ => anyhow::bail!(CoreError::UnsupportedImageFormat),
        };
        let (width, height) = reader
            .into_dimensions()
            .with_context(|| format!("Reading {}", source.display()))?;
        ensure!(
            height > 0 && (width as f64 / height as f64 - 2.).abs() < 0.02,
            CoreError::NotEquirectangular(width, height)
        );
        let id = Uuid::new_v4();
        let import_id = Uuid::new_v4();
        let extension = match format {
            PanoramaFormat::Jpeg => "jpg",
            PanoramaFormat::Png => "png",
        };
        let stage = self.root.join("staging").join(import_id.to_string());
        fs::create_dir(&stage)?;
        let staged = (|| -> Result<()> {
            let file = stage.join(format!("panorama.{extension}"));
            fs::copy(source, &file).with_context(|| format!("Copying {}", source.display()))?;
            fs::OpenOptions::new().write(true).open(&file)?.sync_all()?;
            let prefix = format!("data/{import_id}");
            fs::rename(&stage, self.path(&prefix)?).context("Publishing imported assets")?;
            Ok(())
        })();
        if let Err(e) = staged {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        let file_name = source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let panorama = Panorama {
            id,
            guid: id.to_string(),
            name: source
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            source_name: file_name.clone(),
            file: format!("data/{import_id}/panorama.{extension}"),
            format,
            width,
            height,
        };
        // Where the scans are, rather than at the origin, which may lie far
        // away in georeferenced projects.
        let start = self
            .scans()
            .next()
            .is_some()
            .then(|| self.bounds().center());
        let mut next = self.clone();
        next.manifest.panoramas.push(panorama);
        next.edit(
            json!({"kind": "import_panorama", "file": file_name, "panorama": id}),
            |s| {
                s.panoramas.push(id);
                if let Some(start) = start {
                    s.transforms.insert(
                        id,
                        Pose {
                            translation: start.to_array(),
                            ..Pose::default()
                        },
                    );
                }
                Ok(())
            },
        )?;
        *self = next;
        Ok(id)
    }
    /// Renames a panorama. A name equal to the imported one keeps no rename.
    pub fn rename_panorama(&mut self, id: Uuid, name: &str) -> Result<()> {
        ensure!(self.panorama(id).is_some(), "Missing panorama");
        self.rename_scans(&[(id, name.to_owned())])
    }
    /// Places a panorama: sets its own transform to `own`, keeps the
    /// correspondences it was found with and records the fit. One edit.
    pub fn place_panorama(
        &mut self,
        id: Uuid,
        own: Pose,
        pairs: Vec<PanoramaPair>,
        rms_degrees: f64,
    ) -> Result<()> {
        ensure!(self.panorama(id).is_some(), "Missing panorama");
        let placed = Pose::from_matrix(self.correction_with(id, id, own));
        let registration = Registration {
            fit: AlignmentFit {
                method: RegistrationMethod::Panorama,
                rms: rms_degrees,
                overlap: None,
                distance: None,
                stopped: false,
                references: pairs.len(),
            },
            placed,
            at: crate::history::now(),
        };
        self.edit(
            json!({"kind": "place_panorama", "id": id, "pose": own, "pairs": pairs.len()}),
            |s| {
                if own == Pose::default() {
                    s.transforms.remove(&id);
                } else {
                    s.transforms.insert(id, own);
                }
                s.registrations.insert(id, registration);
                s.panorama_pairs.insert(id, pairs);
                Ok(())
            },
        )
    }
    /// Where a panorama is in the project frame: its folders' transforms and
    /// its own, or with `own` in place of its own.
    pub fn panorama_matrix(&self, id: Uuid, own: Option<Pose>) -> DMat4 {
        match own {
            Some(pose) => self.correction_with(id, id, pose),
            None => self.correction(id),
        }
    }
    /// The own transform that puts a panorama at `world` in the project frame.
    pub fn panorama_own_pose(&self, id: Uuid, world: DMat4) -> Pose {
        let above = self
            .parent_of(id)
            .map_or(DMat4::IDENTITY, |g| self.correction(g));
        Pose::from_matrix(above.inverse() * world)
    }
    /// Places a panorama from its correspondences whose scans are in the
    /// current state: each pair's pixel direction and point.
    pub fn panorama_rays(
        &self,
        panorama: &Panorama,
        pairs: &[PanoramaPair],
    ) -> (Vec<DVec3>, Vec<DVec3>) {
        pairs
            .iter()
            .filter_map(|pair| {
                let scan = self.scan(pair.scan)?;
                let point = self
                    .world_matrix(scan)
                    .transform_point3(DVec3::from(pair.local));
                Some((panorama.bearing(pair.pixel), point))
            })
            .unzip()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panorama() -> Panorama {
        Panorama {
            id: Uuid::nil(),
            guid: String::new(),
            name: String::new(),
            source_name: String::new(),
            file: String::new(),
            format: PanoramaFormat::Jpeg,
            width: 4000,
            height: 2000,
        }
    }

    #[test]
    fn pixels_follow_the_e57_spherical_convention() {
        let p = panorama();
        // The centre looks along +X, a quarter to the left along +Y, the top up.
        assert!(p.bearing([2000., 1000.]).abs_diff_eq(DVec3::X, 1e-12));
        assert!(p.bearing([1000., 1000.]).abs_diff_eq(DVec3::Y, 1e-12));
        assert!(p.bearing([2000., 0.]).abs_diff_eq(DVec3::Z, 1e-12));
        for pixel in [[0.5, 3.25], [1234.5, 999.], [3999.9, 1500.]] {
            let back = p.pixel(p.bearing(pixel));
            assert!(
                p.pixel_distance(back, pixel) < 1e-6,
                "{pixel:?} -> {back:?}"
            );
        }
        assert!((p.pixel_distance([1., 5.], [3999., 5.]) - 2.).abs() < 1e-9);
    }

    /// Correspondences of a panorama at `world` seeing points around it.
    fn scene(world: DMat4) -> (Vec<DVec3>, Vec<DVec3>) {
        let points: Vec<DVec3> = (0..8)
            .map(|i| {
                let a = i as f64 * 0.8;
                DVec3::new(
                    a.cos() * (3. + i as f64),
                    a.sin() * (4. - i as f64 * 0.3),
                    (i % 3) as f64 - 1.2,
                ) + DVec3::new(1000., -2000., 50.)
            })
            .collect();
        let inverse = world.inverse();
        let bearings = points
            .iter()
            .map(|p| inverse.transform_point3(*p).normalize())
            .collect();
        (bearings, points)
    }

    #[test]
    fn finds_a_levelled_panorama_from_any_heading() {
        for yaw in [0.3, 2.0, -2.9] {
            let world = DMat4::from_rotation_translation(
                DQuat::from_rotation_z(yaw),
                DVec3::new(1000.5, -1999.2, 50.4),
            );
            let (bearings, points) = scene(world);
            let s = solve_panorama(&bearings, &points, true).unwrap();
            assert!(s.world.abs_diff_eq(world, 1e-6), "{yaw}: {:?}", s.world);
            assert!(s.rms < 1e-6);
        }
    }

    #[test]
    fn finds_a_tilted_panorama_and_reports_a_wrong_pair() {
        let world = DMat4::from_rotation_translation(
            DQuat::from_rotation_z(1.1)
                * DQuat::from_rotation_x(0.05)
                * DQuat::from_rotation_y(-0.03),
            DVec3::new(1001., -2000.5, 49.8),
        );
        let (mut bearings, points) = scene(world);
        let s = solve_panorama(&bearings, &points, false).unwrap();
        assert!(s.world.abs_diff_eq(world, 1e-6));
        // Levelled, the tilt shows as residuals.
        assert!(solve_panorama(&bearings, &points, true).unwrap().rms > 0.5);
        // A pixel 10 degrees off stands out among the residuals.
        bearings[3] = DQuat::from_rotation_z(10f64.to_radians()) * bearings[3];
        let s = solve_panorama(&bearings, &points, false).unwrap();
        let worst = s
            .residuals
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(worst, 3);
    }

    #[test]
    fn needs_four_pairs() {
        let (bearings, points) = scene(DMat4::IDENTITY);
        assert!(solve_panorama(&bearings[..3], &points[..3], true).is_err());
    }
}
