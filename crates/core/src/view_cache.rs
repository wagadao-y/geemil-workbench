use crate::layers::is_set;
use crate::{JobControl, Project, Sample, Scan};
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
    /// Hidden points per chunk for this epoch; none when nothing is hidden.
    /// Display nodes near the root hold points of many chunks.
    hidden: HashMap<(Uuid, u32), Option<Arc<Vec<u8>>>>,
    limit: usize,
    clock: u64,
    stats: ViewCacheStats,
}
impl ViewCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            epoch: None,
            entries: HashMap::new(),
            hidden: HashMap::new(),
            limit: max_bytes,
            clock: 0,
            stats: ViewCacheStats::default(),
        }
    }
    pub fn stats(&self) -> ViewCacheStats {
        self.stats
    }
    /// Room for about `points` samples, like Potree's point load limit of
    /// twice the point budget. Shrinking evicts the least recently used.
    pub fn set_point_limit(&mut self, points: usize) {
        self.limit = points.saturating_mul(std::mem::size_of::<Sample>());
        self.evict(0);
    }
    /// Evicts least recently used entries until `incoming` more bytes fit.
    fn evict(&mut self, incoming: usize) {
        while self.stats.resident_bytes + incoming > self.limit {
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
            self.hidden.clear();
            self.stats.resident_bytes = 0;
            self.epoch = Some(epoch);
        }
    }
    /// Whether a node's points for the project's current state are cached.
    pub fn contains(&mut self, project: &Project, scan: Uuid, node: u32) -> bool {
        self.prepare(project);
        self.entries.contains_key(&Key { scan, node })
    }
    pub(crate) fn samples(
        &mut self,
        project: &Project,
        scan: &Scan,
        node: u32,
        job: &JobControl,
    ) -> Result<Arc<[Sample]>> {
        let key = Key {
            scan: scan.id,
            node,
        };
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            self.stats.hits += 1;
            entry.used = self.clock;
            return Ok(entry.samples.clone());
        }
        self.stats.misses += 1;
        let samples = project.read_view(scan, node)?;
        let mut result = Vec::with_capacity(samples.len());
        for (i, sample) in samples.into_iter().enumerate() {
            if i % 8192 == 0 {
                job.check()?;
            }
            let hidden = match self.hidden.entry((scan.id, sample.chunk)) {
                std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
                std::collections::hash_map::Entry::Vacant(e) => e
                    .insert(
                        (project.hidden_count(scan, sample.chunk) > 0)
                            .then(|| project.hidden_mask(scan, sample.chunk).map(Arc::new))
                            .transpose()?,
                    )
                    .clone(),
            };
            if hidden.is_some_and(|hidden| is_set(&hidden, sample.index as usize)) {
                continue;
            }
            result.push(sample);
        }
        job.check()?;
        let samples: Arc<[Sample]> = result.into();
        // Charge entry/Arc/hash-table overhead too, including nodes left empty.
        let bytes = std::mem::size_of_val(samples.as_ref()) + 128;
        if bytes <= self.limit {
            self.evict(bytes);
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
