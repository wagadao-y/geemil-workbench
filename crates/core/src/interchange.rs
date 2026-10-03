//! Numeric E57 point attributes stay in their original types. A small E57 template
//! stores scan metadata and image blobs, without a second copy of the point cloud.
use crate::parallel::OrderedPool;
use crate::storage::{index, make_record};
use crate::{Bounds, CoreError, ImageInfo, ImportOptions, JobControl, Pose, Project, Scan, Stage};
use anyhow::{Context, Result, ensure};
use e57::{
    E57Reader, E57Writer, PointCloud, PointCloudWriter, Projection, Record, RecordDataType,
    RecordName, RecordValue,
};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
};
use uuid::Uuid;

pub fn raw_size(prototype: &[Record]) -> usize {
    prototype
        .iter()
        .map(|p| match p.data_type {
            RecordDataType::Single { .. } => 4,
            _ => 8,
        })
        .sum()
}
fn encode(values: &[RecordValue]) -> Vec<u8> {
    let mut b = vec![];
    for v in values {
        match v {
            RecordValue::Single(v) => b.extend(v.to_le_bytes()),
            RecordValue::Double(v) => b.extend(v.to_le_bytes()),
            RecordValue::Integer(v) | RecordValue::ScaledInteger(v) => b.extend(v.to_le_bytes()),
        }
    }
    b
}
pub fn decode(bytes: &[u8], prototype: &[Record]) -> Result<Vec<RecordValue>> {
    ensure!(
        bytes.len() == raw_size(prototype),
        "Point record schema mismatch"
    );
    let mut offset = 0;
    Ok(prototype
        .iter()
        .map(|p| {
            let n = if matches!(p.data_type, RecordDataType::Single { .. }) {
                4
            } else {
                8
            };
            let b = &bytes[offset..offset + n];
            offset += n;
            match p.data_type {
                RecordDataType::Single { .. } => {
                    RecordValue::Single(f32::from_le_bytes(b.try_into().unwrap()))
                }
                RecordDataType::Double { .. } => {
                    RecordValue::Double(f64::from_le_bytes(b.try_into().unwrap()))
                }
                RecordDataType::Integer { .. } => {
                    RecordValue::Integer(i64::from_le_bytes(b.try_into().unwrap()))
                }
                RecordDataType::ScaledInteger { .. } => {
                    RecordValue::ScaledInteger(i64::from_le_bytes(b.try_into().unwrap()))
                }
            }
        })
        .collect())
}
/// Position, display colour and validity of an E57 record. Without colour,
/// intensity is shown as grey, stretched over `grey` (the scan's typical
/// intensity range) when given, else over its declared limits.
fn geometry(
    pc: &PointCloud,
    values: &[RecordValue],
    grey: Option<(f64, f64)>,
) -> Result<([f64; 3], [u8; 4], bool)> {
    let value = |name: RecordName| -> Result<Option<f64>> {
        pc.prototype
            .iter()
            .position(|p| p.name == name)
            .map(|i| {
                values[i]
                    .to_f64(&pc.prototype[i].data_type)
                    .map_err(anyhow::Error::from)
            })
            .transpose()
    };
    let cart = [
        value(RecordName::CartesianX)?,
        value(RecordName::CartesianY)?,
        value(RecordName::CartesianZ)?,
    ];
    let mut p = [0.; 3];
    let mut ok = false;
    if let [Some(x), Some(y), Some(z)] = cart
        && value(RecordName::CartesianInvalidState)?.unwrap_or(0.) == 0.
    {
        p = [x, y, z];
        ok = true;
    }
    if !ok
        && let (Some(r), Some(a), Some(e)) = (
            value(RecordName::SphericalRange)?,
            value(RecordName::SphericalAzimuth)?,
            value(RecordName::SphericalElevation)?,
        )
        && value(RecordName::SphericalInvalidState)?.unwrap_or(0.) == 0.
    {
        p = [r * e.cos() * a.cos(), r * e.cos() * a.sin(), r * e.sin()];
        ok = true;
    }
    ok &= p.iter().all(|v| v.is_finite());
    if !ok {
        p = [0.; 3];
    }
    let mut color = [180, 195, 210, 255];
    let colored = pc.has_color() && value(RecordName::IsColorInvalid)?.unwrap_or(0.) == 0.;
    // Without colour, show intensity as grey, as scanner software does.
    if !colored
        && let Some(i) = pc
            .prototype
            .iter()
            .position(|p| p.name == RecordName::Intensity)
    {
        let dtype = &pc.prototype[i].data_type;
        let v = values[i].to_f64(dtype)?;
        let given = pc.intensity_limits.as_ref().and_then(|l| {
            Some((
                l.intensity_min.as_ref()?.to_f64(dtype).ok()?,
                l.intensity_max.as_ref()?.to_f64(dtype).ok()?,
            ))
        });
        let (min, max) = grey.or(given).unwrap_or(match *dtype {
            RecordDataType::Integer { min, max } => (min as f64, max as f64),
            RecordDataType::ScaledInteger {
                min,
                max,
                scale,
                offset,
            } => (min as f64 * scale + offset, max as f64 * scale + offset),
            _ => (0., 1.),
        });
        let grey = ((v - min) / (max - min).max(f64::EPSILON) * 255.)
            .clamp(0., 255.)
            .round() as u8;
        color = [grey, grey, grey, 255];
    }
    if colored {
        for (i, name) in [
            RecordName::ColorRed,
            RecordName::ColorGreen,
            RecordName::ColorBlue,
        ]
        .into_iter()
        .enumerate()
        {
            let v = value(name.clone())?.unwrap_or(0.);
            let dtype = &pc
                .prototype
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .data_type;
            let range = pc.color_limits.as_ref().and_then(|l| {
                let (min, max) = match i {
                    0 => (&l.red_min, &l.red_max),
                    1 => (&l.green_min, &l.green_max),
                    _ => (&l.blue_min, &l.blue_max),
                };
                Some((
                    min.as_ref()?.to_f64(dtype).ok()?,
                    max.as_ref()?.to_f64(dtype).ok()?,
                ))
            });
            let (min, max) = range.unwrap_or_else(|| match dtype {
                RecordDataType::Integer { min, max } => (*min as f64, *max as f64),
                RecordDataType::Single { min, max } => {
                    (min.unwrap_or(0.) as f64, max.unwrap_or(1.) as f64)
                }
                _ => (0., 65535.),
            });
            color[i] = ((v - min) / (max - min).max(f64::EPSILON) * 255.)
                .clamp(0., 255.)
                .round() as u8;
        }
    }
    Ok((p, color, ok))
}

