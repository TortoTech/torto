//! Optional, lossless source mapping. Reading IR never decodes this sidecar.
use super::{MAX_SECTION_BYTES, SourceSlice};
use bincode::Options;
use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};
use rebook_publication::{SourceAnchor, SourceRange, SpineItemId};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Read, Write},
    path::Path,
};

const MAGIC: &[u8; 8] = b"TRTPRV01";

#[derive(Serialize, Deserialize)]
struct Packed {
    nodes: Vec<(SpineItemId, String)>,
    slices: Vec<Slice>,
}

#[derive(Serialize, Deserialize)]
struct Slice {
    start_node: usize,
    start_offset: u64,
    end_node: usize,
    end_offset: u64,
    page: usize,
    rect: [f64; 4],
    glyph: usize,
}

fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_varint_encoding()
        .with_limit(MAX_SECTION_BYTES)
        .reject_trailing_bytes()
}

pub(super) fn name(index: usize) -> String {
    format!("resources/provenance-{index}.bin.z")
}

fn pack<'a>(slices: &'a [SourceSlice]) -> Packed {
    let mut nodes = Vec::new();
    let mut indices = HashMap::new();
    let mut node = |anchor: &'a SourceAnchor| {
        let key = (&anchor.spine, anchor.node.as_str());
        // Own just one dictionary entry per distinct node, not per character.
        if let Some(&index) = indices.get(&key) {
            return index;
        }
        let index = nodes.len();
        nodes.push((anchor.spine.clone(), anchor.node.clone()));
        indices.insert(key, index);
        index
    };
    let slices = slices
        .iter()
        .map(|s| Slice {
            start_node: node(&s.source.start),
            start_offset: s.source.start.text_offset,
            end_node: node(&s.source.end),
            end_offset: s.source.end.text_offset,
            page: s.page,
            rect: s.rect,
            glyph: s.original_glyph,
        })
        .collect();
    Packed { nodes, slices }
}

fn unpack(packed: Packed) -> Result<Vec<SourceSlice>, String> {
    let anchor = |node: usize, offset| {
        let (spine, node) = packed
            .nodes
            .get(node)
            .ok_or("Invalid PDF provenance node")?;
        Ok::<_, String>(SourceAnchor {
            spine: spine.clone(),
            node: node.clone(),
            text_offset: offset,
        })
    };
    packed
        .slices
        .iter()
        .map(|s| {
            Ok(SourceSlice {
                source: SourceRange {
                    start: anchor(s.start_node, s.start_offset)?,
                    end: anchor(s.end_node, s.end_offset)?,
                },
                page: s.page,
                rect: s.rect,
                original_glyph: s.glyph,
            })
        })
        .collect()
}

pub(super) fn write(path: &Path, slices: &[SourceSlice]) -> Result<(), String> {
    let packed = pack(slices);
    let raw = codec().serialize(&packed).map_err(|e| e.to_string())?;
    if raw.len() as u64 > MAX_SECTION_BYTES {
        return Err("PDF provenance exceeds decompressed size limit".into());
    }
    drop(packed);
    let mut writer = BufWriter::new(fs::File::create(path).map_err(|e| e.to_string())?);
    writer.write_all(MAGIC).map_err(|e| e.to_string())?;
    // Fast deflate keeps this background write from stalling raster producers.
    let mut encoder = ZlibEncoder::new(writer, Compression::fast());
    // Feed the bounded section buffer in one call. Streaming bincode directly
    // into zlib invokes compression for each small integer/float and is costly.
    encoder.write_all(&raw).map_err(|e| e.to_string())?;
    let mut writer = encoder.finish().map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    if writer
        .get_ref()
        .metadata()
        .map_err(|e| e.to_string())?
        .len()
        > MAX_SECTION_BYTES
    {
        return Err("PDF provenance exceeds size limit".into());
    }
    Ok(())
}

