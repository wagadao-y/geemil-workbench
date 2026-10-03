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

/// 3 adds the unsaved working state (`Manifest::draft`) and the scan tree.
/// 4 moves scan and layer metadata, which never change once written, out of
/// `project.json` into files of their own; saving an edit then writes only
/// the small history. Versions up to 3 still load and are saved as 4.
pub const FORMAT_VERSION: u32 = 4;

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
    #[serde(default)]
    pub codec: BlockCodec,
    #[serde(default)]
    pub stored_bytes: u32,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockCodec {
    #[default]
    Raw,
    ZstdShuffle,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub bounds: Bounds,
    pub children: Vec<u32>,
    pub chunk: Option<u32>,
    pub lod_offset: u64,
    pub lod_count: u32,
    pub point_count: u64,
    #[serde(default)]
    pub lod_codec: BlockCodec,
    #[serde(default)]
    pub lod_bytes: u32,
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
    pub lod_file: String,
    pub chunks: Vec<Chunk>,
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkMask {
    pub scan: Uuid,
    pub chunk: u32,
    pub offset: u64,
    pub bytes: u32,
    pub excluded: u64,
}
/// What produced an exclusion layer.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayerKind {
    /// A range exclusion drawn in the viewport.
    #[default]
    Manual,
    /// Voxel subsampling: one original point kept per voxel of `size` metres,
    /// per scan in scan coordinates, or with `merged` over all the scans
    /// together in the project frame (where scans overlap, one point in all).
    Subsample {
        size: f64,
        #[serde(default)]
        merged: bool,
    },
    /// Isolated point removal: points with fewer than `min_neighbours` other
    /// points within `radius` metres.
    Noise { radius: f64, min_neighbours: u32 },
    /// A 3D box crop: the points inside the box, or outside it.
    Box { inside: bool },
    /// Statistical outlier removal: points whose mean distance to their
    /// `neighbours` nearest points exceeds the scan's mean by `deviations`
    /// standard deviations.
    Statistical { neighbours: u32, deviations: f64 },
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layer {
    pub id: Uuid,
    pub name: String,
    pub mask_file: String,
    pub masks: Vec<ChunkMask>,
    pub excluded: u64,
    #[serde(default)]
    pub kind: LayerKind,
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
    pub layers: Vec<Uuid>,
    /// Additional rigid transforms of scans and groups.
    pub transforms: BTreeMap<Uuid, Pose>,
    #[serde(default)]
    pub groups: Vec<Group>,
    /// The folder of each scan in `groups`; absent scans are at the top level.
    #[serde(default)]
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
    pub layers: Vec<Layer>,
    pub revisions: Vec<Revision>,
    /// The saved revision the project shows, or the working state is based on.
    pub current: Uuid,
    /// Unsaved working state on top of `current`. Edits change only this; saving
    /// turns it into a revision. Kept on disk so a crash loses nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Revision>,
}
/// `project.json` from format 4: scans and layers are referenced by the
/// project-relative paths of their metadata files.
#[derive(Serialize, Deserialize)]
struct ManifestFile {
    format_version: u32,
    name: String,
    scans: Vec<String>,
    images: Vec<ImageInfo>,
    layers: Vec<String>,
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
/// Where a layer's metadata lives: next to its mask.
pub(crate) fn layer_metadata_path(layer: &Layer) -> String {
    let stem = layer
        .mask_file
        .strip_suffix(".mask")
        .unwrap_or(&layer.mask_file);
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
        for dir in ["data", "layers", "staging"] {
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
                layers: vec![],
                revisions: vec![Revision {
                    id,
                    parent: None,
                    name: "Project created".into(),
                    operation: serde_json::json!({"kind":"create"}),
                    scans: vec![],
                    layers: vec![],
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
            (1..=FORMAT_VERSION).contains(&version),
            CoreError::UnsupportedProjectFormat(version)
        );
        let manifest = if version < 4 {
            serde_json::from_value(value).context("Invalid project metadata")?
        } else {
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
            let mut layers = vec![];
            for path in &file.layers {
                let layer: Layer = serde_json::from_slice(&read(path)?)
                    .with_context(|| format!("Invalid layer metadata {path}"))?;
                ensure!(
                    layer_metadata_path(&layer) == *path,
                    "Misplaced layer metadata"
                );
                layers.push(layer);
            }
            Manifest {
                format_version: version,
                name: file.name,
                scans,
                images: file.images,
                layers,
                revisions: file.revisions,
                current: file.current,
                draft: file.draft,
            }
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
            p.path(&s.lod_file)?;
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
    /// Writes `project.json`, after the metadata of any scan or layer that has
    /// no file yet. Those never change once written, so an edit writes only
    /// the history.
    pub fn save(&self) -> Result<()> {
        let m = &self.manifest;
        let scans: Vec<_> = m.scans.iter().map(scan_metadata_path).collect();
        let layers: Vec<_> = m.layers.iter().map(layer_metadata_path).collect();
        for (scan, path) in m.scans.iter().zip(&scans) {
            self.write_once(path, scan)?;
        }
        for (layer, path) in m.layers.iter().zip(&layers) {
            self.write_once(path, layer)?;
        }
        let file = ManifestFile {
            format_version: FORMAT_VERSION,
            name: m.name.clone(),
            scans,
            images: m.images.clone(),
            layers,
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
