use std::collections::{HashMap, HashSet};

use encoding_rs::WINDOWS_1252;

use crate::source::{SourceResource, SourceTocEntry};
use crate::{BookFormat, FormatError, conversion_error};

const MAX_UNCOMPRESSED_TEXT: usize = 512 * 1024 * 1024;
const INVALID_INDEX: u32 = u32::MAX;

pub(crate) struct Kf8Book {
    pub sections: Vec<Kf8Section>,
    pub table_of_contents: Vec<SourceTocEntry>,
    pub resources: Vec<SourceResource>,
}

pub(crate) struct Kf8Section {
    pub title: String,
    pub html: String,
    source_index: usize,
}

pub(crate) struct MobiMetadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub languages: Vec<String>,
    pub cover_path: Option<String>,
}

pub(crate) struct Mobi6Book {
    pub sections: Vec<Mobi6Section>,
    pub resources: Vec<SourceResource>,
    pub image_sources: HashMap<usize, String>,
}

pub(crate) struct Mobi6Section {
    pub title: String,
    pub html: String,
}

#[derive(Clone, Copy)]
struct Header {
    compression: u16,
    text_records: usize,
    encoding: u32,
    version: u32,
    resource_start: usize,
    huff_start: usize,
    huff_count: usize,
    trailing_flags: u32,
    ncx: u32,
    fdst: u32,
    frag: u32,
    skel: u32,
}

impl Header {
    fn parse(record: &[u8]) -> Result<Self, String> {
        if bytes(record, 16, 4)? != b"MOBI" {
            return Err("missing MOBI header".to_owned());
        }
        Ok(Self {
            compression: u16_at(record, 0)?,
            text_records: usize_at_u16(record, 8)?,
            encoding: u32_at(record, 28)?,
            version: u32_at(record, 36)?,
            resource_start: usize_at_u32(record, 108)?,
            huff_start: usize_at_u32(record, 112)?,
            huff_count: usize_at_u32(record, 116)?,
            trailing_flags: optional_u32(record, 240).unwrap_or(0),
            ncx: optional_u32(record, 244).unwrap_or(INVALID_INDEX),
            fdst: optional_u32(record, 192).unwrap_or(INVALID_INDEX),
            frag: optional_u32(record, 248).unwrap_or(INVALID_INDEX),
            skel: optional_u32(record, 252).unwrap_or(INVALID_INDEX),
        })
    }
}

struct Context<'a> {
    pdb: Pdb<'a>,
    base: usize,
    header: Header,
}

pub(crate) fn is_kf8(bytes: &[u8]) -> bool {
    Context::open(bytes).is_ok()
}

pub(crate) fn parse(bytes: &[u8], format: BookFormat) -> Result<Kf8Book, FormatError> {
    parse_inner(bytes).map_err(|error| conversion_error(format, error))
}

pub(crate) fn metadata(bytes: &[u8], format: BookFormat) -> Result<MobiMetadata, FormatError> {
    metadata_inner(bytes).map_err(|error| conversion_error(format, error))
}

pub(crate) fn parse_mobi6(bytes: &[u8], format: BookFormat) -> Result<Mobi6Book, FormatError> {
    parse_mobi6_inner(bytes).map_err(|error| conversion_error(format, error))
}

fn metadata_inner(data: &[u8]) -> Result<MobiMetadata, String> {
    let context = Context::primary(data)?;
    let record = context.pdb.record(0)?;
    let header_length = usize_at_u32(record, 20)?;
    let title_offset = usize_at_u32(record, 84)?;
    let title_length = usize_at_u32(record, 88)?;
    let mut title = bytes(record, title_offset, title_length)
        .ok()
        .map(|value| decode_text(value, context.header.encoding))
        .filter(|value| !value.trim().is_empty());
    let mut authors = Vec::new();
    let mut languages = Vec::new();
    let mut cover_offset = None;
    let mut thumbnail_offset = None;
    if optional_u32(record, 128).unwrap_or_default() & 0x40 != 0 {
        let exth_start = 16usize
            .checked_add(header_length)
            .ok_or_else(|| "EXTH offset overflow".to_owned())?;
        for entry in parse_exth(record, exth_start)? {
            match entry.kind {
                100 => push_metadata_text(&mut authors, entry.data, context.header.encoding),
                201 => cover_offset = uint_from_bytes(entry.data).ok(),
                202 => thumbnail_offset = uint_from_bytes(entry.data).ok(),
                503 => {
                    let value = decode_text(entry.data, context.header.encoding);
                    if !value.trim().is_empty() {
                        title = Some(value);
                    }
                }
                524 => push_metadata_text(&mut languages, entry.data, context.header.encoding),
                _ => {}
            }
        }
    }
    if languages.is_empty()
        && let Some(language) = mobi_locale(record)
    {
        languages.push(language.to_owned());
    }
    let invalid_index = INVALID_INDEX as usize;
    let cover_path = cover_offset
        .filter(|offset| *offset != invalid_index)
        .or_else(|| thumbnail_offset.filter(|offset| *offset != invalid_index))
        .and_then(|offset| resource_path(&context, offset + 1).ok().flatten());
    Ok(MobiMetadata {
        title: title.map(|value| decode_html_entities(value.trim())),
        authors: authors
            .into_iter()
            .map(|value| decode_html_entities(value.trim()))
            .filter(|value| !value.is_empty())
            .collect(),
        languages,
        cover_path,
    })
}

fn parse_mobi6_inner(bytes: &[u8]) -> Result<Mobi6Book, String> {
    let context = Context::primary(bytes)?;
    if context.header.version >= 8 || hybrid_boundary(context.pdb.record(0)?)?.is_some() {
        return Err("MOBI container contains KF8 content".to_owned());
    }
    let raw = context.load_text()?;
    let file_positions = find_numeric_attributes(&raw, b"filepos");
    let ranges = split_mobi6_sections(&raw);
    let mut sections = Vec::with_capacity(ranges.len());
    for range in ranges {
        let section_raw = raw
            .get(range.clone())
            .ok_or_else(|| "MOBI6 section points outside decompressed text".to_owned())?;
        let anchors = file_positions
            .iter()
            .copied()
            .filter(|position| *position >= range.start && *position < range.end)
            .map(|position| (position - range.start, position))
            .collect::<Vec<_>>();
        let anchored = insert_filepos_anchors(section_raw, &anchors)?;
        let html = decode_text(&anchored, context.header.encoding);
        if html.trim().is_empty() {
            continue;
        }
        let title = extract_document_title(&html)
            .unwrap_or_else(|| format!("Chapter {}", sections.len() + 1));
        sections.push(Mobi6Section { title, html });
    }
    if sections.is_empty() {
        return Err("MOBI6 book has no readable content sections".to_owned());
    }

    let mut resources = Vec::new();
    let mut image_sources = HashMap::new();
    let start = context
        .base
        .checked_add(context.header.resource_start)
        .ok_or_else(|| "MOBI resource index overflow".to_owned())?;
    for absolute_index in start..context.pdb.len() {
        let record = context.pdb.record(absolute_index)?;
        let Some((extension, media_type)) = image_type(record) else {
            continue;
        };
        let id = absolute_index - start + 1;
        let path = format!("Images/kindle-{id}.{extension}");
        image_sources.insert(id, path.clone());
        resources.push(SourceResource {
            path,
            media_type: media_type.to_owned(),
            bytes: record.to_vec(),
        });
    }
    Ok(Mobi6Book {
        sections,
        resources,
        image_sources,
    })
}

