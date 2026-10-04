//! Layers: every original point belongs to exactly one, recorded as one byte
//! per point in label patches that never change once written. Moving points
//! writes a patch of the chunks whose labels changed and lists it in the
//! working state, so undo and revision switches only swap states.
use crate::codec::{pack_labels, unpack_labels};
use crate::parallel::for_each_unordered;
use crate::{
    Bounds, DEFAULT_LAYER, JobControl, LabelBlock, LabelPatch, Layer, LayerTarget, Project,
    Revision, Scan, Stage,
};
use anyhow::{Result, anyhow, ensure};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

/// Whether bit `i` of a point mask is set.
pub(crate) fn is_set(mask: &[u8], i: usize) -> bool {
    mask[i / 8] & (1 << (i % 8)) != 0
}

/// Points per layer of a chunk of `total` points whose labels `block` holds;
/// none means every point is in the default layer.
fn layer_points(total: u32, block: Option<&LabelBlock>) -> Vec<(u8, u64)> {
    let others = block.map_or(&[][..], |b| &b.counts);
    let moved: u64 = others.iter().map(|(_, n)| n).sum();
    let mut counts = vec![(DEFAULT_LAYER, (total as u64).saturating_sub(moved))];
    counts.extend_from_slice(others);
    counts
}

/// The block holding each chunk's labels in one state, with its patch file.
#[derive(Debug)]
pub(crate) struct LabelIndex {
    /// The state's patches the index was built from. Patches never change
    /// once written, so the same list means the same index.
    labels: Vec<Uuid>,
    blocks: HashMap<(Uuid, u32), (Arc<str>, LabelBlock)>,
}
impl LabelIndex {
    fn new(project: &Project) -> Self {
        let labels = project.current().labels.clone();
        let patches: HashMap<Uuid, &LabelPatch> =
            project.manifest.patches.iter().map(|p| (p.id, p)).collect();
        let mut blocks = HashMap::new();
        // The last patch listing a chunk holds its labels.
        for patch in labels.iter().rev().filter_map(|id| patches.get(id)) {
            let file: Arc<str> = patch.file.as_str().into();
            for block in &patch.blocks {
                blocks
                    .entry((block.scan, block.chunk))
                    .or_insert_with(|| (file.clone(), block.clone()));
            }
        }
        Self { labels, blocks }
    }
}
/// The [`LabelIndex`] of the state last asked about. Clones of a project
/// share it, and a different state's question replaces it.
#[derive(Clone, Debug, Default)]
pub(crate) struct LabelIndexCache(Arc<Mutex<Option<Arc<LabelIndex>>>>);

/// The bounds [`Project::visible_bounds`] found for each scan in the state
/// last asked about. Clones of a project share it, and a different state's
/// question empties it.
#[derive(Clone, Debug, Default)]
pub(crate) struct VisibleBoundsCache(Arc<Mutex<VisibleBounds>>);
/// A state and each scan's visible bounds in it.
type VisibleBounds = (Uuid, HashMap<Uuid, Option<Bounds>>);

/// Whether `outer` holds all of `inner`.
fn holds(outer: &Bounds, inner: &Bounds) -> bool {
    (0..3).all(|k| outer.min[k] <= inner.min[k] && inner.max[k] <= outer.max[k])
}

/// The lowest code no layer of `state` uses.
fn free_code(state: &Revision) -> Result<u8> {
    (1..=u8::MAX)
        .find(|c| !state.layers.iter().any(|l| l.code == *c))
        .ok_or_else(|| anyhow!("Too many layers"))
}

