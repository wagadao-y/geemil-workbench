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
    pub hidden_bytes: usize,
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
struct HiddenEntry {
    mask: Option<Arc<Vec<u8>>>,
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
    hidden: HashMap<(Uuid, u32), HiddenEntry>,
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
            let node = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(key, e)| (*key, e.used));
            let hidden = self
                .hidden
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(key, e)| (*key, e.used));
            if let Some((key, used)) = hidden
                && node.is_none_or(|(_, node_used)| used <= node_used)
            {
                let entry = self.hidden.remove(&key).unwrap();
                self.stats.resident_bytes -= entry.bytes;
                self.stats.hidden_bytes -= entry.bytes;
            } else if let Some((key, _)) = node {
                let entry = self.entries.remove(&key).unwrap();
                self.stats.resident_bytes -= entry.bytes;
            } else {
                break;
            }
        }
    }
    fn hidden_mask(
        &mut self,
        project: &Project,
        scan: &Scan,
        chunk: u32,
    ) -> Result<Option<Arc<Vec<u8>>>> {
        self.clock += 1;
        if let Some(entry) = self.hidden.get_mut(&(scan.id, chunk)) {
            entry.used = self.clock;
            return Ok(entry.mask.clone());
        }
        let mask = (project.hidden_count(scan, chunk) > 0)
            .then(|| project.hidden_mask(scan, chunk).map(Arc::new))
            .transpose()?;
        let bytes = mask.as_ref().map_or(0, |m| m.capacity()) + 128;
        if bytes <= self.limit {
            self.evict(bytes);
            self.hidden.insert(
                (scan.id, chunk),
                HiddenEntry {
                    mask: mask.clone(),
                    used: self.clock,
                    bytes,
                },
            );
            self.stats.resident_bytes += bytes;
            self.stats.hidden_bytes += bytes;
        }
        Ok(mask)
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
            self.stats.hidden_bytes = 0;
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
        let samples = project.read_view(scan, node)?;
        self.store_decoded(project, scan, node, samples, job)
    }

    /// Filters decoded samples and installs them on the cache's owning thread.
    pub(crate) fn store_decoded(
        &mut self,
        project: &Project,
        scan: &Scan,
        node: u32,
        samples: Vec<Sample>,
        job: &JobControl,
    ) -> Result<Arc<[Sample]>> {
        self.prepare(project);
        self.stats.misses += 1;
        let key = Key {
            scan: scan.id,
            node,
        };
        self.clock += 1;
        let samples: Arc<[Sample]> = if project.current().layers.iter().all(|l| l.visible) {
            job.check()?;
            samples.into()
        } else {
            let mut result = Vec::with_capacity(samples.len());
            // Keep the last mask locally even when it cannot fit in the cache.
            let mut last: Option<(u32, Option<Arc<Vec<u8>>>)> = None;
            for (i, sample) in samples.into_iter().enumerate() {
                if i % 8192 == 0 {
                    job.check()?;
                }
                if last
                    .as_ref()
                    .is_none_or(|(chunk, _)| *chunk != sample.chunk)
                {
                    last = Some((sample.chunk, self.hidden_mask(project, scan, sample.chunk)?));
                }
                let hidden = &last.as_ref().unwrap().1;
                if hidden
                    .as_ref()
                    .is_some_and(|hidden| is_set(hidden, sample.index as usize))
                {
                    continue;
                }
                result.push(sample);
            }
            job.check()?;
            result.into()
        };
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