fn parse_inner(bytes: &[u8]) -> Result<Kf8Book, String> {
    let context = Context::open(bytes)?;
    let raw = context.load_text()?;
    let flow_table = context.load_fdst().unwrap_or_default();
    let skeletons = parse_skeletons(&context)?;
    let fragments = parse_fragments(&context)?;
    let toc = parse_toc(&context, &skeletons, &fragments).unwrap_or_default();

    let mut resolved_anchors = HashMap::new();
    let mut raw_sections = reconstruct_sections(
        &raw,
        context.header.encoding,
        &skeletons,
        &fragments,
        &toc.titles,
        &toc.fragment_anchors,
        &mut resolved_anchors,
    )?;
    if raw_sections.is_empty() {
        return Err("KF8 book has no readable content sections".to_owned());
    }

    let mut section_map = vec![None; skeletons.len()];
    for (section_index, section) in raw_sections.iter().enumerate() {
        section_map[section.source_index] = Some(section_index);
    }
    let table_of_contents = remap_toc_entries(toc.entries, &section_map, &resolved_anchors);

    let mut resources = Vec::new();
    let mut resource_paths = HashMap::new();
    load_embedded_images(&context, &mut resources, &mut resource_paths)?;
    load_flow_resources(
        &raw,
        &flow_table,
        &raw_sections,
        &mut resources,
        &mut resource_paths,
    )?;

    for section in &mut raw_sections {
        section.html = replace_resource_uris(&section.html, &resource_paths);
    }
    for resource in &mut resources {
        if resource.media_type == "text/css" {
            let css = String::from_utf8_lossy(&resource.bytes);
            // CSS URLs resolve from Styles/, the same depth as Text/.
            resource.bytes = replace_resource_uris(&css, &resource_paths).into_bytes();
        }
    }
    Ok(Kf8Book {
        sections: raw_sections,
        table_of_contents,
        resources,
    })
}

impl<'a> Context<'a> {
    fn primary(bytes: &'a [u8]) -> Result<Self, String> {
        let pdb = Pdb::open(bytes)?;
        let header = Header::parse(pdb.record(0)?)?;
        Ok(Self {
            pdb,
            base: 0,
            header,
        })
    }

    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let primary = Self::primary(bytes)?;
        if primary.header.version >= 8 {
            return Ok(primary);
        }

        let boundary = hybrid_boundary(primary.pdb.record(0)?)?
            .ok_or_else(|| "MOBI container does not contain a KF8 header".to_owned())?;
        let header = Header::parse(primary.pdb.record(boundary)?)?;
        if header.version < 8 {
            return Err("hybrid boundary does not point to a KF8 header".to_owned());
        }
        Ok(Self {
            pdb: primary.pdb,
            base: boundary,
            header,
        })
    }

    fn relative_record(&self, index: usize) -> Result<&'a [u8], String> {
        self.pdb.record(
            self.base
                .checked_add(index)
                .ok_or_else(|| "KF8 record index overflow".to_owned())?,
        )
    }

    fn load_text(&self) -> Result<Vec<u8>, String> {
        let mut decompressor = Decompressor::new(self)?;
        let mut output = Vec::new();
        for index in 0..self.header.text_records {
            let record = self.relative_record(index + 1)?;
            let record = remove_trailing_entries(record, self.header.trailing_flags)?;
            let decoded = decompressor.decompress(record)?;
            let next_length = output
                .len()
                .checked_add(decoded.len())
                .ok_or_else(|| "KF8 text length overflow".to_owned())?;
            if next_length > MAX_UNCOMPRESSED_TEXT {
                return Err("KF8 text exceeds the 512 MiB safety limit".to_owned());
            }
            output.extend_from_slice(&decoded);
        }
        Ok(output)
    }

    fn load_fdst(&self) -> Result<Vec<(usize, usize)>, String> {
        let index = valid_index(self.header.fdst)?;
        let record = self.relative_record(index)?;
        if bytes(record, 0, 4)? != b"FDST" {
            return Err("invalid FDST record".to_owned());
        }
        let count = usize_at_u32(record, 8)?;
        (0..count)
            .map(|entry| {
                let offset = 12usize
                    .checked_add(
                        entry
                            .checked_mul(8)
                            .ok_or_else(|| "FDST table overflow".to_owned())?,
                    )
                    .ok_or_else(|| "FDST table overflow".to_owned())?;
                Ok((
                    usize_at_u32(record, offset)?,
                    usize_at_u32(record, offset + 4)?,
                ))
            })
            .collect()
    }
}

struct Pdb<'a> {
    bytes: &'a [u8],
    offsets: Vec<usize>,
}

impl<'a> Pdb<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < 78 || bytes.get(60..68) != Some(b"BOOKMOBI".as_slice()) {
            return Err("not a Palm database MOBI container".to_owned());
        }
        let count = usize_at_u16(bytes, 76)?;
        let table_length = count
            .checked_mul(8)
            .and_then(|length| length.checked_add(78))
            .ok_or_else(|| "PDB record table overflow".to_owned())?;
        if table_length > bytes.len() {
            return Err("truncated PDB record table".to_owned());
        }
        let mut offsets = Vec::with_capacity(count);
        for index in 0..count {
            let offset = usize_at_u32(bytes, 78 + index * 8)?;
            if offset < table_length || offset > bytes.len() {
                return Err("invalid PDB record offset".to_owned());
            }
            if offsets.last().is_some_and(|previous| *previous > offset) {
                return Err("PDB record offsets are not ordered".to_owned());
            }
            offsets.push(offset);
        }
        if offsets.is_empty() {
            return Err("PDB contains no records".to_owned());
        }
        Ok(Self { bytes, offsets })
    }

    fn record(&self, index: usize) -> Result<&'a [u8], String> {
        let start = *self
            .offsets
            .get(index)
            .ok_or_else(|| format!("PDB record {index} is out of bounds"))?;
        let end = self
            .offsets
            .get(index + 1)
            .copied()
            .unwrap_or(self.bytes.len());
        self.bytes
            .get(start..end)
            .ok_or_else(|| format!("PDB record {index} is truncated"))
    }

    fn len(&self) -> usize {
        self.offsets.len()
    }
}

struct ExthEntry<'a> {
    kind: u32,
    data: &'a [u8],
}

fn parse_exth(record: &[u8], start: usize) -> Result<Vec<ExthEntry<'_>>, String> {
    if bytes(record, start, 4)? != b"EXTH" {
        return Err("invalid EXTH header".to_owned());
    }
    let length = usize_at_u32(record, start + 4)?;
    let count = usize_at_u32(record, start + 8)?;
    if length < 12 || count > 4_096 {
        return Err("invalid EXTH size".to_owned());
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| "EXTH range overflow".to_owned())?;
    bytes(record, start, length)?;
    let mut position = start + 12;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if position + 8 > end {
            return Err("truncated EXTH entry".to_owned());
        }
        let kind = u32_at(record, position)?;
        let entry_length = usize_at_u32(record, position + 4)?;
        if entry_length < 8 || position + entry_length > end {
            return Err("invalid EXTH entry length".to_owned());
        }
        entries.push(ExthEntry {
            kind,
            data: bytes(record, position + 8, entry_length - 8)?,
        });
        position += entry_length;
    }
    Ok(entries)
}

