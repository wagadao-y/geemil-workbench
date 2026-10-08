//! Point records and where their coordinates come from.
//!
//! A record starts with an 8-byte head (display colour and validity), then
//! the source values. Scans with Cartesian source values keep no computed
//! coordinates: every reader derives them from those values with
//! [`Coordinates::position`], so the result is the same bit for bit each
//! time. Scans with only spherical source values keep f64 coordinates after
//! the source values instead, so reading needs no trigonometry. The display
//! octree keeps its coordinates in the same representation.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Bytes before a record's source values.
pub const HEAD: usize = 8;

/// Where a scan's coordinates are in its point records.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Coordinates {
    /// Derived from the Cartesian source values X, Y and Z.
    Cartesian { fields: [CoordinateField; 3] },
    /// f64 X, Y and Z at `offset`, for scans with only spherical source
    /// values.
    Stored { offset: usize },
}
/// A Cartesian source value in a record.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct CoordinateField {
    /// Bytes from the start of the record.
    pub offset: usize,
    pub encoding: FieldEncoding,
}
/// How a source value is stored, as E57 numbers are in point records.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FieldEncoding {
    F32,
    F64,
    /// An i64 that is `raw * scale + offset`, like E57 scaled integers and
    /// LAS coordinates; plain integers have scale 1 and offset 0.
    Integer {
        scale: f64,
        offset: f64,
    },
}
impl FieldEncoding {
    fn bytes(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F64 | Self::Integer { .. } => 8,
        }
    }
    fn decode(self, b: &[u8]) -> f64 {
        match self {
            Self::F32 => f32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
            Self::F64 => f64::from_le_bytes(b[..8].try_into().unwrap()),
            // As e57 and las compute it.
            Self::Integer { scale, offset } => {
                i64::from_le_bytes(b[..8].try_into().unwrap()) as f64 * scale + offset
            }
        }
    }
    /// The bytes `decode` turns into `x`; an error when none do.
    fn encode(self, x: f64, out: &mut Vec<u8>) -> Result<()> {
        let start = out.len();
        match self {
            Self::F32 => out.extend((x as f32).to_le_bytes()),
            Self::F64 => out.extend(x.to_le_bytes()),
            Self::Integer { scale, offset } => {
                out.extend((((x - offset) / scale).round() as i64).to_le_bytes())
            }
        }
        ensure!(
            self.decode(&out[start..]).to_bits() == x.to_bits(),
            "Coordinate {x} has no exact {self:?} form"
        );
        Ok(())
    }
}

impl Coordinates {
    /// Validate untrusted metadata before any record slicing or size arithmetic.
    pub(crate) fn validate_layout(&self, stride: usize) -> Result<()> {
        let fits = |offset: usize, bytes: usize| {
            offset >= HEAD && offset.checked_add(bytes).is_some_and(|end| end <= stride)
        };
        match self {
            Self::Cartesian { fields } => {
                for field in fields {
                    ensure!(
                        fits(field.offset, field.encoding.bytes()),
                        "Invalid coordinate layout"
                    );
                    if let FieldEncoding::Integer { scale, offset } = field.encoding {
                        ensure!(
                            scale.is_finite() && offset.is_finite(),
                            "Invalid coordinate encoding"
                        );
                    }
                }
            }
            Self::Stored { offset } => {
                ensure!(fits(*offset, 24), "Invalid coordinate layout");
            }
        }
        Ok(())
    }

