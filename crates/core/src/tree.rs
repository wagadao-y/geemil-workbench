//! The scan tree: folders, membership and composed transforms.
//!
//! Every scan and folder may have an additional rigid transform. A scan's
//! correction is the product of its folders' transforms, outermost first, and
//! its own. Moving items between folders compensates their own transform so the
//! points stay where they are.
use crate::{Group, JobControl, Pose, Project, Revision, Scan, ViewCache};
use anyhow::{Result, ensure};
use glam::{DMat4, DQuat, DVec3};
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;

/// The folder containing a scan or folder in `state`.
pub(crate) fn parent_of(state: &Revision, id: Uuid) -> Option<Uuid> {
    match state.groups.iter().find(|g| g.id == id) {
        Some(group) => group.parent,
        None => state.scan_groups.get(&id).copied(),
    }
}

/// Folder transforms above `id` and its own, outermost first.
pub(crate) fn correction(state: &Revision, id: Uuid) -> DMat4 {
    correction_with(state, id, None)
}

/// [`correction`] with the own transform of one scan or folder replaced.
fn correction_with(state: &Revision, id: Uuid, replaced: Option<(Uuid, Pose)>) -> DMat4 {
    let own = |id| {
        match replaced {
            Some((r, pose)) if r == id => Some(pose),
            _ => state.transforms.get(&id).copied(),
        }
        .unwrap_or_default()
        .matrix()
    };
    let mut matrix = own(id);
    let mut parent = parent_of(state, id);
    // Validated states are acyclic; the bound only guards corrupt input.
    for _ in 0..=state.groups.len() {
        let Some(group) = parent else { break };
        matrix = own(group) * matrix;
        parent = parent_of(state, group);
    }
    matrix
}

fn is_within(state: &Revision, id: Uuid, ancestor: Uuid) -> bool {
    let mut current = Some(id);
    for _ in 0..=state.groups.len() {
        match current {
            Some(c) if c == ancestor => return true,
            Some(c) => current = parent_of(state, c),
            None => return false,
        }
    }
    false
}

/// Moves `id` into `target` and adjusts its own transform to keep its place.
fn place(state: &mut Revision, id: Uuid, target: Option<Uuid>) {
    let world = correction(state, id);
    let above = target.map_or(DMat4::IDENTITY, |t| correction(state, t));
    if let Some(group) = state.groups.iter_mut().find(|g| g.id == id) {
        group.parent = target;
    } else if let Some(target) = target {
        state.scan_groups.insert(id, target);
    } else {
        state.scan_groups.remove(&id);
    }
    let own = Pose::from_matrix(above.inverse() * world);
    if own.matrix().abs_diff_eq(DMat4::IDENTITY, 1e-12) {
        state.transforms.remove(&id);
    } else {
        state.transforms.insert(id, own);
    }
}