fn push_metadata_text(values: &mut Vec<String>, data: &[u8], encoding: u32) {
    let value = decode_text(data, encoding);
    let value = value.trim_matches(['\0', ' ', '\r', '\n', '\t']);
    if !value.is_empty() {
        values.push(value.to_owned());
    }
}

fn mobi_locale(record: &[u8]) -> Option<&'static str> {
    let region = usize::from(*record.get(94)? >> 2);
    match *record.get(95)? {
        1 => Some("ar"),
        2 => Some("bg"),
        3 => Some("ca"),
        4 => Some(match region {
            1 => "zh-TW",
            2 => "zh-CN",
            3 => "zh-HK",
            4 => "zh-SG",
            _ => "zh",
        }),
        5 => Some("cs"),
        6 => Some("da"),
        7 => Some("de"),
        8 => Some("el"),
        9 => Some(match region {
            1 => "en-US",
            2 => "en-GB",
            3 => "en-AU",
            4 => "en-CA",
            _ => "en",
        }),
        10 => Some("es"),
        11 => Some("fi"),
        12 => Some("fr"),
        13 => Some("he"),
        14 => Some("hu"),
        16 => Some("it"),
        17 => Some("ja"),
        18 => Some("ko"),
        19 => Some("nl"),
        20 => Some("no"),
        21 => Some("pl"),
        22 => Some("pt"),
        24 => Some("ro"),
        25 => Some("ru"),
        27 => Some("sk"),
        29 => Some("sv"),
        30 => Some("th"),
        31 => Some("tr"),
        33 => Some("id"),
        34 => Some("uk"),
        39 => Some("lt"),
        42 => Some("vi"),
        57 => Some("hi"),
        _ => None,
    }
}

fn resource_path(context: &Context<'_>, id: usize) -> Result<Option<String>, String> {
    let start = context
        .base
        .checked_add(context.header.resource_start)
        .ok_or_else(|| "MOBI resource index overflow".to_owned())?;
    let absolute = start
        .checked_add(id.saturating_sub(1))
        .ok_or_else(|| "MOBI resource index overflow".to_owned())?;
    let record = context.pdb.record(absolute)?;
    Ok(image_type(record).map(|(extension, _)| format!("Images/kindle-{id}.{extension}")))
}

