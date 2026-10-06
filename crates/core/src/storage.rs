use crate::codec::{MAX_BLOCK_BYTES, pack, read_block, read_block_columns};
use crate::parallel::OrderedPool;
use crate::{BlockCodec, Bounds, Chunk, CoreError, Node, Project, Scan, Stage};
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
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
    /// Display octree: cells along the longest side of a node's box; each
    /// node keeps one point per cell of what its children hold.
    pub view_grid: u32,
    /// Display octree nodes below a chunk split until they hold at most this
    /// many points.
    pub view_leaf_points: u32,
    /// Zero chooses available CPUs minus one, capped at 32 workers.
    pub worker_threads: usize,
    /// Budget for in-flight conversion buffers, excluding project metadata.
    /// Half of it may also hold a scan's records while indexing. The default
    /// is half the memory available when the options are made.
    pub worker_memory_bytes: usize,
}
impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            chunk_points: 65_536,
            view_grid: 128,
            view_leaf_points: 16_384,
            worker_threads: 0,
            worker_memory_bytes: default_memory_budget(),
        }
    }
}

/// Half the physical memory available now, at least 256 MiB.
fn default_memory_budget() -> usize {
    let floor = 256 * 1024 * 1024;
    available_memory()
        .map_or(floor, |m| usize::try_from(m / 2).unwrap_or(usize::MAX))
        .max(floor)
}

#[cfg(windows)]
fn available_memory() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // SAFETY: `status` is a correctly sized MEMORYSTATUSEX with its length set.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0).then_some(status.avail_phys)
}

#[cfg(target_os = "linux")]
fn available_memory() -> Option<u64> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn available_memory() -> Option<u64> {
    None
}
impl ImportOptions {
    /// Largest run of records loaded into memory at once while indexing.
    pub(crate) fn load_bytes(self) -> usize {
        self.worker_memory_bytes / 2
    }
    pub(crate) fn workers(self) -> Result<usize> {
        ensure!(self.worker_threads <= 64, "Too many conversion workers");
        ensure!(
            self.worker_memory_bytes >= 1024 * 1024,
            "Conversion memory budget is below 1 MiB"
        );
        Ok(if self.worker_threads == 0 {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 32))
        } else {
            self.worker_threads
        })
    }
}

/// Creates a scratch file that the OS keeps in memory while it can, rather
/// than writing it to disk.
pub(crate) fn create_scratch(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
        options.attributes(FILE_ATTRIBUTE_TEMPORARY);
    }
    options.open(path)
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
    push_record_head(&mut v, p, color, is_valid);
    v.extend(raw);
    v
}
/// The first 32 bytes of a point record, before its source values.
pub(crate) fn push_record_head(out: &mut Vec<u8>, p: [f64; 3], color: [u8; 4], is_valid: bool) {
    for x in p {
        out.extend(x.to_le_bytes());
    }
    out.extend(color);
    out.extend([is_valid as u8, 0, 0, 0]);
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

/// Splits a scan into chunks and builds its display octree.
///
/// Chunks are the leaves of an octree that stops splitting at the chunk size.
/// Workers pack each chunk and build the display subtree inside it; the
/// points left at a chunk's subtree root wait in a spool file. Afterwards the
/// octree above the chunks is walked bottom up: each node picks its points
/// from its children's waiting points, and what it leaves becomes theirs.
struct Builder<'a> {
    scan: &'a mut Scan,
    points: BufWriter<File>,
    view: BufWriter<File>,
    /// Points waiting at subtree roots for their parent to pick from.
    spool: BufWriter<File>,
    spool_path: PathBuf,
    /// Where each node's waiting points are in the spool.
    waiting: Vec<Option<(u64, u32)>>,
    spool_offset: u64,
    /// Nodes above the chunks, in creation (pre-)order.
    above: Vec<u32>,
    /// Display subtrees below the chunks, in chunk order, with children as
    /// indexes into the subtree (0 is the chunk's node). They are numbered
    /// after the octree above them, so the numbering does not depend on when
    /// workers finish.
    subtrees: Vec<(u32, Vec<usize>, Vec<Node>)>,
    point_offset: u64,
    processed_points: u64,
    view_offset: u64,
    options: ImportOptions,
    /// Largest region loaded at once; the next one loads while workers pack
    /// the last, besides the workers' budget.
    memory_bytes: usize,
    workers: usize,
    job: &'a JobControl,
    pool: OrderedPool<LeafTask, PackedLeaf>,
}

struct LeafTask {
    chunk: u32,
    node: u32,
    bounds: Bounds,
    data: LeafData,
}
/// A display node below a chunk's subtree root, in the subtree's own order.
struct PackedNode {
    bounds: Bounds,
    /// Indexes into the subtree's nodes, where 0 is the root.
    children: Vec<usize>,
    count: u32,
    chunks: Vec<(u32, u32)>,
    block: (BlockCodec, Vec<u8>),
}
struct PackedLeaf {
    chunk: u32,
    node: u32,
    points: (BlockCodec, Vec<u8>),
    /// The subtree root's children.
    root_children: Vec<usize>,
    /// Subtree nodes 1.., packed.
    nodes: Vec<PackedNode>,
    /// The points left at the subtree root, as spooled.
    waiting: Vec<u8>,
}

fn leaf_memory(count: usize, stride: usize) -> usize {
    // Input, shuffle and compressed output, plus the display subtree's samples,
    // picks and packed blocks. Reservations last until the writer takes them.
    count * (stride * 4 + 64 + SAMPLE_BYTES * 6)
}

