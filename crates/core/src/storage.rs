use crate::codec::{pack, read_block, write_block};
use crate::parallel::OrderedPool;
use crate::{BlockCodec, Bounds, Chunk, CoreError, Node, Project, Scan, Stage};
use anyhow::{Result, ensure};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

pub const SAMPLE_BYTES: usize = 36;
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub chunk: u32,
    pub index: u32,
    pub position: [f64; 3],
    pub color: [u8; 4],
}
#[derive(Clone)]
pub struct JobControl {
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<ProgressCallback>,
}
pub type ProgressCallback = dyn Fn(Stage, u64, u64) + Send + Sync;
impl Default for JobControl {
    fn default() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(|_, _, _| {}),
        }
    }
}
impl JobControl {
    pub fn check(&self) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), CoreError::Cancelled);
        Ok(())
    }
    pub fn report(&self, stage: Stage, done: u64, total: u64) {
        (self.progress)(stage, done, total);
    }
}
#[derive(Clone, Copy)]
pub struct ImportOptions {
    pub chunk_points: u32,
    pub lod_points: usize,
    /// Zero chooses available CPUs minus one, capped at eight workers.
    pub worker_threads: usize,
    /// Budget for in-flight conversion buffers, excluding project metadata.
    pub worker_memory_bytes: usize,
}
impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            chunk_points: 65_536,
            lod_points: 2048,
            worker_threads: 0,
            worker_memory_bytes: 256 * 1024 * 1024,
        }
    }
}
impl ImportOptions {
    pub(crate) fn workers(self) -> Result<usize> {
        ensure!(self.worker_threads <= 64, "Too many conversion workers");
        ensure!(
            self.worker_memory_bytes >= 1024 * 1024,
            "Conversion memory budget is below 1 MiB"
        );
        Ok(if self.worker_threads == 0 {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 8))
        } else {
            self.worker_threads
        })
    }
}

pub(crate) fn position(record: &[u8]) -> [f64; 3] {
    std::array::from_fn(|i| f64::from_le_bytes(record[i * 8..i * 8 + 8].try_into().unwrap()))
}
pub(crate) fn point_color(record: &[u8]) -> [u8; 4] {
    record[24..28].try_into().unwrap()
}
pub(crate) fn valid(record: &[u8]) -> bool {
    record[28] != 0
}
pub(crate) fn make_record(p: [f64; 3], color: [u8; 4], is_valid: bool, raw: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(32 + raw.len());
    for x in p {
        v.extend(x.to_le_bytes());
    }
    v.extend(color);
    v.extend([is_valid as u8, 0, 0, 0]);
    v.extend(raw);
    v
}
pub(crate) fn hash(chunk: u32, index: u32) -> u64 {
    let mut v = ((chunk as u64) << 32) | index as u64;
    v = v.wrapping_add(0x9e3779b97f4a7c15);
    v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
    v ^ (v >> 31)
}
fn write_sample(w: &mut impl Write, s: &Sample) -> Result<()> {
    w.write_all(&s.chunk.to_le_bytes())?;
    w.write_all(&s.index.to_le_bytes())?;
    for x in s.position {
        w.write_all(&x.to_le_bytes())?;
    }
    w.write_all(&s.color)?;
    Ok(())
}
fn read_sample(r: &mut impl Read) -> Result<Sample> {
    let mut b = [0u8; SAMPLE_BYTES];
    r.read_exact(&mut b)?;
    Ok(Sample {
        chunk: u32::from_le_bytes(b[0..4].try_into()?),
        index: u32::from_le_bytes(b[4..8].try_into()?),
        position: std::array::from_fn(|i| {
            f64::from_le_bytes(b[8 + i * 8..16 + i * 8].try_into().unwrap())
        }),
        color: b[32..36].try_into()?,
    })
}

struct Builder<'a> {
    scan: &'a mut Scan,
    points: BufWriter<File>,
    lod: BufWriter<File>,
    point_offset: u64,
    processed_points: u64,
    lod_offset: u64,
    options: ImportOptions,
    job: &'a JobControl,
    pool: OrderedPool<LeafTask, PackedLeaf>,
}

struct LeafTask {
    chunk: u32,
    node: u32,
    data: Vec<u8>,
}
struct PackedLeaf {
    chunk: u32,
    node: u32,
    points: (BlockCodec, Vec<u8>),
    lod: (BlockCodec, Vec<u8>),
    lod_count: u32,
}

