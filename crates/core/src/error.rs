use std::{fmt, path::PathBuf};

/// Long-running job phases. Display gives a stable English label for logs;
/// user interfaces translate the variant instead of the label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    ReadingE57,
    ReadingLas,
    Images,
    Partitioning,
    Indexing,
    BuildingParentLod,
    SelectionNearestDepth,
    SelectionMove,
    MovingLayer,
    Subsampling,
    NoiseFilter,
    BoxCrop,
    OutlierStatistics,
    OutlierFilter,
    WritingE57,
    WritingLas,
    IcpSampling,
    IcpIterations,
    ViewLod,
    ViewPoints,
}
impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ReadingE57 => "Reading E57",
            Self::ReadingLas => "Reading LAS/LAZ",
            Self::Images => "Images",
            Self::Partitioning => "Partitioning",
            Self::Indexing => "Indexing",
            Self::BuildingParentLod => "Building parent LOD",
            Self::SelectionNearestDepth => "Selection: nearest depth",
            Self::SelectionMove => "Selection: moving points",
            Self::MovingLayer => "Moving layer points",
            Self::Subsampling => "Voxel subsampling",
            Self::NoiseFilter => "Noise filter",
            Self::BoxCrop => "Box crop",
            Self::OutlierStatistics => "Outliers: statistics",
            Self::OutlierFilter => "Outliers: filtering",
            Self::WritingE57 => "Writing E57",
            Self::WritingLas => "Writing LAS/LAZ",
            Self::IcpSampling => "ICP: sampling",
            Self::IcpIterations => "ICP: iterating",
            Self::ViewLod => "View LOD",
            Self::ViewPoints => "View points",
        })
    }
}

/// Errors a user can act on. They are carried inside `anyhow::Error`; find them
/// with `CoreError::find`. Other failures are internal and only have English text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreError {
    Cancelled,
    UnsupportedFormat,
    OutputExists(PathBuf),
    ProjectExists(PathBuf),
    NotAProject(PathBuf),
    UnsupportedProjectFormat(u32),
    CoordinateSystemMismatch,
    /// ICP found no reference points within reach of the moved points.
    NoOverlap,
    /// The overlap does not fix the motion, e.g. it is a single plane.
    AlignmentUndetermined,
}
impl CoreError {
    pub fn find(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|e| e.downcast_ref())
    }
}
impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "Cancelled"),
            Self::UnsupportedFormat => write!(f, "Supported formats: E57, LAS, LAZ"),
            Self::OutputExists(p) => write!(f, "Output already exists: {}", p.display()),
            Self::ProjectExists(p) => {
                write!(f, "Project directory already exists: {}", p.display())
            }
            Self::NotAProject(p) => write!(f, "Not a project directory: {}", p.display()),
            Self::UnsupportedProjectFormat(v) => write!(f, "Unsupported project format {v}"),
            Self::CoordinateSystemMismatch => write!(f, "Scans have different coordinate systems"),
            Self::NoOverlap => write!(f, "No overlap with the reference within the distance"),
            Self::AlignmentUndetermined => {
                write!(f, "The overlap does not determine the alignment")
            }
        }
    }
}
impl std::error::Error for CoreError {}
