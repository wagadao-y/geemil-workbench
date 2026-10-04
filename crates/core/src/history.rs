//! The unsaved working state, saved revisions and cleanup of unused data.
//!
//! Edits change only the working state (`Manifest::draft`), which is written to
//! disk after every edit but becomes a revision only when the user saves. Undo
//! and redo replace the working state with an earlier snapshot of it; point
//! data and labels are immutable, so this never rewrites them.
use crate::{DEFAULT_LAYER, FORMAT_VERSION, Manifest, Project, Revision};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Checks that a state only refers to scans, label patches and folders that
/// exist, that its layers have distinct codes including the default one, and
/// that folders form a tree.
pub(crate) fn validate_state(manifest: &Manifest, state: &Revision) -> Result<()> {
    for id in &state.scans {
        ensure!(
            manifest.scans.iter().any(|s| s.id == *id),
            "State refers to a missing scan"
        );
    }
    for id in &state.labels {
        ensure!(
            manifest.patches.iter().any(|p| p.id == *id),
            "State refers to missing labels"
        );
    }
    let codes: BTreeSet<_> = state.layers.iter().map(|l| l.code).collect();
    ensure!(codes.len() == state.layers.len(), "Duplicate layer");
    ensure!(codes.contains(&DEFAULT_LAYER), "Missing default layer");
    let groups: BTreeSet<_> = state.groups.iter().map(|g| g.id).collect();
    ensure!(groups.len() == state.groups.len(), "Duplicate folder");
    for group in &state.groups {
        let mut parent = group.parent;
        let mut steps = 0;
        while let Some(id) = parent {
            ensure!(groups.contains(&id), "Folder refers to a missing parent");
            ensure!(steps <= groups.len(), "Folders form a cycle");
            parent = state.groups.iter().find(|g| g.id == id).unwrap().parent;
            steps += 1;
        }
    }
    for (scan, group) in &state.scan_groups {
        ensure!(
            state.scans.contains(scan) && groups.contains(group),
            "Scan folder refers to a missing scan or folder"
        );
    }
    Ok(())
}

/// What `Project::cleanup` removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub scans: usize,
    /// Label patches no longer used.
    pub labels: usize,
    pub files: usize,
    pub bytes: u64,
}