impl Project {
    pub fn layer(&self, code: u8) -> Option<&Layer> {
        self.current().layers.iter().find(|l| l.code == code)
    }
    /// The layer named `name`, or a new one by that name.
    pub fn layer_named(&self, name: &str) -> LayerTarget {
        match self.current().layers.iter().find(|l| l.name == name) {
            Some(layer) => LayerTarget::Existing(layer.code),
            None => LayerTarget::New(name.into()),
        }
    }
    /// Where every chunk's labels are in the current state, built once per
    /// state rather than searching the state's patches for each chunk.
    fn label_index(&self) -> Arc<LabelIndex> {
        let mut cached = self.label_index.0.lock().unwrap();
        match &*cached {
            Some(index) if index.labels == self.current().labels => index.clone(),
            _ => cached.insert(Arc::new(LabelIndex::new(self))).clone(),
        }
    }
    /// The patch file and block holding a chunk's labels in the current
    /// state; none means every point is in the default layer.
    fn label_block(&self, scan: Uuid, chunk: u32) -> Option<(Arc<str>, LabelBlock)> {
        self.label_index().blocks.get(&(scan, chunk)).cloned()
    }
    /// The layer code of every point of a chunk.
    pub fn labels(&self, scan: &Scan, chunk: u32) -> Result<Vec<u8>> {
        let count = scan
            .chunks
            .get(chunk as usize)
            .ok_or_else(|| anyhow!("Invalid chunk"))?
            .count as usize;
        let Some((file, block)) = self
            .label_block(scan.id, chunk)
            .filter(|(_, b)| b.bytes > 0)
        else {
            return Ok(vec![DEFAULT_LAYER; count]);
        };
        let mut f = File::open(self.path(&file)?)?;
        ensure!(
            block.offset + block.bytes as u64 <= f.metadata()?.len(),
            "Truncated labels"
        );
        f.seek(SeekFrom::Start(block.offset))?;
        let mut bytes = vec![0; block.bytes as usize];
        f.read_exact(&mut bytes)?;
        unpack_labels(&bytes, count)
    }
    /// Points of a chunk per layer, without reading its labels.
    fn chunk_counts(&self, scan: &Scan, chunk: u32) -> Vec<(u8, u64)> {
        let block = self.label_block(scan.id, chunk).map(|(_, b)| b);
        layer_points(scan.chunks[chunk as usize].count, block.as_ref())
    }
    /// The block holding each chunk's labels in the current state for every
    /// chunk of `scans`, parallel to them: [`Project::label_block`] for all
    /// chunks at once, visiting each block of the state's patches once
    /// instead of searching every patch for every chunk.
    fn chunk_blocks<'a>(&'a self, scans: &[&Scan]) -> Vec<Vec<Option<&'a LabelBlock>>> {
        let mut blocks: Vec<_> = scans.iter().map(|s| vec![None; s.chunks.len()]).collect();
        let position: HashMap<Uuid, usize> =
            scans.iter().enumerate().map(|(i, s)| (s.id, i)).collect();
        let patches: HashMap<Uuid, &LabelPatch> =
            self.manifest.patches.iter().map(|p| (p.id, p)).collect();
        // The last patch listing a chunk holds its labels.
        for patch in self.current().labels.iter().rev() {
            let Some(patch) = patches.get(patch) else {
                continue;
            };
            for block in &patch.blocks {
                let slot = position
                    .get(&block.scan)
                    .and_then(|&i| blocks[i].get_mut(block.chunk as usize));
                if let Some(slot @ None) = slot {
                    *slot = Some(block);
                }
            }
        }
        blocks
    }
    /// The number of points in hidden layers of each chunk of `scans`,
    /// parallel to them.
    pub(crate) fn hidden_counts(&self, scans: &[&Scan]) -> Vec<Vec<u64>> {
        let hidden = self.hidden_codes();
        self.chunk_blocks(scans)
            .into_iter()
            .zip(scans)
            .map(|(blocks, scan)| {
                blocks
                    .into_iter()
                    .zip(&scan.chunks)
                    .map(|(block, chunk)| {
                        layer_points(chunk.count, block)
                            .iter()
                            .filter(|(code, _)| hidden[*code as usize])
                            .map(|(_, n)| n)
                            .sum()
                    })
                    .collect()
            })
            .collect()
    }
    /// Whether each code is hidden; codes of no layer count as hidden.
    fn hidden_codes(&self) -> [bool; 256] {
        let mut hidden = [true; 256];
        for layer in &self.current().layers {
            hidden[layer.code as usize] = !layer.visible;
        }
        hidden
    }
    /// The points of a chunk in hidden layers, one bit per point.
    pub fn hidden_mask(&self, scan: &Scan, chunk: u32) -> Result<Vec<u8>> {
        let count = scan
            .chunks
            .get(chunk as usize)
            .ok_or_else(|| anyhow!("Invalid chunk"))?
            .count as usize;
        let mut mask = vec![0; count.div_ceil(8)];
        if self.hidden_count(scan, chunk) == 0 {
            return Ok(mask);
        }
        let hidden = self.hidden_codes();
        for (i, label) in self.labels(scan, chunk)?.into_iter().enumerate() {
            if hidden[label as usize] {
                mask[i / 8] |= 1 << (i % 8);
            }
        }
        Ok(mask)
    }
    /// The number of points of a chunk in hidden layers.
    pub(crate) fn hidden_count(&self, scan: &Scan, chunk: u32) -> u64 {
        let hidden = self.hidden_codes();
        self.chunk_counts(scan, chunk)
            .iter()
            .filter(|(code, _)| hidden[*code as usize])
            .map(|(_, n)| n)
            .sum()
    }
    /// Workers for judging chunks of `scans` one at a time: the CPUs (or the
    /// filter options' count), at most 16, while each can decode the largest
    /// chunk (compressed, shuffled and final bytes) with its labels and mask.
    pub(crate) fn chunk_workers(&self, scans: &[&Scan]) -> Result<usize> {
        let per_worker = scans
            .iter()
            .flat_map(|s| {
                s.chunks
                    .iter()
                    .map(move |c| c.count as usize * (s.stride * 3 + 2))
            })
            .max()
            .unwrap_or(0)
            .max(1);
        let options = self.filter_options;
        let capacity = (options.memory_bytes - options.memory_bytes / 4) / per_worker;
        ensure!(capacity > 0, crate::CoreError::FilterMemoryBudgetTooSmall);
        let workers = match options.worker_threads {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get().min(16)),
            n => n,
        };
        Ok(workers.min(capacity))
    }
    /// The bounds, in scan coordinates, of the scan's points in visible
    /// layers; none when none is visible. A chunk wholly visible counts with
    /// its bounds and one wholly hidden not at all, both without reading it;
    /// a chunk with both is read only when it reaches outside the bounds
    /// found so far, so points moved out of the work, such as distant
    /// noise, stop widening the scan's box at little cost. Kept per state.
    pub fn visible_bounds(&self, scan: &Scan) -> Result<Option<Bounds>> {
        let state = self.current().id;
        {
            let cache = self.visible_bounds.0.lock().unwrap();
            if cache.0 == state
                && let Some(bounds) = cache.1.get(&scan.id)
            {
                return Ok(*bounds);
            }
        }
        let hidden = self.hidden_counts(&[scan]).remove(0);
        let mut bounds: Option<Bounds> = None;
        let mut mixed = vec![];
        for (chunk, (c, hidden)) in scan.chunks.iter().zip(hidden).enumerate() {
            if hidden == 0 {
                match &mut bounds {
                    Some(b) => {
                        b.include(c.bounds.min);
                        b.include(c.bounds.max);
                    }
                    None => bounds = Some(c.bounds),
                }
            } else if hidden < c.count as u64 {
                mixed.push(chunk as u32);
            }
        }
        for chunk in mixed {
            if bounds.is_some_and(|b| holds(&b, &scan.chunks[chunk as usize].bounds)) {
                continue;
            }
            for sample in self.points(scan, chunk)? {
                match &mut bounds {
                    Some(b) => b.include(sample.position),
                    None => bounds = Some(Bounds::at(sample.position)),
                }
            }
        }
        let mut cache = self.visible_bounds.0.lock().unwrap();
        if cache.0 != state {
            *cache = (state, HashMap::new());
        }
        cache.1.insert(scan.id, bounds);
        Ok(bounds)
    }
    /// Points per layer over the scans of the current state, for every layer.
    pub fn layer_counts(&self) -> BTreeMap<u8, u64> {
        let mut result: BTreeMap<_, _> =
            self.current().layers.iter().map(|l| (l.code, 0)).collect();
        let scans: Vec<_> = self.scans().collect();
        for (blocks, scan) in self.chunk_blocks(&scans).into_iter().zip(&scans) {
            for (block, chunk) in blocks.into_iter().zip(&scan.chunks) {
                for (code, n) in layer_points(chunk.count, block) {
                    *result.entry(code).or_default() += n;
                }
            }
        }
        result
    }

    /// Adds a visible, empty layer.
    pub fn create_layer(&mut self, name: String) -> Result<u8> {
        let code = free_code(self.current())?;
        self.edit(
            serde_json::json!({"kind": "create_layer", "code": code, "name": name}),
            |s| {
                s.layers.push(Layer {
                    code,
                    name,
                    visible: true,
                });
                Ok(())
            },
        )?;
        Ok(code)
    }
    pub fn rename_layer(&mut self, code: u8, name: String) -> Result<()> {
        ensure!(code != DEFAULT_LAYER, "The default layer cannot be renamed");
        self.edit(
            serde_json::json!({"kind": "rename_layer", "code": code, "name": name}),
            |s| {
                let layer = s.layers.iter_mut().find(|l| l.code == code);
                layer.ok_or_else(|| anyhow!("Missing layer"))?.name = name;
                Ok(())
            },
        )
    }
    pub fn set_layer_visible(&mut self, code: u8, visible: bool) -> Result<()> {
        self.edit(
            serde_json::json!({"kind": "layer_visibility", "code": code, "visible": visible}),
            |s| {
                let layer = s.layers.iter_mut().find(|l| l.code == code);
                layer.ok_or_else(|| anyhow!("Missing layer"))?.visible = visible;
                Ok(())
            },
        )
    }
    /// Moves every point of layer `from`, visible or not, to layer `to`.
    pub fn move_layer_points(&mut self, from: u8, to: u8, job: &JobControl) -> Result<u64> {
        ensure!(from != to, "Moving a layer onto itself");
        ensure!(self.layer(from).is_some(), "Missing layer");
        let mut writer = LabelWriter::new(self, &LayerTarget::Existing(to))?;
        writer.push_layer_all(self, from, job)?;
        writer.commit(
            self,
            serde_json::json!({"kind": "move_layer", "from": from}),
            |_| Ok(()),
            false,
        )
    }
    /// Removes a layer; its points return to the default layer.
    pub fn delete_layer(&mut self, code: u8, job: &JobControl) -> Result<u64> {
        ensure!(code != DEFAULT_LAYER, "The default layer cannot be deleted");
        let name = self
            .layer(code)
            .ok_or_else(|| anyhow!("Missing layer"))?
            .name
            .clone();
        let mut writer = LabelWriter::new(self, &LayerTarget::Existing(DEFAULT_LAYER))?;
        writer.push_layer_all(self, code, job)?;
        writer.commit(
            self,
            serde_json::json!({"kind": "delete_layer", "code": code, "name": name}),
            |s| {
                s.layers.retain(|l| l.code != code);
                Ok(())
            },
            true,
        )
    }
}