impl Project {
    /// The additional transform of a scan or folder, including all folders above.
    pub fn correction(&self, id: Uuid) -> DMat4 {
        correction(self.current(), id)
    }
    /// [`Project::correction`] as it would be with `pose` as the own transform
    /// of `item`, for previewing an edit before applying it.
    pub fn correction_with(&self, id: Uuid, item: Uuid, pose: Pose) -> DMat4 {
        correction_with(self.current(), id, Some((item, pose)))
    }
    /// [`Project::world_matrix`] as it would be with `pose` as the own transform
    /// of `item` (a scan or folder), for previewing an edit before applying it.
    pub fn world_matrix_with(&self, scan: &Scan, item: Uuid, pose: Pose) -> DMat4 {
        correction_with(self.current(), scan.id, Some((item, pose)))
            * scan.original_pose.unwrap_or_default().matrix()
    }
    pub fn parent_of(&self, id: Uuid) -> Option<Uuid> {
        parent_of(self.current(), id)
    }
    pub fn groups(&self) -> &[Group] {
        &self.current().groups
    }
    /// Folders and scans directly in `parent` (None: top level), in display order.
    pub fn children(&self, parent: Option<Uuid>) -> (Vec<&Group>, Vec<&Scan>) {
        let state = self.current();
        let groups = state.groups.iter().filter(|g| g.parent == parent).collect();
        let scans = self
            .scans()
            .filter(|s| state.scan_groups.get(&s.id).copied() == parent)
            .collect();
        (groups, scans)
    }
    /// Scans in `id` and all folders below it; a scan id yields itself.
    pub fn scans_within(&self, id: Uuid) -> Vec<Uuid> {
        let state = self.current();
        let parents: HashMap<Uuid, Option<Uuid>> =
            state.groups.iter().map(|g| (g.id, g.parent)).collect();
        // `is_within` with the folders looked up by id.
        let within = |scan: Uuid| {
            let mut current = Some(scan);
            for _ in 0..=state.groups.len() {
                match current {
                    Some(c) if c == id => return true,
                    Some(c) => {
                        current = match parents.get(&c) {
                            Some(parent) => *parent,
                            None => state.scan_groups.get(&c).copied(),
                        }
                    }
                    None => return false,
                }
            }
            false
        };
        self.scans()
            .filter(|s| within(s.id))
            .map(|s| s.id)
            .collect()
    }
    pub fn create_group(&mut self, name: String, parent: Option<Uuid>) -> Result<Uuid> {
        let id = Uuid::new_v4();
        self.edit(
            json!({"kind": "create_group", "id": id, "name": name}),
            |s| {
                ensure!(
                    parent.is_none_or(|p| s.groups.iter().any(|g| g.id == p)),
                    "Missing folder"
                );
                s.groups.push(Group { id, name, parent });
                Ok(())
            },
        )?;
        Ok(id)
    }
    pub fn rename_group(&mut self, id: Uuid, name: String) -> Result<()> {
        self.edit(
            json!({"kind": "rename_group", "id": id, "name": name}),
            |s| {
                let group = s.groups.iter_mut().find(|g| g.id == id);
                group.ok_or_else(|| anyhow::anyhow!("Missing folder"))?.name = name;
                Ok(())
            },
        )
    }
    /// Moves scans and folders into `target` (None: top level). Points keep
    /// their place in the project frame.
    pub fn move_to_group(&mut self, ids: &[Uuid], target: Option<Uuid>) -> Result<()> {
        self.edit(
            json!({"kind": "move", "items": ids, "target": target}),
            |s| {
                ensure!(
                    target.is_none_or(|t| s.groups.iter().any(|g| g.id == t)),
                    "Missing folder"
                );
                for &id in ids {
                    let is_group = s.groups.iter().any(|g| g.id == id);
                    ensure!(is_group || s.scans.contains(&id), "Missing scan or folder");
                    ensure!(
                        !is_group || target.is_none_or(|t| !is_within(s, t, id)),
                        "A folder cannot move into itself"
                    );
                    place(s, id, target);
                }
                Ok(())
            },
        )
    }
    /// Removes a folder; its contents move up one level and keep their place.
    pub fn ungroup(&mut self, id: Uuid) -> Result<()> {
        self.edit(json!({"kind": "ungroup", "id": id}), |s| {
            let group = s.groups.iter().find(|g| g.id == id);
            let parent = group
                .ok_or_else(|| anyhow::anyhow!("Missing folder"))?
                .parent;
            let children: Vec<_> = s
                .groups
                .iter()
                .filter(|g| g.parent == Some(id))
                .map(|g| g.id)
                .chain(
                    s.scan_groups
                        .iter()
                        .filter(|(_, g)| **g == id)
                        .map(|(scan, _)| *scan),
                )
                .collect();
            for child in children {
                place(s, child, parent);
            }
            s.groups.retain(|g| g.id != id);
            s.transforms.remove(&id);
            Ok(())
        })
    }
    /// Takes scans out of the working state. Their data stays until cleanup,
    /// so undo can bring them back.
    pub fn remove_scans(&mut self, ids: &[Uuid]) -> Result<()> {
        self.edit(json!({"kind": "remove_scans", "scans": ids}), |s| {
            for id in ids {
                ensure!(s.scans.contains(id), "Missing scan");
                s.scan_groups.remove(id);
                s.transforms.remove(id);
            }
            s.scans.retain(|id| !ids.contains(id));
            Ok(())
        })
    }
    /// Where a scan sits for [`Project::scatter_scans`], in the project
    /// frame: the median X and Y and the 1st percentile Z (its floor) of the
    /// points of its root display node in visible layers. The root holds an
    /// even sample of the whole scan, cheap to read, and stray points or
    /// points moved to a hidden layer barely move or do not move these. None
    /// when the root has no such point.
    pub fn scan_anchor(&self, scan: &Scan) -> Result<Option<DVec3>> {
        if scan.nodes.is_empty() {
            return Ok(None);
        }
        let root = ViewCache::new(0).samples(self, scan, 0, &JobControl::default())?;
        if root.is_empty() {
            return Ok(None);
        }
        let world = self.world_matrix(scan);
        let points: Vec<DVec3> = root
            .positions()
            .map(|p| world.transform_point3(p))
            .collect();
        let quantile = |axis: usize, q: f64| {
            let mut values: Vec<f64> = points.iter().map(|p| p[axis]).collect();
            let at = ((values.len() - 1) as f64 * q).round() as usize;
            *values.select_nth_unstable_by(at, f64::total_cmp).1
        };
        Ok(Some(DVec3::new(
            quantile(0, 0.5),
            quantile(1, 0.5),
            quantile(2, 0.01),
        )))
    }
    /// Undoes the registration on purpose, for practising it: turns each scan
    /// by a random angle about Z, gathers their [`Project::scan_anchor`]s at
    /// the mean of their X and Y and drops them to the lowest anchor's Z, so
    /// the scans overlap and their floors meet. Scans without points in
    /// visible layers stay. One edit, one undo.
    pub fn scatter_scans(&mut self, seed: u64) -> Result<()> {
        let mut anchors = vec![];
        for scan in self.scans() {
            if let Some(anchor) = self.scan_anchor(scan)? {
                anchors.push((scan, anchor));
            }
        }
        ensure!(!anchors.is_empty(), "No points in visible layers");
        let mean = anchors.iter().map(|(_, a)| *a).sum::<DVec3>() / anchors.len() as f64;
        let floor = anchors
            .iter()
            .map(|(_, a)| a.z)
            .fold(f64::INFINITY, f64::min);
        let target = DVec3::new(mean.x, mean.y, floor);
        let mut random = seed;
        let mut next_angle = || {
            // SplitMix64; no randomness crate for a practice tool.
            random = random.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = random;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            (z >> 11) as f64 / (1u64 << 53) as f64 * std::f64::consts::TAU
        };
        let poses: Vec<(Uuid, Pose)> = anchors
            .iter()
            .map(|(scan, anchor)| {
                // A turn about Z keeps the anchor's height, which then drops
                // to the floor.
                let motion = DMat4::from_translation(target)
                    * DMat4::from_quat(DQuat::from_rotation_z(next_angle()))
                    * DMat4::from_translation(-*anchor);
                let above = self
                    .parent_of(scan.id)
                    .map_or(DMat4::IDENTITY, |g| self.correction(g));
                let own = above.inverse() * motion * self.correction(scan.id);
                (scan.id, Pose::from_matrix(own))
            })
            .collect();
        let ids: Vec<Uuid> = poses.iter().map(|(id, _)| *id).collect();
        let scans = self.scans_record(&ids);
        self.edit(
            json!({"kind": "scatter", "scans": scans, "seed": seed}),
            |s| {
                for (id, pose) in poses {
                    s.transforms.insert(id, pose);
                }
                Ok(())
            },
        )
    }
}