pub(super) fn read(path: &Path) -> Result<Vec<SourceSlice>, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_SECTION_BYTES {
        return Err("PDF provenance exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SECTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Result<Vec<SourceSlice>, String> {
    if bytes.len() as u64 > MAX_SECTION_BYTES || !bytes.starts_with(MAGIC) {
        return Err("Invalid PDF provenance header or size".into());
    }
    let compressed = &bytes[MAGIC.len()..];
    let mut decoder = Decompress::new(true);
    let mut raw = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(
                &compressed[before_in as usize..],
                &mut buffer,
                FlushDecompress::None,
            )
            .map_err(|e| e.to_string())?;
        let produced = (decoder.total_out() - before_out) as usize;
        if raw.len() as u64 + produced as u64 > MAX_SECTION_BYTES {
            return Err("PDF provenance exceeds decompressed size limit".into());
        }
        raw.extend_from_slice(&buffer[..produced]);
        if status == Status::StreamEnd {
            if decoder.total_in() != compressed.len() as u64 {
                return Err("Trailing PDF provenance bytes".into());
            }
            break;
        }
        if decoder.total_in() == before_in && produced == 0 {
            return Err("Truncated PDF provenance stream".into());
        }
    }
    let packed = codec().deserialize(&raw).map_err(|e| e.to_string())?;
    unpack(packed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<SourceSlice> {
        let anchor = |node: &str, offset| SourceAnchor {
            spine: SpineItemId::new("native").unwrap(),
            node: node.into(),
            text_offset: offset,
        };
        (0..1000)
            .map(|i| SourceSlice {
                source: SourceRange {
                    start: anchor("中文👩‍💻", i),
                    end: anchor("different end", i + 2),
                },
                page: 3,
                rect: [f64::from_bits(0x406d_71f3_b27a_819d), -0.0, 300.0, i as f64],
                original_glyph: i as usize,
            })
            .collect()
    }

    fn encode(packed: &Packed) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(MAGIC.to_vec(), Compression::fast());
        codec().serialize_into(&mut encoder, packed).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn dictionary_mapping_preserves_every_field_and_coordinate_bit() {
        let original = sample();
        let packed = pack(&original);
        assert_eq!(packed.nodes.len(), 2);
        let encoded = encode(&packed);
        assert!(encoded.len() < serde_json::to_vec(&original).unwrap().len() / 10);
        let restored = decode(&encoded).unwrap();
        assert_eq!(
            bincode::serialize(&original).unwrap(),
            bincode::serialize(&restored).unwrap()
        );
        assert_eq!(restored[0].rect[1].to_bits(), (-0.0f64).to_bits());
        assert!(decode(&encode(&pack(&[]))).unwrap().is_empty());
    }

    #[test]
    fn corrupt_truncated_trailing_and_invalid_node_mappings_are_rejected() {
        let original = encode(&pack(&sample()));
        for length in [
            0,
            7,
            original.len() / 2,
            original.len() - 1,
            original.len() - 4,
        ] {
            assert!(
                decode(&original[..length]).is_err(),
                "accepted length {length}"
            );
        }
        let mut corrupt = original.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode(&corrupt).is_err());
        let mut trailing = original;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        let mut packed = pack(&sample());
        packed.slices[0].start_node = 9999;
        assert!(decode(&encode(&packed)).is_err());
    }

    #[test]
    fn provenance_limits_cover_inflation_and_forged_dictionary_lengths() {
        let mut encoder = ZlibEncoder::new(MAGIC.to_vec(), Compression::fast());
        encoder
            .write_all(&vec![0; MAX_SECTION_BYTES as usize + 1])
            .unwrap();
        assert!(
            decode(&encoder.finish().unwrap())
                .unwrap_err()
                .contains("decompressed size")
        );
        let mut encoder = ZlibEncoder::new(MAGIC.to_vec(), Compression::fast());
        // A forged bincode dictionary length must fail before allocating it.
        encoder
            .write_all(&codec().serialize(&u64::MAX).unwrap())
            .unwrap();
        assert!(decode(&encoder.finish().unwrap()).is_err());
    }
}
