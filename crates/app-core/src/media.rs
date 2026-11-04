//! Bounded source-owned figure/table/equation reference targets shared by hosts.
use rebook_publication::{
    media_references::{self, MediaKey, MediaKind, MediaReference},
    *,
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};
const BUDGET: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub section: usize,
    pub block: usize,
    pub images: Vec<usize>,
    pub captions: Vec<usize>,
    pub group: Option<usize>,
    pub key: MediaKey,
}

impl Target {
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.key.number.len()
            + (self.images.len() + self.captions.len()) * std::mem::size_of::<usize>()
    }
}

pub fn plain(text: &TextBlock) -> String {
    text.content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|run| run.text.as_str())
        .collect()
}

pub fn caption_text(text: &TextBlock) -> String {
    text.content
        .iter()
        .map(|inline| match inline {
            Inline::Math(math) => math
                .original_text()
                .unwrap_or_else(|| format!("\\({}\\)", math.latex)),
            Inline::Image(image) => image
                .image
                .formula
                .as_ref()
                .map(|formula| format!("\\({}\\)", formula.latex))
                .unwrap_or_else(|| {
                    let alt = &image.image.alt;
                    if [".png", ".jpg", ".jpeg", ".svg", ".gif", ".webp"]
                        .iter()
                        .any(|suffix| alt.to_ascii_lowercase().ends_with(suffix))
                    {
                        "□".into()
                    } else {
                        alt.clone()
                    }
                }),
            _ => inline
                .text_runs()
                .iter()
                .map(|run| run.text.as_str())
                .collect(),
        })
        .collect()
}

