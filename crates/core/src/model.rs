use crate::CoreError;
use anyhow::{Context, Result, ensure};
use glam::{DMat4, DQuat, DVec3};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

/// The first format. It changes in place until a release needs compatibility.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}
impl Bounds {
    pub fn at(p: [f64; 3]) -> Self {
        Self { min: p, max: p }
    }
    pub fn include(&mut self, p: [f64; 3]) {
        for (i, v) in p.into_iter().enumerate() {
            self.min[i] = self.min[i].min(v);
            self.max[i] = self.max[i].max(v);
        }
    }
    pub fn center(&self) -> DVec3 {
        (DVec3::from(self.min) + DVec3::from(self.max)) * 0.5
    }
    pub fn radius(&self) -> f64 {
        DVec3::from(self.max).distance(DVec3::from(self.min)) * 0.5
    }
    pub fn corners(&self) -> impl Iterator<Item = DVec3> + '_ {
        (0..8).map(|i| {
            DVec3::new(
                if i & 1 == 0 { self.min[0] } else { self.max[0] },
                if i & 2 == 0 { self.min[1] } else { self.max[1] },
                if i & 4 == 0 { self.min[2] } else { self.max[2] },
            )
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pose {
    pub translation: [f64; 3],
    pub rotation_xyzw: [f64; 4],
}
impl Default for Pose {
    fn default() -> Self {
        Self {
            translation: [0.; 3],
            rotation_xyzw: [0., 0., 0., 1.],
        }
    }
}
impl Pose {
    pub fn matrix(&self) -> DMat4 {
        DMat4::from_rotation_translation(
            DQuat::from_array(self.rotation_xyzw).normalize(),
            DVec3::from(self.translation),
        )
    }
    pub fn from_e57(t: &e57::Transform) -> Self {
        Self {
            translation: [t.translation.x, t.translation.y, t.translation.z],
            rotation_xyzw: [t.rotation.x, t.rotation.y, t.rotation.z, t.rotation.w],
        }
    }
    pub fn to_e57(&self) -> e57::Transform {
        let [x, y, z, w] = self.rotation_xyzw;
        e57::Transform {
            rotation: e57::Quaternion { x, y, z, w },
            translation: e57::Translation {
                x: self.translation[0],
                y: self.translation[1],
                z: self.translation[2],
            },
        }
    }
    pub fn from_matrix(m: DMat4) -> Self {
        let (_, r, t) = m.to_scale_rotation_translation();
        let mut q = r.normalize().to_array();
        if q[3] < 0. {
            for v in &mut q {
                *v = -*v;
            }
        }
        Self {
            translation: t.to_array(),
            rotation_xyzw: q,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub offset: u64,
    pub count: u32,
    pub bounds: Bounds,
    pub codec: BlockCodec,
    pub stored_bytes: u32,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockCodec {
    Raw,
    ZstdShuffle,
}
/// A node of a scan's display octree. Like Potree 2's, the tree is additive:
/// every valid point is in exactly one node, so drawing a node and its
/// children adds detail without drawing any point twice. Node 0 is the root.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    /// Tight bounds of the points in this node and below, in scan coordinates.
    pub bounds: Bounds,
    pub children: Vec<u32>,
    /// This node's own points in the view file.
    pub count: u32,
    pub offset: u64,
    pub codec: BlockCodec,
    pub stored_bytes: u32,
    /// How many of this node's points come from each chunk, by chunk.
    pub chunks: Vec<(u32, u32)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scan {
    pub id: Uuid,
    pub guid: String,
    pub name: String,
    pub source_name: String,
    pub template: String,
    pub template_index: usize,
    pub original_pose: Option<Pose>,
    pub stride: usize,
    pub records: u64,
    pub valid_points: u64,
    pub omitted_attributes: Vec<String>,
    pub points_file: String,
    /// The display octree's points.
    pub view_file: String,
    /// Original points, the unit of processing: leaves of the octree that
    /// split the scan at import, never changed.
    pub chunks: Vec<Chunk>,
    /// The display octree.
    pub nodes: Vec<Node>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageInfo {
    pub guid: Option<String>,
    pub name: Option<String>,
    pub scan_id: Option<Uuid>,
    pub pose: Option<Pose>,
    pub projection: String,
}
/// The layer every point starts in. It cannot be removed.
pub const DEFAULT_LAYER: u8 = 0;

/// A layer of a state. Every point belongs to exactly one; work applies to
/// the points of visible layers only.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Layer {
    /// What point labels store; unique within a state.
    pub code: u8,
    pub name: String,
    pub visible: bool,
}
/// Where an operation moves points: an existing layer, or a new one it creates.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerTarget {
    Existing(u8),
    New(String),
}
/// The layer codes of one chunk's points, in point order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabelBlock {
    pub scan: Uuid,
    pub chunk: u32,
    pub offset: u64,
    /// Zstd frame of one byte per point; 0 when every point is in the default layer.
    pub bytes: u32,
    /// Points per layer other than the default one, by ascending code.
    pub counts: Vec<(u8, u64)>,
}
/// The labels of the chunks one operation changed. Never changes once
/// written; a state lists the patches it applies, and for each chunk the
/// last patch listing it holds its labels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabelPatch {
    pub id: Uuid,
    pub file: String,
    /// Sorted by scan and chunk.
    pub blocks: Vec<LabelBlock>,
}
impl LabelPatch {
    pub fn block(&self, scan: Uuid, chunk: u32) -> Option<&LabelBlock> {
        self.blocks
            .binary_search_by(|b| (b.scan, b.chunk).cmp(&(scan, chunk)))
            .ok()
            .map(|i| &self.blocks[i])
    }
}
/// A box in the project frame, turned about the vertical axis.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct CropBox {
    pub center: [f64; 3],
    /// Full edge lengths along the box's own axes, in metres.
    pub size: [f64; 3],
    /// Turn about Z, in radians.
    pub yaw: f64,
}
impl CropBox {
    /// Maps the project frame onto box coordinates in which the box is
    /// `[-1, 1]` on every axis.
    pub fn unit_matrix(&self) -> DMat4 {
        DMat4::from_scale(DVec3::from(self.size).map(|s| 2. / s))
            * DMat4::from_rotation_z(-self.yaw)
            * DMat4::from_translation(-DVec3::from(self.center))
    }
    pub fn contains(&self, p: DVec3) -> bool {
        self.unit_matrix()
            .transform_point3(p)
            .abs()
            .cmple(DVec3::ONE)
            .all()
    }
    /// The eight corners in the project frame.
    pub fn corners(&self) -> [DVec3; 8] {
        let to_world = self.unit_matrix().inverse();
        std::array::from_fn(|i| {
            to_world.transform_point3(DVec3::new(
                if i & 1 == 0 { -1. } else { 1. },
                if i & 2 == 0 { -1. } else { 1. },
                if i & 4 == 0 { -1. } else { 1. },
            ))
        })
    }
}
/// A folder in the scan tree. Its transform, kept in `Revision::transforms`
/// under its id, moves everything below it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub parent: Option<Uuid>,
}
/// A project state: a saved revision, or the unsaved working state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub name: String,
    /// What led here; for saved revisions `{"kind":"edits","operations":[...]}`.
    pub operation: serde_json::Value,
    pub scans: Vec<Uuid>,
    pub layers: Vec<Layer>,
    /// Label patches, applied in order.
    pub labels: Vec<Uuid>,
    /// Additional rigid transforms of scans and groups.
    pub transforms: BTreeMap<Uuid, Pose>,
    pub groups: Vec<Group>,
    /// The folder of each scan in `groups`; absent scans are at the top level.
    pub scan_groups: BTreeMap<Uuid, Uuid>,
    /// Unix seconds when the user saved this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub name: String,
    pub scans: Vec<Scan>,
    pub images: Vec<ImageInfo>,
    pub patches: Vec<LabelPatch>,
    pub revisions: Vec<Revision>,
    /// The saved revision the project shows, or the working state is based on.
    pub current: Uuid,
    /// Unsaved working state on top of `current`. Edits change only this; saving
    /// turns it into a revision. Kept on disk so a crash loses nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Revision>,
}
/// `project.json`: scans and label patches, which never change once written,
/// are referenced by the project-relative paths of their metadata files, so
/// an edit rewrites only the small history.
#[derive(Serialize, Deserialize)]
struct ManifestFile {
    format_version: u32,
    name: String,
    scans: Vec<String>,
    images: Vec<ImageInfo>,
    labels: Vec<String>,
    revisions: Vec<Revision>,
    current: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    draft: Option<Revision>,
}
/// Where a scan's metadata lives: next to its points.
pub(crate) fn scan_metadata_path(scan: &Scan) -> String {
    let stem = scan
        .points_file
        .strip_suffix(".points")
        .unwrap_or(&scan.points_file);
    format!("{stem}.scan.json")
}
/// Where a label patch's metadata lives: next to its labels.
pub(crate) fn patch_metadata_path(patch: &LabelPatch) -> String {
    let stem = patch.file.strip_suffix(".labels").unwrap_or(&patch.file);
    format!("{stem}.json")
}

