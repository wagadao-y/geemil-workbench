use crate::CoreError;
use anyhow::{Context, Result, ensure};
use glam::{DMat4, DQuat, DVec3};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufWriter, Write},
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
    pub(crate) fn validate(&self) -> Result<()> {
        let norm = DQuat::from_array(self.rotation_xyzw).length_squared();
        ensure!(
            self.translation
                .iter()
                .chain(&self.rotation_xyzw)
                .all(|v| v.is_finite())
                && norm.is_finite()
                && norm > 0.,
            "Invalid pose"
        );
        Ok(())
    }

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
    /// Where the coordinates are in the point records.
    pub coordinates: crate::Coordinates,
    pub records: u64,
    pub valid_points: u64,
    pub omitted_attributes: Vec<String>,
    /// LAS-only source attributes appended after the E57 numeric record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub las: Option<LasMetadata>,
    pub points_file: String,
    /// The display octree's points.
    pub view_file: String,
    /// Original points, the unit of processing: leaves of the octree that
    /// split the scan at import, never changed.
    pub chunks: Vec<Chunk>,
    /// The display octree.
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LasVlr {
    pub user_id: String,
    pub record_id: u16,
    pub description: String,
    pub data: Vec<u8>,
}
impl LasVlr {
    /// File offsets/chunk layout must be regenerated after editing. Ordinary
    /// LAS/LAZ output is not COPC, so it cannot reuse COPC's hierarchy pointers.
    pub fn is_output_metadata(&self) -> bool {
        self.user_id != "copc" && self.user_id != "laszip encoded"
    }
}
impl From<&las::Vlr> for LasVlr {
    fn from(v: &las::Vlr) -> Self {
        Self {
            user_id: v.user_id.clone(),
            record_id: v.record_id,
            description: v.description.clone(),
            data: v.data.clone(),
        }
    }
}
impl From<&LasVlr> for las::Vlr {
    fn from(v: &LasVlr) -> Self {
        Self {
            user_id: v.user_id.clone(),
            record_id: v.record_id,
            description: v.description.clone(),
            data: v.data.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LasMetadata {
    pub point_format: u8,
    pub extra_bytes: u16,
    pub record_offset: usize,
    pub version: (u8, u8),
    pub gps_standard: bool,
    pub has_wkt_crs: bool,
    pub file_source_id: u16,
    pub system_identifier: String,
    pub synthetic_returns: bool,
    pub vlrs: Vec<LasVlr>,
    pub evlrs: Vec<LasVlr>,
}
impl LasMetadata {
    pub fn format(&self) -> Result<las::point::Format> {
        ensure!(self.point_format <= 10, "Invalid LAS point format");
        let mut format = las::point::Format::new(self.point_format)?;
        ensure!(
            format.len().checked_add(self.extra_bytes).is_some(),
            "LAS record length exceeds u16"
        );
        format.is_compressed = false;
        format.extra_bytes = self.extra_bytes;
        Ok(format)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageInfo {
    pub guid: Option<String>,
    pub name: Option<String>,
    pub scan_id: Option<Uuid>,
    pub pose: Option<Pose>,
    pub projection: String,
}
/// The file format of a panorama.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PanoramaFormat {
    Jpeg,
    Png,
}
/// An equirectangular photo placed in the project, taken apart from any
/// scan. Its own transform, kept in `Revision::transforms` under its id like
/// a scan's, places it: in the panorama's frame the image centre looks along
/// +X with +Z up, and azimuth grows to the left, as E57 spherical images
/// have it. Never changes once written.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Panorama {
    pub id: Uuid,
    /// For E57 export.
    pub guid: String,
    pub name: String,
    pub source_name: String,
    /// The imported file as it was, project-relative.
    pub file: String,
    pub format: PanoramaFormat,
    pub width: u32,
    pub height: u32,
}
/// A correspondence between a panorama and the points: a pixel of the
/// photo and the scan point seen there.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct PanoramaPair {
    /// Image coordinates with (0, 0) the top left corner of the top left pixel.
    pub pixel: [f64; 2],
    pub scan: Uuid,
    /// In scan coordinates, which stay valid however the scan moves.
    pub local: [f64; 3],
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
    /// Bounds of each layer's valid points in scan coordinates, by ascending
    /// code and the default layer included, so a scan's visible bounds need
    /// no points. None in patches written before they were recorded, whose
    /// chunks are read instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Vec<(u8, Bounds)>>,
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
/// An oriented box in the project frame.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct CropBox {
    pub center: [f64; 3],
    /// Full edge lengths along the box's own axes, in metres.
    pub size: [f64; 3],
    /// Unit quaternion (x, y, z, w) orienting the box's axes in the project.
    pub rotation: [f64; 4],
}
impl CropBox {
    /// Maps the project frame onto box coordinates in which the box is
    /// `[-1, 1]` on every axis.
    pub fn unit_matrix(&self) -> DMat4 {
        DMat4::from_scale(DVec3::from(self.size).map(|s| 2. / s))
            * DMat4::from_quat(DQuat::from_array(self.rotation).conjugate())
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
    /// The folder of each scan and panorama in `groups`; absent ones are at
    /// the top level.
    pub scan_groups: BTreeMap<Uuid, Uuid>,
    /// Names given to scans and panoramas, by id; others keep their imported
    /// name. Their metadata never changes once written, so renames live here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scan_names: BTreeMap<Uuid, String>,
    /// Unix seconds when the user saved this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<u64>,
    /// How scans and folders were last aligned, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub registrations: BTreeMap<Uuid, Registration>,
    /// Panoramas in this state, in import order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panoramas: Vec<Uuid>,
    /// The correspondences each panorama was last placed with.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub panorama_pairs: BTreeMap<Uuid, Vec<PanoramaPair>>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMethod {
    /// A fit to picked point pairs.
    Pairs,
    Icp,
    /// All scans adjusted together.
    Global,
    /// A panorama placed by pixels and the points seen there.
    Panorama,
}
/// How well an alignment fits its reference.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct AlignmentFit {
    pub method: RegistrationMethod,
    /// RMS in metres: of the picked pairs, else to the reference surface;
    /// for a panorama the RMS angle in degrees.
    pub rms: f64,
    /// Fraction of the item's samples near the reference; not for pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap: Option<f64>,
    /// The narrowest correspondence distance reached, in metres; not for pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    /// Whether it stopped narrowing short of the end distance.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
    /// How many scans it was aligned to.
    pub references: usize,
}
/// The last alignment of a scan or folder.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Registration {
    #[serde(flatten)]
    pub fit: AlignmentFit,
    /// Where the alignment put the item in the project frame: its folders'
    /// transforms and its own. Another place now means it moved since.
    pub placed: Pose,
    /// Unix seconds.
    pub at: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub name: String,
    pub scans: Vec<Scan>,
    pub images: Vec<ImageInfo>,
    pub panoramas: Vec<Panorama>,
    pub patches: Vec<LabelPatch>,
    pub revisions: Vec<Revision>,
    /// The saved revision the project shows, or the working state is based on.
    pub current: Uuid,
    /// Unsaved working state on top of `current`. Edits change only this; saving
    /// turns it into a revision. Kept on disk so a crash loses nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Revision>,
}
/// `project.json`: scans, label patches and saved revisions, which never
/// change once written, are referenced by the project-relative paths of their
/// files, so an edit rewrites only the working state and a list of names.
#[derive(Serialize, Deserialize)]
struct ManifestFile {
    format_version: u32,
    name: String,
    scans: Vec<String>,
    images: Vec<ImageInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    panoramas: Vec<Panorama>,
    labels: Vec<String>,
    revisions: Vec<StoredRevision>,
    current: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    draft: Option<Revision>,
}
/// A saved revision in `project.json`: what may change after saving, its name
/// and (when a revision before it is deleted) its parent, and the file holding
/// the rest. The values here win over those in the file.
#[derive(Serialize, Deserialize)]
struct RevisionRef {
    id: Uuid,
    parent: Option<Uuid>,
    name: String,
    file: String,
}
/// Projects saved before revisions had files of their own list them whole;
/// they move to files on the next save.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum StoredRevision {
    File(RevisionRef),
    Inline(Box<Revision>),
}
/// Where a saved revision lives.
pub(crate) fn revision_path(id: Uuid) -> String {
    format!("history/{id}.json")
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
    pub filter_options: crate::FilterOptions,
    // Kept alive by UI, worker and undo-state clones, including during reload.
    pub(crate) _lock: std::sync::Arc<fs::File>,
    /// Where each chunk's labels are in the current state, shared by clones.
    pub(crate) label_index: crate::layers::LabelIndexCache,
    /// Bounds of the scans' visible points in the current state, shared by clones.
    pub(crate) visible_bounds: crate::layers::VisibleBoundsCache,
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
            _lock: crate::project_lock::acquire(&fs::canonicalize(root)?)?,
            label_index: Default::default(),
            visible_bounds: Default::default(),
            filter_options: crate::FilterOptions::default(),
            manifest: Manifest {
                format_version: FORMAT_VERSION,
                name: name.into(),
                scans: vec![],
                images: vec![],
                panoramas: vec![],
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
                    registrations: BTreeMap::new(),
                    scan_names: BTreeMap::new(),
                    panoramas: vec![],
                    panorama_pairs: BTreeMap::new(),
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
        let lock = crate::project_lock::acquire(&root)?;
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
        let mut revisions = vec![];
        for stored in file.revisions {
            let revision = match stored {
                StoredRevision::Inline(revision) => *revision,
                StoredRevision::File(r) => {
                    ensure!(r.file == revision_path(r.id), "Misplaced revision");
                    let saved: Revision = serde_json::from_slice(&read(&r.file)?)
                        .with_context(|| format!("Invalid revision {}", r.file))?;
                    ensure!(saved.id == r.id, "Misplaced revision");
                    Revision {
                        parent: r.parent,
                        name: r.name,
                        ..saved
                    }
                }
            };
            revisions.push(revision);
        }
        let manifest = Manifest {
            format_version: version,
            name: file.name,
            scans,
            images: file.images,
            panoramas: file.panoramas,
            patches,
            revisions,
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
        let p = Self {
            root,
            manifest,
            _lock: lock,
            label_index: Default::default(),
            visible_bounds: Default::default(),
            filter_options: crate::FilterOptions::default(),
        };
        for s in &p.manifest.scans {
            ensure!(
                (crate::coords::HEAD..=1_048_576).contains(&s.stride),
                "Invalid point stride"
            );
            s.coordinates.validate_layout(s.stride)?;
            if let Some(pose) = s.original_pose {
                pose.validate()?;
            }
            // Display estimates descend in reverse index order. Every child
            // must follow its one parent, so cycles and shared nodes are invalid.
            let mut parents = vec![false; s.nodes.len()];
            for (index, node) in s.nodes.iter().enumerate() {
                for &child in &node.children {
                    let child = child as usize;
                    ensure!(
                        child > index && child < s.nodes.len(),
                        "Invalid display tree child"
                    );
                    ensure!(!parents[child], "Display node has multiple parents");
                    parents[child] = true;
                }
                ensure!(
                    node.chunks
                        .iter()
                        .all(|&(chunk, _)| (chunk as usize) < s.chunks.len()),
                    "Display node refers to a missing chunk"
                );
            }
            ensure!(
                parents.iter().skip(1).all(|parent| *parent),
                "Disconnected display tree"
            );
            if let Some(las) = &s.las {
                let format = las.format()?;
                ensure!(
                    !format.has_waveform
                        && las.record_offset >= crate::coords::HEAD
                        && las.record_offset.checked_add(format.len() as usize) == Some(s.stride),
                    "Invalid LAS record layout"
                );
            }
            p.path(&s.template)?;
            p.path(&s.points_file)?;
            p.path(&s.view_file)?;
        }
        for patch in &p.manifest.patches {
            p.path(&patch.file)?;
        }
        for panorama in &p.manifest.panoramas {
            p.path(&panorama.file)?;
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
    /// Writes `project.json`, after the metadata of any scan, label patch or
    /// saved revision that has no file yet. Those never change once written,
    /// so an edit writes only the working state and the list of revisions.
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
        // Saved revisions only change name and parent, which stay below.
        fs::create_dir_all(self.root.join("history"))?;
        let revisions = m
            .revisions
            .iter()
            .map(|r| {
                let file = revision_path(r.id);
                self.write_once(&file, r)?;
                Ok(StoredRevision::File(RevisionRef {
                    id: r.id,
                    parent: r.parent,
                    name: r.name.clone(),
                    file,
                }))
            })
            .collect::<Result<_>>()?;
        let file = ManifestFile {
            format_version: FORMAT_VERSION,
            name: m.name.clone(),
            scans,
            images: m.images.clone(),
            panoramas: m.panoramas.clone(),
            labels,
            revisions,
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
        write: impl FnOnce(&mut BufWriter<fs::File>) -> Result<()>,
    ) -> Result<()> {
        let tmp = self.root.join(format!("project-{}.tmp", Uuid::new_v4()));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        // serde_json writes token by token; unbuffered, a history of a few
        // MB took over a second in small system calls.
        let mut f = BufWriter::with_capacity(1 << 20, file);
        let written = write(&mut f).and_then(|_| {
            let file = f.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            Ok(())
        });
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(error) = fs::rename(&tmp, self.root.join(path)) {
            let _ = fs::remove_file(&tmp);
            return Err(error.into());
        }
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
    /// Where the scanner stood, in the scan's own coordinates, when its pose
    /// says so. E57 places each scan's scanner-centred coordinates with a
    /// pose, but some exporters give every scan of a registered file one
    /// shared pose, or a pose far from all points; then the origin is no
    /// scanner position. LAS/LAZ keep none.
    pub fn scanner_position(&self, scan: &Scan) -> Option<DVec3> {
        let pose = scan.original_pose?;
        let shared = self
            .scans()
            .any(|other| other.id != scan.id && other.original_pose == Some(pose));
        let bounds = scan.nodes.first()?.bounds;
        let (min, max) = (DVec3::from(bounds.min), DVec3::from(bounds.max));
        // A scan cropped afterwards may leave its scanner outside, but near.
        let far = DVec3::ZERO.clamp(min, max).length() > (max - min).length();
        (!shared && !far).then_some(DVec3::ZERO)
    }
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
    /// The scans of the current state, in import order. Linear in the number
    /// of scans, so projects with hundreds of scans list them cheaply.
    pub fn scans(&self) -> impl Iterator<Item = &Scan> {
        let current: HashSet<Uuid> = self.current().scans.iter().copied().collect();
        self.manifest
            .scans
            .iter()
            .filter(move |s| current.contains(&s.id))
    }
    /// A scan of the current state.
    pub fn scan(&self, id: Uuid) -> Option<&Scan> {
        if !self.current().scans.contains(&id) {
            return None;
        }
        self.manifest.scans.iter().find(|s| s.id == id)
    }
    /// Positions in `manifest.scans` of the current state's scans, for
    /// looking many of them up by id.
    pub(crate) fn scan_index(&self) -> HashMap<Uuid, usize> {
        let current: HashSet<Uuid> = self.current().scans.iter().copied().collect();
        self.manifest
            .scans
            .iter()
            .enumerate()
            .filter(|(_, s)| current.contains(&s.id))
            .map(|(i, s)| (s.id, i))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_atomic_write_preserves_the_previous_file_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let p = Project::create(&dir.path().join("project"), "Safety").unwrap();
        let before = fs::read(p.root.join("project.json")).unwrap();
        let error = p
            .write_atomic(Path::new("project.json"), |file| {
                file.write_all(b"incomplete replacement")?;
                file.flush()?;
                anyhow::bail!("Simulated write failure")
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "Simulated write failure");
        assert_eq!(fs::read(p.root.join("project.json")).unwrap(), before);
        assert_eq!(Project::load(&p.root).unwrap().current().id, p.current().id);
        assert!(!fs::read_dir(&p.root).unwrap().any(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "tmp")
        }));
    }
}