pub fn targets(section: &Section, index: usize) -> Vec<Target> {
    let mut result = Vec::new();
    for (at, block) in section.blocks.iter().enumerate() {
        if let Some(key) = media_references::equation_target(block) {
            result.push(Target {
                section: index,
                block: at,
                images: vec![],
                captions: vec![],
                group: None,
                key,
            });
            continue;
        }
        let mut captions = Vec::new();
        let mut images = Vec::new();
        let (kind, texts): (Option<MediaKind>, Vec<&TextBlock>) = match block {
            // Some books publish their numbered tables as images.
            Block::Figure(figure) => {
                captions.extend(adjacent_captions(section, at));
                (
                    None,
                    figure
                        .text_blocks()
                        .filter(|text| text.kind == rebook_publication::TextBlockKind::Caption)
                        .collect(),
                )
            }
            Block::Table(table) => {
                let mut texts: Vec<_> = table.before.iter().chain(&table.after).collect();
                if let Some(Block::Text(text)) =
                    at.checked_sub(1).and_then(|i| section.blocks.get(i))
                    && media_references::caption_label(&plain(text))
                        .is_some_and(|label| label.key.kind == MediaKind::Table)
                {
                    captions.push(at - 1);
                    texts.push(text);
                }
                for (next, adjacent) in section.blocks.iter().enumerate().skip(at + 1) {
                    let Block::Text(text) = adjacent else { break };
                    let value = plain(text);
                    let lower = value.trim_start().to_ascii_lowercase();
                    if [
                        "note:",
                        "notes:",
                        "source:",
                        "sources:",
                        "注：",
                        "注:",
                        "来源：",
                        "来源:",
                    ]
                    .iter()
                    .any(|prefix| lower.starts_with(prefix))
                    {
                        captions.push(next);
                    } else {
                        break;
                    }
                }
                (Some(MediaKind::Table), texts)
            }
            Block::Text(text) if media_references::caption_label(&plain(text)).is_some() => {
                // A label alone (e.g. a list of illustrations) is never a target.
                captions.push(at);
                let mut start = at;
                while start > 0 && matches!(section.blocks[start - 1], Block::Image(_)) {
                    start -= 1;
                }
                let mut end = at + 1;
                while end < section.blocks.len() && matches!(section.blocks[end], Block::Image(_)) {
                    end += 1;
                }
                images.extend(start..at);
                if images.is_empty() {
                    images.extend(at + 1..end);
                }
                if images.is_empty() {
                    continue;
                }
                for image in &images {
                    captions.extend(adjacent_captions(section, *image));
                }
                captions.extend(adjacent_captions(section, at));
                captions.sort_unstable();
                captions.dedup();
                (None, vec![text])
            }
            _ => continue,
        };
        for text in texts {
            let Some(label) = media_references::caption_label(&plain(text)) else {
                continue;
            };
            if kind.is_some_and(|kind| label.key.kind != kind) {
                continue;
            }
            let target = Target {
                section: index,
                block: at,
                images: images.clone(),
                captions: captions.clone(),
                group: None,
                key: label.key,
            };
            if !result.contains(&target) {
                result.push(target);
            }
        }
    }
    // Publishers sometimes split (a), (b), ... into adjacent figure elements,
    // with the number and page anchor present only in the first element.
    // Require an explicit, consecutive caption sequence; never infer from class names.
    let mut group: Option<(usize, MediaKey, u8)> = None;
    for (at, block) in section.blocks.iter().enumerate() {
        let Block::Figure(figure) = block else {
            group = None;
            continue;
        };
        let numbered = figure
            .caption_blocks()
            .find_map(|text| media_references::caption_label(&plain(text)));
        let mut letters = figure
            .caption_blocks()
            .filter_map(|text| subfigure_letter(&plain(text)));
        let letter = letters
            .next()
            .filter(|first| letters.all(|letter| letter == *first));
        if let Some(label) = numbered {
            group = None;
            if label.key.kind == MediaKind::Figure
                && letter == Some(b'a')
                && !label
                    .key
                    .number
                    .ends_with(|c: char| c.is_ascii_alphabetic())
            {
                let mut key = label.key.clone();
                key.number.push('a');
                result.push(Target {
                    section: index,
                    block: at,
                    images: vec![],
                    captions: vec![],
                    group: None,
                    key,
                });
                group = Some((at, label.key, b'b'));
            }
        } else if let Some((root, key, next)) = group.as_mut() {
            if letter == Some(*next) && *next <= b'z' {
                let mut key = key.clone();
                key.number.push(char::from(*next));
                result.push(Target {
                    section: index,
                    block: at,
                    images: vec![],
                    captions: vec![],
                    group: Some(*root),
                    key,
                });
                *next += 1;
            } else {
                group = None;
            }
        }
    }
    result
}

// Collect only explicit adjacent caption/source semantics; prose is a boundary.
pub fn adjacent_captions(section: &Section, at: usize) -> Vec<usize> {
    let eligible = |block: &Block| {
        matches!(block, Block::Text(text)
        if text.kind == rebook_publication::TextBlockKind::Caption || rebook_layout::is_media_source(text))
    };
    let mut media_start = at;
    let mut media_end = at + 1;
    if matches!(section.blocks[at], Block::Image(_)) {
        while media_start > 0 && matches!(section.blocks[media_start - 1], Block::Image(_)) {
            media_start -= 1;
        }
        while media_end < section.blocks.len()
            && matches!(section.blocks[media_end], Block::Image(_))
        {
            media_end += 1;
        }
    }
    let mut start = media_start;
    while start > 0 && eligible(&section.blocks[start - 1]) {
        start -= 1;
    }
    let mut end = media_end;
    while end < section.blocks.len() && eligible(&section.blocks[end]) {
        end += 1;
    }
    (start..media_start).chain(media_end..end).collect()
}