pub(crate) fn copy_scan_metadata<T: Read + Write + std::io::Seek>(
    pc: &PointCloud,
    out: &mut PointCloudWriter<'_, T>,
    pose: Option<Pose>,
) {
    out.set_name(pc.name.clone());
    out.set_description(pc.description.clone());
    out.set_transform(pose.map(|p| p.to_e57()));
    out.set_color_limits(pc.color_limits.clone());
    out.set_intensity_limits(pc.intensity_limits.clone());
    out.set_acquisition_start(pc.acquisition_start.clone());
    out.set_acquisition_end(pc.acquisition_end.clone());
    out.set_sensor_vendor(pc.sensor_vendor.clone());
    out.set_sensor_model(pc.sensor_model.clone());
    out.set_sensor_serial(pc.sensor_serial.clone());
    out.set_sensor_hw_version(pc.sensor_hw_version.clone());
    out.set_sensor_sw_version(pc.sensor_sw_version.clone());
    out.set_sensor_fw_version(pc.sensor_fw_version.clone());
    out.set_temperature(pc.temperature);
    out.set_humidity(pc.humidity);
    out.set_atmospheric_pressure(pc.atmospheric_pressure);
}

fn copy_images(
    reader: &mut E57Reader<BufReader<File>>,
    writer: &mut E57Writer<File>,
    stage: &Path,
    scans: &[Scan],
    job: &JobControl,
    delta: impl Fn(Option<Uuid>) -> Pose,
) -> Result<Vec<ImageInfo>> {
    let mut result = vec![];
    for (i, image) in reader.images().into_iter().enumerate() {
        job.check()?;
        job.report(Stage::Images, i as u64, reader.images().len() as u64);
        let association = image
            .pointcloud_guid
            .as_ref()
            .and_then(|guid| scans.iter().find(|s| &s.guid == guid))
            .map(|s| s.id);
        let guid = image
            .guid
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut out = writer.add_image(&guid)?;
        if let Some(v) = &image.name {
            out.set_name(v);
        }
        if let Some(v) = &image.description {
            out.set_description(v);
        }
        if let Some(v) = &image.pointcloud_guid {
            out.set_pointcloud_guid(v);
        }
        if let Some(v) = &image.acquisition {
            out.set_acquisition(v.clone());
        }
        if let Some(v) = &image.sensor_vendor {
            out.set_sensor_vendor(v);
        }
        if let Some(v) = &image.sensor_model {
            out.set_sensor_model(v);
        }
        if let Some(v) = &image.sensor_serial {
            out.set_sensor_serial(v);
        }
        let correction = delta(association);
        let pose = image.transform.as_ref().map(Pose::from_e57);
        if pose.is_some() || correction != Pose::default() {
            out.set_transform(
                Pose::from_matrix(correction.matrix() * pose.unwrap_or_default().matrix()).to_e57(),
            );
        }
        let mut reps = vec![];
        if let Some(rep) = &image.visual_reference {
            reps.push(("visual", &rep.blob, rep.mask.as_ref()));
        }
        if let Some(rep) = &image.projection {
            let (blob, mask) = match rep {
                Projection::Spherical(v) => (&v.blob, v.mask.as_ref()),
                Projection::Pinhole(v) => (&v.blob, v.mask.as_ref()),
                Projection::Cylindrical(v) => (&v.blob, v.mask.as_ref()),
            };
            reps.push(("projected", blob, mask));
        }
        for (kind, blob, mask) in reps {
            let img_path = stage.join(format!("image-{i}-{kind}.tmp"));
            let mask_path = stage.join(format!("mask-{i}-{kind}.tmp"));
            reader.blob(&blob.data, &mut File::create(&img_path)?)?;
            if let Some(mask) = mask {
                reader.blob(mask, &mut File::create(&mask_path)?)?;
            }
            job.check()?;
            let mut img = File::open(&img_path)?;
            let mut mask_file = if mask.is_some() {
                Some(File::open(&mask_path)?)
            } else {
                None
            };
            let mask_reader = mask_file.as_mut().map(|f| f as &mut dyn Read);
            if kind == "visual" {
                out.add_visual_reference(
                    blob.format.clone(),
                    &mut img,
                    image.visual_reference.as_ref().unwrap().properties.clone(),
                    mask_reader,
                )?;
            } else {
                match image.projection.as_ref().unwrap() {
                    Projection::Spherical(v) => out.add_spherical(
                        blob.format.clone(),
                        &mut img,
                        v.properties.clone(),
                        mask_reader,
                    )?,
                    Projection::Pinhole(v) => out.add_pinhole(
                        blob.format.clone(),
                        &mut img,
                        v.properties.clone(),
                        mask_reader,
                    )?,
                    Projection::Cylindrical(v) => out.add_cylindrical(
                        blob.format.clone(),
                        &mut img,
                        v.properties.clone(),
                        mask_reader,
                    )?,
                }
            }
            drop(img);
            drop(mask_file);
            fs::remove_file(img_path)?;
            if mask.is_some() {
                fs::remove_file(mask_path)?;
            }
        }
        out.finalize()?;
        let projection = match image.projection {
            Some(Projection::Spherical(_)) => "spherical",
            Some(Projection::Pinhole(_)) => "pinhole",
            Some(Projection::Cylindrical(_)) => "cylindrical",
            None => "visual reference",
        };
        result.push(ImageInfo {
            guid: Some(guid),
            name: image.name,
            scan_id: association,
            pose,
            projection: projection.into(),
        });
    }
    Ok(result)
}

