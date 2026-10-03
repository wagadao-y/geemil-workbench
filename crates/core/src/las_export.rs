//! LAS/LAZ output of the current state: points in hidden layers are left out and
//! coordinates are in the common project frame, with all transforms applied.
//!
//! Without a coordinate system the files are LAS 1.2 (point format 2 with
//! colour, 0 without) for the widest compatibility. A WKT coordinate system
//! needs LAS 1.4 (formats 7 and 6). Intensity and colour come from the original
//! attributes, rescaled from their E57 ranges to 16 bits.
use crate::interchange::decode;
use crate::layers::is_set;
use crate::storage::{point_color, position, valid};
use crate::{CoreError, JobControl, Project, Scan, Stage};
use anyhow::{Context, Result, ensure};
use e57::{E57Reader, PointCloud, RecordDataType, RecordName, RecordValue};
use glam::DVec3;
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Where intensity and colour are in a scan's raw record, with the range each
/// maps from onto 0..=65535.
struct Attributes {
    prototype: Vec<e57::Record>,
    intensity: Option<(usize, f64, f64)>,
    color: Option<[(usize, f64, f64); 3]>,
    color_invalid: Option<usize>,
}
impl Attributes {
    fn new(pc: &PointCloud) -> Self {
        let find = |name: RecordName| pc.prototype.iter().position(|r| r.name == name);
        let range = |i: usize, limits: Option<(&Option<RecordValue>, &Option<RecordValue>)>| {
            let dtype = &pc.prototype[i].data_type;
            let limit = |v: &Option<RecordValue>| v.as_ref()?.to_f64(dtype).ok();
            let given = limits.and_then(|(min, max)| Some((limit(min)?, limit(max)?)));
            let (min, max) = given.unwrap_or(match *dtype {
                RecordDataType::Integer { min, max } => (min as f64, max as f64),
                RecordDataType::ScaledInteger {
                    min,
                    max,
                    scale,
                    offset,
                } => (min as f64 * scale + offset, max as f64 * scale + offset),
                RecordDataType::Single { min, max } => {
                    (min.unwrap_or(0.) as f64, max.unwrap_or(1.) as f64)
                }
                RecordDataType::Double { min, max } => (min.unwrap_or(0.), max.unwrap_or(1.)),
            });
            (i, min, max)
        };
        let intensity = find(RecordName::Intensity).map(|i| {
            range(
                i,
                pc.intensity_limits
                    .as_ref()
                    .map(|l| (&l.intensity_min, &l.intensity_max)),
            )
        });
        let color = (|| {
            let l = pc.color_limits.as_ref();
            Some([
                range(
                    find(RecordName::ColorRed)?,
                    l.map(|l| (&l.red_min, &l.red_max)),
                ),
                range(
                    find(RecordName::ColorGreen)?,
                    l.map(|l| (&l.green_min, &l.green_max)),
                ),
                range(
                    find(RecordName::ColorBlue)?,
                    l.map(|l| (&l.blue_min, &l.blue_max)),
                ),
            ])
        })();
        Self {
            prototype: pc.prototype.clone(),
            intensity,
            color,
            color_invalid: find(RecordName::IsColorInvalid),
        }
    }
    fn unit(&self, values: &[RecordValue], (i, min, max): (usize, f64, f64)) -> Result<u16> {
        let v = values[i].to_f64(&self.prototype[i].data_type)?;
        Ok(((v - min) / (max - min).max(f64::EPSILON) * 65535.)
            .clamp(0., 65535.)
            .round() as u16)
    }
}

/// Shortens a scan name to a safe, unique file stem.
fn file_stem(name: &str, used: &mut Vec<String>) -> String {
    let mut stem: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    stem = stem.trim().trim_end_matches('.').to_owned();
    if stem.is_empty() {
        stem = "scan".into();
    }
    let base = stem.clone();
    let mut n = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&stem)) {
        stem = format!("{base}_{n}");
        n += 1;
    }
    used.push(stem.clone());
    stem
}

