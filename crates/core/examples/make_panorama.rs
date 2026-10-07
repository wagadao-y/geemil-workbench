//! Renders a project's points into an equirectangular JPEG as a panorama
//! taken at a known place, for checking panorama placement. The pixels follow
//! the panorama convention (centre along +X, azimuth growing to the left), so
//! the photo placed at X Y Z turned by YAW degrees about Z sees the points
//! where they are.
use anyhow::{Result, ensure};
use geemil_core::{Panorama, PanoramaFormat, Project};
use glam::{DMat4, DQuat, DVec3};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 {
        // Where the scanners stood, as places to take the photo from.
        let project = Project::load(Path::new(&args[0]))?;
        println!("bounds {:?}", project.bounds());
        for scan in project.scans() {
            let at = project.world_matrix(scan).transform_point3(DVec3::ZERO);
            println!("{}: scanner at {:.3?}", scan.name, at.to_array());
        }
        return Ok(());
    }
    ensure!(
        args.len() >= 6,
        "Usage: make_panorama PROJECT [OUT.jpg X Y Z YAW_DEGREES [WIDTH]]"
    );
    let project = Project::load(Path::new(&args[0]))?;
    let [x, y, z, yaw] = [2, 3, 4, 5].map(|i| args[i].parse::<f64>());
    let place = DVec3::new(x?, y?, z?);
    let width: u32 = args.get(6).map(|w| w.parse()).transpose()?.unwrap_or(4096);
    let height = width / 2;
    let pose = DMat4::from_rotation_translation(DQuat::from_rotation_z(yaw?.to_radians()), place);
    let to_camera = pose.inverse();
    let panorama = Panorama {
        id: uuid::Uuid::nil(),
        guid: String::new(),
        name: String::new(),
        source_name: String::new(),
        file: String::new(),
        format: PanoramaFormat::Jpeg,
        width,
        height,
    };
    let pixel_angle = std::f64::consts::TAU / width as f64;
    let mut depth = vec![f32::INFINITY; (width * height) as usize];
    let mut image = image::RgbImage::from_fn(width, height, |_, y| {
        // A sky to floor gradient where no point is.
        let t = y as f32 / height as f32;
        image::Rgb([
            (150. - 90. * t) as u8,
            (180. - 110. * t) as u8,
            (210. - 120. * t) as u8,
        ])
    });
    let mut count = 0u64;
    for scan in project.scans() {
        let world = to_camera * project.world_matrix(scan);
        for node in 0..scan.nodes.len() as u32 {
            for sample in project.read_view(scan, node)? {
                let local = world.transform_point3(DVec3::from(sample.position));
                let distance = local.length();
                if distance < 0.05 {
                    continue;
                }
                let [px, py] = panorama.pixel(local);
                // Splats about 2 cm across, so near surfaces stay closed.
                let radius = ((0.02 / distance / pixel_angle).ceil() as i64).clamp(1, 12);
                for dy in -radius / 2..=radius / 2 {
                    for dx in -radius / 2..=radius / 2 {
                        let row = py as i64 + dy;
                        if row < 0 || row >= height as i64 {
                            continue;
                        }
                        let column = (px as i64 + dx).rem_euclid(width as i64);
                        let i = (row * width as i64 + column) as usize;
                        if (distance as f32) < depth[i] {
                            depth[i] = distance as f32;
                            let [r, g, b, _] = sample.color;
                            image.put_pixel(column as u32, row as u32, image::Rgb([r, g, b]));
                        }
                    }
                }
                count += 1;
            }
        }
    }
    image.save(&args[1])?;
    println!(
        "{count} points into {width} x {height} at {:.3?}, yaw {} degrees",
        place.to_array(),
        args[5]
    );
    Ok(())
}