/// Hashes grid cell keys, which need no protection against collisions.
#[derive(Clone, Copy, Default)]
struct CellHash;
impl std::hash::BuildHasher for CellHash {
    type Hasher = CellHasher;
    fn build_hasher(&self) -> CellHasher {
        CellHasher(0)
    }
}
struct CellHasher(u64);
impl std::hash::Hasher for CellHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(self.0 ^ b as u64);
        }
    }
    fn write_u64(&mut self, v: u64) {
        let h = (self.0 ^ v).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = h ^ (h >> 32);
    }
}

/// Picks one sample per cell of a grid with `grid` cells along the longest
/// side of `bounds`: the one nearest the cell centre, ties by chunk and index.
/// Returns whether each sample was picked.
fn grid_pick(samples: &[Sample], bounds: &Bounds, grid: u32) -> Vec<bool> {
    let min = glam::DVec3::from(bounds.min);
    let extent = (glam::DVec3::from(bounds.max) - min).max_element();
    let cell = if extent > 0. {
        extent / grid as f64
    } else {
        1.
    };
    let last = grid.saturating_sub(1) as u64;
    let mut best: HashMap<u64, (f64, u32, u32, usize), CellHash> =
        HashMap::with_capacity_and_hasher(samples.len().min(1 << 20), CellHash);
    for (i, s) in samples.iter().enumerate() {
        let p = (glam::DVec3::from(s.position) - min) / cell;
        let k = p.floor().as_u64vec3().min(glam::U64Vec3::splat(last));
        let key = k.x | (k.y << 21) | (k.z << 42);
        let centre = (k.as_dvec3() + 0.5) * cell + min;
        let score = (
            centre.distance_squared(glam::DVec3::from(s.position)),
            s.chunk,
            s.index,
            i,
        );
        let entry = best.entry(key).or_insert(score);
        if score
            .0
            .total_cmp(&entry.0)
            .then((score.1, score.2).cmp(&(entry.1, entry.2)))
            .is_lt()
        {
            *entry = score;
        }
    }
    let mut picked = vec![false; samples.len()];
    for (_, _, _, i) in best.into_values() {
        picked[i] = true;
    }
    picked
}

/// Points per chunk, by chunk.
fn chunk_counts(samples: &[Sample]) -> Vec<(u32, u32)> {
    let mut counts = std::collections::BTreeMap::new();
    for s in samples {
        *counts.entry(s.chunk).or_insert(0u32) += 1;
    }
    counts.into_iter().collect()
}

fn sample_bytes(samples: &[Sample]) -> Vec<u8> {
    let mut data = Vec::with_capacity(samples.len() * SAMPLE_BYTES);
    for s in samples {
        data.extend(s.chunk.to_le_bytes());
        data.extend(s.index.to_le_bytes());
        for x in s.position {
            data.extend(x.to_le_bytes());
        }
        data.extend(s.color);
    }
    data
}

fn pack_view(samples: &[Sample]) -> Result<(BlockCodec, Vec<u8>)> {
    pack(&sample_bytes(samples), SAMPLE_BYTES)
}

/// One node of a display subtree under construction.
struct SubNode {
    bounds: Bounds,
    children: Vec<usize>,
    samples: Vec<Sample>,
}

/// Builds the display subtree of `samples` into `nodes` and returns its root.
/// Leaves keep all their points; every other node picks its points from its
/// children's with `grid_pick`.
fn subtree(
    nodes: &mut Vec<SubNode>,
    samples: Vec<Sample>,
    bounds: Bounds,
    depth: u32,
    options: &ImportOptions,
    job: &JobControl,
) -> Result<usize> {
    job.check()?;
    let id = nodes.len();
    nodes.push(SubNode {
        bounds,
        children: vec![],
        samples: vec![],
    });
    if samples.len() <= options.view_leaf_points as usize || depth >= 24 || bounds.radius() < 1e-9 {
        nodes[id].samples = samples;
        return Ok(id);
    }
    let mid = bounds.center().to_array();
    let mut parts: [Vec<Sample>; 8] = Default::default();
    for s in samples {
        let p = s.position;
        let c = (p[0] >= mid[0]) as usize
            | (((p[1] >= mid[1]) as usize) << 1)
            | (((p[2] >= mid[2]) as usize) << 2);
        parts[c].push(s);
    }
    for part in parts {
        let Some(first) = part.first() else { continue };
        let mut b = Bounds::at(first.position);
        for s in &part {
            b.include(s.position);
        }
        let child = subtree(nodes, part, b, depth + 1, options, job)?;
        nodes[id].children.push(child);
    }
    let children = nodes[id].children.clone();
    let mut pool = vec![];
    for &c in &children {
        pool.extend(
            std::mem::take(&mut nodes[c].samples)
                .into_iter()
                .map(|s| (c, s)),
        );
    }
    let candidates: Vec<_> = pool.iter().map(|(_, s)| s.clone()).collect();
    let picked = grid_pick(&candidates, &bounds, options.view_grid);
    for ((c, s), keep) in pool.into_iter().zip(picked) {
        if keep {
            nodes[id].samples.push(s);
        } else {
            nodes[c].samples.push(s);
        }
    }
    Ok(id)
}

fn pack_leaf(
    task: LeafTask,
    stride: usize,
    options: ImportOptions,
    job: &JobControl,
) -> Result<PackedLeaf> {
    job.check()?;
    let data = task.data.into_bytes(stride);
    let samples: Vec<_> = data
        .chunks_exact(stride)
        .enumerate()
        .filter(|(_, b)| valid(b))
        .map(|(i, b)| Sample {
            chunk: task.chunk,
            index: i as u32,
            position: position(b),
            color: point_color(b),
        })
        .collect();
    let mut nodes = vec![];
    subtree(&mut nodes, samples, task.bounds, 0, &options, job)?;
    job.check()?;
    let mut packed = Vec::with_capacity(nodes.len().saturating_sub(1));
    let mut nodes = nodes.into_iter();
    let root = nodes.next().expect("subtree root");
    for node in nodes {
        packed.push(PackedNode {
            bounds: node.bounds,
            children: node.children,
            count: node.samples.len() as u32,
            chunks: chunk_counts(&node.samples),
            block: pack_view(&node.samples)?,
        });
    }
    let points = pack(&data, stride)?;
    job.check()?;
    Ok(PackedLeaf {
        chunk: task.chunk,
        node: task.node,
        points,
        root_children: root.children,
        nodes: packed,
        waiting: sample_bytes(&root.samples),
    })
}