/// For a scan without colour, the 1st to 99th percentile of the intensity of
/// its first points, so grey uses the range the scan really has rather than
/// the declared limits (often much wider). None when there is nothing to do.
fn intensity_range<T: Read + std::io::Seek>(
    reader: &mut E57Reader<T>,
    pc: &PointCloud,
) -> Result<Option<(f64, f64)>> {
    let Some(i) = pc
        .prototype
        .iter()
        .position(|r| r.name == RecordName::Intensity)
        .filter(|_| !pc.has_color())
    else {
        return Ok(None);
    };
    let dtype = &pc.prototype[i].data_type;
    let mut values = vec![];
    for record in reader.pointcloud_raw(pc)?.take(500_000) {
        values.push(record?[i].to_f64(dtype)?);
    }
    values.retain(|v| v.is_finite());
    if values.len() < 100 {
        return Ok(None);
    }
    values.sort_by(f64::total_cmp);
    let at = |q: f64| values[((values.len() - 1) as f64 * q) as usize];
    let (low, high) = (at(0.01), at(0.99));
    Ok((high > low).then_some((low, high)))
}

struct GeometryBatch {
    data: Vec<u8>,
    bounds: Bounds,
    valid: u64,
    count: u64,
}

fn convert_e57_batch(
    pc: &PointCloud,
    values: Vec<Vec<RecordValue>>,
    grey: Option<(f64, f64)>,
    job: &JobControl,
) -> Result<GeometryBatch> {
    let stride = 32 + raw_size(&pc.prototype);
    let mut data = Vec::with_capacity(values.len() * stride);
    let mut bounds: Option<Bounds> = None;
    let mut valid = 0;
    let count = values.len() as u64;
    for (i, values) in values.into_iter().enumerate() {
        if i % 1024 == 0 {
            job.check()?;
        }
        let (p, color, ok) = geometry(pc, &values, grey)?;
        data.extend(make_record(p, color, ok, &encode(&values)));
        match &mut bounds {
            Some(b) => b.include(p),
            None => bounds = Some(Bounds::at(p)),
        }
        valid += ok as u64;
    }
    Ok(GeometryBatch {
        data,
        bounds: bounds.unwrap_or_default(),
        valid,
        count,
    })
}