fn leaf_memory(count: usize, stride: usize, lod_points: usize) -> usize {
    // Input, shuffle, compressed output/raw fallback, candidate sort and LOD
    // scratch/output. Reservations remain live until the writer consumes them.
    count * (stride * 4 + 64) + count.min(lod_points) * SAMPLE_BYTES * 6
}

fn pack_leaf(
    task: LeafTask,
    stride: usize,
    lod_points: usize,
    job: &JobControl,
) -> Result<PackedLeaf> {
    job.check()?;
    let mut candidates = Vec::with_capacity(task.data.len() / stride);
    candidates.extend(
        task.data
            .chunks_exact(stride)
            .enumerate()
            .filter(|(_, b)| valid(b))
            .map(|(i, b)| {
                (
                    hash(task.chunk, i as u32),
                    Sample {
                        chunk: task.chunk,
                        index: i as u32,
                        position: position(b),
                        color: point_color(b),
                    },
                )
            }),
    );
    // Only the smallest K hashes are needed; avoid sorting the entire chunk.
    if candidates.len() > lod_points {
        candidates.select_nth_unstable_by_key(lod_points, |s| s.0);
        candidates.truncate(lod_points);
    }
    candidates.sort_unstable_by_key(|s| s.0);
    job.check()?;
    let mut lod = Vec::with_capacity(candidates.len() * SAMPLE_BYTES);
    for (_, sample) in &candidates {
        write_sample(&mut lod, sample)?;
    }
    let points = pack(&task.data, stride)?;
    job.check()?;
    Ok(PackedLeaf {
        chunk: task.chunk,
        node: task.node,
        points,
        lod: pack(&lod, SAMPLE_BYTES)?,
        lod_count: candidates.len() as u32,
    })
}