fn empty_node(bounds: Bounds) -> Node {
    Node {
        bounds,
        children: vec![],
        count: 0,
        offset: 0,
        codec: BlockCodec::Raw,
        stored_bytes: 0,
        chunks: vec![],
    }
}

/// A spool file, removed once the last region in it is done with.
pub(crate) struct TempFile(pub(crate) PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Converted records waiting to be indexed.
pub(crate) enum Spool {
    File(TempFile),
    Memory(Arc<Vec<u8>>),
}

/// Collects a scan's converted records in memory when they fit the loading
/// budget, else in a scratch file.
pub(crate) enum SpoolWriter {
    Memory(Vec<u8>),
    File(BufWriter<File>, PathBuf),
}
impl SpoolWriter {
    pub fn new(path: &Path, bytes: u64, options: ImportOptions) -> Result<Self> {
        Ok(if bytes <= options.load_bytes() as u64 {
            SpoolWriter::Memory(Vec::with_capacity(bytes as usize))
        } else {
            SpoolWriter::File(BufWriter::new(create_scratch(path)?), path.to_owned())
        })
    }
    pub fn finish(self) -> Result<Spool> {
        Ok(match self {
            SpoolWriter::Memory(data) => Spool::Memory(Arc::new(data)),
            SpoolWriter::File(mut out, path) => {
                out.flush()?;
                drop(out);
                Spool::File(TempFile(path))
            }
        })
    }
}
impl Write for SpoolWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            SpoolWriter::Memory(data) => data.write(buf),
            SpoolWriter::File(out, _) => out.write(buf),
        }
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            SpoolWriter::Memory(data) => {
                data.extend_from_slice(buf);
                Ok(())
            }
            SpoolWriter::File(out, _) => out.write_all(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            SpoolWriter::Memory(_) => Ok(()),
            SpoolWriter::File(out, _) => out.flush(),
        }
    }
}

/// Consecutive records of a spool and their bounds.
#[derive(Clone)]
struct Region {
    file: Arc<Spool>,
    /// First record.
    offset: u64,
    count: u64,
    bounds: Bounds,
}

/// Cells along each side of the grid that splits a region too large for memory.
const GRID_LEVELS: u32 = 7;

/// A region's octree down to its parts: cells of the grid, or of its coarser
/// levels, small enough to load. Nodes with a single child are skipped.
enum GridTree {
    Part { part: usize, level: u32 },
    Node(Vec<GridTree>),
}

/// The grid tree below cell `at` of `level`, adding its parts (level, cell,
/// point count) to `parts` in tree order. A cell is a part once it holds at
/// most `limit` points, or at the finest level.
fn grid_tree(
    levels: &[Vec<u64>],
    limit: u64,
    level: u32,
    at: [usize; 3],
    parts: &mut Vec<(u32, [usize; 3], u64)>,
) -> Option<GridTree> {
    let side = 1usize << level;
    let count = levels[level as usize][at[0] + at[1] * side + at[2] * side * side];
    if count == 0 {
        return None;
    }
    if count <= limit || level == GRID_LEVELS {
        parts.push((level, at, count));
        return Some(GridTree::Part {
            part: parts.len() - 1,
            level,
        });
    }
    // Same child order as splitting at a node's centre: x, then y, then z.
    let mut children: Vec<_> = (0..8)
        .filter_map(|c| {
            let child = [
                at[0] * 2 + (c & 1),
                at[1] * 2 + ((c >> 1) & 1),
                at[2] * 2 + ((c >> 2) & 1),
            ];
            grid_tree(levels, limit, level + 1, child, parts)
        })
        .collect();
    if children.len() == 1 {
        children.pop()
    } else {
        Some(GridTree::Node(children))
    }
}

/// A region being loaded and split in memory.
type Loading = std::thread::JoinHandle<Result<(Arc<Vec<u8>>, MemoryTree)>>;

/// A split region's parts, in tree order, with their grid levels.
struct Parts {
    levels: Vec<u32>,
    regions: Vec<Option<Region>>,
    loading: Vec<Option<Loading>>,
}

/// Calls `f` with the records of `region` in order, a few MiB at a time.
fn read_region(
    region: &Region,
    stride: usize,
    job: &JobControl,
    mut f: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let block = (8 * 1024 * 1024 / stride).max(1) as u64;
    match &*region.file {
        Spool::Memory(data) => {
            let start = region.offset as usize * stride;
            let records = &data[start..start + region.count as usize * stride];
            for part in records.chunks(block as usize * stride) {
                job.check()?;
                f(part)?;
            }
        }
        Spool::File(file) => {
            let mut reader = File::open(&file.0)?;
            reader.seek(SeekFrom::Start(region.offset * stride as u64))?;
            let mut data = vec![];
            let mut done = 0;
            while done < region.count {
                job.check()?;
                let n = (region.count - done).min(block) as usize;
                data.resize(n * stride, 0);
                reader.read_exact(&mut data)?;
                f(&data)?;
                done += n as u64;
            }
        }
    }
    Ok(())
}

/// The octree of loaded records, before nodes are numbered.
enum MemoryTree {
    /// At most a chunk, or coincident or extremely spread points in
    /// chunk-sized batches.
    Leaf { bounds: Bounds, indices: Vec<u32> },
    Node {
        bounds: Bounds,
        children: Vec<MemoryTree>,
    },
}