impl Project {
    /// Applies `change` to the working state, creating it from the current
    /// revision first, records `operation` for the next save and writes the
    /// project. On failure nothing changes.
    pub(crate) fn edit(
        &mut self,
        operation: Value,
        change: impl FnOnce(&mut Revision) -> Result<()>,
    ) -> Result<()> {
        let mut state = self.current().clone();
        if self.manifest.draft.is_none() {
            state.parent = Some(self.manifest.current);
            state.name = String::new();
            state.operation = json!({"kind": "edits", "operations": []});
            state.saved_at = None;
        }
        // A new identity per edit: caches and previews key on it.
        state.id = Uuid::new_v4();
        change(&mut state)?;
        if let Some(operations) = state
            .operation
            .get_mut("operations")
            .and_then(Value::as_array_mut)
        {
            operations.push(operation);
        }
        validate_state(&self.manifest, &state)?;
        let mut next = self.clone();
        next.manifest.draft = Some(state);
        next.manifest.format_version = FORMAT_VERSION;
        next.save()?;
        *self = next;
        Ok(())
    }
    /// How an operation records the scans of the current state it applied
    /// to: `"all"`, `{"except": [...]}` naming the ones left out, or the list,
    /// whichever is shortest. Every edit keeps its record in the working
    /// state and its undo snapshots, so a list of hundreds of scans per edit
    /// made the history grow with edits times scans.
    pub(crate) fn scans_record(&self, ids: &[Uuid]) -> Value {
        let state = &self.current().scans;
        let wanted: HashSet<&Uuid> = ids.iter().collect();
        let applied: Vec<&Uuid> = state.iter().filter(|id| wanted.contains(id)).collect();
        let skipped: Vec<&Uuid> = state.iter().filter(|id| !wanted.contains(id)).collect();
        if skipped.is_empty() {
            json!("all")
        } else if skipped.len() < applied.len() {
            json!({ "except": skipped })
        } else {
            json!(applied)
        }
    }
    pub fn has_unsaved_changes(&self) -> bool {
        self.manifest.draft.is_some()
    }
    /// Turns the working state into a new revision, a child of the revision it
    /// was based on, and makes it current.
    pub fn save_revision(&mut self, name: String) -> Result<Uuid> {
        let Some(draft) = &self.manifest.draft else {
            anyhow::bail!("No unsaved changes");
        };
        let mut revision = draft.clone();
        // A saved revision's file is written once; never let another state
        // take its id.
        if self.manifest.revisions.iter().any(|r| r.id == revision.id) {
            revision.id = Uuid::new_v4();
        }
        revision.name = name;
        revision.saved_at = Some(now());
        let id = revision.id;
        let mut next = self.clone();
        next.manifest.revisions.push(revision);
        next.manifest.current = id;
        next.manifest.draft = None;
        next.manifest.format_version = FORMAT_VERSION;
        next.save()?;
        *self = next;
        Ok(id)
    }
    /// Shows a saved revision, discarding unsaved changes.
    pub fn switch(&mut self, id: Uuid) -> Result<()> {
        ensure!(
            self.manifest.revisions.iter().any(|r| r.id == id),
            "Revision does not exist"
        );
        let mut next = self.clone();
        next.manifest.current = id;
        next.manifest.draft = None;
        next.save()?;
        *self = next;
        Ok(())
    }
    pub fn discard_changes(&mut self) -> Result<()> {
        self.switch(self.manifest.current)
    }
    /// Replaces the working state, for undo and redo. `None` returns to the
    /// current revision. The state keeps its identity, so cached views of it
    /// stay valid.
    pub fn restore_working_state(&mut self, state: Option<Revision>) -> Result<()> {
        let mut next = self.clone();
        if let Some(mut state) = state {
            validate_state(&next.manifest, &state)?;
            state.parent = Some(next.manifest.current);
            state.saved_at = None;
            next.manifest.draft = Some(state);
        } else {
            next.manifest.draft = None;
        }
        next.save()?;
        *self = next;
        Ok(())
    }
    pub fn rename_revision(&mut self, id: Uuid, name: String) -> Result<()> {
        let mut next = self.clone();
        let revision = next
            .manifest
            .revisions
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("Revision does not exist"))?;
        revision.name = name;
        next.save()?;
        *self = next;
        Ok(())
    }
    /// Removes a saved revision other than the current one. Its children move
    /// to its parent. Data it used stays until `cleanup`.
    pub fn delete_revision(&mut self, id: Uuid) -> Result<()> {
        ensure!(
            id != self.manifest.current,
            "The current revision cannot be deleted"
        );
        let mut next = self.clone();
        let index = next
            .manifest
            .revisions
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("Revision does not exist"))?;
        let removed = next.manifest.revisions.remove(index);
        for r in &mut next.manifest.revisions {
            if r.parent == Some(id) {
                r.parent = removed.parent;
            }
        }
        next.save()?;
        *self = next;
        Ok(())
    }
    /// Deletes scans, label patches and files that no saved revision and not the
    /// working state refer to, and leftovers of interrupted jobs. Undo history
    /// kept elsewhere may refer to removed data and must be dropped.
    pub fn cleanup(&mut self) -> Result<CleanupReport> {
        let states: Vec<_> = self
            .manifest
            .revisions
            .iter()
            .chain(&self.manifest.draft)
            .collect();
        let scans: BTreeSet<_> = states.iter().flat_map(|s| &s.scans).copied().collect();
        let labels: BTreeSet<_> = states.iter().flat_map(|s| &s.labels).copied().collect();
        // Metadata first, so an interruption leaves only unreferenced files.
        let mut next = self.clone();
        next.manifest.scans.retain(|s| scans.contains(&s.id));
        next.manifest
            .images
            .retain(|i| i.scan_id.is_none_or(|id| scans.contains(&id)));
        next.manifest.patches.retain(|p| labels.contains(&p.id));
        let mut report = CleanupReport {
            scans: self.manifest.scans.len() - next.manifest.scans.len(),
            labels: self.manifest.patches.len() - next.manifest.patches.len(),
            ..Default::default()
        };
        next.save()?;
        *self = next;

        let mut referenced = BTreeSet::new();
        for s in &self.manifest.scans {
            referenced.extend([&s.template, &s.points_file, &s.view_file].map(|p| p.clone()));
            referenced.insert(crate::model::scan_metadata_path(s));
        }
        for p in &self.manifest.patches {
            referenced.insert(p.file.clone());
            referenced.insert(crate::model::patch_metadata_path(p));
        }
        let remove = |path: &std::path::Path, report: &mut CleanupReport| -> Result<()> {
            let (files, bytes) = measure(path)?;
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
            report.files += files;
            report.bytes += bytes;
            Ok(())
        };
        // Each import owns one directory under data/: shared metadata plus the
        // points and LOD of its scans.
        for entry in fs::read_dir(self.root.join("data"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let prefix = format!("data/{name}/");
            if !referenced.iter().any(|p| p.starts_with(&prefix)) {
                remove(&entry.path(), &mut report)?;
                continue;
            }
            if !entry.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(entry.path())? {
                let file = file?;
                let file_name = file.file_name().to_string_lossy().into_owned();
                let unused = !referenced.contains(&format!("{prefix}{file_name}"));
                let scan_asset = [".points", ".view", ".scan.json"]
                    .iter()
                    .any(|ext| file_name.ends_with(ext));
                if unused && scan_asset {
                    remove(&file.path(), &mut report)?;
                }
            }
        }
        for entry in fs::read_dir(self.root.join("labels"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !referenced.contains(&format!("labels/{name}")) {
                remove(&entry.path(), &mut report)?;
            }
        }
        // Files of deleted revisions.
        let saved: BTreeSet<_> = self
            .manifest
            .revisions
            .iter()
            .map(|r| crate::model::revision_path(r.id))
            .collect();
        let history = self.root.join("history");
        if history.is_dir() {
            for entry in fs::read_dir(history)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !saved.contains(&format!("history/{name}")) {
                    remove(&entry.path(), &mut report)?;
                }
            }
        }
        for entry in fs::read_dir(self.root.join("staging"))? {
            remove(&entry?.path(), &mut report)?;
        }
        // Temporary manifests from interrupted saves.
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("project-") && name.ends_with(".tmp") {
                remove(&entry.path(), &mut report)?;
            }
        }
        Ok(report)
    }
}

/// File count and bytes below `path`.
fn measure(path: &std::path::Path) -> Result<(usize, u64)> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok((1, meta.len()));
    }
    let mut total = (0, 0);
    for entry in fs::read_dir(path)? {
        let (files, bytes) = measure(&entry?.path())?;
        total.0 += files;
        total.1 += bytes;
    }
    Ok(total)
}