fn find_numeric_attributes(data: &[u8], name: &[u8]) -> Vec<usize> {
    let lower = data.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    let mut values = Vec::new();
    let mut search = 0usize;
    while search + name.len() <= lower.len() {
        let Some(relative) = lower[search..]
            .windows(name.len())
            .position(|window| window == name)
        else {
            break;
        };
        let start = search + relative;
        search = start + name.len();
        if start > 0 && lower[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut position = search;
        while lower.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if lower.get(position) != Some(&b'=') {
            continue;
        }
        position += 1;
        while lower.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        let quote = lower
            .get(position)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        if quote.is_some() {
            position += 1;
        }
        let value_start = position;
        while lower.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position > value_start
            && quote.is_none_or(|quote| lower.get(position) == Some(&quote))
            && let Ok(value) = std::str::from_utf8(&lower[value_start..position])
                .unwrap_or_default()
                .parse()
        {
            values.push(value);
        }
    }
    values.sort_unstable();
    values.dedup();
    values
}

fn split_mobi6_sections(data: &[u8]) -> Vec<std::ops::Range<usize>> {
    let lower = data.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut section_start = 0usize;
    let mut position = 0usize;
    while let Some(relative_start) = lower[position..].iter().position(|byte| *byte == b'<') {
        let tag_start = position + relative_start;
        let Some(relative_end) = lower[tag_start..].iter().position(|byte| *byte == b'>') else {
            break;
        };
        let tag_end = tag_start + relative_end + 1;
        let name = lower[tag_start + 1..tag_end - 1]
            .iter()
            .copied()
            .skip_while(|byte| byte.is_ascii_whitespace() || *byte == b'/')
            .take_while(|byte| !byte.is_ascii_whitespace() && *byte != b'/')
            .collect::<Vec<_>>();
        if matches!(name.as_slice(), b"mbp:pagebreak" | b"pagebreak") {
            if section_start < tag_start {
                ranges.push(section_start..tag_start);
            }
            section_start = tag_end;
        }
        position = tag_end;
    }
    if section_start < data.len() {
        ranges.push(section_start..data.len());
    }
    if ranges.is_empty() && !data.is_empty() {
        ranges.push(0..data.len());
    }
    ranges
}

fn insert_filepos_anchors(data: &[u8], anchors: &[(usize, usize)]) -> Result<Vec<u8>, String> {
    let extra = anchors.len().saturating_mul(40);
    let mut output = Vec::with_capacity(data.len().saturating_add(extra));
    let mut copied = 0usize;
    for &(offset, position) in anchors {
        if offset > data.len() || offset < copied {
            return Err("invalid MOBI6 file position".to_owned());
        }
        output.extend_from_slice(&data[copied..offset]);
        output.extend_from_slice(format!("<a id=\"filepos{position}\"></a>").as_bytes());
        copied = offset;
    }
    output.extend_from_slice(&data[copied..]);
    Ok(output)
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn hybrid_boundary(record: &[u8]) -> Result<Option<usize>, String> {
    let flags = optional_u32(record, 128).unwrap_or(0);
    if flags & 0x40 == 0 {
        return Ok(None);
    }
    let mobi_length = usize_at_u32(record, 20)?;
    let exth_start = 16usize
        .checked_add(mobi_length)
        .ok_or_else(|| "EXTH offset overflow".to_owned())?;
    if bytes(record, exth_start, 4)? != b"EXTH" {
        return Ok(None);
    }
    let count = usize_at_u32(record, exth_start + 8)?;
    let mut position = exth_start + 12;
    for _ in 0..count {
        let kind = u32_at(record, position)?;
        let length = usize_at_u32(record, position + 4)?;
        if length < 8 {
            return Err("invalid EXTH record length".to_owned());
        }
        if kind == 121 {
            let payload = bytes(record, position + 8, length - 8)?;
            return Ok(Some(uint_from_bytes(payload)?));
        }
        position = position
            .checked_add(length)
            .ok_or_else(|| "EXTH record offset overflow".to_owned())?;
    }
    Ok(None)
}

#[derive(Clone)]
struct Skeleton {
    fragment_count: usize,
    offset: usize,
    length: usize,
}

#[derive(Clone)]
struct Fragment {
    insert_offset: usize,
    index: usize,
    offset: usize,
    length: usize,
}

fn parse_skeletons(context: &Context<'_>) -> Result<Vec<Skeleton>, String> {
    let index = parse_index(context, valid_index(context.header.skel)?)
        .map_err(|error| format!("SKEL index: {error}"))?;
    index
        .entries
        .into_iter()
        .map(|entry| {
            let range = required_tag(&entry.tags, 6, "SKEL range")?;
            Ok(Skeleton {
                fragment_count: required_tag(&entry.tags, 1, "SKEL fragment count")?[0],
                offset: range[0],
                length: range[1],
            })
        })
        .collect()
}

fn parse_fragments(context: &Context<'_>) -> Result<Vec<Fragment>, String> {
    let index = parse_index(context, valid_index(context.header.frag)?)
        .map_err(|error| format!("FRAG index: {error}"))?;
    index
        .entries
        .into_iter()
        .map(|entry| {
            let range = required_tag(&entry.tags, 6, "FRAG range")?;
            Ok(Fragment {
                insert_offset: entry
                    .name
                    .parse()
                    .map_err(|_| "invalid FRAG insertion offset".to_owned())?,
                index: required_tag(&entry.tags, 4, "FRAG index")?[0],
                offset: range[0],
                length: range[1],
            })
        })
        .collect()
}

fn reconstruct_sections(
    raw: &[u8],
    encoding: u32,
    skeletons: &[Skeleton],
    fragments: &[Fragment],
    toc_titles: &HashMap<usize, String>,
    fragment_anchors: &HashMap<usize, Vec<FragmentAnchor>>,
    resolved_anchors: &mut HashMap<String, String>,
) -> Result<Vec<Kf8Section>, String> {
    let mut sections = Vec::new();
    let mut fragment_start = 0usize;
    for (section_index, skeleton) in skeletons.iter().enumerate() {
        let fragment_end = fragment_start
            .checked_add(skeleton.fragment_count)
            .ok_or_else(|| "SKEL fragment range overflow".to_owned())?;
        let section_fragments = fragments
            .get(fragment_start..fragment_end)
            .ok_or_else(|| "SKEL references missing FRAG entries".to_owned())?;
        fragment_start = fragment_end;
        if section_fragments.is_empty() {
            continue;
        }

        let fragment_length = section_fragments.iter().try_fold(0usize, |sum, fragment| {
            sum.checked_add(fragment.length)
                .ok_or_else(|| "FRAG length overflow".to_owned())
        })?;
        let section_length = skeleton
            .length
            .checked_add(fragment_length)
            .ok_or_else(|| "KF8 section length overflow".to_owned())?;
        let section_end = skeleton
            .offset
            .checked_add(section_length)
            .ok_or_else(|| "KF8 section range overflow".to_owned())?;
        let section_raw = raw
            .get(skeleton.offset..section_end)
            .ok_or_else(|| "KF8 section points outside decompressed text".to_owned())?;
        let mut document = section_raw
            .get(..skeleton.length)
            .ok_or_else(|| "truncated KF8 skeleton".to_owned())?
            .to_vec();

        for fragment in section_fragments {
            let insert = fragment
                .insert_offset
                .checked_sub(skeleton.offset)
                .ok_or_else(|| "FRAG insertion precedes its skeleton".to_owned())?;
            if insert > document.len() {
                return Err("FRAG insertion points outside its skeleton".to_owned());
            }
            let start = skeleton
                .length
                .checked_add(fragment.offset)
                .ok_or_else(|| "FRAG data offset overflow".to_owned())?;
            let end = start
                .checked_add(fragment.length)
                .ok_or_else(|| "FRAG data range overflow".to_owned())?;
            let fragment_raw = section_raw
                .get(start..end)
                .ok_or_else(|| "FRAG data points outside its section".to_owned())?;
            resolve_fragment_anchors(
                fragment_raw,
                encoding,
                fragment_anchors.get(&fragment.index).map(Vec::as_slice),
                resolved_anchors,
            );
            document.splice(insert..insert, fragment_raw.iter().copied());
        }

        let html = decode_text(&document, encoding).replace('\0', "");
        if html.trim().is_empty() || is_navigation_document(&html) {
            continue;
        }
        let title = extract_document_title(&html)
            .or_else(|| toc_titles.get(&section_index).cloned())
            .unwrap_or_else(|| format!("Chapter {}", sections.len() + 1));
        sections.push(Kf8Section {
            title,
            html,
            source_index: section_index,
        });
    }
    Ok(sections)
}

fn resolve_fragment_anchors(
    raw: &[u8],
    encoding: u32,
    anchors: Option<&[FragmentAnchor]>,
    resolved: &mut HashMap<String, String>,
) {
    let Some(anchors) = anchors else {
        return;
    };
    for anchor in anchors {
        let Some(value) = raw
            .get(anchor.offset..)
            .map(|tail| decode_text(tail, encoding))
            .and_then(|tail| first_fragment_identifier(&tail))
        else {
            continue;
        };
        resolved.insert(anchor.id.clone(), value);
    }
}

fn first_fragment_identifier(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut position = 0usize;
    while position < bytes.len() {
        if !bytes[position].is_ascii_whitespace() {
            position += 1;
            continue;
        }
        position += 1;
        let name_start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_alphabetic) {
            position += 1;
        }
        let name = value.get(name_start..position)?;
        if !["id", "name", "aid"]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            continue;
        }
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if bytes.get(position) != Some(&b'=') {
            continue;
        }
        position += 1;
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        let quote = *bytes.get(position)?;
        if !matches!(quote, b'\'' | b'"') {
            continue;
        }
        position += 1;
        let value_start = position;
        while bytes.get(position).is_some_and(|byte| *byte != quote) {
            position += 1;
        }
        let identifier = value.get(value_start..position)?.trim();
        if !identifier.is_empty() {
            return Some(identifier.to_owned());
        }
    }
    None
}

fn remap_toc_entries(
    entries: Vec<SourceTocEntry>,
    section_map: &[Option<usize>],
    resolved_anchors: &HashMap<String, String>,
) -> Vec<SourceTocEntry> {
    let mut remapped = Vec::new();
    for entry in entries {
        let children = remap_toc_entries(entry.children, section_map, resolved_anchors);
        let (path, fragment) = entry
            .href
            .split_once('#')
            .map_or((entry.href.as_str(), None), |(path, fragment)| {
                (path, Some(fragment))
            });
        let source_index = path
            .strip_prefix("Text/section-")
            .and_then(|value| value.strip_suffix(".xhtml"))
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|number| number.checked_sub(1));
        let Some(section_index) = source_index
            .and_then(|index| section_map.get(index))
            .copied()
            .flatten()
        else {
            remapped.extend(children);
            continue;
        };
        let fragment = fragment
            .map(|fragment| {
                resolved_anchors
                    .get(fragment)
                    .map_or(fragment, String::as_str)
            })
            .map(|fragment| format!("#{fragment}"))
            .unwrap_or_default();
        remapped.push(SourceTocEntry {
            label: entry.label,
            href: format!("Text/section-{}.xhtml{fragment}", section_index + 1),
            children,
        });
    }
    remapped
}

fn is_navigation_document(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let has_navigation_semantics = [
        "epub:type=\"toc\"",
        "epub:type='toc'",
        "role=\"doc-toc\"",
        "role='doc-toc'",
        "epub:type=\"landmarks\"",
        "epub:type='landmarks'",
        "epub:type=\"page-list\"",
        "epub:type='page-list'",
    ]
    .iter()
    .any(|value| lower.contains(value));
    if !has_navigation_semantics {
        return false;
    }
    let body = lower.find("<body").and_then(|body_start| {
        let content_start = lower[body_start..].find('>')? + body_start + 1;
        let content_end = lower[content_start..].rfind("</body>")? + content_start;
        lower.get(content_start..content_end)
    });
    let Some(body) = body else {
        return false;
    };
    let body = body.trim();
    body.starts_with("<nav")
        && body
            .rfind("</nav>")
            .is_some_and(|end| body[end + "</nav>".len()..].trim().is_empty())
}