pub fn image_target(
    section: &Section,
    index: usize,
    href: &PublicationUrl,
    source: Option<&SourceRange>,
) -> Option<Target> {
    fn matches(block: &Block, href: &PublicationUrl, source: Option<&SourceRange>) -> bool {
        let image_matches = |image: &rebook_publication::ImageBlock| {
            &image.href == href
                && source.is_none_or(|source| {
                    image
                        .source
                        .as_ref()
                        .is_some_and(|range| range.start.node == source.start.node)
                })
        };
        match block {
            Block::Image(image) => image_matches(image),
            Block::Figure(figure) => {
                figure.images.iter().any(image_matches)
                    || figure.body.iter().any(|block| matches(block, href, source))
            }
            _ => false,
        }
    }
    let mut candidates = section
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| matches(block, href, source));
    let (at, _) = candidates.next()?;
    // Without an authored identity, repeated logos/photos must not borrow another caption.
    if candidates.next().is_some() {
        return None;
    }
    if let Some(target) = targets(section, index)
        .into_iter()
        .find(|target| target.block == at || target.images.contains(&at))
    {
        return Some(target);
    }
    let captions = adjacent_captions(section, at);
    let images = if matches!(section.blocks[at], Block::Image(_)) {
        vec![at]
    } else {
        vec![]
    };
    Some(Target {
        section: index,
        block: at,
        images,
        captions,
        group: None,
        key: MediaKey {
            kind: MediaKind::Figure,
            number: String::new(),
        },
    })
}

pub fn subfigure_letter(value: &str) -> Option<u8> {
    let value = value.trim_start();
    let value = value
        .strip_prefix('(')
        .or_else(|| value.strip_prefix('（'))
        .unwrap_or(value);
    let mut chars = value.chars();
    let letter = chars.next()?.to_ascii_lowercase();
    if !letter.is_ascii_lowercase() || !matches!(chars.next()?, ')' | '）' | '.') {
        return None;
    }
    chars
        .next()
        .is_none_or(char::is_whitespace)
        .then_some(letter as u8)
}

pub fn text_blocks(block: &Block) -> Vec<&TextBlock> {
    match block {
        Block::Text(t) => vec![t],
        Block::Figure(f) => f.text_blocks().collect(),
        Block::Table(t) => t.text_blocks().collect(),
        Block::Quote(q) => q.body.iter().chain(&q.attribution).collect(),
        Block::Note(n) => n.blocks.iter().flat_map(text_blocks).collect(),
        _ => vec![],
    }
}

pub fn authored_link(
    section: &Section,
    origin: &SourceRange,
    key: &MediaKey,
) -> Result<Option<PublicationUrl>, String> {
    let mut found = None;
    for text in section.blocks.iter().flat_map(text_blocks) {
        if !text
            .source
            .as_ref()
            .is_some_and(|s| s.start.node == origin.start.node)
        {
            continue;
        }
        let value = plain(text);
        // Authored source offsets count text scalars, without image placeholders.
        let content: Vec<_> = text
            .content
            .iter()
            .filter(|inline| matches!(inline, Inline::Text(_)))
            .cloned()
            .collect();
        for reference in media_references::inline_references(&content)
            .into_iter()
            .filter(|r| &r.key == key)
        {
            let base = text.source.as_ref().unwrap().start.text_offset;
            let start = base + value[..reference.range.start].chars().count() as u64;
            let end = base + value[..reference.range.end].chars().count() as u64;
            if end <= origin.start.text_offset || start >= origin.end.text_offset {
                continue;
            }
            let mut offset = 0;
            for run in text.content.iter().flat_map(Inline::text_runs) {
                let end = offset + run.text.len();
                if offset < reference.range.end
                    && end > reference.range.start
                    && let Some(href) = run
                        .link
                        .as_ref()
                        .filter(|href| href.website_url().is_none())
                {
                    if found.as_ref().is_some_and(|previous| previous != href) {
                        return Err("This reference has several different authored targets.".into());
                    }
                    found = Some(href.clone());
                }
                offset = end;
            }
        }
    }
    Ok(found)
}

pub fn target_has_anchor(section: &Section, target: &Target, href: &PublicationUrl) -> bool {
    let Some(fragment) = href.fragment() else {
        return false;
    };
    let Some(anchor) = section.anchors.iter().find(|a| a.fragment == fragment) else {
        return false;
    };
    target_has_node(section, target, &anchor.source.node)
}