#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
}
impl Project {
    pub fn create(root: &Path, name: &str) -> Result<Self> {
        ensure!(!root.exists(), CoreError::ProjectExists(root.to_owned()));
        fs::create_dir_all(root)?;
        for dir in ["data", "labels", "staging"] {
            fs::create_dir(root.join(dir))?;
        }
        let id = Uuid::new_v4();
        let p = Self {
            root: fs::canonicalize(root)?,
            manifest: Manifest {
                format_version: FORMAT_VERSION,
                name: name.into(),
                scans: vec![],
                images: vec![],
                patches: vec![],
                revisions: vec![Revision {
                    id,
                    parent: None,
                    name: "Project created".into(),
                    operation: serde_json::json!({"kind":"create"}),
                    scans: vec![],
                    layers: vec![Layer {
                        code: DEFAULT_LAYER,
                        name: "Points".into(),
                        visible: true,
                    }],
                    labels: vec![],
                    transforms: BTreeMap::new(),
                    groups: vec![],
                    scan_groups: BTreeMap::new(),
                    saved_at: Some(crate::history::now()),
                }],
                current: id,
                draft: None,
            },
        };
        p.save()?;
        Ok(p)
    }
    pub fn load(root: &Path) -> Result<Self> {
        ensure!(
            root.join("project.json").is_file(),
            CoreError::NotAProject(root.to_owned())
        );
        let root = fs::canonicalize(root)?;
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("project.json"))?)
                .context("Invalid project metadata")?;
        let version = value
            .get("format_version")
            .and_then(|v| v.as_u64())
            .context("Invalid project metadata")? as u32;
        ensure!(
            version == FORMAT_VERSION,
            CoreError::UnsupportedProjectFormat(version)
        );
        let file: ManifestFile =
            serde_json::from_value(value).context("Invalid project metadata")?;
        let read = |relative: &str| -> Result<Vec<u8>> {
            let p = Path::new(relative);
            ensure!(
                !p.as_os_str().is_empty()
                    && p.components().all(|c| matches!(c, Component::Normal(_))),
                "Invalid project asset path"
            );
            fs::read(root.join(p)).with_context(|| format!("Reading {relative}"))
        };
        let mut scans = vec![];
        for path in &file.scans {
            let scan: Scan = serde_json::from_slice(&read(path)?)
                .with_context(|| format!("Invalid scan metadata {path}"))?;
            ensure!(
                scan_metadata_path(&scan) == *path,
                "Misplaced scan metadata"
            );
            scans.push(scan);
        }
        let mut patches = vec![];
        for path in &file.labels {
            let patch: LabelPatch = serde_json::from_slice(&read(path)?)
                .with_context(|| format!("Invalid label metadata {path}"))?;
            ensure!(
                patch_metadata_path(&patch) == *path,
                "Misplaced label metadata"
            );
            ensure!(
                patch
                    .blocks
                    .windows(2)
                    .all(|w| (w[0].scan, w[0].chunk) < (w[1].scan, w[1].chunk)),
                "Unsorted label blocks"
            );
            patches.push(patch);
        }
        let manifest = Manifest {
            format_version: version,
            name: file.name,
            scans,
            images: file.images,
            patches,
            revisions: file.revisions,
            current: file.current,
            draft: file.draft,
        };
        ensure!(
            manifest.revisions.iter().any(|r| r.id == manifest.current),
            "Current revision is missing"
        );
        for state in manifest.revisions.iter().chain(&manifest.draft) {
            crate::history::validate_state(&manifest, state)?;
        }
        let p = Self { root, manifest };
        for s in &p.manifest.scans {
            ensure!((32..=1_048_576).contains(&s.stride), "Invalid point stride");
            p.path(&s.template)?;
            p.path(&s.points_file)?;
            p.path(&s.view_file)?;
        }
        for patch in &p.manifest.patches {
            p.path(&patch.file)?;
        }
        Ok(p)
    }
    pub fn path(&self, relative: &str) -> Result<PathBuf> {
        let p = Path::new(relative);
        ensure!(
            !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_))),
            "Invalid project asset path"
        );
        let mut result = self.root.clone();
        for component in p.components() {
            result.push(component.as_os_str());
        }
        Ok(result)
    }
    /// Writes `project.json`, after the metadata of any scan or label patch
    /// that has no file yet. Those never change once written, so an edit writes only
    /// the history.
    pub fn save(&self) -> Result<()> {
        let m = &self.manifest;
        let scans: Vec<_> = m.scans.iter().map(scan_metadata_path).collect();
        let labels: Vec<_> = m.patches.iter().map(patch_metadata_path).collect();
        for (scan, path) in m.scans.iter().zip(&scans) {
            self.write_once(path, scan)?;
        }
        for (patch, path) in m.patches.iter().zip(&labels) {
            self.write_once(path, patch)?;
        }
        let file = ManifestFile {
            format_version: FORMAT_VERSION,
            name: m.name.clone(),
            scans,
            images: m.images.clone(),
            labels,
            revisions: m.revisions.clone(),
            current: m.current,
            draft: m.draft.clone(),
        };
        self.write_atomic(Path::new("project.json"), |f| {
            serde_json::to_writer_pretty(&mut *f, &file)?;
            f.write_all(b"\n")?;
            Ok(())
        })
    }
    /// Writes `value` to `relative` unless the file exists.
    fn write_once(&self, relative: &str, value: &impl Serialize) -> Result<()> {
        let path = self.path(relative)?;
        if path.is_file() {
            return Ok(());
        }
        self.write_atomic(&path, |f| Ok(serde_json::to_writer(f, value)?))
    }
    /// Writes a file through a temporary file in the project root and renames
    /// it into place, so a crash never leaves it half written.
    fn write_atomic(
        &self,
        path: &Path,
        write: impl FnOnce(&mut fs::File) -> Result<()>,
    ) -> Result<()> {
        let tmp = self.root.join(format!("project-{}.tmp", Uuid::new_v4()));
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        let written = write(&mut f).and_then(|_| Ok(f.sync_all()?));
        drop(f);
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        fs::rename(tmp, self.root.join(path))?;
        Ok(())
    }
    /// The state everything reads: the working state, else the current revision.
    pub fn current(&self) -> &Revision {
        self.manifest.draft.as_ref().unwrap_or_else(|| self.base())
    }
    /// The saved revision the current state is, or is based on.
    pub fn base(&self) -> &Revision {
        self.manifest
            .revisions
            .iter()
            .find(|r| r.id == self.manifest.current)
            .expect("validated revision")
    }
    /// Local scan coordinates to the common project frame: folder transforms
    /// (outermost first), the scan's own additional transform, then its pose.
    pub fn world_matrix(&self, scan: &Scan) -> DMat4 {
        self.correction(scan.id) * scan.original_pose.unwrap_or_default().matrix()
    }
    pub fn bounds(&self) -> Bounds {
        let mut result: Option<Bounds> = None;
        for s in self.scans() {
            if let Some(n) = s.nodes.first() {
                for c in n.bounds.corners() {
                    let p = self.world_matrix(s).transform_point3(c).to_array();
                    match &mut result {
                        Some(b) => b.include(p),
                        None => result = Some(Bounds::at(p)),
                    }
                }
            }
        }
        result.unwrap_or_default()
    }
    pub fn scans(&self) -> impl Iterator<Item = &Scan> {
        self.manifest
            .scans
            .iter()
            .filter(|s| self.current().scans.contains(&s.id))
    }
}