#[derive(Default)]
struct ParsedToc {
    titles: HashMap<usize, String>,
    entries: Vec<SourceTocEntry>,
    fragment_anchors: HashMap<usize, Vec<FragmentAnchor>>,
}

#[derive(Clone)]
struct FragmentAnchor {
    offset: usize,
    id: String,
}

struct FlatTocEntry {
    original_index: usize,
    label: String,
    section: usize,
    parent: Option<usize>,
    heading_level: usize,
    anchor_id: String,
}

fn parse_toc(
    context: &Context<'_>,
    skeletons: &[Skeleton],
    fragments: &[Fragment],
) -> Result<ParsedToc, String> {
    let ncx = valid_index(context.header.ncx)?;
    let index = parse_index(context, ncx)?;
    let mut ranges = Vec::with_capacity(skeletons.len());
    let mut start = 0usize;
    for skeleton in skeletons {
        let end = start
            .checked_add(skeleton.fragment_count)
            .ok_or_else(|| "SKEL fragment range overflow".to_owned())?;
        ranges.push(start..end);
        start = end;
    }

    let mut titles = HashMap::new();
    let mut flat = Vec::with_capacity(index.entries.len());
    let mut fragment_anchors: HashMap<usize, Vec<FragmentAnchor>> = HashMap::new();
    for (original_index, entry) in index.entries.into_iter().enumerate() {
        let Some(position) = entry.tags.get(&6) else {
            continue;
        };
        let Some(&fragment_id) = position.first() else {
            continue;
        };
        let fragment_offset = position.get(1).copied().unwrap_or_default();
        let Some(&label_offset) = entry.tags.get(&3).and_then(|values| values.first()) else {
            continue;
        };
        let Some(label) = index.cncx.get(&label_offset) else {
            continue;
        };
        if label.trim().is_empty() {
            continue;
        }
        let Some(section) = ranges.iter().position(|range| {
            fragments
                .get(range.clone())
                .is_some_and(|items| items.iter().any(|fragment| fragment.index == fragment_id))
        }) else {
            continue;
        };
        let label = label.trim().to_owned();
        let anchor_id = format!("kf8-{fragment_id:x}-{fragment_offset:x}");
        titles.entry(section).or_insert_with(|| label.clone());
        let anchors = fragment_anchors.entry(fragment_id).or_default();
        if !anchors
            .iter()
            .any(|anchor| anchor.offset == fragment_offset)
        {
            anchors.push(FragmentAnchor {
                offset: fragment_offset,
                id: anchor_id.clone(),
            });
        }
        flat.push(FlatTocEntry {
            original_index,
            label,
            section,
            parent: entry
                .tags
                .get(&21)
                .and_then(|values| values.first())
                .copied(),
            heading_level: entry
                .tags
                .get(&4)
                .and_then(|values| values.first())
                .copied()
                .unwrap_or_default(),
            anchor_id,
        });
    }
    let roots = flat
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.heading_level == 0 || entry.parent.is_none())
        .map(|(index, _)| build_toc_entry(index, &flat, &mut HashSet::new()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ParsedToc {
        titles,
        entries: roots,
        fragment_anchors,
    })
}

fn build_toc_entry(
    index: usize,
    entries: &[FlatTocEntry],
    visiting: &mut HashSet<usize>,
) -> Result<SourceTocEntry, String> {
    if !visiting.insert(index) {
        return Err("cyclic KF8 table of contents".to_owned());
    }
    let entry = entries
        .get(index)
        .ok_or_else(|| "KF8 table of contents index is out of bounds".to_owned())?;
    let children = entries
        .iter()
        .enumerate()
        .filter(|(_, child)| child.parent == Some(entry.original_index))
        .map(|(child, _)| build_toc_entry(child, entries, visiting))
        .collect::<Result<Vec<_>, _>>()?;
    visiting.remove(&index);
    Ok(SourceTocEntry {
        label: entry.label.clone(),
        href: format!(
            "Text/section-{}.xhtml#{}",
            entry.section + 1,
            entry.anchor_id
        ),
        children,
    })
}

struct IndexData {
    entries: Vec<IndexEntry>,
    cncx: HashMap<usize, String>,
}

struct IndexEntry {
    name: String,
    tags: HashMap<u8, Vec<usize>>,
}

#[allow(clippy::too_many_lines)]
fn parse_index(context: &Context<'_>, index: usize) -> Result<IndexData, String> {
    let main = context.relative_record(index)?;
    let header = IndexHeader::parse(main)?;
    let tagx_start = header.length;
    if bytes(main, tagx_start, 4)? != b"TAGX" {
        return Err("invalid TAGX section".to_owned());
    }
    let tagx_length = usize_at_u32(main, tagx_start + 4)?;
    let control_bytes = usize_at_u32(main, tagx_start + 8)?;
    if tagx_length < 12 || (tagx_length - 12) % 4 != 0 {
        return Err("invalid TAGX length".to_owned());
    }
    let mut tag_table = Vec::with_capacity((tagx_length - 12) / 4);
    for position in (tagx_start + 12..tagx_start + tagx_length).step_by(4) {
        let values = bytes(main, position, 4)?;
        tag_table.push([values[0], values[1], values[2], values[3]]);
    }

    let mut cncx = HashMap::new();
    for cncx_index in 0..header.cncx_records {
        let record = context.relative_record(index + header.index_records + cncx_index + 1)?;
        let mut position = 0usize;
        while position < record.len() {
            let entry_offset = position;
            let (length, consumed) = variable_length(record, position)?;
            position += consumed;
            let value = bytes(record, position, length)?;
            position += length;
            cncx.insert(
                cncx_index * 0x1_0000 + entry_offset,
                decode_text(value, header.encoding),
            );
        }
    }

    let mut entries = Vec::new();
    for record_index in 0..header.index_records {
        let record = context.relative_record(index + 1 + record_index)?;
        let sub = IndexHeader::parse(record)?;
        for entry_index in 0..sub.entries {
            let idxt_offset = sub
                .idxt
                .checked_add(4 + entry_index * 2)
                .ok_or_else(|| "IDXT offset overflow".to_owned())?;
            let entry_offset = usize_at_u16(record, idxt_offset)?;
            let name_length = usize::from(*bytes(record, entry_offset, 1)?.first().unwrap());
            let name_start = entry_offset + 1;
            let name =
                String::from_utf8_lossy(bytes(record, name_start, name_length)?).into_owned();
            let start = name_start + name_length;
            let mut control_index = 0usize;
            let mut position = start
                .checked_add(control_bytes)
                .ok_or_else(|| "TAGX value offset overflow".to_owned())?;
            let mut tag_specs = Vec::new();

            for [tag, value_count, mask, end] in &tag_table {
                if end & 1 != 0 {
                    control_index += 1;
                    continue;
                }
                let control = *bytes(record, start + control_index, 1)?.first().unwrap();
                let value = control & mask;
                if value == *mask {
                    if mask.count_ones() > 1 {
                        let (byte_count, consumed) = variable_length(record, position)?;
                        position += consumed;
                        tag_specs.push((*tag, None, Some(byte_count), usize::from(*value_count)));
                    } else {
                        tag_specs.push((*tag, Some(1), None, usize::from(*value_count)));
                    }
                } else {
                    tag_specs.push((
                        *tag,
                        Some(usize::from(value >> mask.trailing_zeros())),
                        None,
                        usize::from(*value_count),
                    ));
                }
            }

            let mut tags = HashMap::new();
            for (tag, value_count, byte_count, values_per_entry) in tag_specs {
                let mut values = Vec::new();
                if let Some(value_count) = value_count {
                    let count = value_count
                        .checked_mul(values_per_entry)
                        .ok_or_else(|| "TAGX value count overflow".to_owned())?;
                    for _ in 0..count {
                        let (value, consumed) = variable_length(record, position)?;
                        position += consumed;
                        values.push(value);
                    }
                } else {
                    let mut consumed_total = 0usize;
                    let byte_count = byte_count.unwrap_or_default();
                    while consumed_total < byte_count {
                        let (value, consumed) = variable_length(record, position)?;
                        position += consumed;
                        consumed_total += consumed;
                        values.push(value);
                    }
                    if consumed_total != byte_count {
                        return Err("TAGX values exceed their byte range".to_owned());
                    }
                }
                tags.insert(tag, values);
            }
            entries.push(IndexEntry { name, tags });
        }
    }
    Ok(IndexData { entries, cncx })
}

