use crate::BlockCodec;
use anyhow::{Result, ensure};
use std::io::Read;

pub(crate) const MAX_BLOCK_BYTES: usize = 32 * 1024 * 1024;

/// Byte-column transposition preserves every bit, including IEEE floating-point
/// representations and the original order used by PointRef/point labels.
pub(crate) fn pack(data: &[u8], stride: usize) -> Result<(BlockCodec, Vec<u8>)> {
    ensure!(
        stride > 0 && data.len().is_multiple_of(stride),
        "Invalid block layout"
    );
    ensure!(data.len() <= MAX_BLOCK_BYTES, "Block exceeds budget");
    let count = data.len() / stride;
    let mut shuffled = vec![0; data.len()];
    for column in 0..stride {
        for row in 0..count {
            shuffled[column * count + row] = data[row * stride + column];
        }
    }
    let mut compressor = zstd::bulk::Compressor::new(1)?;
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
            let count = expected / stride;
            let mut data = vec![0; expected];
            for column in 0..stride {
                for row in 0..count {
                    data[row * stride + column] = shuffled[column * count + row];
                }
            }
            Ok(data)
        }
    }
}

/// One Zstd frame of point labels; long runs of one layer shrink to almost nothing.
pub(crate) fn pack_labels(labels: &[u8]) -> Result<Vec<u8>> {
    ensure!(labels.len() <= MAX_BLOCK_BYTES, "Block exceeds budget");
    let mut compressor = zstd::bulk::Compressor::new(3)?;
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
}