impl Builder<'_> {
    fn node(&mut self, path: PathBuf, count: u64, bounds: Bounds, depth: u32) -> Result<u32> {
        self.job.check()?;
        let id = u32::try_from(self.scan.nodes.len())?;
        self.scan.nodes.push(Node {
            bounds,
            children: vec![],
            chunk: None,
            lod_offset: 0,
            lod_count: 0,
            point_count: count,
            lod_codec: BlockCodec::Raw,
            lod_bytes: 0,
        });
        let stride = self.scan.stride;
        // Coincident points and extreme coordinate ranges still split by bounded file batches.
        if count <= self.options.chunk_points as u64 || depth >= 40 || bounds.radius() < 1e-12 {
            let mut reader = BufReader::new(File::open(&path)?);
            let mut remaining = count;
            while remaining > 0 {
                self.job.check()?;
                let n = remaining.min(self.options.chunk_points as u64) as u32;
                let reservation = leaf_memory(n as usize, stride, self.options.lod_points);
                while !self.pool.has_capacity(reservation) {
                    self.finish_leaf()?;
                }
                let mut data = vec![0; n as usize * stride];
                reader.read_exact(&mut data)?;
                let chunk = u32::try_from(self.scan.chunks.len())?;
                self.scan.chunks.push(Chunk {
                    offset: 0,
                    count: n,
                    bounds,
                    codec: BlockCodec::Raw,
                    stored_bytes: 0,
                });
                let leaf_id = if count <= self.options.chunk_points as u64 {
                    id
                } else {
                    let child = u32::try_from(self.scan.nodes.len())?;
                    self.scan.nodes.push(Node {
                        bounds,
                        children: vec![],
                        chunk: None,
                        lod_offset: 0,
                        lod_count: 0,
                        point_count: n as u64,
                        lod_codec: BlockCodec::Raw,
                        lod_bytes: 0,
                    });
                    self.scan.nodes[id as usize].children.push(child);
                    child
                };
                self.scan.nodes[leaf_id as usize].chunk = Some(chunk);
                self.pool.submit(
                    LeafTask {
                        chunk,
                        node: leaf_id,
                        data,
                    },
                    reservation,
                )?;
                remaining -= n as u64;
            }
            drop(reader);
            fs::remove_file(&path)?;
            return Ok(id);
        }
        let mid = bounds.center().to_array();
        let child_paths: Vec<_> = (0..8)
            .map(|i| path.with_file_name(format!("{}-{i}.bin", Uuid::new_v4())))
            .collect();
        let mut writers: Vec<_> = child_paths
            .iter()
            .map(|p| File::create(p).map(BufWriter::new))
            .collect::<std::io::Result<_>>()?;
        let mut counts = [0u64; 8];
        let mut boxes: [Option<Bounds>; 8] = [None; 8];
        let mut reader = BufReader::new(File::open(&path)?);
        let mut record = vec![0; stride];
        for i in 0..count {
            if i % 8192 == 0 {
                self.job.check()?;
                self.job.report(Stage::Partitioning, i, count);
            }
            reader.read_exact(&mut record)?;
            let p = position(&record);
            let c = (p[0] >= mid[0]) as usize
                | (((p[1] >= mid[1]) as usize) << 1)
                | (((p[2] >= mid[2]) as usize) << 2);
            writers[c].write_all(&record)?;
            counts[c] += 1;
            match &mut boxes[c] {
                Some(b) => b.include(p),
                None => boxes[c] = Some(Bounds::at(p)),
            };
        }
        for w in &mut writers {
            w.flush()?;
        }
        drop(writers);
        drop(reader);
        fs::remove_file(&path)?;
        for c in 0..8 {
            if counts[c] == 0 {
                fs::remove_file(&child_paths[c])?;
                continue;
            }
            let child = self.node(
                child_paths[c].clone(),
                counts[c],
                boxes[c].unwrap(),
                depth + 1,
            )?;
            self.scan.nodes[id as usize].children.push(child);
        }
        Ok(id)
    }
    fn finish_leaf(&mut self) -> Result<bool> {
        let Some(leaf) = self.pool.pop()? else {
            return Ok(false);
        };
        self.job.check()?;
        let chunk = &mut self.scan.chunks[leaf.chunk as usize];
        chunk.offset = self.point_offset;
        chunk.codec = leaf.points.0;
        chunk.stored_bytes = leaf.points.1.len() as u32;
        self.points.write_all(&leaf.points.1)?;
        self.point_offset += leaf.points.1.len() as u64;
        self.processed_points += chunk.count as u64;
        let node = &mut self.scan.nodes[leaf.node as usize];
        node.lod_offset = self.lod_offset;
        node.lod_count = leaf.lod_count;
        node.lod_codec = leaf.lod.0;
        node.lod_bytes = leaf.lod.1.len() as u32;
        self.lod.write_all(&leaf.lod.1)?;
        self.lod_offset += leaf.lod.1.len() as u64;
        self.job
            .report(Stage::Indexing, self.processed_points, self.scan.records);
        Ok(true)
    }
    fn finish_parents(&mut self, root: &Path) -> Result<()> {
        self.lod.flush()?;
        let mut reader = BufReader::new(File::open(root.join(&self.scan.lod_file))?);
        // IDs are assigned in preorder, so every child is ready before its
        // parent. Read representatives from disk rather than retain all LODs.
        for id in (0..self.scan.nodes.len()).rev() {
            self.job.check()?;
            if self.scan.nodes[id].chunk.is_some() {
                continue;
            }
            self.job.report(
                Stage::BuildingParentLod,
                (self.scan.nodes.len() - id) as u64,
                self.scan.nodes.len() as u64,
            );
            let mut samples = vec![];
            for child in &self.scan.nodes[id].children {
                self.job.check()?;
                let node = &self.scan.nodes[*child as usize];
                reader.seek(SeekFrom::Start(node.lod_offset))?;
                let bytes = read_block(
                    &mut reader,
                    node.lod_codec,
                    node.lod_bytes as usize,
                    node.lod_count as usize * SAMPLE_BYTES,
                    SAMPLE_BYTES,
                )?;
                let mut bytes = bytes.as_slice();
                for _ in 0..node.lod_count {
                    samples.push(read_sample(&mut bytes)?);
                }
                self.reduce(&mut samples);
            }
            self.store_lod(id as u32, &samples)?;
            self.lod.flush()?;
        }
        Ok(())
    }
    fn reduce(&self, samples: &mut Vec<Sample>) {
        samples.sort_unstable_by_key(|s| hash(s.chunk, s.index));
        samples.truncate(self.options.lod_points);
    }
    fn store_lod(&mut self, id: u32, samples: &[Sample]) -> Result<()> {
        let n = &mut self.scan.nodes[id as usize];
        n.lod_offset = self.lod_offset;
        n.lod_count = samples.len() as u32;
        let mut data = Vec::with_capacity(samples.len() * SAMPLE_BYTES);
        for s in samples {
            write_sample(&mut data, s)?;
        }
        let (codec, stored) = write_block(&mut self.lod, &data, SAMPLE_BYTES)?;
        n.lod_codec = codec;
        n.lod_bytes = stored;
        self.lod_offset += stored as u64;
        Ok(())
    }
}
pub(crate) fn index(
    scan: &mut Scan,
    spool: &Path,
    root: &Path,
    bounds: Bounds,
    options: ImportOptions,
    job: &JobControl,
) -> Result<()> {
    ensure!(
        options.chunk_points > 0 && (1..=65_536).contains(&options.lod_points),
        "Invalid import limits"
    );
    let max = (32 * 1024 * 1024 / scan.stride)
        .max(1)
        .min(options.chunk_points as usize) as u32;
    let workers = options.workers()?;
    // Shrink unusually wide-attribute chunks to fit even a single worker job.
    let mut chunk_points = max;
    while leaf_memory(chunk_points as usize, scan.stride, options.lod_points)
        > options.worker_memory_bytes
    {
        ensure!(
            chunk_points > 1,
            "One point exceeds conversion memory budget"
        );
        chunk_points = chunk_points.div_ceil(2);
    }
    let stride = scan.stride;
    let worker_job = job.clone();
    let pool = OrderedPool::new(workers, options.worker_memory_bytes, job, move |task| {
        pack_leaf(task, stride, options.lod_points, &worker_job)
    })?;
    let mut b = Builder {
        points: BufWriter::new(File::create(root.join(&scan.points_file))?),
        lod: BufWriter::new(File::create(root.join(&scan.lod_file))?),
        point_offset: 0,
        processed_points: 0,
        lod_offset: 0,
        options: ImportOptions {
            chunk_points,
            ..options
        },
        scan,
        job,
        pool,
    };
    b.node(spool.to_owned(), b.scan.records, bounds, 0)?;
    while b.finish_leaf()? {}
    b.finish_parents(root)?;
    job.check()?;
    b.points.flush()?;
    b.lod.flush()?;
    b.points.get_ref().sync_all()?;
    b.lod.get_ref().sync_all()?;
    Ok(())
}

