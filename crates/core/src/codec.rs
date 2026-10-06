use crate::BlockCodec;
use anyhow::{Result, ensure};
use std::io::Read;

pub(crate) const MAX_BLOCK_BYTES: usize = 32 * 1024 * 1024;
/// Zstd level of every block. Level 1 looks too short a way back to find a
/// transposed block's repeated columns, such as the coordinates a LAS scan
/// keeps both as computed and as source values, 2 MB apart in a full chunk;
/// level 3 finds them at about the same speed and decodes faster.
const ZSTD_LEVEL: i32 = 3;

/// Byte-column transposition preserves every bit, including IEEE floating-point
/// representations and the original order used by PointRef/point labels.
pub(crate) fn pack(data: &[u8], stride: usize) -> Result<(BlockCodec, Vec<u8>)> {
    ensure!(
        stride > 0 && data.len().is_multiple_of(stride),
        "Invalid block layout"
    );
    ensure!(data.len() <= MAX_BLOCK_BYTES, "Block exceeds budget");
    let shuffled = transpose(data, data.len() / stride, stride);
    let mut compressor = zstd::bulk::Compressor::new(ZSTD_LEVEL)?;
    compressor.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
    let compressed = compressor.compress(&shuffled)?;
    if compressed.len() >= data.len() {
        Ok((BlockCodec::Raw, data.to_vec()))
    } else {
        Ok((BlockCodec::ZstdShuffle, compressed))
    }
}

pub(crate) fn read_block(
    reader: &mut impl Read,
    codec: BlockCodec,
    stored: usize,
    expected: usize,
    stride: usize,
) -> Result<Vec<u8>> {
    ensure!(
        stride > 0 && expected.is_multiple_of(stride) && expected <= MAX_BLOCK_BYTES,
        "Invalid block layout/size"
    );
    match codec {
        BlockCodec::Raw => {
            ensure!(stored == expected, "Invalid raw block size");
            let mut bytes = vec![0; expected];
            reader.read_exact(&mut bytes)?;
            Ok(bytes)
        }
        BlockCodec::ZstdShuffle => {
            ensure!(
                stored > 0 && stored <= zstd::zstd_safe::compress_bound(expected),
                "Invalid compressed block size"
            );
            let mut bytes = vec![0; stored];
            reader.read_exact(&mut bytes)?;
            let shuffled = zstd::bulk::decompress(&bytes, expected)?;
            ensure!(shuffled.len() == expected, "Decoded block size differs");
            Ok(transpose(&shuffled, stride, expected / stride))
        }
    }
}

/// The first `columns` byte columns of a block's records, column after
/// column: for each, one byte per record. Decodes no further than they reach,
/// so the frame's checksum, at its end, goes unchecked.
pub(crate) fn read_block_columns(
    reader: &mut impl Read,
    codec: BlockCodec,
    stored: usize,
    expected: usize,
    stride: usize,
    columns: usize,
) -> Result<Vec<u8>> {
    ensure!(
        stride > 0
            && columns <= stride
            && expected.is_multiple_of(stride)
            && expected <= MAX_BLOCK_BYTES,
        "Invalid block layout/size"
    );
    let rows = expected / stride;
    match codec {
        BlockCodec::Raw => {
            let records = read_block(reader, codec, stored, expected, stride)?;
            let mut result = transpose(&records, rows, stride);
            result.truncate(rows * columns);
            Ok(result)
        }
        BlockCodec::ZstdShuffle => {
            ensure!(
                stored > 0 && stored <= zstd::zstd_safe::compress_bound(expected),
                "Invalid compressed block size"
            );
            let mut bytes = vec![0; stored];
            reader.read_exact(&mut bytes)?;
            let mut result = vec![0; rows * columns];
            zstd::stream::read::Decoder::with_buffer(bytes.as_slice())?.read_exact(&mut result)?;
            Ok(result)
        }
    }
}