pub fn target_has_node(section: &Section, target: &Target, node: &str) -> bool {
    let mut indices = vec![target.block];
    indices.extend(target.group);
    indices.extend(&target.images);
    indices.extend(&target.captions);
    indices.into_iter().any(|index| {
        let block = &section.blocks[index];
        let mut sources: Vec<_> = text_blocks(block)
            .into_iter()
            .filter_map(|t| t.source.as_ref())
            .collect();
        match block {
            Block::Image(i) => sources.extend(i.source.as_ref()),
            Block::Figure(f) => {
                sources.extend(f.source.as_ref());
                sources.extend(f.images.iter().filter_map(|i| i.source.as_ref()));
            }
            Block::Table(t) => sources.extend(t.source.as_ref()),
            _ => {}
        }
        sources.iter().any(|s| s.start.node == node)
    })
}

pub fn matching(target: &Target, key: &MediaKey) -> bool {
    target.key == *key
        || (target.key.kind == key.kind
            && key.number.strip_suffix(|c: char| c.is_ascii_alphabetic())
                == Some(target.key.number.as_str()))
}

pub fn unique(candidates: impl Iterator<Item = Target>) -> Result<Option<Target>, String> {
    let mut candidates = candidates.collect::<Vec<_>>();
    // Multiple subfigure labels can describe the same complete figure.
    candidates.dedup_by(|a, b| a.section == b.section && a.block == b.block);
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err("This figure/table number has several possible targets in this book.".into()),
    }
}

pub fn select(
    candidates: impl Iterator<Item = Target>,
    key: &MediaKey,
) -> Result<Option<Target>, String> {
    let matches = candidates
        .filter(|target| matching(target, key))
        .collect::<Vec<_>>();
    if matches.iter().any(|t| t.key == *key) {
        unique(matches.into_iter().filter(|t| t.key == *key))
    } else {
        unique(matches.into_iter())
    }
}

pub fn chapter_scope(source: &dyn BookSource, origin: usize) -> Range<usize> {
    fn collect(
        entries: &[rebook_publication::TocEntry],
        source: &dyn BookSource,
        starts: &mut Vec<usize>,
    ) {
        for entry in entries {
            let label = entry.label.to_ascii_lowercase();
            if (label.contains("chapter") || (label.contains('第') && label.contains('章')))
                && let Some(href) = &entry.href
                && let Some(index) = source
                    .book()
                    .sections
                    .iter()
                    .position(|s| s.href.path() == href.path())
            {
                starts.push(index);
            }
            collect(&entry.children, source, starts);
        }
    }
    let mut starts = vec![];
    collect(&source.book().table_of_contents, source, &mut starts);
    starts.sort_unstable();
    starts.dedup();
    let Some(start) = starts.iter().copied().take_while(|s| *s <= origin).last() else {
        return origin..origin + 1;
    };
    start
        ..starts
            .iter()
            .copied()
            .find(|s| *s > origin)
            .unwrap_or(source.book().sections.len())
}

pub struct Entry {
    pub target: Target,
    pub node: Option<String>,
    pub anchors: Vec<String>,
}

#[derive(Default)]
pub struct Catalog {
    pub has_references: bool,
    pub revision: Option<u64>,
    pub entries: Vec<Entry>,
    pub by_key: HashMap<MediaKey, Vec<usize>>,
    // Authored links survive translation and popup flattening through source identity.
    pub links: HashMap<(usize, String), Vec<(MediaKey, PublicationUrl)>>,
    // Existing non-media anchors used by references. Keep only exceptional links,
    // rather than retaining every paragraph/page anchor in the book.
    pub document_links: HashSet<PublicationUrl>,
}

pub fn href_for(
    content: &[Inline],
    reference: &MediaReference,
) -> Result<Option<PublicationUrl>, ()> {
    let mut offset = 0;
    let mut found = None;
    for inline in content {
        if let Inline::Text(run) = inline {
            let end = offset + run.text.len();
            if offset < reference.range.end
                && end > reference.range.start
                && let Some(href) = run
                    .link
                    .as_ref()
                    .filter(|href| href.website_url().is_none())
            {
                if found.as_ref().is_some_and(|previous| previous != href) {
                    return Err(());
                }
                found = Some(href.clone());
            }
            offset = end;
        } else {
            offset += 1;
        }
    }
    Ok(found)
}

