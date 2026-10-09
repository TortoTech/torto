use std::collections::{HashMap, HashSet};

use encoding_rs::GBK;
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::object::{
    Array, Dict, MaybeRef, Name, Object, ObjectIdentifier, String as PdfString,
};

use crate::source::SourceTocEntry;

const MAX_OUTLINE_DEPTH: usize = 64;
const MAX_NAME_TREE_DEPTH: usize = 64;

pub(super) struct CatalogInfo {
    pub(super) title: Option<String>,
    pub(super) author: Option<String>,
    pub(super) language: Option<String>,
    pub(super) table_of_contents: Vec<SourceTocEntry>,
}

pub(super) fn read(pdf: &Pdf) -> CatalogInfo {
    let metadata = pdf.metadata();
    CatalogInfo {
        title: metadata.title.as_deref().and_then(decode_pdf_text),
        author: metadata.author.as_deref().and_then(decode_pdf_text),
        language: pdf
            .xref()
            .get::<Dict<'_>>(pdf.xref().root_id())
            .and_then(|catalog| catalog.get::<PdfString<'_>>(b"Lang"))
            .and_then(|language| decode_pdf_text(language.as_bytes())),
        table_of_contents: OutlineReader::new(pdf).read(),
    }
}

struct OutlineReader<'a> {
    pdf: &'a Pdf,
    page_indices: HashMap<ObjectIdentifier, usize>,
    named_destinations: HashMap<String, Object<'a>>,
    seen_outline_nodes: HashSet<NodeKey>,
    locations: Vec<OutlineLocation>,
}

pub(super) struct OutlineLocation {
    pub(super) label: String,
    pub(super) page: usize,
    pub(super) left: Option<f64>,
    pub(super) top: Option<f64>,
}