fn write_geometry_batch(
    file: &mut impl Write,
    scan: &mut Scan,
    bounds: &mut Option<Bounds>,
    count: &mut u64,
    batch: GeometryBatch,
    job: &JobControl,
) -> Result<()> {
    job.check()?;
    file.write_all(&batch.data)?;
    match bounds {
        Some(bounds) => {
            bounds.include(batch.bounds.min);
            bounds.include(batch.bounds.max);
        }
        None => *bounds = Some(batch.bounds),
    }
    *count += batch.count;
    scan.valid_points += batch.valid;
    job.report(Stage::ReadingE57, *count, scan.records);
    Ok(())
}

fn import_e57(
    source: &Path,
    stage: &Path,
    options: ImportOptions,
    job: &JobControl,
) -> Result<(Vec<Scan>, Vec<ImageInfo>)> {
    let mut reader = E57Reader::from_file(source)?;
    fs::write(stage.join("source.xml"), reader.xml())?;
    let mut template =
        E57Writer::from_file(stage.join("metadata.e57"), &Uuid::new_v4().to_string())?;
    template.set_coordinate_metadata(reader.coordinate_metadata().map(str::to_owned));
    template.set_creation(reader.creation());
    for ext in reader.extensions() {
        template.register_extension(ext)?;
    }
    let mut scans = vec![];
    for (i, pc) in reader.pointclouds().iter().enumerate() {
        job.check()?;
        let id = Uuid::new_v4();
        let guid = pc.guid.clone().unwrap_or_else(|| id.to_string());
        let mut scan = Scan {
            id,
            guid: guid.clone(),
            name: pc.name.clone().unwrap_or_else(|| format!("Scan {}", i + 1)),
            source_name: source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            template: "metadata.e57".into(),
            template_index: i,
            original_pose: pc.transform.as_ref().map(Pose::from_e57),
            stride: 32 + raw_size(&pc.prototype),
            records: pc.records,
            valid_points: 0,
            omitted_attributes: vec![],
            points_file: format!("{id}.points"),
            lod_file: format!("{id}.lod"),
            chunks: vec![],
            nodes: vec![],
        };
        ensure!(
            scan.stride <= 1_048_576,
            "Point attributes exceed record budget"
        );
        let spool = stage.join(format!("{id}.spool"));
        let mut file = BufWriter::new(File::create(&spool)?);
        let mut bounds: Option<Bounds> = None;
        let mut count = 0;
        job.report(Stage::ReadingE57, 0, pc.records);
        let workers = options.workers()?;
        // Includes RecordValue vectors, output bytes and temporary encodings.
        // Also cover Vec capacity growth for a one-point batch with unusually
        // wide attributes, not just normal 8K-point batches.
        let bytes_per_point =
            24 + pc.prototype.len() * std::mem::size_of::<RecordValue>() + scan.stride * 6;
        ensure!(
            bytes_per_point <= options.worker_memory_bytes,
            "E57 point exceeds conversion memory budget"
        );
        let batch_points = (options.worker_memory_bytes / bytes_per_point).clamp(1, 8192);
        let reservation = batch_points * bytes_per_point;
        let grey = intensity_range(&mut reader, pc)?;
        let worker_pc = pc.clone();
        let worker_job = job.clone();
        let mut pool =
            OrderedPool::new(workers, options.worker_memory_bytes, job, move |values| {
                convert_e57_batch(&worker_pc, values, grey, &worker_job)
            })?;
        let mut batch = Vec::with_capacity(batch_points);
        let mut read_count = 0;
        for values in reader.pointcloud_raw(pc)? {
            if batch.is_empty() {
                job.check()?;
                while !pool.has_capacity(reservation) {
                    write_geometry_batch(
                        &mut file,
                        &mut scan,
                        &mut bounds,
                        &mut count,
                        pool.pop()?.unwrap(),
                        job,
                    )?;
                }
            }
            batch.push(values?);
            read_count += 1;
            if batch.len() == batch_points {
                pool.submit(batch, reservation)?;
                batch = Vec::with_capacity(batch_points);
            }
        }
        if !batch.is_empty() {
            pool.submit(batch, reservation)?;
        }
        while let Some(batch) = pool.pop()? {
            write_geometry_batch(&mut file, &mut scan, &mut bounds, &mut count, batch, job)?;
        }
        drop(pool);
        ensure!(read_count == pc.records, "E57 decoded point count mismatch");
        ensure!(count == pc.records, "E57 point count mismatch");
        file.flush()?;
        drop(file);
        index(
            &mut scan,
            &spool,
            stage,
            bounds.unwrap_or_default(),
            options,
            job,
        )?;
        let mut out = template.add_pointcloud(&guid, pc.prototype.clone())?;
        copy_scan_metadata(pc, &mut out, scan.original_pose);
        out.set_cartesian_bounds(pc.cartesian_bounds.clone());
        out.set_spherical_bounds(pc.spherical_bounds.clone());
        out.set_index_bounds(pc.index_bounds.clone());
        out.finalize()?;
        scans.push(scan);
    }
    let images = copy_images(&mut reader, &mut template, stage, &scans, job, |_| {
        Pose::default()
    })?;
    template.finalize()?;
    drop(template);
    fs::OpenOptions::new()
        .write(true)
        .open(stage.join("metadata.e57"))?
        .sync_all()?;
    Ok((scans, images))
}

