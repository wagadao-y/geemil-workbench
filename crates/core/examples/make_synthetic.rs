//! Writes LAS files that look like terrestrial scans of one site, for
//! measuring large projects: each scan sends rays evenly in angle from its own
//! position, so points are dense near the scanner, and records where they hit
//! the ground, a ring of walls and a few boxes.
//!
//! cargo run -p geemil-core --release --example make_synthetic -- OUT_DIR SCANS POINTS_PER_SCAN
use anyhow::{Result, ensure};
use glam::DVec3;
use std::path::Path;

/// Deterministic pseudo-random values in [0, 1).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Distance along `dir` from `origin` to the first surface, if any.
fn hit(origin: DVec3, dir: DVec3) -> Option<f64> {
    let mut best = f64::INFINITY;
    // Ground.
    if dir.z < -1e-6 {
        best = best.min(-origin.z / dir.z);
    }
    // A ring of walls, 60 m across and 15 m high, around the site centre.
    let (a, b, c) = (
        dir.x * dir.x + dir.y * dir.y,
        2. * (origin.x * dir.x + origin.y * dir.y),
        origin.x * origin.x + origin.y * origin.y - 60. * 60.,
    );
    if a > 1e-12 {
        let t = (-b + (b * b - 4. * a * c).max(0.).sqrt()) / (2. * a);
        if t > 0. && (origin.z + dir.z * t) < 15. {
            best = best.min(t);
        }
    }
    // Boxes: 8 m cubes on a grid.
    for i in -2..=2 {
        for j in -2..=2 {
            let lo = DVec3::new(i as f64 * 22. - 4., j as f64 * 22. - 4., 0.);
            let hi = lo + DVec3::new(8., 8., 6. + ((i + j) & 3) as f64 * 2.);
            let inv = dir.recip();
            let t0 = (lo - origin) * inv;
            let t1 = (hi - origin) * inv;
            let near = t0.min(t1).max_element();
            let far = t0.max(t1).min_element();
            if near > 0. && near <= far {
                best = best.min(near);
            }
        }
    }
    best.is_finite().then_some(best)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(args.len() == 3, "OUT_DIR SCANS POINTS_PER_SCAN");
    let out = Path::new(&args[0]);
    let scans: usize = args[1].parse()?;
    let points: u64 = args[2].parse()?;
    std::fs::create_dir_all(out)?;
    for s in 0..scans {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (s as u64 + 1) * 0x2545_f491_4f6c_dd1d);
        // Scanners on a spiral inside the walls, 1.6 m above the ground.
        let angle = s as f64 * 2.4;
        let radius = 8. + 40. * (s as f64 / scans.max(1) as f64);
        let origin = DVec3::new(radius * angle.cos(), radius * angle.sin(), 1.6);
        let path = out.join(format!("scan_{s:03}.las"));
        let mut builder = las::Builder::from((1, 2));
        builder.point_format = las::point::Format::new(2)?;
        for t in [
            &mut builder.transforms.x,
            &mut builder.transforms.y,
            &mut builder.transforms.z,
        ] {
            t.scale = 0.0005;
        }
        let mut writer = las::Writer::from_path(&path, builder.into_header()?)?;
        let mut written = 0;
        while written < points {
            // Even in angle, like a scanner's raster.
            let azimuth = rng.next() * std::f64::consts::TAU;
            let elevation = (rng.next() * 2. - 1.) * 1.2;
            let dir = DVec3::new(
                elevation.cos() * azimuth.cos(),
                elevation.cos() * azimuth.sin(),
                elevation.sin(),
            );
            let Some(t) = hit(origin, dir) else { continue };
            if t > 120. {
                continue;
            }
            let noise = (rng.next() - 0.5) * 0.004;
            let p = origin + dir * (t + noise);
            let shade = |v: f64| ((v.sin() * 0.5 + 0.5) * 50000.) as u16 + 8000;
            writer.write_point(las::Point {
                x: p.x,
                y: p.y,
                z: p.z,
                intensity: (60000. / (1. + t * 0.05)) as u16,
                color: Some(las::Color::new(
                    shade(p.x * 0.3),
                    shade(p.y * 0.3 + 1.),
                    shade(p.z * 0.7 + 2.),
                )),
                ..Default::default()
            })?;
            written += 1;
        }
        writer.close()?;
        println!("{}", path.display());
    }
    Ok(())
}