/// `data`, `rows` rows of `columns` bytes, written column by column.
fn transpose(data: &[u8], rows: usize, columns: usize) -> Vec<u8> {
    // Square tiles keep the rows read and the rows written in cache; going
    // down whole columns touched a new cache line per byte and took most of
    // a chunk's decode time.
    const TILE: usize = 64;
    let mut result = vec![0; data.len()];
    for row in (0..rows).step_by(TILE) {
        let row_end = (row + TILE).min(rows);
        for column in (0..columns).step_by(TILE) {
            let column_end = (column + TILE).min(columns);
            for c in column..column_end {
                let target = &mut result[c * rows + row..c * rows + row_end];
                for (r, byte) in (row..row_end).zip(target) {
                    *byte = data[r * columns + c];
                }
            }
        }
    }
    result
}

/// One Zstd frame of point labels; long runs of one layer shrink to almost nothing.
pub(crate) fn pack_labels(labels: &[u8]) -> Result<Vec<u8>> {
    ensure!(labels.len() <= MAX_BLOCK_BYTES, "Block exceeds budget");
    let mut compressor = zstd::bulk::Compressor::new(ZSTD_LEVEL)?;
    compressor.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
    Ok(compressor.compress(labels)?)
}

pub(crate) fn unpack_labels(bytes: &[u8], count: usize) -> Result<Vec<u8>> {
    ensure!(count <= MAX_BLOCK_BYTES, "Block exceeds budget");
    let labels = zstd::bulk::decompress(bytes, count)?;
    ensure!(labels.len() == count, "Decoded label count differs");
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bits_and_point_order_survive_compression() {
        let bytes: Vec<_> = (0..4096u64)
            .flat_map(|i| {
                [
                    i,
                    f64::NAN.to_bits(),
                    f64::NEG_INFINITY.to_bits(),
                    (-0.0f64).to_bits(),
                ]
            })
            .flat_map(u64::to_le_bytes)
            .collect();
        let (codec, packed) = pack(&bytes, 32).unwrap();
        assert_eq!(codec, BlockCodec::ZstdShuffle);
        assert_eq!(
            read_block(&mut packed.as_slice(), codec, packed.len(), bytes.len(), 32).unwrap(),
            bytes
        );
        let mut damaged = packed.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        assert!(
            read_block(
                &mut damaged.as_slice(),
                codec,
                damaged.len(),
                bytes.len(),
                32
            )
            .is_err()
        );
        assert!(read_block(&mut packed.as_slice(), codec, packed.len(), 32, 32).is_err());
    }
    #[test]
    fn transposition_matches_the_column_definition_for_ragged_tiles() {
        for (rows, columns) in [(1, 1), (3, 114), (130, 7), (200, 65), (64, 64)] {
            let data: Vec<u8> = (0..rows * columns).map(|i| (i * 31 % 251) as u8).collect();
            let shuffled = transpose(&data, rows, columns);
            for r in 0..rows {
                for c in 0..columns {
                    assert_eq!(shuffled[c * rows + r], data[r * columns + c]);
                }
            }
            assert_eq!(transpose(&shuffled, columns, rows), data);
        }
    }
    #[test]
    fn leading_columns_decode_alone_from_both_codecs() {
        let (rows, stride, columns): (usize, usize, usize) = (3000, 40, 29);
        // Repeating records compress; noise does not and is stored raw.
        let repeating: Vec<u8> = (0..rows * stride).map(|i| (i % stride) as u8).collect();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let noise: Vec<u8> = (0..rows * stride)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 56) as u8
            })
            .collect();
        for (data, codec) in [
            (repeating, BlockCodec::ZstdShuffle),
            (noise, BlockCodec::Raw),
        ] {
            let (packed_codec, packed) = pack(&data, stride).unwrap();
            assert_eq!(packed_codec, codec);
            let leading = read_block_columns(
                &mut packed.as_slice(),
                codec,
                packed.len(),
                data.len(),
                stride,
                columns,
            )
            .unwrap();
            assert_eq!(leading, transpose(&data, rows, stride)[..rows * columns]);
        }
    }
}