impl Project {
    /// Writes the current state's scans into one LAS file, or LAZ when the
    /// destination ends in `.laz`. Returns the number of points written.
    pub fn export_las(&self, destination: &Path, job: &JobControl) -> Result<u64> {
        let scans: Vec<_> = self.scans().collect();
        self.write_las(destination, &scans, job, 0, 1)
    }
    /// Writes one LAS (or LAZ) file per scan of the current state into
    /// `directory`, named after the scans. No existing file is replaced.
    pub fn export_las_per_scan(
        &self,
        directory: &Path,
        laz: bool,
        job: &JobControl,
    ) -> Result<Vec<PathBuf>> {
        let scans: Vec<_> = self.scans().collect();
        let mut used = vec![];
        let paths: Vec<_> = scans
            .iter()
            .map(|s| {
                let stem = file_stem(&s.name, &mut used);
                directory.join(format!("{stem}.{}", if laz { "laz" } else { "las" }))
            })
            .collect();
        for path in &paths {
            ensure!(!path.exists(), CoreError::OutputExists(path.clone()));
        }
        for (i, (scan, path)) in scans.iter().zip(&paths).enumerate() {
            self.write_las(path, &[scan], job, i, scans.len())?;
        }
        Ok(paths)
    }
    /// Writes `scans` to `destination` through a temporary file. `part` of
    /// `parts` places this file's progress within a multi-file export.
    fn write_las(
        &self,
        destination: &Path,
        scans: &[&Scan],
        job: &JobControl,
        part: usize,
        parts: usize,
    ) -> Result<u64> {
        ensure!(
            !destination.exists(),
            CoreError::OutputExists(destination.to_owned())
        );
        let laz = destination
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("laz"));
        // Keep the extension: the writer picks LAZ compression from it.
        let tmp = destination.with_file_name(format!(
            ".{}.tmp.{}",
            Uuid::new_v4(),
            if laz { "laz" } else { "las" }
        ));
        let result = self.write_las_file(&tmp, scans, job, part, parts);
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
            return result;
        }
        let renamed = (|| {
            fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
            job.check()?;
            ensure!(
                !destination.exists(),
                "Output was created by another process"
            );
            fs::rename(&tmp, destination)?;
            Ok(())
        })();
        if renamed.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        renamed.and(result)
    }
    fn write_las_file(
        &self,
        path: &Path,
        scans: &[&Scan],
        job: &JobControl,
        part: usize,
        parts: usize,
    ) -> Result<u64> {
        let mut attributes = vec![];
        let mut crs: Option<Option<String>> = None;
        for scan in scans {
            let reader = E57Reader::from_file(self.path(&scan.template)?)?;
            let cm = reader
                .coordinate_metadata()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            if let Some(existing) = &crs {
                ensure!(&cm == existing, CoreError::CoordinateSystemMismatch);
            } else {
                crs = Some(cm);
            }
            let pc = reader
                .pointclouds()
                .get(scan.template_index)
                .cloned()
                .context("Missing scan template")?;
            attributes.push(Attributes::new(&pc));
        }
        let crs = crs.flatten();
        let has_color = attributes.iter().any(|a| a.color.is_some());
        // Bounds in the project frame, for an offset near the data and a scale
        // that keeps 32-bit coordinates in range.
        let mut lo = DVec3::INFINITY;
        let mut hi = DVec3::NEG_INFINITY;
        for scan in scans {
            if let Some(root) = scan.nodes.first() {
                let world = self.world_matrix(scan);
                for c in root.bounds.corners() {
                    let p = world.transform_point3(c);
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
            }
        }
        if !lo.is_finite() {
            (lo, hi) = (DVec3::ZERO, DVec3::ZERO);
        }
        let extent = (hi - lo).max_element();
        let scale = [1e-4, 1e-3, 1e-2, 1e-1]
            .into_iter()
            .find(|s| extent / s < 2e9)
            .context("Extent too large for LAS coordinates")?;
        let offset = lo.floor();
        let mut builder = las::Builder::from(if crs.is_some() { (1, 4) } else { (1, 2) });
        builder.point_format = las::point::Format::new(match (crs.is_some(), has_color) {
            (false, false) => 0,
            (false, true) => 2,
            (true, false) => 6,
            (true, true) => 7,
        })?;
        builder.generating_software = format!("Geemil Workbench {}", env!("CARGO_PKG_VERSION"));
        builder.transforms = las::Vector {
            x: las::Transform {
                scale,
                offset: offset.x,
            },
            y: las::Transform {
                scale,
                offset: offset.y,
            },
            z: las::Transform {
                scale,
                offset: offset.z,
            },
        };
        let mut header = builder.into_header()?;
        if let Some(wkt) = &crs {
            header.set_wkt_crs(wkt.as_bytes().to_vec())?;
        }
        let mut writer = las::Writer::from_path(path, header)?;
        let total: u64 = scans.iter().map(|s| s.chunks.len() as u64).sum::<u64>() * parts as u64;
        let mut done = (part as u64) * scans.iter().map(|s| s.chunks.len() as u64).sum::<u64>();
        let mut written = 0;
        for (scan, attributes) in scans.iter().zip(&attributes) {
            let world = self.world_matrix(scan);
            for chunk in 0..scan.chunks.len() as u32 {
                job.check()?;
                job.report(Stage::WritingLas, done, total);
                done += 1;
                let data = self.read_chunk(scan, chunk)?;
                let hidden = self.hidden_mask(scan, chunk)?;
                for (i, record) in data.chunks_exact(scan.stride).enumerate() {
                    if i % 8192 == 0 {
                        job.check()?;
                    }
                    if !valid(record) || is_set(&hidden, i) {
                        continue;
                    }
                    let p = world.transform_point3(DVec3::from(position(record)));
                    let values = decode(&record[32..], &attributes.prototype)?;
                    let intensity = match attributes.intensity {
                        Some(range) => attributes.unit(&values, range)?,
                        None => 0,
                    };
                    let color = if !has_color {
                        None
                    } else if let Some(ranges) = attributes.color.filter(|_| {
                        attributes.color_invalid.is_none_or(|i| {
                            values[i]
                                .to_f64(&attributes.prototype[i].data_type)
                                .is_ok_and(|v| v == 0.)
                        })
                    }) {
                        Some(las::Color::new(
                            attributes.unit(&values, ranges[0])?,
                            attributes.unit(&values, ranges[1])?,
                            attributes.unit(&values, ranges[2])?,
                        ))
                    } else {
                        // Scans without colour keep the colour they are shown in.
                        let [r, g, b, _] = point_color(record).map(|c| c as u16 * 257);
                        Some(las::Color::new(r, g, b))
                    };
                    writer.write_point(las::Point {
                        x: p.x,
                        y: p.y,
                        z: p.z,
                        intensity,
                        color,
                        gps_time: crs.is_some().then_some(0.),
                        ..Default::default()
                    })?;
                    written += 1;
                }
            }
        }
        writer.close()?;
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::file_stem;

    #[test]
    fn file_stems_are_safe_and_unique() {
        let mut used = vec![];
        assert_eq!(file_stem("Scan 1", &mut used), "Scan 1");
        assert_eq!(file_stem("scan 1", &mut used), "scan 1_2");
        assert_eq!(file_stem("a/b:c?", &mut used), "a_b_c_");
        assert_eq!(file_stem(" . ", &mut used), "scan");
        assert_eq!(file_stem("", &mut used), "scan_2");
    }
}