/// Splits `indices`, in record order, at the centre of each node's bounds
/// until nodes hold at most `chunk_points`. Large nodes split their children
/// on up to `threads` threads.
#[allow(clippy::too_many_arguments)]
fn memory_tree(
    data: &[u8],
    stride: usize,
    chunk_points: usize,
    indices: Vec<u32>,
    bounds: Bounds,
    depth: u32,
    threads: usize,
    job: &JobControl,
) -> Result<MemoryTree> {
    job.check()?;
    if indices.len() <= chunk_points || depth >= 40 || bounds.radius() < 1e-12 {
        return Ok(MemoryTree::Leaf { bounds, indices });
    }
    let mid = bounds.center().to_array();
    let octants = |indices: &[u32]| {
        let mut parts: [Vec<u32>; 8] = Default::default();
        let mut boxes: [Option<Bounds>; 8] = [None; 8];
        for &i in indices {
            let p = position(&data[i as usize * stride..]);
            let c = (p[0] >= mid[0]) as usize
                | (((p[1] >= mid[1]) as usize) << 1)
                | (((p[2] >= mid[2]) as usize) << 2);
            parts[c].push(i);
            match &mut boxes[c] {
                Some(b) => b.include(p),
                None => boxes[c] = Some(Bounds::at(p)),
            };
        }
        (parts, boxes)
    };
    // Large nodes sort ranges of their points in parallel, then join them in order.
    let (parts, boxes) = if threads > 1 && indices.len() >= 1 << 20 {
        let ranges: Vec<_> = indices.chunks(indices.len().div_ceil(threads)).collect();
        let sorted: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = ranges.iter().map(|r| scope.spawn(|| octants(r))).collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .map_err(|_| anyhow::anyhow!("Split worker panicked"))
                })
                .collect::<Result<_>>()
        })?;
        let mut parts: [Vec<u32>; 8] = Default::default();
        let mut boxes: [Option<Bounds>; 8] = [None; 8];
        for c in 0..8 {
            parts[c].reserve(sorted.iter().map(|s| s.0[c].len()).sum());
            for (ps, bs) in &sorted {
                parts[c].extend_from_slice(&ps[c]);
                if let Some(b) = bs[c] {
                    match &mut boxes[c] {
                        Some(all) => {
                            all.include(b.min);
                            all.include(b.max);
                        }
                        None => boxes[c] = Some(b),
                    }
                }
            }
        }
        (parts, boxes)
    } else {
        octants(&indices)
    };
    drop(indices);
    let parts: Vec<_> = parts
        .into_iter()
        .zip(boxes)
        .filter_map(|(part, b)| Some((part, b?)))
        .collect();
    let large = parts
        .iter()
        .filter(|(p, _)| p.len() > chunk_points * 4)
        .count();
    let children = if threads > 1 && large > 1 {
        let share = (threads / parts.len()).max(1);
        std::thread::scope(|scope| {
            let handles: Vec<_> = parts
                .into_iter()
                .map(|(part, b)| {
                    scope.spawn(move || {
                        memory_tree(data, stride, chunk_points, part, b, depth + 1, share, job)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Split worker panicked")))
                })
                .collect::<Result<Vec<_>>>()
        })?
    } else {
        parts
            .into_iter()
            .map(|(part, b)| {
                memory_tree(data, stride, chunk_points, part, b, depth + 1, threads, job)
            })
            .collect::<Result<Vec<_>>>()?
    };
    Ok(MemoryTree::Node { bounds, children })
}

/// A chunk's records, or the loaded records and the chunk's indices into them.
enum LeafData {
    Owned(Vec<u8>),
    Gather(Arc<Vec<u8>>, Vec<u32>),
}
impl LeafData {
    fn len(&self, stride: usize) -> usize {
        match self {
            LeafData::Owned(data) => data.len() / stride,
            LeafData::Gather(_, indices) => indices.len(),
        }
    }
    fn into_bytes(self, stride: usize) -> Vec<u8> {
        match self {
            LeafData::Owned(data) => data,
            LeafData::Gather(data, indices) => {
                let mut out = Vec::with_capacity(indices.len() * stride);
                for i in indices {
                    out.extend_from_slice(&data[i as usize * stride..(i as usize + 1) * stride]);
                }
                out
            }
        }
    }
}

impl Builder<'_> {
    /// Builds the octree of `region` and returns its root node.
    fn place(&mut self, region: Region, depth: u32) -> Result<u32> {
        self.job.check()?;
        if self.fits(&region) {
            let loaded = self.load(region, depth);
            return self.loaded_node(loaded);
        }
        if depth >= 40 || region.bounds.radius() < 1e-12 {
            return self.stream_leaf(region);
        }
        self.split_region(region, depth)
    }

    fn fits(&self, region: &Region) -> bool {
        region.count * self.scan.stride as u64 <= self.memory_bytes as u64
    }

    /// Reads a region and splits it in memory on another thread.
    fn load(&self, region: Region, depth: u32) -> Loading {
        let stride = self.scan.stride;
        let chunk_points = self.options.chunk_points as usize;
        let workers = self.workers;
        let job = self.job.clone();
        std::thread::spawn(move || {
            let data = match &*region.file {
                Spool::Memory(data)
                    if region.offset == 0 && region.count as usize * stride == data.len() =>
                {
                    data.clone()
                }
                _ => {
                    let mut data = Vec::with_capacity(region.count as usize * stride);
                    read_region(&region, stride, &job, |b| {
                        data.extend_from_slice(b);
                        Ok(())
                    })?;
                    Arc::new(data)
                }
            };
            let bounds = region.bounds;
            drop(region);
            let indices = (0..u32::try_from(data.len() / stride)?).collect();
            let tree = memory_tree(
                &data,
                stride,
                chunk_points,
                indices,
                bounds,
                depth,
                workers,
                &job,
            )?;
            Ok((data, tree))
        })
    }

    fn loaded_node(&mut self, loading: Loading) -> Result<u32> {
        let (data, tree) = loading
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Split worker panicked")))?;
        self.memory_tree_nodes(&data, tree)
    }

    /// Splits a region too large for memory by counting its points on a grid,
    /// then copying each point once to the part of the grid it belongs to.
    fn split_region(&mut self, region: Region, depth: u32) -> Result<u32> {
        let stride = self.scan.stride;
        let n = 1usize << GRID_LEVELS;
        let min = region.bounds.min;
        let extent: [f64; 3] = std::array::from_fn(|i| region.bounds.max[i] - min[i]);
        let cell = |p: [f64; 3]| {
            let c: [usize; 3] = std::array::from_fn(|i| {
                if extent[i] > 0. {
                    (((p[i] - min[i]) / extent[i] * n as f64) as usize).min(n - 1)
                } else {
                    0
                }
            });
            c[0] | (c[1] << GRID_LEVELS) | (c[2] << (2 * GRID_LEVELS))
        };
        let total = region.count;
        let mut counts = vec![0u32; n * n * n];
        let mut seen = 0;
        read_region(&region, stride, self.job, |block| {
            for r in block.chunks_exact(stride) {
                counts[cell(position(r))] += 1;
            }
            seen += (block.len() / stride) as u64;
            self.job.report(Stage::Partitioning, seen / 2, total);
            Ok(())
        })?;
        // Point counts of every grid level, finest last.
        let mut levels = vec![counts.into_iter().map(u64::from).collect::<Vec<_>>()];
        for level in (0..GRID_LEVELS).rev() {
            let side = 1usize << level;
            let finer = levels.last().unwrap();
            let mut sums = vec![0u64; side * side * side];
            for z in 0..side * 2 {
                for y in 0..side * 2 {
                    for x in 0..side * 2 {
                        sums[(x / 2) + (y / 2) * side + (z / 2) * side * side] +=
                            finer[x + y * side * 2 + z * side * side * 4];
                    }
                }
            }
            levels.push(sums);
        }
        levels.reverse();
        let limit = (self.memory_bytes / stride) as u64;
        let mut parts = vec![];
        let Some(tree) = grid_tree(&levels, limit, 0, [0; 3], &mut parts) else {
            return self.stream_leaf(region);
        };
        if parts.len() < 2 {
            return self.stream_leaf(region);
        }
        // Each part's records go together, in their original order.
        let mut lookup = vec![u32::MAX; n * n * n];
        let mut starts = Vec::with_capacity(parts.len());
        let mut offset = 0u64;
        for (i, &(level, at, count)) in parts.iter().enumerate() {
            starts.push(offset);
            offset += count;
            let side = 1usize << (GRID_LEVELS - level);
            for z in 0..side {
                for y in 0..side {
                    for x in 0..side {
                        let c = [at[0] * side + x, at[1] * side + y, at[2] * side + z];
                        lookup[c[0] | (c[1] << GRID_LEVELS) | (c[2] << (2 * GRID_LEVELS))] =
                            i as u32;
                    }
                }
            }
        }
        let path = self
            .spool_path
            .with_file_name(format!("{}-parts.bin", Uuid::new_v4()));
        let mut out = create_scratch(&path)?;
        let file = Arc::new(Spool::File(TempFile(path)));
        let buffer = ((32 * 1024 * 1024 / parts.len()) / stride).max(1) * stride;
        let mut buffers = vec![Vec::<u8>::new(); parts.len()];
        let mut cursors: Vec<u64> = starts.iter().map(|s| s * stride as u64).collect();
        let mut boxes: Vec<Option<Bounds>> = vec![None; parts.len()];
        let flush = |out: &mut File, buf: &mut Vec<u8>, cursor: &mut u64| -> Result<()> {
            out.seek(SeekFrom::Start(*cursor))?;
            out.write_all(buf)?;
            *cursor += buf.len() as u64;
            buf.clear();
            Ok(())
        };
        read_region(&region, stride, self.job, |block| {
            for r in block.chunks_exact(stride) {
                let p = position(r);
                let i = lookup[cell(p)] as usize;
                match &mut boxes[i] {
                    Some(b) => b.include(p),
                    None => boxes[i] = Some(Bounds::at(p)),
                }
                buffers[i].extend_from_slice(r);
                if buffers[i].len() >= buffer {
                    flush(&mut out, &mut buffers[i], &mut cursors[i])?;
                }
            }
            seen += (block.len() / stride) as u64;
            self.job.report(Stage::Partitioning, seen / 2, total);
            Ok(())
        })?;
        for i in 0..parts.len() {
            flush(&mut out, &mut buffers[i], &mut cursors[i])?;
        }
        out.flush()?;
        drop(out);
        drop(region);
        let levels_of_parts: Vec<_> = parts.iter().map(|p| p.0).collect();
        let parts: Vec<_> = parts
            .iter()
            .zip(starts)
            .zip(boxes)
            .map(|((&(_, _, count), offset), bounds)| Region {
                file: file.clone(),
                offset,
                count,
                bounds: bounds.unwrap_or_default(),
            })
            .collect();
        drop(file);
        let mut parts = Parts {
            levels: levels_of_parts,
            regions: parts.into_iter().map(Some).collect(),
            loading: vec![],
        };
        parts.loading.resize_with(parts.regions.len(), || None);
        self.grid_node(tree, &mut parts, depth)
    }

    fn grid_node(&mut self, tree: GridTree, parts: &mut Parts, depth: u32) -> Result<u32> {
        match tree {
            GridTree::Part { part, level } => {
                let loading = match (parts.loading[part].take(), parts.regions[part].take()) {
                    (Some(loading), _) => loading,
                    (None, Some(region)) if self.fits(&region) => self.load(region, depth + level),
                    (None, Some(region)) => return self.place(region, depth + level),
                    (None, None) => unreachable!("each part placed once"),
                };
                let (data, tree) = loading
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Split worker panicked")))?;
                // Load the next part while workers pack this one.
                let next = part + 1;
                if let Some(Some(region)) = parts.regions.get(next)
                    && self.fits(region)
                {
                    let region = parts.regions[next].take().unwrap();
                    parts.loading[next] = Some(self.load(region, depth + parts.levels[next]));
                }
                self.memory_tree_nodes(&data, tree)
            }
            GridTree::Node(children) => {
                let id = u32::try_from(self.scan.nodes.len())?;
                self.scan.nodes.push(empty_node(Bounds::default()));
                self.above.push(id);
                let mut bounds: Option<Bounds> = None;
                for child in children {
                    let child = self.grid_node(child, parts, depth)?;
                    let b = self.scan.nodes[child as usize].bounds;
                    match &mut bounds {
                        Some(bounds) => {
                            bounds.include(b.min);
                            bounds.include(b.max);
                        }
                        None => bounds = Some(b),
                    }
                    self.scan.nodes[id as usize].children.push(child);
                }
                self.scan.nodes[id as usize].bounds = bounds.unwrap_or_default();
                Ok(id)
            }
        }
    }

    fn memory_tree_nodes(&mut self, data: &Arc<Vec<u8>>, tree: MemoryTree) -> Result<u32> {
        let id = u32::try_from(self.scan.nodes.len())?;
        match tree {
            MemoryTree::Leaf { bounds, indices } => {
                self.scan.nodes.push(empty_node(bounds));
                let chunk_points = self.options.chunk_points as usize;
                if indices.len() > chunk_points {
                    self.above.push(id);
                }
                for piece in indices.chunks(chunk_points) {
                    let leaf = if indices.len() <= chunk_points {
                        id
                    } else {
                        let child = u32::try_from(self.scan.nodes.len())?;
                        self.scan.nodes.push(empty_node(bounds));
                        self.scan.nodes[id as usize].children.push(child);
                        child
                    };
                    self.submit_leaf(leaf, bounds, LeafData::Gather(data.clone(), piece.to_vec()))?;
                }
            }
            MemoryTree::Node { bounds, children } => {
                self.scan.nodes.push(empty_node(bounds));
                self.above.push(id);
                for child in children {
                    let child = self.memory_tree_nodes(data, child)?;
                    self.scan.nodes[id as usize].children.push(child);
                }
            }
        }
        Ok(id)
    }

    /// A region of coincident or extremely spread points, too large for
    /// memory: chunks of it in record order under one node.
    fn stream_leaf(&mut self, region: Region) -> Result<u32> {
        let id = u32::try_from(self.scan.nodes.len())?;
        self.scan.nodes.push(empty_node(region.bounds));
        self.above.push(id);
        let stride = self.scan.stride;
        let chunk_points = self.options.chunk_points as u64;
        let mut start = 0;
        while start < region.count {
            let n = (region.count - start).min(chunk_points);
            let piece = Region {
                offset: region.offset + start,
                count: n,
                ..region.clone()
            };
            let mut data = Vec::with_capacity(n as usize * stride);
            read_region(&piece, stride, self.job, |b| {
                data.extend_from_slice(b);
                Ok(())
            })?;
            let child = u32::try_from(self.scan.nodes.len())?;
            self.scan.nodes.push(empty_node(region.bounds));
            self.scan.nodes[id as usize].children.push(child);
            self.submit_leaf(child, region.bounds, LeafData::Owned(data))?;
            start += n;
        }
        Ok(id)
    }

    fn submit_leaf(&mut self, node: u32, bounds: Bounds, data: LeafData) -> Result<()> {
        self.job.check()?;
        let n = data.len(self.scan.stride);
        let reservation = leaf_memory(n, self.scan.stride);
        while !self.pool.has_capacity(reservation) {
            self.finish_leaf()?;
        }
        let chunk = u32::try_from(self.scan.chunks.len())?;
        self.scan.chunks.push(Chunk {
            offset: 0,
            count: n as u32,
            bounds,
            codec: BlockCodec::Raw,
            stored_bytes: 0,
        });
        self.pool.submit(
            LeafTask {
                chunk,
                node,
                bounds,
                data,
            },
            reservation,
        )
    }
    /// Writes a node's final points to the view file.
    fn store_view(&mut self, id: usize, samples: &[Sample]) -> Result<()> {
        let (codec, bytes) = pack_view(samples)?;
        self.view.write_all(&bytes)?;
        let n = &mut self.scan.nodes[id];
        n.count = samples.len() as u32;
        n.offset = self.view_offset;
        n.codec = codec;
        n.stored_bytes = bytes.len() as u32;
        n.chunks = chunk_counts(samples);
        self.view_offset += bytes.len() as u64;
        Ok(())
    }
    /// Spools the points waiting at node `id` for its parent.
    fn wait(&mut self, id: u32, samples: &[u8]) -> Result<()> {
        self.spool.write_all(samples)?;
        if self.waiting.len() <= id as usize {
            self.waiting.resize(id as usize + 1, None);
        }
        let count = samples.len() / SAMPLE_BYTES;
        self.waiting[id as usize] = Some((self.spool_offset, count as u32));
        self.spool_offset += samples.len() as u64;
        Ok(())
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
        let mut nodes = Vec::with_capacity(leaf.nodes.len());
        for node in leaf.nodes {
            self.view.write_all(&node.block.1)?;
            nodes.push(Node {
                bounds: node.bounds,
                children: node.children.into_iter().map(|c| c as u32).collect(),
                count: node.count,
                offset: self.view_offset,
                codec: node.block.0,
                stored_bytes: node.block.1.len() as u32,
                chunks: node.chunks,
            });
            self.view_offset += node.block.1.len() as u64;
        }
        self.subtrees.push((leaf.node, leaf.root_children, nodes));
        self.wait(leaf.node, &leaf.waiting)?;
        self.job
            .report(Stage::Indexing, self.processed_points, self.scan.records);
        Ok(true)
    }
    /// Numbers the display subtrees below the chunks after all other nodes.
    fn attach_subtrees(&mut self) {
        for (root, children, nodes) in std::mem::take(&mut self.subtrees) {
            // Subtree node i (from 1) becomes node base + i - 1.
            let base = self.scan.nodes.len() as u32;
            let global = |i: u32| base + i - 1;
            self.scan.nodes[root as usize].children =
                children.into_iter().map(|c| global(c as u32)).collect();
            for mut node in nodes {
                node.children = node.children.into_iter().map(global).collect();
                self.scan.nodes.push(node);
            }
        }
    }
    /// Walks the nodes above the chunks bottom up; each picks its points from
    /// its children's waiting points, and the rest become the children's.
    /// Nodes of the same height above the chunks are independent, so workers
    /// pick them in parallel; results are written in a fixed order.
    fn finish_parents(&mut self) -> Result<()> {
        self.spool.flush()?;
        let above = std::mem::take(&mut self.above);
        let mut height = vec![0u32; self.scan.nodes.len()];
        let mut levels: Vec<Vec<u32>> = vec![];
        // Children are created after their parents, so they come first here.
        for &id in above.iter().rev() {
            let h = 1 + self.scan.nodes[id as usize]
                .children
                .iter()
                .map(|&c| height[c as usize])
                .max()
                .unwrap_or(0);
            height[id as usize] = h;
            if levels.len() < h as usize {
                levels.resize(h as usize, vec![]);
            }
            levels[h as usize - 1].push(id);
        }
        let spool_path = self.spool_path.clone();
        let grid = self.options.view_grid;
        let worker_job = self.job.clone();
        let budget = self.options.worker_memory_bytes;
        let mut pool = OrderedPool::new(self.workers, budget, self.job, move |task| {
            pick_parent(task, &spool_path, grid, &worker_job)
        })?;
        let mut done = 0;
        for level in levels {
            self.spool.flush()?;
            for id in level {
                let children: Vec<_> = self.scan.nodes[id as usize]
                    .children
                    .iter()
                    .map(|&c| {
                        let (offset, count) = self.waiting[c as usize]
                            .take()
                            .expect("child points waiting");
                        (c, offset, count)
                    })
                    .collect();
                let count: usize = children.iter().map(|c| c.2 as usize).sum();
                let reservation = (count * (SAMPLE_BYTES * 6 + 64)).clamp(1, budget);
                while !pool.has_capacity(reservation) {
                    let picked = pool.pop()?.expect("pending parent");
                    self.store_parent(picked)?;
                    done += 1;
                    self.job
                        .report(Stage::BuildingViewTree, done, above.len() as u64);
                }
                let bounds = self.scan.nodes[id as usize].bounds;
                pool.submit(
                    ParentTask {
                        id,
                        bounds,
                        children,
                    },
                    reservation,
                )?;
            }
            while let Some(picked) = pool.pop()? {
                self.store_parent(picked)?;
                done += 1;
                self.job
                    .report(Stage::BuildingViewTree, done, above.len() as u64);
            }
        }
        drop(pool);
        self.spool.flush()?;
        // An empty scan has no chunks and nothing waiting.
        let (offset, count) = self
            .waiting
            .first_mut()
            .and_then(Option::take)
            .unwrap_or((0, 0));
        let mut reader = BufReader::new(File::open(&self.spool_path)?);
        reader.seek(SeekFrom::Start(offset))?;
        let root: Vec<_> = (0..count)
            .map(|_| read_sample(&mut reader))
            .collect::<Result<_>>()?;
        self.store_view(0, &root)
    }

    /// Writes the points a parent left to its children and spools its own.
    fn store_parent(&mut self, picked: PickedParent) -> Result<()> {
        self.job.check()?;
        for ChildView {
            id,
            count,
            chunks,
            block: (codec, bytes),
        } in picked.children
        {
            self.view.write_all(&bytes)?;
            let n = &mut self.scan.nodes[id as usize];
            n.count = count;
            n.offset = self.view_offset;
            n.codec = codec;
            n.stored_bytes = bytes.len() as u32;
            n.chunks = chunks;
            self.view_offset += bytes.len() as u64;
        }
        self.wait(picked.id, &picked.own)
    }
}

