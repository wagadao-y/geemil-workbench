use crate::codec::{MAX_BLOCK_BYTES, pack, read_block};
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
    /// Zero chooses available CPUs minus one, capped at eight workers.
    pub worker_threads: usize,
    /// Budget for in-flight conversion buffers, excluding project metadata.
    pub worker_memory_bytes: usize,
}
impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            chunk_points: 65_536,
            view_grid: 128,
            view_leaf_points: 16_384,
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
    job: &'a JobControl,
    pool: OrderedPool<LeafTask, PackedLeaf>,
}

struct LeafTask {
    chunk: u32,
    node: u32,
    bounds: Bounds,
    data: Vec<u8>,
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
    /// The points left at the subtree root.
    waiting: Vec<Sample>,
}

fn leaf_memory(count: usize, stride: usize) -> usize {
    // Input, shuffle and compressed output, plus the display subtree's samples,
    // picks and packed blocks. Reservations last until the writer takes them.
    count * (stride * 4 + 64 + SAMPLE_BYTES * 6)
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
    let mut best: HashMap<u64, (f64, u32, u32, usize)> = HashMap::new();
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

fn pack_view(samples: &[Sample]) -> Result<(BlockCodec, Vec<u8>)> {
    let mut data = Vec::with_capacity(samples.len() * SAMPLE_BYTES);
    for s in samples {
        write_sample(&mut data, s)?;
    }
    pack(&data, SAMPLE_BYTES)
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
    let samples: Vec<_> = task
        .data
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
    let points = pack(&task.data, stride)?;
    job.check()?;
    Ok(PackedLeaf {
        chunk: task.chunk,
        node: task.node,
        points,
        root_children: root.children,
        nodes: packed,
        waiting: root.samples,
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

impl Builder<'_> {
    fn node(&mut self, path: PathBuf, count: u64, bounds: Bounds, depth: u32) -> Result<u32> {
        self.job.check()?;
        let id = u32::try_from(self.scan.nodes.len())?;
        self.scan.nodes.push(empty_node(bounds));
        let stride = self.scan.stride;
        // Coincident points and extreme coordinate ranges still split by bounded file batches.
        if count <= self.options.chunk_points as u64 || depth >= 40 || bounds.radius() < 1e-12 {
            if count > self.options.chunk_points as u64 {
                self.above.push(id);
            }
            let mut reader = BufReader::new(File::open(&path)?);
            let mut remaining = count;
            while remaining > 0 {
                self.job.check()?;
                let n = remaining.min(self.options.chunk_points as u64) as u32;
                let reservation = leaf_memory(n as usize, stride);
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
                    self.scan.nodes.push(empty_node(bounds));
                    self.scan.nodes[id as usize].children.push(child);
                    child
                };
                self.pool.submit(
                    LeafTask {
                        chunk,
                        node: leaf_id,
                        bounds,
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
        self.above.push(id);
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
    fn wait(&mut self, id: u32, samples: &[Sample]) -> Result<()> {
        for s in samples {
            write_sample(&mut self.spool, s)?;
        }
        if self.waiting.len() <= id as usize {
            self.waiting.resize(id as usize + 1, None);
        }
        self.waiting[id as usize] = Some((self.spool_offset, samples.len() as u32));
        self.spool_offset += (samples.len() * SAMPLE_BYTES) as u64;
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
    fn finish_parents(&mut self) -> Result<()> {
        self.spool.flush()?;
        let mut reader = BufReader::new(File::open(&self.spool_path)?);
        let above = std::mem::take(&mut self.above);
        // Children are created after their parents, so they come first here.
        for (done, &id) in above.iter().rev().enumerate() {
            self.job.check()?;
            self.job
                .report(Stage::BuildingViewTree, done as u64, above.len() as u64);
            let children = self.scan.nodes[id as usize].children.clone();
            let mut pool = vec![];
            for &child in &children {
                let (offset, count) = self.waiting[child as usize]
                    .take()
                    .expect("child points waiting");
                reader.seek(SeekFrom::Start(offset))?;
                for _ in 0..count {
                    pool.push((child, read_sample(&mut reader)?));
                }
            }
            let candidates: Vec<_> = pool.iter().map(|(_, s)| s.clone()).collect();
            let bounds = self.scan.nodes[id as usize].bounds;
            let picked = grid_pick(&candidates, &bounds, self.options.view_grid);
            let mut own = vec![];
            let mut left: std::collections::BTreeMap<u32, Vec<Sample>> =
                children.iter().map(|c| (*c, vec![])).collect();
            for ((child, s), keep) in pool.into_iter().zip(picked) {
                if keep {
                    own.push(s);
                } else {
                    left.get_mut(&child).unwrap().push(s);
                }
            }
            for (child, samples) in left {
                self.store_view(child as usize, &samples)?;
            }
            self.wait(id, &own)?;
            self.spool.flush()?;
        }
        // An empty scan has no chunks and nothing waiting.
        let (offset, count) = self
            .waiting
            .first_mut()
            .and_then(Option::take)
            .unwrap_or((0, 0));
        reader.seek(SeekFrom::Start(offset))?;
        let root: Vec<_> = (0..count)
            .map(|_| read_sample(&mut reader))
            .collect::<Result<_>>()?;
        self.store_view(0, &root)
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
    let spool_path = spool.with_file_name(format!("{}-waiting.bin", Uuid::new_v4()));
    let mut b = Builder {
        points: BufWriter::new(File::create(root.join(&scan.points_file))?),
        view: BufWriter::new(File::create(root.join(&scan.view_file))?),
        spool: BufWriter::new(File::create(&spool_path)?),
        spool_path: spool_path.clone(),
        waiting: vec![],
        spool_offset: 0,
        above: vec![],
        subtrees: vec![],
        point_offset: 0,
        processed_points: 0,
        view_offset: 0,
        options,
        scan,
        job,
        pool,
    };
    b.node(spool.to_owned(), b.scan.records, bounds, 0)?;
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