fn import_las(
    source: &Path,
    stage: &Path,
    options: ImportOptions,
    job: &JobControl,
) -> Result<(Vec<Scan>, Vec<ImageInfo>)> {
    let mut reader = las::Reader::from_path(source)?;
    let header = reader.header().clone();
    let id = Uuid::new_v4();
    let mut omitted = vec![];
    if header.point_format().has_gps_time {
        omitted.push("GPS time".into());
    }
    if header.point_format().extra_bytes > 0 {
        omitted.push("Extra Bytes".into());
    }
    omitted.push("Classification, return and other LAS attributes (prototype scope)".into());
    let mut schema = vec![
        Record {
            name: RecordName::CartesianX,
            data_type: RecordDataType::F64,
        },
        Record {
            name: RecordName::CartesianY,
            data_type: RecordDataType::F64,
        },
        Record {
            name: RecordName::CartesianZ,
            data_type: RecordDataType::F64,
        },
        Record {
            name: RecordName::Intensity,
            data_type: RecordDataType::Integer { min: 0, max: 65535 },
        },
    ];
    if header.point_format().has_color {
        for name in [
            RecordName::ColorRed,
            RecordName::ColorGreen,
            RecordName::ColorBlue,
        ] {
            schema.push(Record {
                name,
                data_type: RecordDataType::Integer { min: 0, max: 65535 },
            });
        }
    }
    let mut writer = E57Writer::from_file(stage.join("metadata.e57"), &Uuid::new_v4().to_string())?;
    if let Some(vlr) = header
        .vlrs()
        .iter()
        .find(|v| v.user_id == "LASF_Projection" && v.record_id == 2112)
    {
        writer.set_coordinate_metadata(Some(
            String::from_utf8(vlr.data.clone())?
                .trim_end_matches('\0')
                .into(),
        ));
    }
    let mut pcwriter = writer.add_pointcloud(&id.to_string(), schema.clone())?;
    let name = source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    pcwriter.set_name(Some(name.clone()));
    pcwriter.finalize()?;
    writer.finalize()?;
    drop(writer);
    let mut scan = Scan {
        id,
        guid: id.to_string(),
        name,
        source_name: source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        template: "metadata.e57".into(),
        template_index: 0,
        original_pose: None,
        stride: 32 + raw_size(&schema),
        records: header.number_of_points(),
        valid_points: 0,
        omitted_attributes: omitted,
        points_file: format!("{id}.points"),
        lod_file: format!("{id}.lod"),
        chunks: vec![],
        nodes: vec![],
    };
    let spool = stage.join(format!("{id}.spool"));
    let mut out = BufWriter::new(File::create(&spool)?);
    let mut bounds: Option<Bounds> = None;
    let mut count = 0;
    let mut batch = las::PointDataBuilder::new()
        .for_header(reader.header())
        .build();
    loop {
        job.check()?;
        job.report(Stage::ReadingLas, count, scan.records);
        if reader.fill_points(8192, &mut batch)? == 0 {
            break;
        }
        for point in batch.points() {
            let point = point?;
            let p = [point.x, point.y, point.z];
            ensure!(
                p.iter().all(|v: &f64| v.is_finite()),
                "LAS has non-finite coordinates"
            );
            let mut raw = vec![
                RecordValue::Double(p[0]),
                RecordValue::Double(p[1]),
                RecordValue::Double(p[2]),
                RecordValue::Integer(point.intensity as i64),
            ];
            let mut color = [180, 195, 210, 255];
            if header.point_format().has_color {
                let c = point.color.unwrap_or_default();
                raw.extend([
                    RecordValue::Integer(c.red as i64),
                    RecordValue::Integer(c.green as i64),
                    RecordValue::Integer(c.blue as i64),
                ]);
                color = [
                    (c.red >> 8) as u8,
                    (c.green >> 8) as u8,
                    (c.blue >> 8) as u8,
                    255,
                ];
            }
            out.write_all(&make_record(p, color, true, &encode(&raw)))?;
            match &mut bounds {
                Some(b) => b.include(p),
                None => bounds = Some(Bounds::at(p)),
            };
            count += 1;
        }
    }
    ensure!(count == scan.records, "LAS point count mismatch");
    scan.valid_points = count;
    out.flush()?;
    drop(out);
    index(
        &mut scan,
        &spool,
        stage,
        bounds.unwrap_or_default(),
        options,
        job,
    )?;
    Ok((vec![scan], vec![]))
}