pub(super) fn locations(pdf: &Pdf) -> Vec<OutlineLocation> {
    let mut reader = OutlineReader::new(pdf);
    reader.read();
    reader.locations
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct NodeKey {
    id: Option<ObjectIdentifier>,
    address: usize,
    length: usize,
}

impl<'a> OutlineReader<'a> {
    fn new(pdf: &'a Pdf) -> Self {
        let page_indices = pdf
            .pages()
            .iter()
            .enumerate()
            .filter_map(|(index, page)| page.raw().obj_id().map(|id| (id, index)))
            .collect();
        Self {
            pdf,
            page_indices,
            named_destinations: HashMap::new(),
            seen_outline_nodes: HashSet::new(),
            locations: Vec::new(),
        }
    }

    fn read(&mut self) -> Vec<SourceTocEntry> {
        let Some(catalog) = self.pdf.xref().get::<Dict<'_>>(self.pdf.xref().root_id()) else {
            return Vec::new();
        };
        self.read_named_destinations(&catalog);
        let Some(outlines) = catalog.get::<Dict<'_>>(b"Outlines") else {
            return Vec::new();
        };
        let Some(first) = outlines.get::<Dict<'_>>(b"First") else {
            return Vec::new();
        };
        self.read_outline_level(first, 0)
    }

    fn read_named_destinations(&mut self, catalog: &Dict<'a>) {
        if let Some(names) = catalog.get::<Dict<'a>>(b"Names")
            && let Some(destinations) = names.get::<Dict<'a>>(b"Dests")
        {
            let mut seen = HashSet::new();
            self.read_name_tree(&destinations, &mut seen, 0);
        }
        if let Some(destinations) = catalog.get::<Dict<'a>>(b"Dests") {
            for key in destinations.keys() {
                if let Some(destination) = destinations.get::<Object<'a>>(key.as_ref()) {
                    self.named_destinations.insert(
                        String::from_utf8_lossy(key.as_ref()).into_owned(),
                        destination,
                    );
                }
            }
        }
    }

    fn read_name_tree(&mut self, node: &Dict<'a>, seen: &mut HashSet<NodeKey>, depth: usize) {
        if depth > MAX_NAME_TREE_DEPTH || !seen.insert(node_key(node)) {
            return;
        }
        if let Some(names) = node.get::<Array<'a>>(b"Names") {
            let mut items = names.raw_iter();
            while let (Some(name), Some(destination)) = (items.next(), items.next()) {
                let Some(name) = self.resolve_maybe_ref(name).and_then(destination_name) else {
                    continue;
                };
                if let Some(destination) = self.resolve_maybe_ref(destination) {
                    self.named_destinations.insert(name, destination);
                }
            }
        }
        if let Some(children) = node.get::<Array<'a>>(b"Kids") {
            for child in children.iter::<Dict<'a>>() {
                self.read_name_tree(&child, seen, depth + 1);
            }
        }
    }

    fn read_outline_level(&mut self, first: Dict<'a>, depth: usize) -> Vec<SourceTocEntry> {
        if depth > MAX_OUTLINE_DEPTH {
            return Vec::new();
        }
        let mut entries = Vec::new();
        let mut current = Some(first);
        while let Some(item) = current {
            if !self.seen_outline_nodes.insert(node_key(&item)) {
                break;
            }
            let children = item
                .get::<Dict<'a>>(b"First")
                .map_or_else(Vec::new, |child| self.read_outline_level(child, depth + 1));
            let label = item
                .get::<PdfString<'a>>(b"Title")
                .and_then(|title| decode_pdf_text(title.as_bytes()));
            let page_index = self.outline_destination(&item);
            match (label, page_index) {
                (Some(label), Some(page_index)) if !label.trim().is_empty() => {
                    let destination = item.get::<Object<'a>>(b"Dest").or_else(|| {
                        item.get::<Dict<'a>>(b"A")
                            .and_then(|a| a.get::<Object<'a>>(b"D"))
                    });
                    let (left, top) = destination
                        .and_then(|d| self.destination_position(d, &mut HashSet::new()))
                        .unwrap_or((None, None));
                    self.locations.push(OutlineLocation {
                        label: label.trim().to_owned(),
                        page: page_index,
                        left,
                        top,
                    });
                    entries.push(SourceTocEntry {
                        label: label.trim().to_owned(),
                        href: format!("Text/section-{}.xhtml", page_index + 1),
                        children,
                    });
                }
                _ => entries.extend(children),
            }
            current = item.get::<Dict<'a>>(b"Next");
        }
        entries
    }

    fn outline_destination(&self, item: &Dict<'a>) -> Option<usize> {
        if let Some(destination) = item.get::<Object<'a>>(b"Dest") {
            return self.resolve_destination(destination, &mut HashSet::new());
        }
        let action = item.get::<Dict<'a>>(b"A")?;
        let action_type = action.get::<Name<'a>>(b"S")?;
        if action_type.as_ref() != b"GoTo" {
            return None;
        }
        let destination = action.get::<Object<'a>>(b"D")?;
        self.resolve_destination(destination, &mut HashSet::new())
    }

    fn resolve_destination(
        &self,
        destination: Object<'a>,
        seen_names: &mut HashSet<String>,
    ) -> Option<usize> {
        match destination {
            Object::Array(array) => self.page_from_destination_array(&array),
            Object::Dict(dict) => dict
                .get::<Object<'a>>(b"D")
                .and_then(|value| self.resolve_destination(value, seen_names)),
            Object::String(value) => {
                let name = decode_pdf_text(value.as_bytes())?;
                self.resolve_named_destination(&name, seen_names)
            }
            Object::Name(value) => {
                let name = String::from_utf8_lossy(value.as_ref());
                self.resolve_named_destination(&name, seen_names)
            }
            _ => None,
        }
    }

    fn resolve_named_destination(
        &self,
        name: &str,
        seen_names: &mut HashSet<String>,
    ) -> Option<usize> {
        if !seen_names.insert(name.to_owned()) {
            return None;
        }
        let destination = self.named_destinations.get(name)?.clone();
        self.resolve_destination(destination, seen_names)
    }

    fn page_from_destination_array(&self, destination: &Array<'a>) -> Option<usize> {
        match destination.iter::<Object<'a>>().next()? {
            Object::Dict(page) => page
                .obj_id()
                .and_then(|id| self.page_indices.get(&id).copied()),
            Object::Number(number) => usize::try_from(number.as_i64())
                .ok()
                .filter(|index| *index < self.page_indices.len()),
            _ => None,
        }
    }

    fn resolve_maybe_ref(&self, value: MaybeRef<Object<'a>>) -> Option<Object<'a>> {
        match value {
            MaybeRef::Ref(reference) => self.pdf.xref().get(reference.into()),
            MaybeRef::NotRef(value) => Some(value),
        }
    }

    fn destination_position(
        &self,
        destination: Object<'a>,
        seen: &mut HashSet<String>,
    ) -> Option<(Option<f64>, Option<f64>)> {
        match destination {
            Object::Array(array) => {
                let values = array.iter::<Object<'a>>().collect::<Vec<_>>();
                let number = |index: usize| {
                    values.get(index).and_then(|v| {
                        if let Object::Number(n) = v {
                            Some(n.as_f64())
                        } else {
                            None
                        }
                    })
                };
                let Some(Object::Name(kind)) = values.get(1) else {
                    return None;
                };
                match kind.as_ref() {
                    b"XYZ" => Some((number(2), number(3))),
                    b"FitH" | b"FitBH" => Some((None, number(2))),
                    b"FitR" => Some((number(2), number(5))),
                    _ => None,
                }
            }
            Object::Dict(dict) => dict
                .get::<Object<'a>>(b"D")
                .and_then(|d| self.destination_position(d, seen)),
            Object::Name(name) => {
                let name = String::from_utf8_lossy(name.as_ref()).into_owned();
                if !seen.insert(name.clone()) {
                    return None;
                }
                self.named_destinations
                    .get(&name)
                    .cloned()
                    .and_then(|d| self.destination_position(d, seen))
            }
            Object::String(name) => {
                let name = decode_pdf_text(name.as_bytes())?;
                if !seen.insert(name.clone()) {
                    return None;
                }
                self.named_destinations
                    .get(&name)
                    .cloned()
                    .and_then(|d| self.destination_position(d, seen))
            }
            _ => None,
        }
    }
}

