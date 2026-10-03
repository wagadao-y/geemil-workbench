use crate::{JobControl, Project, Sample, Scan};
use crate::{
    layers::is_set,
    storage::{point_color, position, valid},
};
use anyhow::Result;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub resident_bytes: usize,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    scan: Uuid,
    node: u32,
    full: bool,
}
struct Entry {
    samples: Arc<[Sample]>,
    used: u64,
    bytes: usize,
}

/// What decides which samples of a node are shown: the project, the label
/// patches in effect and the visible layers.
type Epoch = (PathBuf, Vec<Uuid>, Vec<u8>);

/// LRU of decoded node samples in visible layers, in scan-local coordinates.
/// Camera and transform changes reuse them; a different project, labels or
/// set of visible layers invalidates them. Labels never change once written.
pub struct ViewCache {
    epoch: Option<Epoch>,
    entries: HashMap<Key, Entry>,
    limit: usize,
    clock: u64,
    stats: ViewCacheStats,
}
impl ViewCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            epoch: None,
            entries: HashMap::new(),
            limit: max_bytes,
            clock: 0,
            stats: ViewCacheStats::default(),
        }
    }
    pub fn stats(&self) -> ViewCacheStats {
        self.stats
    }
    pub(crate) fn prepare(&mut self, project: &Project) {
        let state = project.current();
        let visible = state
            .layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| l.code)
            .collect();
        let epoch = (project.root.clone(), state.labels.clone(), visible);
        if self.epoch.as_ref() != Some(&epoch) {
            self.entries.clear();
            self.stats.resident_bytes = 0;
            self.epoch = Some(epoch);
        }
    }
    pub(crate) fn samples(
        &mut self,
        project: &Project,
        scan: &Scan,
        node: u32,
        full: bool,
        job: &JobControl,
    ) -> Result<Arc<[Sample]>> {
        let key = Key {
            scan: scan.id,
            node,
            full,
        };
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            self.stats.hits += 1;
            entry.used = self.clock;
            return Ok(entry.samples.clone());
        }
        self.stats.misses += 1;
        let n = &scan.nodes[node as usize];
        let samples = if let Some(chunk) = n.chunk.filter(|_| full) {
            project
                .read_chunk(scan, chunk)?
                .chunks_exact(scan.stride)
                .enumerate()
                .filter(|(_, p)| valid(p))
                .map(|(i, p)| Sample {
                    chunk,
                    index: i as u32,
                    position: position(p),
                    color: point_color(p),
                })
                .collect()
        } else {
            project.read_lod(scan, node)?
        };
        let mut masks = HashMap::new();
        let mut result = Vec::with_capacity(samples.len());
        for (i, sample) in samples.into_iter().enumerate() {
            if i % 8192 == 0 {
                job.check()?;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = masks.entry(sample.chunk) {
                // None: nothing of the chunk is hidden.
                let hidden = (project.hidden_count(scan, sample.chunk) > 0)
                    .then(|| project.hidden_mask(scan, sample.chunk))
                    .transpose()?;
                entry.insert(hidden);
            }
            if masks[&sample.chunk]
                .as_ref()
                .is_some_and(|hidden| is_set(hidden, sample.index as usize))
            {
                continue;
            }
            result.push(sample);
        }
        job.check()?;
        let samples: Arc<[Sample]> = result.into();
        // Charge entry/Arc/hash-table overhead too, including nodes left empty.
        let bytes = std::mem::size_of_val(samples.as_ref()) + 128;
        if bytes <= self.limit {
            while self.stats.resident_bytes + bytes > self.limit {
                let Some(key) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.used)
                    .map(|(key, _)| *key)
                else {
                    break;
                };
                let entry = self.entries.remove(&key).unwrap();
                self.stats.resident_bytes -= entry.bytes;
            }
            self.stats.resident_bytes += bytes;
            self.entries.insert(
                key,
                Entry {
                    samples: samples.clone(),
                    used: self.clock,
                    bytes,
                },
            );
        }
        Ok(samples)
    }
}
