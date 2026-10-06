//! LAS/LAZ output of the current state: points in hidden layers are left out and
//! coordinates are in the common project frame, with all transforms applied.
//!
//! Source LAS attributes and VLR/EVLR definitions survive LAS/LAZ output. E57
//! intensity and colour are rescaled to 16 bits. The output point format is the
//! union of supported fields; users may explicitly omit incompatible fields.
use crate::coords::{point_color, valid};
use crate::interchange::{decode, raw_size};
use crate::layers::is_set;
use crate::{CoreError, JobControl, LasVlr, Project, Scan, Stage};
use anyhow::{Context, Result, ensure};
use e57::{E57Reader, PointCloud, RecordDataType, RecordName, RecordValue};
use glam::DVec3;
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Loss is permitted only by an explicit export choice. Stored data is unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LasExportPolicy {
    #[default]
    Preserve,
    OmitIncompatible,
}

/// Preview of fields omitted by `OmitIncompatible`, shared with the writer.
#[derive(Clone, Debug, Default)]
pub struct LasExportCompatibility {
    pub omit_extra_bytes: bool,
    pub omit_gps_time: bool,
    pub omit_crs: bool,
    pub omitted_metadata: Vec<(String, u16)>,
    crs: Option<String>,
    vlrs: Vec<LasVlr>,
    evlrs: Vec<LasVlr>,
    gps_standard: Option<bool>,
}
impl LasExportCompatibility {
    pub fn has_conflicts(&self) -> bool {
        self.omit_extra_bytes
            || self.omit_gps_time
            || self.omit_crs
            || !self.omitted_metadata.is_empty()
    }
}

fn is_extra_definition(v: &LasVlr) -> bool {
    v.user_id == "LASF_Spec" && v.record_id == 4
}

fn same_definition(a: &LasVlr, b: &LasVlr) -> bool {
    a.user_id == b.user_id && a.record_id == b.record_id && a.data == b.data
}