fn node_key(dict: &Dict<'_>) -> NodeKey {
    NodeKey {
        id: dict.obj_id(),
        address: dict.data().as_ptr() as usize,
        length: dict.data().len(),
    }
}

fn destination_name(value: Object<'_>) -> Option<String> {
    match value {
        Object::String(value) => decode_pdf_text(value.as_bytes()),
        Object::Name(value) => Some(String::from_utf8_lossy(value.as_ref()).into_owned()),
        _ => None,
    }
}

fn decode_pdf_text(bytes: &[u8]) -> Option<String> {
    let decoded = if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        decode_utf16(rest, u16::from_be_bytes)
    } else if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        decode_utf16(rest, u16::from_le_bytes)
    } else if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Some(decoded) = decode_legacy_chinese(bytes) {
        decoded
    } else {
        decode_pdf_doc_encoding(bytes)
    };
    let decoded = strip_language_tags(&decoded).trim().to_owned();
    (!decoded.is_empty()).then_some(decoded)
}

fn decode_utf16(bytes: &[u8], decode: fn([u8; 2]) -> u16) -> String {
    let words = bytes
        .chunks_exact(2)
        .map(|chunk| decode([chunk[0], chunk[1]]));
    char::decode_utf16(words)
        .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn decode_legacy_chinese(bytes: &[u8]) -> Option<String> {
    let high_bytes = bytes.iter().filter(|byte| **byte >= 0x80).count();
    if high_bytes < 4 || high_bytes * 4 < bytes.len() {
        return None;
    }
    let (decoded, _, had_errors) = GBK.decode(bytes);
    if had_errors {
        return None;
    }
    let visible = decoded
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    let cjk = decoded
        .chars()
        .filter(|character| is_cjk(*character))
        .count();
    (visible > 0 && cjk >= 2 && cjk * 100 >= visible * 35).then(|| decoded.into_owned())
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x2ebef
    )
}