impl Catalog {
    pub fn build(source: &dyn BookSource, needed: impl Fn() -> bool) -> Result<Self, String> {
        let mut catalog = Self {
            revision: source.content_revision(),
            ..Self::default()
        };
        let mut bytes = 0;
        for index in 0..source.book().sections.len() {
            if !needed() {
                return Err("cancelled".into());
            }
            let section = source.parse_section(index).map_err(|e| e.to_string())?;
            for target in crate::media::targets(&section, index) {
                let anchors: Vec<_> = section
                    .anchors
                    .iter()
                    .filter(|anchor| {
                        let mut href = source.book().sections[index].href.clone();
                        // PublicationUrl owns canonical escaping and resource identity.
                        if let Ok(with_fragment) =
                            PublicationUrl::parse(&format!("{}#{}", href.path(), anchor.fragment))
                        {
                            href = with_fragment;
                        }
                        crate::media::target_has_anchor(&section, &target, &href)
                    })
                    .map(|anchor| anchor.fragment.clone())
                    .collect();
                let block = &section.blocks[target.block];
                let source = match block {
                    Block::Image(image) => image.source.as_ref(),
                    Block::Figure(figure) => figure.source.as_ref(),
                    Block::Table(table) => table.source.as_ref(),
                    _ => None,
                }
                .or_else(|| block.text_blocks().find_map(|text| text.source.as_ref()));
                let node = source.map(|source| source.start.node.clone());
                bytes += std::mem::size_of::<Entry>()
                    + target.retained_bytes()
                    + node.as_ref().map_or(0, String::len)
                    + anchors.iter().map(|a| a.len() + 24).sum::<usize>()
                    + 128;
                if bytes > BUDGET {
                    return Err("media catalog budget exceeded".into());
                }
                bytes += target.key.number.len() + 64;
                if bytes > BUDGET {
                    return Err("media catalog budget exceeded".into());
                }
                catalog
                    .by_key
                    .entry(target.key.clone())
                    .or_default()
                    .push(catalog.entries.len());
                catalog.entries.push(Entry {
                    target,
                    anchors,
                    node,
                });
            }
            for text in section.blocks.iter().flat_map(Block::text_blocks) {
                let Some(origin) = &text.source else {
                    continue;
                };
                for reference in media_references::inline_references(&text.content) {
                    catalog.has_references |= text.kind != TextBlockKind::Caption;
                    if let Ok(Some(href)) = href_for(&text.content, &reference) {
                        bytes += origin.start.node.len()
                            + reference.key.number.len()
                            + href.path().len()
                            + href.fragment().map_or(0, str::len)
                            + 160;
                        if bytes > BUDGET {
                            return Err("media catalog budget exceeded".into());
                        }
                        catalog
                            .links
                            .entry((index, origin.start.node.clone()))
                            .or_default()
                            .push((reference.key, href));
                    }
                }
            }
            // No section, image bytes, or layouts are retained by the catalog.
        }
        let mut pending = HashSet::new();
        for (key, href) in catalog.links.values().flatten() {
            let Some(fragment) = href.fragment() else {
                continue;
            };
            if pending.contains(href)
                || catalog.entries.iter().any(|entry| {
                    source.book().sections[entry.target.section].href.path() == href.path()
                        && entry.anchors.iter().any(|anchor| anchor == fragment)
                })
            {
                continue;
            }
            let candidates = catalog
                .key_entries(key)
                .into_iter()
                .filter(|entry| {
                    source.book().sections[entry.target.section].href.path() == href.path()
                })
                .map(|entry| entry.target.clone());
            if !matches!(crate::media::select(candidates, key), Ok(Some(_))) {
                continue;
            }
            bytes += href.path().len() + fragment.len() + 128;
            if bytes > BUDGET {
                return Err("media catalog budget exceeded".into());
            }
            pending.insert(href.clone());
        }
        // Validate only the documents whose otherwise unique media references
        // missed a media anchor. This exceptional pass stays on the catalog worker.
        for (index, descriptor) in source.book().sections.iter().enumerate() {
            if !needed() {
                return Err("cancelled".into());
            }
            let links: Vec<_> = pending
                .extract_if(|href| href.path() == descriptor.href.path())
                .collect();
            if links.is_empty() {
                continue;
            }
            let section = source.parse_section(index).map_err(|e| e.to_string())?;
            for href in links {
                if section
                    .anchors
                    .iter()
                    .any(|anchor| Some(anchor.fragment.as_str()) == href.fragment())
                {
                    catalog.document_links.insert(href);
                }
            }
        }
        Ok(catalog)
    }