/// A chunk's labels after a move, packed for a patch.
pub(crate) struct ChunkLabels {
    scan: Uuid,
    chunk: u32,
    /// Empty when every point is in the default layer.
    packed: Vec<u8>,
    counts: Vec<(u8, u64)>,
    moved: u64,
}
impl ChunkLabels {
    /// None when the move changed nothing.
    fn new(scan: Uuid, chunk: u32, labels: &[u8], moved: u64) -> Result<Option<Self>> {
        if moved == 0 {
            return Ok(None);
        }
        let mut counts = BTreeMap::new();
        for label in labels.iter().filter(|l| **l != DEFAULT_LAYER) {
            *counts.entry(*label).or_insert(0u64) += 1;
        }
        let packed = if counts.is_empty() {
            vec![]
        } else {
            pack_labels(labels)?
        };
        Ok(Some(Self {
            scan,
            chunk,
            packed,
            counts: counts.into_iter().collect(),
            moved,
        }))
    }
}

/// A chunk's labels with the points set in `mask` moved to `target`.
pub(crate) fn relabel(
    project: &Project,
    target: u8,
    scan: &Scan,
    chunk: u32,
    mask: &[u8],
) -> Result<Option<ChunkLabels>> {
    let mut labels = project.labels(scan, chunk)?;
    let mut moved = 0;
    for (i, label) in labels.iter_mut().enumerate() {
        if is_set(mask, i) && *label != target {
            *label = target;
            moved += 1;
        }
    }
    ChunkLabels::new(scan.id, chunk, &labels, moved)
}

