//! Alignment records: each applied alignment keeps how well it fitted and
//! where it put the item, so the tree can tell aligned scans from the rest and
//! from those moved since.
use crate::{AlignmentFit, Pose, Project, Registration};
use anyhow::{Result, ensure};
use glam::DMat4;
use serde_json::json;
use uuid::Uuid;

/// How far apart two places may be, in matrix entries, and still be the same:
/// well below what any edit moves, above rounding.
const SAME_PLACE: f64 = 1e-6;

/// Where a scan's alignment comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegistrationState {
    /// The scan, or the folder above it that was aligned as a whole.
    pub item: Uuid,
    pub registration: Registration,
    /// Whether the item has moved since.
    pub moved: bool,
}

fn same_place(a: DMat4, b: DMat4) -> bool {
    a.to_cols_array()
        .iter()
        .zip(b.to_cols_array())
        .all(|(x, y)| (x - y).abs() <= SAME_PLACE)
}

impl Project {
    /// The last alignment of a scan or folder, and whether it moved since.
    pub fn registration(&self, id: Uuid) -> Option<RegistrationState> {
        let registration = *self.current().registrations.get(&id)?;
        Some(RegistrationState {
            item: id,
            registration,
            moved: !same_place(self.correction(id), registration.placed.matrix()),
        })
    }
    /// The alignment a scan has: its own, or that of a folder above it
    /// aligned as a whole, the nearest that still holds. A scan aligned alone
    /// and then moved with its folder has the folder's. When none holds, the
    /// nearest, which tells it moved.
    pub fn scan_registration(&self, scan: Uuid) -> Option<RegistrationState> {
        let mut nearest = None;
        let mut at = Some(scan);
        while let Some(id) = at {
            if let Some(state) = self.registration(id) {
                if !state.moved {
                    return Some(state);
                }
                nearest.get_or_insert(state);
            }
            at = self.parent_of(id);
        }
        nearest
    }
    /// Sets the own transform of a scan or folder as an alignment found it,
    /// with how well it fitted. One edit, one undo.
    pub fn apply_alignment(&mut self, id: Uuid, pose: Pose, fit: AlignmentFit) -> Result<()> {
        self.apply_alignments(&[(id, pose, fit)])
    }
    /// [`Project::apply_alignment`] for several items at once.
    pub fn apply_alignments(&mut self, items: &[(Uuid, Pose, AlignmentFit)]) -> Result<()> {
        ensure!(!items.is_empty(), "Nothing to align");
        let at = crate::history::now();
        // Where each item lands: its folders, as they will be, then its own.
        let mut state = self.current().clone();
        for (id, pose, _) in items {
            state.transforms.insert(*id, *pose);
        }
        let placed: Vec<Registration> = items
            .iter()
            .map(|(id, _, fit)| Registration {
                fit: *fit,
                placed: Pose::from_matrix(crate::tree::correction(&state, *id)),
                at,
            })
            .collect();
        let operation = match items {
            [(id, pose, fit)] => json!({
                "kind": "align", "id": id, "pose": pose, "method": fit.method,
            }),
            _ => {
                let ids: Vec<Uuid> = items.iter().map(|(id, ..)| *id).collect();
                json!({"kind": "align_all", "scans": self.scans_record(&ids)})
            }
        };
        self.edit(operation, |s| {
            for ((id, pose, _), registration) in items.iter().zip(placed) {
                ensure!(
                    s.scans.contains(id) || s.groups.iter().any(|g| g.id == *id),
                    "Missing scan or folder"
                );
                if *pose == Pose::default() {
                    s.transforms.remove(id);
                } else {
                    s.transforms.insert(*id, *pose);
                }
                s.registrations.insert(*id, registration);
            }
            Ok(())
        })
    }
}