impl Project {
    pub fn import_file(
        &mut self,
        source: &Path,
        options: ImportOptions,
        job: &JobControl,
    ) -> Result<()> {
        options.workers()?;
        ensure!(
            options.chunk_points > 0 && (1..=65_536).contains(&options.lod_points),
            "Invalid import limits"
        );
        job.check()?;
        let import_id = Uuid::new_v4();
        let stage = self.root.join("staging").join(import_id.to_string());
        fs::create_dir(&stage)?;
        let extension = source
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let (mut scans, images) = match extension.as_str() {
            "e57" => import_e57(source, &stage, options, job),
            "las" | "laz" => import_las(source, &stage, options, job),
            _ => Err(CoreError::UnsupportedFormat.into()),
        }
        .with_context(|| format!("Importing {}", source.display()))?;
        job.check()?;
        let prefix = format!("data/{import_id}");
        fs::rename(&stage, self.path(&prefix)?).context("Publishing imported assets")?;
        for s in &mut scans {
            s.template = format!("{prefix}/{}", s.template);
            s.points_file = format!("{prefix}/{}", s.points_file);
            s.lod_file = format!("{prefix}/{}", s.lod_file);
        }
        let file = source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let ids: Vec<_> = scans.iter().map(|s| s.id).collect();
        let mut next = self.clone();
        next.manifest.scans.extend(scans);
        next.manifest.images.extend(images);
        // Several scans from one file share a folder named after it.
        let group = (ids.len() > 1).then(Uuid::new_v4);
        next.edit(
            serde_json::json!({"kind": "import", "file": file, "scans": ids, "group": group}),
            |s| {
                s.scans.extend(&ids);
                if let Some(group) = group {
                    let name = Path::new(&file).file_stem().unwrap_or_default();
                    s.groups.push(crate::Group {
                        id: group,
                        name: name.to_string_lossy().into_owned(),
                        parent: None,
                    });
                    s.scan_groups.extend(ids.iter().map(|id| (*id, group)));
                }
                Ok(())
            },
        )?;
        *self = next;
        Ok(())
    }