    /// The position of a record in scan coordinates; the origin for an
    /// invalid point, which takes no part beyond where import places it.
    pub fn position(&self, record: &[u8]) -> [f64; 3] {
        if !valid(record) {
            return [0.; 3];
        }
        self.raw_position(record)
    }
    fn raw_position(&self, record: &[u8]) -> [f64; 3] {
        match self {
            Self::Cartesian { fields } => fields.map(|f| {
                f.encoding
                    .decode(&record[f.offset..f.offset + f.encoding.bytes()])
            }),
            Self::Stored { offset } => std::array::from_fn(|i| {
                f64::from_le_bytes(
                    record[offset + i * 8..offset + i * 8 + 8]
                        .try_into()
                        .unwrap(),
                )
            }),
        }
    }
    /// How many leading bytes of a record hold its validity and coordinates.
    pub(crate) fn leading_bytes(&self) -> usize {
        match self {
            Self::Cartesian { fields } => fields
                .iter()
                .map(|f| f.offset + f.encoding.bytes())
                .max()
                .unwrap_or(HEAD)
                .max(HEAD),
            Self::Stored { offset } => offset + 24,
        }
    }
    /// The bytes of a display octree point's coordinates.
    pub(crate) fn view_bytes(&self) -> usize {
        match self {
            Self::Cartesian { fields } => fields.iter().map(|f| f.encoding.bytes()).sum(),
            Self::Stored { .. } => 24,
        }
    }
    /// Appends a display point's coordinates in the scan's representation.
    pub(crate) fn encode_view(&self, p: [f64; 3], out: &mut Vec<u8>) -> Result<()> {
        match self {
            Self::Cartesian { fields } => {
                for (f, x) in fields.iter().zip(p) {
                    f.encoding.encode(x, out)?;
                }
            }
            Self::Stored { .. } => {
                for x in p {
                    out.extend(x.to_le_bytes());
                }
            }
        }
        Ok(())
    }
    /// A display point's coordinates from what `encode_view` wrote.
    pub(crate) fn decode_view(&self, b: &[u8]) -> [f64; 3] {
        match self {
            Self::Cartesian { fields } => {
                let mut at = 0;
                fields.map(|f| {
                    let x = f.encoding.decode(&b[at..]);
                    at += f.encoding.bytes();
                    x
                })
            }
            Self::Stored { .. } => {
                std::array::from_fn(|i| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap()))
            }
        }
    }
}

pub(crate) fn point_color(record: &[u8]) -> [u8; 4] {
    record[..4].try_into().unwrap()
}
pub(crate) fn valid(record: &[u8]) -> bool {
    record[4] != 0
}
/// Appends a record's head: its display colour and validity.
pub(crate) fn push_head(out: &mut Vec<u8>, color: [u8; 4], is_valid: bool) {
    out.extend(color);
    out.extend([is_valid as u8, 0, 0, 0]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_come_back_exactly_in_each_encoding_and_refuse_other_values() {
        let scaled = FieldEncoding::Integer {
            scale: 0.001,
            offset: 4_000_000.,
        };
        for (encoding, x) in [
            (FieldEncoding::F32, 0.1f32 as f64),
            (FieldEncoding::F64, 0.1),
            (scaled, 123_456_789. * 0.001 + 4_000_000.),
            (scaled, -2_147_483_648. * 0.001 + 4_000_000.),
        ] {
            let mut bytes = vec![];
            encoding.encode(x, &mut bytes).unwrap();
            assert_eq!(bytes.len(), encoding.bytes());
            assert_eq!(encoding.decode(&bytes).to_bits(), x.to_bits());
        }
        // Values no point of the scan can have.
        assert!(FieldEncoding::F32.encode(0.1, &mut vec![]).is_err());
        assert!(scaled.encode(4_000_000.000_5, &mut vec![]).is_err());
    }

    #[test]
    fn records_give_their_coordinates_and_invalid_ones_the_origin() {
        let fields = [
            CoordinateField {
                offset: HEAD,
                encoding: FieldEncoding::F32,
            },
            CoordinateField {
                offset: HEAD + 4,
                encoding: FieldEncoding::Integer {
                    scale: 0.5,
                    offset: 10.,
                },
            },
            CoordinateField {
                offset: HEAD + 12,
                encoding: FieldEncoding::F64,
            },
        ];
        let coords = Coordinates::Cartesian { fields };
        let mut record = vec![];
        push_head(&mut record, [1, 2, 3, 255], true);
        record.extend(1.5f32.to_le_bytes());
        record.extend(7i64.to_le_bytes());
        record.extend((-2.25f64).to_le_bytes());
        assert_eq!(coords.position(&record), [1.5, 13.5, -2.25]);
        assert_eq!(coords.leading_bytes(), record.len());
        assert_eq!(point_color(&record), [1, 2, 3, 255]);
        let mut view = vec![];
        coords.encode_view([1.5, 13.5, -2.25], &mut view).unwrap();
        assert_eq!(view.len(), coords.view_bytes());
        assert_eq!(coords.decode_view(&view), [1.5, 13.5, -2.25]);
        record[4] = 0;
        assert_eq!(coords.position(&record), [0.; 3]);
    }
}
