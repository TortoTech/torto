//! Font-container transforms shared by EPUB and Kindle resource loading.
//! These are format-defined embedding transforms; the source archive stays intact.
use sha1::{Digest, Sha1};
use std::io::Read;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) enum Decoder {
    Idpf([u8; 20]),
    Adobe([u8; 16]),
}
impl Decoder {
    pub(crate) fn idpf(identifier: &str) -> Self {
        let identifier: String = identifier
            .chars()
            .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
            .collect();
        Self::Idpf(Sha1::digest(identifier.as_bytes()).into())
    }
    pub(crate) fn adobe(identifier: &str) -> Option<Self> {
        let value = identifier.trim();
        let value = if value
            .get(..9)
            .is_some_and(|s| s.eq_ignore_ascii_case("urn:uuid:"))
        {
            &value[9..]
        } else {
            value
        };
        let value = value.trim_matches(['{', '}']);
        let digits = value.bytes().filter(|b| *b != b'-').collect::<Vec<_>>();
        if digits.len() != 32 {
            return None;
        }
        fn hex(b: u8) -> Option<u8> {
            match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            }
        }
        let mut key = [0; 16];
        for (i, pair) in digits.chunks_exact(2).enumerate() {
            key[i] = hex(pair[0])? * 16 + hex(pair[1])?;
        }
        Some(Self::Adobe(key))
    }
    pub(crate) fn apply(&self, bytes: &mut [u8]) {
        match self {
            Self::Idpf(key) => xor_prefix(bytes, key, 1040),
            Self::Adobe(key) => xor_prefix(bytes, key, 1024),
        }
    }
}

fn xor_prefix(bytes: &mut [u8], key: &[u8], length: usize) {
    for (i, byte) in bytes.iter_mut().take(length).enumerate() {
        *byte ^= key[i % key.len()];
    }
}

pub(crate) fn kindle(record: &[u8]) -> Option<Vec<u8>> {
    if record.len() < 24 || &record[..4] != b"FONT" {
        return None;
    }
    let word = |start| u32::from_be_bytes(record[start..start + 4].try_into().unwrap()) as usize;
    let declared = word(4);
    let flags = word(8);
    let data_start = word(12);
    let key_length = word(16);
    let key_start = word(20);
    let max = crate::font_metadata::MAX_FONT_BYTES;
    if declared > max || data_start < 24 || record.len().saturating_sub(data_start) > max {
        return None;
    }
    let mut data = record.get(data_start..)?.to_vec();
    if flags & 2 != 0 {
        if !(1..=1024).contains(&key_length) {
            return None;
        }
        let key = record.get(key_start..key_start.checked_add(key_length)?)?;
        xor_prefix(&mut data, key, if key_length == 16 { 1024 } else { 1040 });
    }
    if flags & 1 != 0 {
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(data.as_slice())
            .take(max as u64 + 1)
            .read_to_end(&mut decoded)
            .ok()?;
        if decoded.len() > max {
            return None;
        }
        data = decoded;
    }
    if declared != 0 && data.len() != declared {
        return None;
    }
    media_type(&data)?;
    Some(data)
}

pub(crate) fn media_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    match bytes.get(..4)? {
        b"OTTO" => Some(("otf", "font/otf")),
        b"\0\x01\0\0" | b"true" | b"ttcf" => Some(("ttf", "font/ttf")),
        b"wOFF" => Some(("woff", "font/woff")),
        b"wOF2" => Some(("woff2", "font/woff2")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedding_transforms_only_change_the_defined_prefix() {
        for decoder in [
            Decoder::idpf(" id \t\r\n"),
            Decoder::adobe("urn:uuid:00112233-4455-6677-8899-aabbccddeeff").unwrap(),
        ] {
            let original = vec![42; 2000];
            let mut bytes = original.clone();
            decoder.apply(&mut bytes);
            assert_ne!(bytes[..100], original[..100]);
            assert_eq!(bytes[1040..], original[1040..]);
            decoder.apply(&mut bytes);
            assert_eq!(bytes, original);
        }
        let mut a = vec![0; 1100];
        let mut b = a.clone();
        Decoder::idpf(" id \t\r\n").apply(&mut a);
        Decoder::idpf("id").apply(&mut b);
        assert_eq!(a, b);
    }
    #[test]
    fn kindle_fonts_are_deobfuscated_before_bounded_decompression() {
        use std::io::Write;
        let mut plain = vec![0; 2000];
        plain[..4].copy_from_slice(b"OTTO");
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&plain).unwrap();
        let mut compressed = encoder.finish().unwrap();
        let key = [23; 16];
        xor_prefix(&mut compressed, &key, 1024);
        let mut record = b"FONT".to_vec();
        for word in [plain.len() as u32, 3, 40, 16, 24] {
            record.extend(word.to_be_bytes());
        }
        record.extend(key);
        record.extend(compressed);
        assert_eq!(kindle(&record), Some(plain));
        record[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(kindle(&record).is_none());
    }
}