    fn key_entries(&self, key: &MediaKey) -> Vec<&Entry> {
        let fallback = key
            .number
            .strip_suffix(|c: char| c.is_ascii_alphabetic())
            .and_then(|number| {
                self.by_key.get(&MediaKey {
                    kind: key.kind,
                    number: number.into(),
                })
            });
        self.by_key
            .get(key)
            .into_iter()
            .chain(fallback)
            .flatten()
            .map(|index| &self.entries[*index])
            .collect()
    }

    pub fn authored(
        &self,
        origin: usize,
        node: &str,
        key: &MediaKey,
    ) -> Result<Option<PublicationUrl>, ()> {
        let mut found = None;
        if let Some(links) = self.links.get(&(origin, node.to_owned())) {
            for (_, href) in links.iter().filter(|(k, _)| k == key) {
                if found.as_ref().is_some_and(|previous| previous != href) {
                    return Err(());
                }
                found = Some(href.clone());
            }
        }
        Ok(found)
    }

    pub fn resolve(
        &self,
        source: &dyn BookSource,
        origin: usize,
        key: &MediaKey,
        href: Option<&PublicationUrl>,
    ) -> Option<Target> {
        // Exact/subfigure preference is applied inside each document/chapter
        // scope, rather than letting a remote exact label defeat a local group.
        let entries = self.key_entries(key);
        let select = |entries: Vec<&Entry>| {
            crate::media::select(entries.into_iter().map(|entry| entry.target.clone()), key)
        };
        if let Some(href) = href {
            let document: Vec<_> = entries
                .iter()
                .copied()
                .filter(|entry| {
                    source.book().sections[entry.target.section].href.path() == href.path()
                })
                .collect();
            let exact = select(
                document
                    .iter()
                    .copied()
                    .filter(|entry| {
                        href.fragment().is_some_and(|fragment| {
                            entry.anchors.iter().any(|anchor| anchor == fragment)
                        })
                    })
                    .collect(),
            );
            match exact {
                Ok(Some(target)) => return Some(target),
                Err(_) => return None,
                _ => {}
            }
            return (href.fragment().is_none() || self.document_links.contains(href))
                .then(|| select(document).ok().flatten())
                .flatten();
        }
        let path = source.book().sections.get(origin)?.href.path();
        let local = select(
            entries
                .iter()
                .copied()
                .filter(|entry| source.book().sections[entry.target.section].href.path() == path)
                .collect(),
        );
        match local {
            Ok(Some(target)) => return Some(target),
            Err(_) => return None,
            _ => {}
        }
        let scope = crate::media::chapter_scope(source, origin);
        let chapter = select(
            entries
                .iter()
                .copied()
                .filter(|entry| scope.contains(&entry.target.section))
                .collect(),
        );
        match chapter {
            Ok(Some(target)) => return Some(target),
            Err(_) => return None,
            _ => {}
        }
        select(entries).ok().flatten()
    }
}