impl Project {
    pub fn read_chunk(&self, scan: &Scan, id: u32) -> Result<Vec<u8>> {
        let c = scan
            .chunks
            .get(id as usize)
            .ok_or_else(|| anyhow::anyhow!("Invalid chunk"))?;
        let size = (c.count as usize)
            .checked_mul(scan.stride)
            .ok_or_else(|| anyhow::anyhow!("Chunk size overflow"))?;
        ensure!(size <= 32 * 1024 * 1024, "Chunk exceeds format budget");
        let mut f = File::open(self.path(&scan.points_file)?)?;
        let stored = c.stored_bytes as usize;
        ensure!(
            c.offset
                .checked_add(stored as u64)
                .is_some_and(|end| end <= f.metadata().map(|m| m.len()).unwrap_or(0)),
            "Truncated point data"
        );
        f.seek(SeekFrom::Start(c.offset))?;
        read_block(&mut f, c.codec, stored, size, scan.stride)
    }
    pub fn read_lod(&self, scan: &Scan, node: u32) -> Result<Vec<Sample>> {
        let n = scan
            .nodes
            .get(node as usize)
            .ok_or_else(|| anyhow::anyhow!("Invalid node"))?;
        ensure!(n.lod_count <= 65_536, "LOD exceeds format budget");
        let mut f = File::open(self.path(&scan.lod_file)?)?;
        let expected = n.lod_count as usize * SAMPLE_BYTES;
        let stored = n.lod_bytes as usize;
        ensure!(
            n.lod_offset
                .checked_add(stored as u64)
                .is_some_and(|end| end <= f.metadata().map(|m| m.len()).unwrap_or(0)),
            "Truncated LOD"
        );
        f.seek(SeekFrom::Start(n.lod_offset))?;
        let data = read_block(&mut f, n.lod_codec, stored, expected, SAMPLE_BYTES)?;
        let mut reader = data.as_slice();
        (0..n.lod_count).map(|_| read_sample(&mut reader)).collect()
    }
}
