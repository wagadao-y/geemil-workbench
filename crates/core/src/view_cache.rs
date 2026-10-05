use crate::layers::is_set;
use crate::{JobControl, Project, Sample, Scan};
use anyhow::Result;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};
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
/// Visible points per display octree node of a scan: in the node itself and
/// in its subtree. They depend only on the scan and the epoch.
pub(crate) struct NodeEstimates {
    pub own: Vec<f64>,
    pub below: Vec<f64>,
}
/// A cached item, in [`ViewCache::order`].
#[derive(Clone, Copy)]
enum Slot {
    Node(Key),
    Hidden((Uuid, u32)),
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
    /// Per scan for this epoch; small, so not charged to the limit.
    estimates: HashMap<Uuid, Arc<NodeEstimates>>,
    /// Entries and hidden masks by when they were last used, oldest first.
    order: BTreeMap<u64, Slot>,
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
            estimates: HashMap::new(),
            order: BTreeMap::new(),
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
            let Some((_, slot)) = self.order.pop_first() else {
                break;
            };
            self.forget(slot);
        }
    }
    /// Drops a cached item, already taken out of `order`.
    fn forget(&mut self, slot: Slot) {
        match slot {
            Slot::Node(key) => {
                let entry = self.entries.remove(&key).unwrap();
                self.stats.resident_bytes -= entry.bytes;
            }
            Slot::Hidden(key) => {
                let entry = self.hidden.remove(&key).unwrap();
                self.stats.resident_bytes -= entry.bytes;
                self.stats.hidden_bytes -= entry.bytes;
            }
        }
    }
    /// Moves a cached item last used at `used` to the end of `order`.
    fn touch(&mut self, used: u64, slot: Slot) -> u64 {
        self.clock += 1;
        self.order.remove(&used);
        self.order.insert(self.clock, slot);
        self.clock
    }
    fn hidden_mask(
        &mut self,
        project: &Project,
        scan: &Scan,
        chunk: u32,
    ) -> Result<Option<Arc<Vec<u8>>>> {
        let key = (scan.id, chunk);
        if let Some(used) = self.hidden.get(&key).map(|e| e.used) {
            let used = self.touch(used, Slot::Hidden(key));
            let entry = self.hidden.get_mut(&key).unwrap();
            entry.used = used;
            return Ok(entry.mask.clone());
        }
        let mask = (project.hidden_count(scan, chunk) > 0)
            .then(|| project.hidden_mask(scan, chunk).map(Arc::new))
            .transpose()?;
        let bytes = mask.as_ref().map_or(0, |m| m.capacity()) + 128;
        if bytes <= self.limit {
            self.evict(bytes);
            self.clock += 1;
            self.order.insert(self.clock, Slot::Hidden(key));
            self.hidden.insert(
                key,
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
        let visible = || state.layers.iter().filter(|l| l.visible).map(|l| l.code);
        // Called for every node; compare in place and copy only on a change.
        let same = self.epoch.as_ref().is_some_and(|(root, labels, codes)| {
            *root == project.root && *labels == state.labels && codes.iter().copied().eq(visible())
        });
        if !same {
            self.entries.clear();
            self.hidden.clear();
            self.estimates.clear();
            self.order.clear();
            self.stats.resident_bytes = 0;
            self.stats.hidden_bytes = 0;
            self.epoch = Some((
                project.root.clone(),
                state.labels.clone(),
                visible().collect(),
            ));
        }
    }
    /// The [`NodeEstimates`] of `scans` for the epoch [`ViewCache::prepare`]
    /// set, parallel to them. Missing ones are counted together, reading the
    /// state's label patches once.
    pub(crate) fn estimates(
        &mut self,
        project: &Project,
        scans: &[&Scan],
    ) -> Vec<Arc<NodeEstimates>> {
        let missing: Vec<&Scan> = scans
            .iter()
            .filter(|s| !self.estimates.contains_key(&s.id))
            .copied()
            .collect();
        if !missing.is_empty() {
            for (scan, hidden) in missing.iter().zip(project.hidden_counts(&missing)) {
                let estimates = Project::visible_estimates(scan, &hidden);
                self.estimates.insert(scan.id, Arc::new(estimates));
            }
        }
        scans
            .iter()
            .map(|s| self.estimates[&s.id].clone())
            .collect()
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
        if let Some(used) = self.entries.get(&key).map(|e| e.used) {
            self.stats.hits += 1;
            let used = self.touch(used, Slot::Node(key));
            let entry = self.entries.get_mut(&key).unwrap();
            entry.used = used;
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
            // A node decoded twice replaces its first entry.
            if let Some(old) = self.entries.get(&key).map(|e| e.used) {
                self.order.remove(&old);
                self.forget(Slot::Node(key));
            }
            self.evict(bytes);
            self.clock += 1;
            self.order.insert(self.clock, Slot::Node(key));
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