pub fn apply_links(
    original: &dyn BookSource,
    catalog: Option<&Catalog>,
    content: &mut Vec<Inline>,
    origin: Option<&SourceRange>,
) {
    let index = origin.and_then(|s| {
        original
            .book()
            .sections
            .iter()
            .position(|section| section.id == s.start.spine)
    });
    let refs = media_references::inline_references(content);
    if refs.is_empty()
        && !content
            .iter()
            .flat_map(Inline::text_runs)
            .any(|run| run.style.inline_citation != 0)
    {
        return;
    }
    let valid: Vec<_> = refs
        .iter()
        .filter_map(|reference| {
            let (Some(catalog), Some(index), Some(origin)) = (
                catalog.filter(|catalog| catalog.revision == original.content_revision()),
                index,
                origin,
            ) else {
                return None;
            };
            let href = href_for(content, reference).and_then(|href| {
                if href.is_some() {
                    Ok(href)
                } else {
                    catalog.authored(index, &origin.start.node, &reference.key)
                }
            });
            let href = href.ok()?;
            catalog.resolve(original, index, &reference.key, href.as_ref())?;
            Some((reference, href))
        })
        .collect();
    let mut groups: HashMap<u32, (String, std::ops::Range<usize>)> = HashMap::new();
    let mut offset = 0;
    for inline in content.iter() {
        if let Inline::Text(run) = inline {
            if run.style.inline_citation != 0 {
                let group = groups
                    .entry(run.style.inline_citation)
                    .or_insert_with(|| (String::new(), offset..offset));
                group.0.push_str(&run.text);
                group.1.end = offset + run.text.len();
            }
            offset += run.text.len();
        } else {
            offset += 1;
        }
    }
    let restore: Vec<_> = groups
        .into_iter()
        .filter(|(_, (text, range))| {
            media_references::is_citation(text)
                && !valid
                    .iter()
                    .any(|(r, _)| r.range.start < range.end && r.range.end > range.start)
        })
        .map(|(id, _)| id)
        .collect();
    offset = 0;
    let mut result = Vec::with_capacity(content.len());
    for inline in std::mem::take(content) {
        let Inline::Text(run) = inline else {
            offset += 1;
            result.push(inline);
            continue;
        };
        let len = run.text.len();
        let mut cuts = vec![0, len];
        for reference in &refs {
            for at in [reference.range.start, reference.range.end] {
                if at > offset && at < offset + len {
                    cuts.push(at - offset);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let mut part = run.clone();
            part.text = run.text[pair[0]..pair[1]].into();
            if restore.contains(&part.style.inline_citation) {
                part.style.inline_citation = 0;
            }
            if refs
                .iter()
                .any(|r| r.range.start <= offset + pair[0] && r.range.end >= offset + pair[1])
            {
                let target = valid.iter().find(|(r, _)| {
                    r.range.start <= offset + pair[0] && r.range.end >= offset + pair[1]
                });
                part.style.suppress_media_references = target.is_none();
                // Unified layout applies the final clickable brush. Original
                // book layout must not acquire media links from this overlay.
                part.style.media_reference = None;
                if let Some((_, href)) = target {
                    if part.link.is_none() {
                        part.link = href.clone();
                    }
                }
            }
            result.push(Inline::Text(part));
        }
        offset += len;
    }
    *content = result;
}

/// Metadata catalog preparation is explicit worker work. Shaping only reads an
/// immutable snapshot; no UI hit test parses or scans the book.
pub struct MediaReferenceSource {
    inner: Arc<dyn BookSource>,
    original: Arc<dyn BookSource>,
    catalog: RwLock<Option<Arc<Catalog>>>,
    revision: AtomicU64,
}
impl MediaReferenceSource {
    pub fn new(inner: Arc<dyn BookSource>, original: Arc<dyn BookSource>) -> Self {
        Self {
            inner,
            original,
            catalog: RwLock::new(None),
            revision: AtomicU64::new(0),
        }
    }
    pub fn prepare(&self) -> Result<bool, String> {
        if self.original.book().metadata.layout == RenditionLayout::PrePaginated {
            return Ok(false);
        }
        let revision = self.original.content_revision();
        if self
            .catalog
            .read()
            .map_err(|_| "media catalog lock")?
            .as_ref()
            .is_some_and(|c| c.revision == revision)
        {
            return Ok(false);
        }
        let catalog = Catalog::build(self.original.as_ref(), || {
            self.original.content_revision() == revision
        })?;
        if self.original.content_revision() != revision {
            return Ok(false);
        }
        let changed = catalog.has_references && !catalog.entries.is_empty();
        *self.catalog.write().map_err(|_| "media catalog lock")? = Some(Arc::new(catalog));
        if changed {
            self.revision.fetch_add(1, Ordering::Release);
        }
        Ok(changed)
    }
    pub fn resolve(&self, origin: &SourceRange, key: &MediaKey) -> Option<Target> {
        let index = self
            .book()
            .sections
            .iter()
            .position(|s| s.id == origin.start.spine)?;
        let snapshot = self.catalog.read().ok()?.clone()?;
        if snapshot.revision != self.original.content_revision() {
            return None;
        }
        let href = snapshot.authored(index, &origin.start.node, key).ok()?;
        snapshot.resolve(self.original.as_ref(), index, key, href.as_ref())
    }
    pub fn preview(&self, target: &Target) -> Result<Vec<Block>, String> {
        let original = self
            .original
            .parse_section(target.section)
            .map_err(|e| e.to_string())?;
        let current = self
            .inner
            .parse_section(target.section)
            .map_err(|e| e.to_string())?;
        let node = original
            .blocks
            .get(target.block)
            .and_then(|b| b.text_blocks().find_map(|t| t.source.as_ref()))
            .map(|s| s.start.node.as_str());
        let candidates = targets(&current, target.section).into_iter().filter(|t| {
            t.key == target.key && node.is_none_or(|node| target_has_node(&current, t, node))
        });
        if let Ok(Some(current_target)) = select(candidates, &target.key) {
            return preview_blocks(&current, &current_target);
        }
        preview_blocks(&original, target)
    }
    fn apply(&self, content: &mut Vec<Inline>, origin: Option<&SourceRange>) {
        let snapshot = self.catalog.read().ok().and_then(|s| s.clone());
        apply_links(self.original.as_ref(), snapshot.as_deref(), content, origin);
    }
}
impl BookSource for MediaReferenceSource {
    fn book(&self) -> &Book {
        self.inner.book()
    }
    fn inner_source(&self) -> Option<&dyn BookSource> {
        Some(self.inner.as_ref())
    }
    fn content_revision(&self) -> Option<u64> {
        self.inner.content_revision()
    }
    fn media_reference_revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    fn prepare_media_references(&self, content: &mut Vec<Inline>, origin: Option<&SourceRange>) {
        self.apply(content, origin)
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let mut section = self.inner.parse_section(index)?;
        for text in section.blocks.iter_mut().flat_map(Block::text_blocks_mut) {
            self.apply(&mut text.content, text.source.as_ref());
        }
        Ok(section)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.inner.resource(href)
    }
}

pub fn preview_blocks(section: &Section, target: &Target) -> Result<Vec<Block>, String> {
    if !targets(section, target.section).contains(target) {
        return Err("The figure/table changed. Reopen the preview.".into());
    }
    let mut indices = target.images.clone();
    indices.extend(&target.captions);
    indices.push(target.block);
    indices.sort_unstable();
    indices.dedup();
    let mut blocks = Vec::new();
    if let Some(Block::Figure(parent)) = target.group.and_then(|at| section.blocks.get(at)) {
        blocks.extend(
            parent
                .caption_blocks()
                .filter(|t| media_references::caption_label(&plain(t)).is_some())
                .cloned()
                .map(Block::Text),
        );
    }
    blocks.extend(
        indices
            .into_iter()
            .filter_map(|i| section.blocks.get(i).cloned()),
    );
    Ok(blocks)
}