fn decode_pdf_doc_encoding(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| match byte {
            0x18 => '\u{02d8}',
            0x19 => '\u{02c7}',
            0x1a => '\u{02c6}',
            0x1b => '\u{02d9}',
            0x1c => '\u{02dd}',
            0x1d => '\u{02db}',
            0x1e => '\u{02da}',
            0x1f => '\u{02dc}',
            0x80 => '\u{2022}',
            0x81 => '\u{2020}',
            0x82 => '\u{2021}',
            0x83 => '\u{2026}',
            0x84 => '\u{2014}',
            0x85 => '\u{2013}',
            0x86 => '\u{0192}',
            0x87 => '\u{2044}',
            0x88 => '\u{2039}',
            0x89 => '\u{203a}',
            0x8a => '\u{2212}',
            0x8b => '\u{2030}',
            0x8c => '\u{201e}',
            0x8d => '\u{201c}',
            0x8e => '\u{201d}',
            0x8f => '\u{2018}',
            0x90 => '\u{2019}',
            0x91 => '\u{201a}',
            0x92 => '\u{2122}',
            0x93 => '\u{fb01}',
            0x94 => '\u{fb02}',
            0x95 => '\u{0141}',
            0x96 => '\u{0152}',
            0x97 => '\u{0160}',
            0x98 => '\u{0178}',
            0x99 => '\u{017d}',
            0x9a => '\u{0131}',
            0x9b => '\u{0142}',
            0x9c => '\u{0153}',
            0x9d => '\u{0161}',
            0x9e => '\u{017e}',
            0xa0 => '\u{20ac}',
            byte => char::from(*byte),
        })
        .collect()
}

fn strip_language_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for character in value.chars() {
        if character == '\u{1b}' {
            in_tag = !in_tag;
        } else if !in_tag {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn decodes_unicode_and_pdf_doc_strings() {
        assert_eq!(
            decode_pdf_text(&[0xfe, 0xff, 0x4e, 0x2d, 0x65, 0x87]).as_deref(),
            Some("中文")
        );
        assert_eq!(
            decode_pdf_text(b"Price \xa010").as_deref(),
            Some("Price €10")
        );
    }

    #[test]
    fn reads_nested_outlines_actions_and_named_destinations() {
        let pdf = Pdf::new(outline_pdf()).unwrap();
        let toc = read(&pdf).table_of_contents;

        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].label, "Intro");
        assert_eq!(toc[0].href, "Text/section-1.xhtml");
        assert_eq!(toc[1].label, "Part A");
        assert_eq!(toc[1].href, "Text/section-2.xhtml");
        assert_eq!(toc[1].children.len(), 1);
        assert_eq!(toc[1].children[0].label, "Detail");
        assert_eq!(toc[1].children[0].href, "Text/section-2.xhtml");
    }

    #[test]
    fn reads_document_language_without_guessing_missing_or_non_english_tags() {
        for (entry, expected) in [
            ("/Lang (en)", Some("en")),
            ("/Lang ( en-GB )", Some("en-GB")),
            ("/Lang <FEFF00660072>", Some("fr")),
            ("/Lang (zh-Hans)", Some("zh-Hans")),
            ("/Lang ()", None),
            ("/Lang 42", None),
            ("", None),
        ] {
            let pdf = Pdf::new(outline_pdf_with_catalog_entry(entry)).unwrap();
            assert_eq!(read(&pdf).language.as_deref(), expected, "{entry}");
        }
    }

    fn outline_pdf() -> Vec<u8> {
        outline_pdf_with_catalog_entry("")
    }

    fn outline_pdf_with_catalog_entry(entry: &str) -> Vec<u8> {
        let catalog = format!(
            "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R /Names << /Dests 9 0 R >> {entry} >>"
        );
        let objects = [
            catalog.as_bytes(),
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            b"<< /Type /Outlines /First 6 0 R /Last 7 0 R /Count 3 >>",
            b"<< /Title (Intro) /Parent 5 0 R /Dest [3 0 R /Fit] /Next 7 0 R >>",
            b"<< /Title (Part A) /Parent 5 0 R /A << /S /GoTo /D (second) >> /First 8 0 R >>",
            b"<< /Title (Detail) /Parent 7 0 R /Dest [4 0 R /Fit] >>",
            b"<< /Names [(second) [4 0 R /Fit]] >>",
        ];
        let mut output = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            writeln!(&mut output, "{} 0 obj", index + 1).unwrap();
            output.extend_from_slice(object);
            output.extend_from_slice(b"\nendobj\n");
        }
        let xref = output.len();
        write!(&mut output, "xref\n0 {}\n", objects.len() + 1).unwrap();
        output.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            writeln!(&mut output, "{offset:010} 00000 n ").unwrap();
        }
        write!(
            &mut output,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .unwrap();
        output
    }
}
