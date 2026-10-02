//! Streaming, order-independent round-trip verification (not a cryptographic proof).
use anyhow::{Result, ensure};
use e57::{Blob, E57Reader, Image, PointCloud, Projection, RecordValue, Transform};
use geemil_core::Pose;
use std::{
    collections::hash_map::DefaultHasher,
    fs::File,
    hash::{Hash, Hasher},
    io::{self, BufReader, Write},
};
type Reader = E57Reader<BufReader<File>>;

fn points(reader: &mut Reader, pc: &PointCloud) -> Result<(u64, u64, u64)> {
    let (mut count, mut sum, mut xor) = (0u64, 0u64, 0u64);
    for row in reader.pointcloud_raw(pc)? {
        let mut hash = DefaultHasher::new();
        for value in row? {
            match value {
                RecordValue::Double(v) => v.to_bits().hash(&mut hash),
                RecordValue::Single(v) => v.to_bits().hash(&mut hash),
                RecordValue::Integer(v) | RecordValue::ScaledInteger(v) => v.hash(&mut hash),
            }
        }
        let h = hash.finish();
        count += 1;
        sum = sum.wrapping_add(h);
        xor ^= h;
    }
    Ok((count, sum, xor))
}

fn pose(a: &Option<Transform>, b: &Option<Transform>) -> Result<()> {
    ensure!(a.is_some() == b.is_some(), "Pose presence differs");
    if let (Some(a), Some(b)) = (a, b) {
        let a = Pose::from_e57(a).matrix().to_cols_array();
        let b = Pose::from_e57(b).matrix().to_cols_array();
        ensure!(
            a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9),
            "Pose differs"
        );
    }
    Ok(())
}

#[derive(Default)]
struct BlobDigest {
    hash: DefaultHasher,
    bytes: u64,
}
impl Write for BlobDigest {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.hash.write(bytes);
        self.bytes += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn blob(reader: &mut Reader, blob: &Blob) -> Result<(u64, u64)> {
    let mut digest = BlobDigest::default();
    reader.blob(blob, &mut digest)?;
    Ok((digest.bytes, digest.hash.finish()))
}
fn representations(image: &Image) -> (Vec<String>, Vec<Blob>) {
    let mut properties = vec![];
    let mut blobs = vec![];
    if let Some(rep) = &image.visual_reference {
        properties.push(format!("visual {:?} {:?}", rep.properties, rep.blob.format));
        blobs.push(rep.blob.data.clone());
        if let Some(mask) = &rep.mask {
            blobs.push(mask.clone());
        }
    }
    if let Some(rep) = &image.projection {
        let (kind, prop, data, mask) = match rep {
            Projection::Spherical(v) => {
                ("spherical", format!("{:?}", v.properties), &v.blob, &v.mask)
            }
            Projection::Pinhole(v) => ("pinhole", format!("{:?}", v.properties), &v.blob, &v.mask),
            Projection::Cylindrical(v) => (
                "cylindrical",
                format!("{:?}", v.properties),
                &v.blob,
                &v.mask,
            ),
        };
        properties.push(format!("{kind} {prop} {:?}", data.format));
        blobs.push(data.data.clone());
        if let Some(mask) = mask {
            blobs.push(mask.clone());
        }
    }
    (properties, blobs)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2,
        "Usage: verify_e57 ORIGINAL.e57 EXPORTED.e57"
    );
    let mut a = E57Reader::from_file(&args[0])?;
    let mut b = E57Reader::from_file(&args[1])?;
    ensure!(
        a.coordinate_metadata() == b.coordinate_metadata(),
        "Coordinate metadata differs"
    );
    ensure!(
        a.pointclouds().len() == b.pointclouds().len(),
        "Scan count differs"
    );
    let mut total = 0;
    for (pa, pb) in a.pointclouds().iter().zip(b.pointclouds().iter()) {
        ensure!(
            pa.guid == pb.guid && pa.name == pb.name,
            "Scan identity differs"
        );
        ensure!(
            format!("{:?}", pa.prototype) == format!("{:?}", pb.prototype),
            "Point schema differs"
        );
        pose(&pa.transform, &pb.transform)?;
        let original = points(&mut a, pa)?;
        ensure!(
            original == points(&mut b, pb)?,
            "Point attributes differ in {:?}",
            pa.name
        );
        ensure!(
            original.0 == pa.records && original.0 == pb.records,
            "Point count differs"
        );
        total += original.0;
        println!(
            "{:?}: {} records, RGB={}, raw attribute fingerprints match",
            pa.name,
            original.0,
            pa.has_color()
        );
    }
    ensure!(a.images().len() == b.images().len(), "Image count differs");
    for (ia, ib) in a.images().iter().zip(b.images().iter()) {
        ensure!(
            ia.guid == ib.guid && ia.name == ib.name && ia.pointcloud_guid == ib.pointcloud_guid,
            "Image identity/association differs"
        );
        pose(&ia.transform, &ib.transform)?;
        let (ap, ab) = representations(ia);
        let (bp, bb) = representations(ib);
        ensure!(ap == bp && ab.len() == bb.len(), "Image projection differs");
        for (ba, bb) in ab.iter().zip(&bb) {
            ensure!(
                blob(&mut a, ba)? == blob(&mut b, bb)?,
                "Image/mask bytes differ"
            );
        }
    }
    println!(
        "Verified {} scans, {total} points, {} images (including masks and independent poses)",
        a.pointclouds().len(),
        a.images().len()
    );
    Ok(())
}