struct ParentTask {
    id: u32,
    bounds: Bounds,
    /// Each child with where its waiting points are in the spool.
    children: Vec<(u32, u64, u32)>,
}
struct PickedParent {
    id: u32,
    /// The parent's own points, as spooled.
    own: Vec<u8>,
    /// Each child's final points, by child id.
    children: Vec<ChildView>,
}
struct ChildView {
    id: u32,
    count: u32,
    chunks: Vec<(u32, u32)>,
    block: (BlockCodec, Vec<u8>),
}

fn pick_parent(
    task: ParentTask,
    spool: &Path,
    grid: u32,
    job: &JobControl,
) -> Result<PickedParent> {
    let mut reader = File::open(spool)?;
    let mut pool = vec![];
    let mut bytes = vec![];
    for &(child, offset, count) in &task.children {
        job.check()?;
        reader.seek(SeekFrom::Start(offset))?;
        bytes.resize(count as usize * SAMPLE_BYTES, 0);
        reader.read_exact(&mut bytes)?;
        let mut r = &bytes[..];
        for _ in 0..count {
            pool.push((child, read_sample(&mut r)?));
        }
    }
    let candidates: Vec<_> = pool.iter().map(|(_, s)| s.clone()).collect();
    let picked = grid_pick(&candidates, &task.bounds, grid);
    drop(candidates);
    let mut own = vec![];
    let mut left: std::collections::BTreeMap<u32, Vec<Sample>> =
        task.children.iter().map(|c| (c.0, vec![])).collect();
    for ((child, s), keep) in pool.into_iter().zip(picked) {
        if keep {
            own.push(s);
        } else {
            left.get_mut(&child).unwrap().push(s);
        }
    }
    let children = left
        .into_iter()
        .map(|(child, samples)| {
            Ok(ChildView {
                id: child,
                count: samples.len() as u32,
                chunks: chunk_counts(&samples),
                block: pack_view(&samples)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(PickedParent {
        id: task.id,
        own: sample_bytes(&own),
        children,
    })
}

pub(crate) fn index(
    scan: &mut Scan,
    spool: Spool,
    root: &Path,
    bounds: Bounds,
    options: ImportOptions,
    job: &JobControl,
) -> Result<()> {
    ensure!(
        options.chunk_points > 0 && options.view_grid > 0 && options.view_leaf_points > 0,
        "Invalid import limits"
    );
    let max = (32 * 1024 * 1024 / scan.stride)
        .max(1)
        .min(options.chunk_points as usize) as u32;
    let workers = options.workers()?;
    // Shrink unusually wide-attribute chunks to fit even a single worker job.
    let mut chunk_points = max;
    while leaf_memory(chunk_points as usize, scan.stride) > options.worker_memory_bytes {
        ensure!(
            chunk_points > 1,
            "One point exceeds conversion memory budget"
        );
        chunk_points = chunk_points.div_ceil(2);
    }
    let options = ImportOptions {
        chunk_points,
        ..options
    };
    let stride = scan.stride;
    let worker_job = job.clone();
    let pool = OrderedPool::new(workers, options.worker_memory_bytes, job, move |task| {
        pack_leaf(task, stride, options, &worker_job)
    })?;
    let spool_path = root.join(format!("{}-waiting.bin", Uuid::new_v4()));
    let mut b = Builder {
        points: BufWriter::new(File::create(root.join(&scan.points_file))?),
        view: BufWriter::new(File::create(root.join(&scan.view_file))?),
        spool: BufWriter::new(create_scratch(&spool_path)?),
        spool_path: spool_path.clone(),
        waiting: vec![],
        spool_offset: 0,
        above: vec![],
        subtrees: vec![],
        point_offset: 0,
        processed_points: 0,
        view_offset: 0,
        options,
        memory_bytes: options.load_bytes(),
        workers,
        scan,
        job,
        pool,
    };
    let records = b.scan.records;
    let spool = Region {
        file: Arc::new(spool),
        offset: 0,
        count: records,
        bounds,
    };
    b.place(spool, 0)?;
    while b.finish_leaf()? {}
    b.attach_subtrees();
    b.finish_parents()?;
    job.check()?;
    b.points.flush()?;
    b.view.flush()?;
    b.points.get_ref().sync_all()?;
    b.view.get_ref().sync_all()?;
    drop(b);
    fs::remove_file(spool_path)?;
    Ok(())
}

impl Project {
    pub fn read_chunk(&self, scan: &Scan, id: u32) -> Result<Vec<u8>> {
        let (mut f, c, size) = self.open_chunk(scan, id)?;
        read_block(&mut f, c.codec, c.stored_bytes as usize, size, scan.stride)
    }
    /// Each valid point's position of a chunk, or none for an invalid one,
    /// decoding only the records' first bytes.
    pub(crate) fn chunk_positions(&self, scan: &Scan, id: u32) -> Result<Vec<Option<[f64; 3]>>> {
        // Position (three f64) and the valid flag lead each record.
        const COLUMNS: usize = 29;
        let (mut f, c, size) = self.open_chunk(scan, id)?;
        let columns = read_block_columns(
            &mut f,
            c.codec,
            c.stored_bytes as usize,
            size,
            scan.stride,
            COLUMNS,
        )?;
        let rows = c.count as usize;
        let column = |k: usize| &columns[k * rows..(k + 1) * rows];
        let valid = column(28);
        Ok((0..rows)
            .map(|i| {
                (valid[i] != 0).then(|| {
                    std::array::from_fn(|axis| {
                        f64::from_le_bytes(std::array::from_fn(|b| column(axis * 8 + b)[i]))
                    })
                })
            })
            .collect())
    }
    /// The point file opened at a chunk, the chunk and its decoded size.
    fn open_chunk<'a>(&self, scan: &'a Scan, id: u32) -> Result<(File, &'a Chunk, usize)> {
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
        Ok((f, c, size))
    }
    /// The points of a display octree node, in scan coordinates.
    pub fn read_view(&self, scan: &Scan, node: u32) -> Result<Vec<Sample>> {
        let n = scan
            .nodes
            .get(node as usize)
            .ok_or_else(|| anyhow::anyhow!("Invalid node"))?;
        let expected = n.count as usize * SAMPLE_BYTES;
        ensure!(
            expected <= MAX_BLOCK_BYTES,
            "View node exceeds format budget"
        );
        let mut f = File::open(self.path(&scan.view_file)?)?;
        let stored = n.stored_bytes as usize;
        ensure!(
            n.offset
                .checked_add(stored as u64)
                .is_some_and(|end| end <= f.metadata().map(|m| m.len()).unwrap_or(0)),
            "Truncated view data"
        );
        f.seek(SeekFrom::Start(n.offset))?;
        let data = read_block(&mut f, n.codec, stored, expected, SAMPLE_BYTES)?;
        let mut reader = data.as_slice();
        (0..n.count).map(|_| read_sample(&mut reader)).collect()
    }
}