struct IndexHeader {
    length: usize,
    idxt: usize,
    entries: usize,
    encoding: u32,
    index_records: usize,
    cncx_records: usize,
}

impl IndexHeader {
    fn parse(record: &[u8]) -> Result<Self, String> {
        if bytes(record, 0, 4)? != b"INDX" {
            return Err("invalid INDX record".to_owned());
        }
        Ok(Self {
            length: usize_at_u32(record, 4)?,
            idxt: usize_at_u32(record, 20)?,
            entries: usize_at_u32(record, 24)?,
            encoding: u32_at(record, 28)?,
            index_records: usize_at_u32(record, 24)?,
            cncx_records: usize_at_u32(record, 52)?,
        })
    }
}

fn required_tag<'a>(
    tags: &'a HashMap<u8, Vec<usize>>,
    tag: u8,
    description: &str,
) -> Result<&'a [usize], String> {
    let values = tags
        .get(&tag)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| format!("missing {description}"))?;
    if tag == 6 && values.len() < 2 {
        return Err(format!("incomplete {description}"));
    }
    Ok(values)
}

fn load_embedded_images(
    context: &Context<'_>,
    resources: &mut Vec<SourceResource>,
    paths: &mut HashMap<ResourceKey, String>,
) -> Result<(), String> {
    let resource_start = context
        .base
        .checked_add(context.header.resource_start)
        .ok_or_else(|| "KF8 resource index overflow".to_owned())?;
    for absolute_index in resource_start..context.pdb.len() {
        let record = context.pdb.record(absolute_index)?;
        let Some((extension, media_type)) = image_type(record) else {
            continue;
        };
        let id = absolute_index - resource_start + 1;
        let path = format!("Images/kindle-{id}.{extension}");
        paths.insert(ResourceKey::Embed(id), path.clone());
        resources.push(SourceResource {
            path,
            media_type: media_type.to_owned(),
            bytes: record.to_vec(),
        });
    }
    Ok(())
}

