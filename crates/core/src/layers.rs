//! Layers: every original point belongs to exactly one, recorded as one byte
//! per point in label patches that never change once written. Moving points
//! writes a patch of the chunks whose labels changed and lists it in the
//! working state, so undo and revision switches only swap states.
use crate::codec::{pack_labels, unpack_labels};
use crate::{
    DEFAULT_LAYER, JobControl, LabelBlock, LabelPatch, Layer, LayerTarget, Project, Revision, Scan,
    Stage,
};
use anyhow::{Result, anyhow, ensure};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
};
use uuid::Uuid;

/// Whether bit `i` of a point mask is set.
pub(crate) fn is_set(mask: &[u8], i: usize) -> bool {
    mask[i / 8] & (1 << (i % 8)) != 0
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
    /// The block holding a chunk's labels in the current state; none means
    /// every point is in the default layer.
    fn label_block(&self, scan: Uuid, chunk: u32) -> Option<(&LabelPatch, &LabelBlock)> {
        self.current().labels.iter().rev().find_map(|id| {
            let patch = self.manifest.patches.iter().find(|p| p.id == *id)?;
            patch.block(scan, chunk).map(|b| (patch, b))
        })
    }
    /// The layer code of every point of a chunk.
    pub fn labels(&self, scan: &Scan, chunk: u32) -> Result<Vec<u8>> {
        let count = scan
            .chunks
            .get(chunk as usize)
            .ok_or_else(|| anyhow!("Invalid chunk"))?
            .count as usize;
        let Some((patch, block)) = self
            .label_block(scan.id, chunk)
            .filter(|(_, b)| b.bytes > 0)
        else {
            return Ok(vec![DEFAULT_LAYER; count]);
        };
        let mut f = File::open(self.path(&patch.file)?)?;
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
        let total = scan.chunks[chunk as usize].count as u64;
        let others = self
            .label_block(scan.id, chunk)
            .map_or(&[][..], |(_, b)| &b.counts);
        let moved: u64 = others.iter().map(|(_, n)| n).sum();
        let mut counts = vec![(DEFAULT_LAYER, total.saturating_sub(moved))];
        counts.extend_from_slice(others);
        counts
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
    /// Points per layer over the scans of the current state, for every layer.
    pub fn layer_counts(&self) -> BTreeMap<u8, u64> {
        let mut result: BTreeMap<_, _> =
            self.current().layers.iter().map(|l| (l.code, 0)).collect();
        for scan in self.scans() {
            for chunk in 0..scan.chunks.len() as u32 {
                for (code, n) in self.chunk_counts(scan, chunk) {
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
    /// Moves the points of a chunk set in `mask`, of which there are `count`.
    pub(crate) fn push(
        &mut self,
        project: &Project,
        scan: &Scan,
        chunk: u32,
        mask: &[u8],
        count: u64,
    ) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let mut labels = project.labels(scan, chunk)?;
        let mut moved = 0;
        for (i, label) in labels.iter_mut().enumerate() {
            if is_set(mask, i) && *label != self.target {
                *label = self.target;
                moved += 1;
            }
        }
        self.write(scan.id, chunk, &labels, moved)
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
                self.write(scan.id, chunk, &labels, moved)?;
            }
        }
        Ok(())
    }
    fn write(&mut self, scan: Uuid, chunk: u32, labels: &[u8], moved: u64) -> Result<()> {
        if moved == 0 {
            return Ok(());
        }
        let mut counts = BTreeMap::new();
        for label in labels.iter().filter(|l| **l != DEFAULT_LAYER) {
            *counts.entry(*label).or_insert(0u64) += 1;
        }
        let bytes = if counts.is_empty() {
            0
        } else {
            let packed = pack_labels(labels)?;
            self.file.write_all(&packed)?;
            packed.len() as u32
        };
        self.blocks.push(LabelBlock {
            scan,
            chunk,
            offset: self.offset,
            bytes,
            counts: counts.into_iter().collect(),
        });
        self.offset += bytes as u64;
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