/// Writes the labels of the chunks an operation changes to staging, then
/// commits them as one patch that moves points to the target layer.
pub(crate) struct LabelWriter {
    id: Uuid,
    tmp: std::path::PathBuf,
    file: File,
    blocks: Vec<LabelBlock>,
    offset: u64,
    moved: u64,
    target: u8,
    /// A layer the commit creates; new layers start hidden, so moving points
    /// there takes them out of the work.
    new_layer: Option<Layer>,
}
impl LabelWriter {
    pub(crate) fn new(project: &Project, target: &LayerTarget) -> Result<Self> {
        let state = project.current();
        let (target, new_layer) = match target {
            LayerTarget::Existing(code) => {
                ensure!(project.layer(*code).is_some(), "Missing layer");
                (*code, None)
            }
            LayerTarget::New(name) => {
                let code = free_code(state)?;
                let layer = Layer {
                    code,
                    name: name.clone(),
                    visible: false,
                };
                (code, Some(layer))
            }
        };
        let id = Uuid::new_v4();
        let tmp = project.root.join("staging").join(format!("{id}.labels"));
        Ok(Self {
            id,
            file: File::create(&tmp)?,
            tmp,
            blocks: vec![],
            offset: 0,
            moved: 0,
            target,
            new_layer,
        })
    }
    /// Moves the points `judge` picks in each of `chunks`, judging and
    /// relabelling the chunks on worker threads.
    pub(crate) fn push_parallel(
        &mut self,
        project: &Project,
        chunks: &[(&Scan, u32)],
        stage: Stage,
        job: &JobControl,
        judge: impl Fn(&Scan, u32) -> Result<(Vec<u8>, u64)> + Sync,
    ) -> Result<()> {
        let scans: Vec<_> = chunks.iter().map(|(scan, _)| *scan).collect();
        let workers = project.chunk_workers(&scans)?;
        let target = self.target;
        let total = chunks.len() as u64;
        let mut done = 0;
        job.report(stage, 0, total);
        for_each_unordered(
            chunks,
            workers,
            job,
            |&(scan, chunk)| {
                let (mask, count) = judge(scan, chunk)?;
                if count == 0 {
                    return Ok(None);
                }
                relabel(project, target, scan, chunk, &mask)
            },
            |labels| {
                done += 1;
                job.report(stage, done, total);
                labels.map_or(Ok(()), |labels| self.add(labels))
            },
        )
    }
    /// Moves every point of layer `from` in the current scans.
    fn push_layer_all(&mut self, project: &Project, from: u8, job: &JobControl) -> Result<()> {
        let scans: Vec<_> = project.scans().collect();
        let total: usize = scans.iter().map(|s| s.chunks.len()).sum();
        let mut done = 0;
        for scan in scans {
            for chunk in 0..scan.chunks.len() as u32 {
                job.check()?;
                job.report(Stage::MovingLayer, done, total as u64);
                done += 1;
                let has = project
                    .chunk_counts(scan, chunk)
                    .iter()
                    .any(|(code, n)| *code == from && *n > 0);
                if !has {
                    continue;
                }
                let mut labels = project.labels(scan, chunk)?;
                let mut moved = 0;
                for label in labels.iter_mut().filter(|l| **l == from) {
                    *label = self.target;
                    moved += 1;
                }
                if let Some(labels) = ChunkLabels::new(scan.id, chunk, &labels, moved)? {
                    self.add(labels)?;
                }
            }
        }
        Ok(())
    }
    /// The code of the layer points move to.
    pub(crate) fn target(&self) -> u8 {
        self.target
    }
    /// Appends a chunk's new labels to the patch.
    pub(crate) fn add(&mut self, labels: ChunkLabels) -> Result<()> {
        let ChunkLabels {
            scan,
            chunk,
            packed,
            counts,
            moved,
        } = labels;
        self.file.write_all(&packed)?;
        self.blocks.push(LabelBlock {
            scan,
            chunk,
            offset: self.offset,
            bytes: packed.len() as u32,
            counts,
        });
        self.offset += packed.len() as u64;
        self.moved += moved;
        Ok(())
    }
    /// Commits the moves as one edit described by `operation`, to which the
    /// target layer and the number of moved points are added, and applies
    /// `change` to the state too. Moving nothing changes nothing, unless
    /// `always` asks for the edit regardless.
    pub(crate) fn commit(
        self,
        project: &mut Project,
        mut operation: serde_json::Value,
        change: impl FnOnce(&mut Revision) -> Result<()>,
        always: bool,
    ) -> Result<u64> {
        let Self {
            id,
            tmp,
            file,
            mut blocks,
            moved,
            target,
            new_layer,
            ..
        } = self;
        file.sync_all()?;
        drop(file);
        operation["target"] = serde_json::json!(target);
        operation["moved"] = serde_json::json!(moved);
        if blocks.is_empty() {
            fs::remove_file(tmp)?;
            if always {
                project.edit(operation, change)?;
            }
            return Ok(0);
        }
        let relative = format!("labels/{id}.labels");
        fs::rename(tmp, project.path(&relative)?)?;
        blocks.sort_by_key(|b| (b.scan, b.chunk));
        let mut next = project.clone();
        next.manifest.patches.push(LabelPatch {
            id,
            file: relative,
            blocks,
        });
        next.edit(operation, |s| {
            s.labels.push(id);
            s.layers.extend(new_layer);
            change(s)
        })?;
        *project = next;
        Ok(moved)
    }
}