fn load_flow_resources(
    raw: &[u8],
    flow_table: &[(usize, usize)],
    sections: &[Kf8Section],
    resources: &mut Vec<SourceResource>,
    paths: &mut HashMap<ResourceKey, String>,
) -> Result<(), String> {
    let references = sections
        .iter()
        .flat_map(|section| find_resource_references(&section.html))
        .filter(|reference| reference.kind == ResourceKind::Flow)
        .collect::<HashSet<_>>();
    for reference in references {
        let (directory, extension, media_type) = match reference.mime.as_deref() {
            Some("image/svg+xml") => ("Images", "svg", "image/svg+xml"),
            Some("text/css") => ("Styles", "css", "text/css"),
            _ => continue,
        };
        let Some(&(start, end)) = flow_table.get(reference.id) else {
            continue;
        };
        let Some(data) = raw.get(start..end) else {
            return Err("KF8 flow resource points outside decompressed text".to_owned());
        };
        let path = format!("{directory}/flow-{}.{extension}", reference.id);
        paths.insert(ResourceKey::Flow(reference.id), path.clone());
        resources.push(SourceResource {
            path,
            media_type: media_type.to_owned(),
            bytes: data.to_vec(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ResourceKey {
    Embed(usize),
    Flow(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ResourceKind {
    Embed,
    Flow,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResourceReference {
    kind: ResourceKind,
    id: usize,
    mime: Option<String>,
}

fn find_resource_references(value: &str) -> Vec<ResourceReference> {
    let mut references = Vec::new();
    let mut search_from = 0usize;
    while let Some(relative) = value[search_from..].find("kindle:") {
        let start = search_from + relative;
        if let Some((_, reference)) = parse_resource_uri(value, start) {
            references.push(reference);
        }
        search_from = start + "kindle:".len();
    }
    references
}

fn replace_resource_uris(value: &str, paths: &HashMap<ResourceKey, String>) -> String {
    let mut output = String::with_capacity(value.len());
    let mut copied = 0usize;
    let mut search_from = 0usize;
    while let Some(relative) = value[search_from..].find("kindle:") {
        let start = search_from + relative;
        let Some((end, reference)) = parse_resource_uri(value, start) else {
            search_from = start + "kindle:".len();
            continue;
        };
        let key = match reference.kind {
            ResourceKind::Embed => ResourceKey::Embed(reference.id),
            ResourceKind::Flow => ResourceKey::Flow(reference.id),
        };
        if let Some(path) = paths.get(&key) {
            output.push_str(&value[copied..start]);
            output.push_str("../");
            output.push_str(path);
            copied = end;
        }
        search_from = end;
    }
    if copied == 0 {
        value.to_owned()
    } else {
        output.push_str(&value[copied..]);
        output
    }
}

fn parse_resource_uri(value: &str, start: usize) -> Option<(usize, ResourceReference)> {
    let suffix = value.get(start + "kindle:".len()..)?;
    let (kind, prefix_length) = if suffix.starts_with("embed:") {
        (ResourceKind::Embed, "embed:".len())
    } else if suffix.starts_with("flow:") {
        (ResourceKind::Flow, "flow:".len())
    } else {
        return None;
    };
    let id_start = start + "kindle:".len() + prefix_length;
    let id_end = value[id_start..]
        .find(|character: char| !character.is_ascii_alphanumeric())
        .map_or(value.len(), |offset| id_start + offset);
    if id_end == id_start {
        return None;
    }
    let id = usize::from_str_radix(value.get(id_start..id_end)?, 32).ok()?;
    let mut end = id_end;
    let mut mime = None;
    if value.get(id_end..)?.starts_with("?mime=") {
        let mime_start = id_end + "?mime=".len();
        end = value[mime_start..]
            .find(|character: char| {
                character.is_ascii_whitespace()
                    || matches!(character, '\'' | '"' | '<' | '>' | ')' | ']')
            })
            .map_or(value.len(), |offset| mime_start + offset);
        mime = value.get(mime_start..end).map(ToOwned::to_owned);
    }
    Some((end, ResourceReference { kind, id, mime }))
}

enum Decompressor {
    None,
    PalmDoc,
    Huff(Box<HuffDecoder>),
}

impl Decompressor {
    fn new(context: &Context<'_>) -> Result<Self, String> {
        match context.header.compression {
            1 => Ok(Self::None),
            2 => Ok(Self::PalmDoc),
            17_480 => Ok(Self::Huff(Box::new(HuffDecoder::new(context)?))),
            other => Err(format!("unsupported MOBI compression type {other}")),
        }
    }

    fn decompress(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            Self::None => Ok(data.to_vec()),
            Self::PalmDoc => decompress_palmdoc(data),
            Self::Huff(decoder) => decoder.decompress(data, 0),
        }
    }
}

fn decompress_palmdoc(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut index = 0usize;
    while index < data.len() {
        let byte = data[index];
        index += 1;
        match byte {
            0 => output.push(0),
            1..=8 => {
                let end = index + usize::from(byte);
                output.extend_from_slice(
                    data.get(index..end)
                        .ok_or_else(|| "truncated PalmDOC literal run".to_owned())?,
                );
                index = end;
            }
            9..=0x7f => output.push(byte),
            0x80..=0xbf => {
                let next = *data
                    .get(index)
                    .ok_or_else(|| "truncated PalmDOC back-reference".to_owned())?;
                index += 1;
                let pair = (u16::from(byte) << 8) | u16::from(next);
                let distance = usize::from((pair & 0x3fff) >> 3);
                let length = usize::from(pair & 7) + 3;
                if distance == 0 || distance > output.len() {
                    return Err("invalid PalmDOC back-reference".to_owned());
                }
                for _ in 0..length {
                    let value = output[output.len() - distance];
                    output.push(value);
                }
            }
            0xc0..=0xff => {
                output.push(b' ');
                output.push(byte ^ 0x80);
            }
        }
        if output.len() > MAX_UNCOMPRESSED_TEXT {
            return Err("PalmDOC record exceeds the text safety limit".to_owned());
        }
    }
    Ok(output)
}

struct HuffDecoder {
    table1: [(bool, usize, u32); 256],
    table2: Vec<(u32, u32)>,
    dictionary: Vec<DictionaryEntry>,
}

struct DictionaryEntry {
    data: Vec<u8>,
    decoded: bool,
}

impl HuffDecoder {
    fn new(context: &Context<'_>) -> Result<Self, String> {
        if context.header.huff_count < 2 {
            return Err("HUFF/CDIC compression has no dictionary records".to_owned());
        }
        let huff = context.relative_record(context.header.huff_start)?;
        if bytes(huff, 0, 4)? != b"HUFF" {
            return Err("invalid HUFF record".to_owned());
        }
        let table1_offset = usize_at_u32(huff, 8)?;
        let table2_offset = usize_at_u32(huff, 12)?;
        let mut table1 = [(false, 0, 0); 256];
        for (index, entry) in table1.iter_mut().enumerate() {
            let value = u32_at(huff, table1_offset + index * 4)?;
            *entry = (
                value & 0x80 != 0,
                usize::try_from(value & 0x1f).unwrap(),
                value >> 8,
            );
        }
        let mut table2 = vec![(0, 0); 33];
        for (index, entry) in table2.iter_mut().enumerate().skip(1) {
            let offset = table2_offset + (index - 1) * 8;
            *entry = (u32_at(huff, offset)?, u32_at(huff, offset + 4)?);
        }

        let mut dictionary = Vec::new();
        for record_index in 1..context.header.huff_count {
            let record = context.relative_record(context.header.huff_start + record_index)?;
            if bytes(record, 0, 4)? != b"CDIC" {
                return Err("invalid CDIC record".to_owned());
            }
            let header_length = usize_at_u32(record, 4)?;
            let total_entries = usize_at_u32(record, 8)?;
            let code_length = usize_at_u32(record, 12)?;
            if code_length >= usize::BITS as usize {
                return Err("invalid CDIC code length".to_owned());
            }
            let remaining = total_entries.saturating_sub(dictionary.len());
            let count = (1usize << code_length).min(remaining);
            let payload = record
                .get(header_length..)
                .ok_or_else(|| "truncated CDIC payload".to_owned())?;
            for entry_index in 0..count {
                let offset = usize_at_u16(payload, entry_index * 2)?;
                let descriptor = u16_at(payload, offset)?;
                let length = usize::from(descriptor & 0x7fff);
                let data = bytes(payload, offset + 2, length)?.to_vec();
                dictionary.push(DictionaryEntry {
                    data,
                    decoded: descriptor & 0x8000 != 0,
                });
            }
        }
        Ok(Self {
            table1,
            table2,
            dictionary,
        })
    }

    fn decompress(&mut self, data: &[u8], depth: usize) -> Result<Vec<u8>, String> {
        if depth > 64 {
            return Err("HUFF/CDIC dictionary recursion is too deep".to_owned());
        }
        let bit_length = data.len() * 8;
        let mut bit = 0usize;
        let mut output = Vec::new();
        while bit < bit_length {
            let bits = read_32_bits(data, bit);
            let (found, mut code_length, mut value) = self.table1[(bits >> 24) as usize];
            if !found {
                while code_length <= 32 && (bits >> (32 - code_length)) < self.table2[code_length].0
                {
                    code_length += 1;
                }
                if code_length > 32 {
                    return Err("invalid HUFF code".to_owned());
                }
                value = self.table2[code_length].1;
            }
            bit += code_length;
            if bit > bit_length {
                break;
            }
            let prefix = bits >> (32 - code_length);
            let code = value
                .checked_sub(prefix)
                .ok_or_else(|| "invalid HUFF dictionary code".to_owned())?
                as usize;
            let entry = self
                .dictionary
                .get(code)
                .ok_or_else(|| "HUFF dictionary code is out of bounds".to_owned())?;
            let (entry_data, decoded) = (entry.data.clone(), entry.decoded);
            let expanded = if decoded {
                entry_data
            } else {
                let expanded = self.decompress(&entry_data, depth + 1)?;
                let entry = &mut self.dictionary[code];
                entry.data.clone_from(&expanded);
                entry.decoded = true;
                expanded
            };
            output.extend_from_slice(&expanded);
            if output.len() > MAX_UNCOMPRESSED_TEXT {
                return Err("HUFF/CDIC output exceeds the text safety limit".to_owned());
            }
        }
        Ok(output)
    }
}

fn read_32_bits(data: &[u8], bit: usize) -> u32 {
    let start = bit / 8;
    let mut value = 0u64;
    for index in start..start + 5 {
        value = (value << 8) | u64::from(data.get(index).copied().unwrap_or(0));
    }
    let shift = 8 - (bit & 7);
    u32::try_from((value >> shift) & u64::from(u32::MAX))
        .expect("the HUFF bit window is masked to 32 bits")
}

fn remove_trailing_entries(data: &[u8], flags: u32) -> Result<&[u8], String> {
    let mut data = data;
    for _ in 0..(flags >> 1).count_ones() {
        let length = variable_length_from_end(data);
        if length == 0 || length > data.len() {
            return Err("invalid trailing MOBI entry".to_owned());
        }
        data = &data[..data.len() - length];
    }
    if flags & 1 != 0 {
        let last = *data
            .last()
            .ok_or_else(|| "missing MOBI multibyte trailer".to_owned())?;
        let length = usize::from(last & 3) + 1;
        if length > data.len() {
            return Err("invalid MOBI multibyte trailer".to_owned());
        }
        data = &data[..data.len() - length];
    }
    Ok(data)
}

fn variable_length_from_end(data: &[u8]) -> usize {
    let mut value = 0usize;
    for &byte in data.iter().skip(data.len().saturating_sub(4)) {
        if byte & 0x80 != 0 {
            value = 0;
        }
        value = (value << 7) | usize::from(byte & 0x7f);
    }
    value
}

fn variable_length(data: &[u8], start: usize) -> Result<(usize, usize), String> {
    if start >= data.len() {
        return Err(format!(
            "truncated variable-length integer at byte {start} of {}",
            data.len()
        ));
    }
    let mut value = 0usize;
    let available = (data.len() - start).min(4);
    for length in 1..=available {
        let byte = data[start + length - 1];
        value = value
            .checked_shl(7)
            .and_then(|value| value.checked_add(usize::from(byte & 0x7f)))
            .ok_or_else(|| "variable-length integer overflow".to_owned())?;
        if byte & 0x80 != 0 {
            return Ok((value, length));
        }
    }
    Ok((value, available))
}

fn valid_index(value: u32) -> Result<usize, String> {
    if value == INVALID_INDEX {
        Err("required KF8 index is missing".to_owned())
    } else {
        usize::try_from(value).map_err(|_| "KF8 index does not fit this platform".to_owned())
    }
}

fn decode_text(data: &[u8], encoding: u32) -> String {
    if encoding == 1252 {
        WINDOWS_1252.decode(data).0.into_owned()
    } else {
        String::from_utf8_lossy(data).into_owned()
    }
}

fn extract_document_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    for tag in ["title", "h1", "h2"] {
        let Some(open) = lower.find(&format!("<{tag}")) else {
            continue;
        };
        let Some(relative_start) = lower[open..].find('>') else {
            continue;
        };
        let content_start = relative_start + open + 1;
        let Some(relative_close) = lower[content_start..].find(&format!("</{tag}>")) else {
            continue;
        };
        let close = relative_close + content_start;
        let title = strip_tags(html.get(content_start..close)?);
        if !title.trim().is_empty() {
            return Some(title.trim().to_owned());
        }
    }
    None
}

fn strip_tags(value: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn image_type(data: &[u8]) -> Option<(&'static str, &'static str)> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("png", "image/png"))
    } else if data.starts_with(b"\xff\xd8\xff") {
        Some(("jpg", "image/jpeg"))
    } else if data.starts_with(b"GIF8") {
        Some(("gif", "image/gif"))
    } else if data.starts_with(b"BM") {
        Some(("bmp", "image/bmp"))
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

fn bytes(data: &[u8], offset: usize, length: usize) -> Result<&[u8], String> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| "binary range overflow".to_owned())?;
    data.get(offset..end)
        .ok_or_else(|| "truncated binary structure".to_owned())
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, String> {
    let value: [u8; 2] = bytes(data, offset, 2)?
        .try_into()
        .map_err(|_| "invalid 16-bit integer".to_owned())?;
    Ok(u16::from_be_bytes(value))
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32, String> {
    let value: [u8; 4] = bytes(data, offset, 4)?
        .try_into()
        .map_err(|_| "invalid 32-bit integer".to_owned())?;
    Ok(u32::from_be_bytes(value))
}

fn optional_u32(data: &[u8], offset: usize) -> Option<u32> {
    u32_at(data, offset).ok()
}

fn usize_at_u16(data: &[u8], offset: usize) -> Result<usize, String> {
    Ok(usize::from(u16_at(data, offset)?))
}

fn usize_at_u32(data: &[u8], offset: usize) -> Result<usize, String> {
    usize::try_from(u32_at(data, offset)?)
        .map_err(|_| "32-bit value does not fit this platform".to_owned())
}

fn uint_from_bytes(data: &[u8]) -> Result<usize, String> {
    if data.is_empty() || data.len() > 4 {
        return Err("invalid big-endian integer width".to_owned());
    }
    data.iter().try_fold(0usize, |value, byte| {
        value
            .checked_shl(8)
            .and_then(|value| value.checked_add(usize::from(*byte)))
            .ok_or_else(|| "big-endian integer overflow".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_flows_are_loaded_and_references_keep_publication_relative_paths() {
        let css = b".cell { text-align:right; background:url(kindle:embed:00001?mime=image/png) }";
        let sections = [Kf8Section { title: "Chapter".into(), html: "<html><head><link rel='stylesheet' href='kindle:flow:00001?mime=text/css'/></head><body><p>Text</p></body></html>".into(), source_index: 0 }];
        let mut resources = Vec::new();
        let mut paths = HashMap::from([(ResourceKey::Embed(1), "Images/photo.png".into())]);
        load_flow_resources(
            css,
            &[(0, 0), (0, css.len())],
            &sections,
            &mut resources,
            &mut paths,
        )
        .unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].media_type, "text/css");
        assert_eq!(resources[0].path, "Styles/flow-1.css");
        assert!(replace_resource_uris(&sections[0].html, &paths).contains("../Styles/flow-1.css"));
        assert!(
            replace_resource_uris(std::str::from_utf8(&resources[0].bytes).unwrap(), &paths)
                .contains("../Images/photo.png")
        );
    }

    #[test]
    fn decompresses_palmdoc_literals_back_references_and_spaces() {
        let compressed = [3, b'a', b'b', b'c', 0x80, 0x18, 0xc1];
        assert_eq!(decompress_palmdoc(&compressed).unwrap(), b"abcabc A");
    }

    #[test]
    fn parses_and_replaces_kindle_resource_uris() {
        let source = r#"<img src="kindle:embed:0000A?mime=image/jpeg"/>"#;
        let reference = find_resource_references(source).pop().unwrap();
        assert_eq!(reference.kind, ResourceKind::Embed);
        assert_eq!(reference.id, 10);
        assert_eq!(reference.mime.as_deref(), Some("image/jpeg"));
        let paths = HashMap::from([(ResourceKey::Embed(10), "Images/kindle-10.jpg".to_owned())]);
        assert_eq!(
            replace_resource_uris(source, &paths),
            r#"<img src="../Images/kindle-10.jpg"/>"#
        );
    }

    #[test]
    fn distinguishes_navigation_metadata_from_an_authored_contents_page() {
        assert!(is_navigation_document(
            r#"<html><body><nav epub:type="toc"><ol><li>One</li></ol></nav></body></html>"#
        ));
        assert!(!is_navigation_document(
            r"<html><body><h2>Table of Contents</h2><ul><li>One</li></ul></body></html>"
        ));
    }
}