fn definitions(scan: &Scan) -> Vec<&LasVlr> {
    scan.las
        .as_ref()
        .map(|m| {
            m.vlrs
                .iter()
                .chain(&m.evlrs)
                .filter(|v| is_extra_definition(v))
                .collect()
        })
        .unwrap_or_default()
}
fn projection(scan: &Scan) -> Vec<&LasVlr> {
    scan.las
        .as_ref()
        .map(|m| {
            m.vlrs
                .iter()
                .chain(&m.evlrs)
                .filter(|v| v.user_id == "LASF_Projection" && v.record_id != 2112)
                .collect()
        })
        .unwrap_or_default()
}
fn metadata_records(scan: &Scan, evlr: bool) -> Option<&Vec<LasVlr>> {
    scan.las
        .as_ref()
        .map(|m| if evlr { &m.evlrs } else { &m.vlrs })
}

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
    /// Inspects header definitions only; no point chunks are loaded.
    pub fn las_export_compatibility(&self) -> Result<LasExportCompatibility> {
        self.las_compatibility(&self.scans().collect::<Vec<_>>())
    }

    fn las_compatibility(&self, scans: &[&Scan]) -> Result<LasExportCompatibility> {
        let mut result = LasExportCompatibility::default();
        let mut coordinate_metadata = Vec::new();
        for scan in scans {
            let reader = E57Reader::from_file(self.path(&scan.template)?)?;
            coordinate_metadata.push(
                reader
                    .coordinate_metadata()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        }
        result.crs = coordinate_metadata.first().cloned().flatten();
        result.omit_crs = coordinate_metadata.iter().any(|cm| cm != &result.crs);
        let source = scans.iter().find_map(|s| s.las.as_ref());
        result.gps_standard = scans
            .iter()
            .filter_map(|s| s.las.as_ref())
            .find(|m| m.format().is_ok_and(|f| f.has_gps_time))
            .map(|m| m.gps_standard);
        if let Some(first) = source {
            let first_definitions = first
                .vlrs
                .iter()
                .chain(&first.evlrs)
                .filter(|v| is_extra_definition(v))
                .collect::<Vec<_>>();
            result.omit_extra_bytes = scans.iter().any(|s| {
                s.las.as_ref().map_or(0, |m| m.extra_bytes) != first.extra_bytes
                    || definitions(s).len() != first_definitions.len()
                    || definitions(s)
                        .iter()
                        .any(|v| !first_definitions.iter().any(|f| same_definition(v, f)))
            });
            result.omit_gps_time = scans
                .iter()
                .filter_map(|s| s.las.as_ref())
                .filter(|m| m.format().is_ok_and(|f| f.has_gps_time))
                .any(|m| Some(m.gps_standard) != result.gps_standard);
            // GeoTIFF definitions also carry a CRS, independently of template WKT.
            let first_projection = first
                .vlrs
                .iter()
                .chain(&first.evlrs)
                .filter(|v| v.user_id == "LASF_Projection" && v.record_id != 2112)
                .collect::<Vec<_>>();
            result.omit_crs |= scans.iter().any(|s| {
                projection(s).len() != first_projection.len()
                    || projection(s)
                        .iter()
                        .any(|v| !first_projection.iter().any(|f| same_definition(v, f)))
            });
            for evlr in [false, true] {
                for scan in scans {
                    for v in metadata_records(scan, evlr)
                        .into_iter()
                        .flatten()
                        .filter(|v| v.is_output_metadata())
                    {
                        let omitted = (result.omit_crs && v.user_id == "LASF_Projection")
                            || (result.omit_extra_bytes && is_extra_definition(v))
                            || !scans.iter().all(|s| {
                                metadata_records(s, evlr).is_some_and(|r| {
                                    r.iter().any(|other| same_definition(v, other))
                                })
                            });
                        if omitted {
                            let key = (v.user_id.clone(), v.record_id);
                            if !result.omitted_metadata.contains(&key) {
                                result.omitted_metadata.push(key);
                            }
                        } else {
                            let dest = if evlr {
                                &mut result.evlrs
                            } else {
                                &mut result.vlrs
                            };
                            if !dest.iter().any(|other| same_definition(v, other)) {
                                dest.push(v.clone());
                            }
                        }
                    }
                }
            }
        }
        if result.omit_crs {
            result.crs = None;
        }
        Ok(result)
    }

    /// Writes the current state's scans into one LAS file, or LAZ when the
    /// destination ends in `.laz`. Returns the number of points written.
    pub fn export_las(&self, destination: &Path, job: &JobControl) -> Result<u64> {
        self.export_las_with_policy(destination, LasExportPolicy::Preserve, job)
    }
    pub fn export_las_with_policy(
        &self,
        destination: &Path,
        policy: LasExportPolicy,
        job: &JobControl,
    ) -> Result<u64> {
        let scans: Vec<_> = self.scans().collect();
        self.write_las(destination, &scans, job, 0, 1, policy)
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
                let stem = file_stem(self.scan_name(s), &mut used);
                directory.join(format!("{stem}.{}", if laz { "laz" } else { "las" }))
            })
            .collect();
        for path in &paths {
            ensure!(!path.exists(), CoreError::OutputExists(path.clone()));
        }
        for (i, (scan, path)) in scans.iter().zip(&paths).enumerate() {
            self.write_las(
                path,
                &[scan],
                job,
                i,
                scans.len(),
                LasExportPolicy::Preserve,
            )?;
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
        policy: LasExportPolicy,
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
        let result = self.write_las_file(&tmp, scans, job, part, parts, policy);
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
        policy: LasExportPolicy,
    ) -> Result<u64> {
        let compatibility = self.las_compatibility(scans)?;
        if policy == LasExportPolicy::Preserve {
            ensure!(!compatibility.omit_crs, CoreError::CoordinateSystemMismatch);
            ensure!(
                !compatibility.has_conflicts(),
                CoreError::LasMetadataMismatch
            );
        }
        let mut attributes = vec![];
        for scan in scans {
            let reader = E57Reader::from_file(self.path(&scan.template)?)?;
            let pc = reader
                .pointclouds()
                .get(scan.template_index)
                .cloned()
                .context("Missing scan template")?;
            attributes.push(Attributes::new(&pc));
        }
        let crs = &compatibility.crs;
        let has_color = attributes.iter().any(|a| a.color.is_some());
        let source = scans.iter().find_map(|s| s.las.as_ref());
        let source_formats = scans
            .iter()
            .filter_map(|s| s.las.as_ref().map(|m| m.format()))
            .collect::<Result<Vec<_>>>()?;
        let has_gps = crs.is_some()
            || (!compatibility.omit_gps_time && source_formats.iter().any(|f| f.has_gps_time));
        let has_nir = source_formats.iter().any(|f| f.has_nir);
        let extended = crs.is_some() || has_nir || source_formats.iter().any(|f| f.is_extended);
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
        let version =
            source
                .map_or((1, 2), |m| m.version)
                .max(if extended { (1, 4) } else { (1, 2) });
        let mut builder = las::Builder::from(version);
        builder.point_format = las::point::Format::new(if extended {
            if has_nir {
                8
            } else if has_color {
                7
            } else {
                6
            }
        } else {
            match (has_gps, has_color) {
                (false, false) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (true, true) => 3,
            }
        })?;
        if let Some(meta) = source {
            builder.point_format.extra_bytes = if compatibility.omit_extra_bytes {
                0
            } else {
                meta.extra_bytes
            };
            builder.gps_time_type = if compatibility.omit_gps_time
                || compatibility.gps_standard.unwrap_or(meta.gps_standard)
            {
                las::GpsTimeType::Standard
            } else {
                las::GpsTimeType::Week
            };
            builder.file_source_id = if scans.len() == 1 {
                meta.file_source_id
            } else {
                0
            };
            builder.system_identifier = meta.system_identifier.clone();
            builder.has_synthetic_return_numbers = scans
                .iter()
                .filter_map(|s| s.las.as_ref())
                .any(|m| m.synthetic_returns);
            builder.has_wkt_crs = !compatibility.omit_crs && meta.has_wkt_crs;
            builder.vlrs = compatibility
                .vlrs
                .iter()
                .filter(|v| v.is_output_metadata())
                .map(las::Vlr::from)
                .collect();
            builder.evlrs = compatibility
                .evlrs
                .iter()
                .filter(|v| v.is_output_metadata())
                .map(las::Vlr::from)
                .collect();
        }
        let output_format = builder.point_format;
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
        if let Some(wkt) = &crs
            && header.get_wkt_crs_bytes().is_none()
        {
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
                    let p = world.transform_point3(DVec3::from(scan.coordinates.position(record)));
                    if let Some(meta) = &scan.las {
                        let raw = las::raw::Point::read_from(
                            &record[meta.record_offset..],
                            &meta.format()?,
                        )?;
                        let mut point = las::Point::new(raw, &Default::default());
                        point.x = p.x;
                        point.y = p.y;
                        point.z = p.z;
                        if compatibility.omit_extra_bytes {
                            point.extra_bytes.clear();
                        }
                        if compatibility.omit_gps_time {
                            point.gps_time = output_format.has_gps_time.then_some(0.);
                        }
                        if output_format.has_gps_time && point.gps_time.is_none() {
                            point.gps_time = Some(0.);
                        }
                        if output_format.has_color && point.color.is_none() {
                            let [r, g, b, _] = point_color(record).map(|c| c as u16 * 257);
                            point.color = Some(las::Color::new(r, g, b));
                        }
                        if output_format.has_nir && point.nir.is_none() {
                            point.nir = Some(0);
                        }
                        writer.write_point(point)?;
                        written += 1;
                        continue;
                    }
                    let values = decode(
                        &record[crate::coords::HEAD
                            ..crate::coords::HEAD + raw_size(&attributes.prototype)],
                        &attributes.prototype,
                    )?;
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
                        gps_time: output_format.has_gps_time.then_some(0.),
                        nir: output_format.has_nir.then_some(0),
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