    pub fn export_e57(&self, destination: &Path, job: &JobControl) -> Result<()> {
        ensure!(
            !destination.exists(),
            CoreError::OutputExists(destination.to_owned())
        );
        let tmp = destination.with_file_name(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut writer = E57Writer::from_file(&tmp, &Uuid::new_v4().to_string())?;
            let mut templates = std::collections::BTreeSet::new();
            let mut coordinate_metadata: Option<Option<String>> = None;
            let mut registered = std::collections::BTreeMap::new();
            let active: Vec<_> = self.scans().cloned().collect();
            for scan in &active {
                let reader = E57Reader::from_file(self.path(&scan.template)?)?;
                let cm = reader.coordinate_metadata().map(str::to_owned);
                if let Some(existing) = &coordinate_metadata {
                    ensure!(&cm == existing, CoreError::CoordinateSystemMismatch);
                } else {
                    coordinate_metadata = Some(cm);
                }
                for extension in reader.extensions() {
                    if let Some(url) = registered.get(&extension.namespace) {
                        ensure!(
                            url == &extension.url,
                            "Conflicting E57 extension namespaces"
                        );
                    } else {
                        registered.insert(extension.namespace.clone(), extension.url.clone());
                        writer.register_extension(extension)?;
                    }
                }
                templates.insert(scan.template.clone());
                let pc = reader
                    .pointclouds()
                    .get(scan.template_index)
                    .cloned()
                    .context("Missing scan template")?;
                let corrected = !self
                    .correction(scan.id)
                    .abs_diff_eq(glam::DMat4::IDENTITY, 0.);
                let pose = if scan.original_pose.is_some() || corrected {
                    Some(Pose::from_matrix(self.world_matrix(scan)))
                } else {
                    None
                };
                let mut out = writer.add_pointcloud(&scan.guid, pc.prototype.clone())?;
                copy_scan_metadata(&pc, &mut out, pose);
                for id in 0..scan.chunks.len() {
                    job.check()?;
                    job.report(Stage::WritingE57, id as u64, scan.chunks.len() as u64);
                    let data = self.read_chunk(scan, id as u32)?;
                    let mask = self.exclusion_mask(scan, id as u32)?;
                    for (i, record) in data.chunks_exact(scan.stride).enumerate() {
                        if i % 8192 == 0 {
                            job.check()?;
                        }
                        if !crate::edit::is_excluded(&mask, i) {
                            out.add_point(decode(&record[32..], &pc.prototype)?)?;
                        }
                    }
                }
                out.finalize()?;
            }
            writer.set_coordinate_metadata(coordinate_metadata.flatten());
            let stage = self.root.join("staging").join(Uuid::new_v4().to_string());
            fs::create_dir(&stage)?;
            for template in templates {
                let mut reader = E57Reader::from_file(self.path(&template)?)?;
                let owned: Vec<_> = active
                    .iter()
                    .filter(|s| s.template == template)
                    .cloned()
                    .collect();
                // Images move with their scan, including its folders' transforms.
                copy_images(&mut reader, &mut writer, &stage, &owned, job, |id| {
                    id.map_or_else(Pose::default, |id| Pose::from_matrix(self.correction(id)))
                })?;
            }
            fs::remove_dir(&stage)?;
            writer.finalize()?;
            drop(writer);
            fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
            job.check()?;
            ensure!(
                !destination.exists(),
                "Output was created by another process"
            );
            fs::rename(&tmp, destination)?;
            Ok(())
        })();
        if result.is_err() && tmp.exists() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

/// A tiny structured two-scan E57 with two independently posed images per scan.
pub fn create_demo(path: &Path) -> Result<()> {
    ensure!(!path.exists(), "Demo file already exists");
    let mut writer = E57Writer::from_file(path, &Uuid::new_v4().to_string())?;
    let prototype = vec![
        Record {
            name: RecordName::CartesianX,
            data_type: RecordDataType::F64,
        },
        Record {
            name: RecordName::CartesianY,
            data_type: RecordDataType::F64,
        },
        Record {
            name: RecordName::CartesianZ,
            data_type: RecordDataType::F64,
        },
        Record {
            name: RecordName::RowIndex,
            data_type: RecordDataType::Integer { min: 0, max: 127 },
        },
        Record {
            name: RecordName::ColumnIndex,
            data_type: RecordDataType::Integer { min: 0, max: 127 },
        },
        Record {
            name: RecordName::ColorRed,
            data_type: RecordDataType::U8,
        },
        Record {
            name: RecordName::ColorGreen,
            data_type: RecordDataType::U8,
        },
        Record {
            name: RecordName::ColorBlue,
            data_type: RecordDataType::U8,
        },
    ];
    // Valid 1x1 PNG, generated once; used only as a binary round-trip fixture.
    let png: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0,
        3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    for scan in 0..2 {
        let guid = Uuid::new_v4().to_string();
        let mut out = writer.add_pointcloud(&guid, prototype.clone())?;
        out.set_name(Some(format!("Demo scan {}", scan + 1)));
        out.set_transform(Some(
            Pose {
                translation: [scan as f64 * 3., 0., 0.],
                ..Pose::default()
            }
            .to_e57(),
        ));
        for row in 0..128 {
            for col in 0..128 {
                let x = col as f64 / 32. - 2.;
                let y = row as f64 / 32. - 2.;
                let z = (x * 2.).sin() * (y * 2.).cos() * 0.3;
                out.add_point(vec![
                    RecordValue::Double(x),
                    RecordValue::Double(y),
                    RecordValue::Double(z),
                    RecordValue::Integer(row),
                    RecordValue::Integer(col),
                    RecordValue::Integer((col * 2).min(255)),
                    RecordValue::Integer((row * 2).min(255)),
                    RecordValue::Integer(150),
                ])?;
            }
        }
        out.finalize()?;
        for side in [-1., 1.] {
            let mut image = writer.add_image(&Uuid::new_v4().to_string())?;
            image.set_pointcloud_guid(&guid);
            image.set_name(if side < 0. { "Left" } else { "Right" });
            image.set_transform(
                Pose {
                    translation: [scan as f64 * 3. + side * 0.1, 0., 0.1],
                    ..Pose::default()
                }
                .to_e57(),
            );
            image.add_spherical(
                e57::ImageFormat::Png,
                &mut &*png,
                e57::SphericalImageProperties {
                    width: 1,
                    height: 1,
                    pixel_width: std::f64::consts::TAU,
                    pixel_height: std::f64::consts::PI,
                },
                None,
            )?;
            image.finalize()?;
        }
    }
    writer.finalize()?;
    Ok(())
}
