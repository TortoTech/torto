use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rebook_publication::{
    Block, Book, BookSource, Inline, InlineRole, LinkRole, PublicationError, PublicationUrl,
    RasterResource, Resource, Section, TextBaseline, TextBlock, TextBlockKind, TextRun,
};
use rebook_reader::sentence_char_ranges;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ParagraphStructureKey {
    pub(crate) section_index: usize,
    pub(crate) node: String,
}

#[derive(Clone, Debug)]
struct ParagraphAtom {
    text: String,
    start: usize,
    end: usize,
}

#[derive(Default)]
struct StructureState {
    active: HashMap<ParagraphStructureKey, bool>,
}

/// A derived view layered after translation. The model only chooses atom ranges;
/// source text and source ranges remain owned by the publication.
pub(crate) struct ParagraphStructureSource {
    inner: Arc<dyn BookSource>,
    language_hint: String,
    state: RwLock<StructureState>,
}

impl ParagraphStructureSource {
    pub(crate) fn new(inner: Arc<dyn BookSource>) -> Self {
        let language_hint = inner
            .book()
            .metadata
            .languages
            .first()
            .map_or_else(|| "en".to_owned(), Clone::clone);
        Self {
            inner,
            language_hint,
            state: RwLock::new(StructureState::default()),
        }
    }

    pub(crate) fn is_active(&self, key: &ParagraphStructureKey) -> bool {
        self.state
            .read()
            .ok()
            .and_then(|state| state.active.get(key).copied())
            .unwrap_or(false)
    }

    pub(crate) fn is_structured(&self, key: &ParagraphStructureKey) -> bool {
        self.is_active(key)
    }

    pub(crate) fn set_active(
        &self,
        key: ParagraphStructureKey,
        active: bool,
    ) -> Result<(), String> {
        self.state
            .write()
            .map_err(|_| "按句分段状态已损坏".to_owned())?
            .active
            .insert(key, active);
        Ok(())
    }

    pub(crate) fn can_structure(&self, key: &ParagraphStructureKey) -> Result<bool, String> {
        let section = self
            .inner
            .parse_section(key.section_index)
            .map_err(|error| error.to_string())?;
        let Some((primary, split_semicolons)) = section.blocks.iter().find_map(|block| {
            structurable_text_for_node(block, &key.node).map(|text| {
                (
                    text,
                    !matches!(block, Block::Quote(_)) && text.kind != TextBlockKind::Blockquote,
                )
            })
        }) else {
            return Ok(false);
        };
        Ok(paragraph_atoms_for_content_mode(
            &primary.content,
            &self.language_hint,
            split_semicolons,
        )
        .len()
            >= 2)
    }
}

impl BookSource for ParagraphStructureSource {
    fn book(&self) -> &Book {
        self.inner.book()
    }

    fn table_of_contents_origin(&self) -> rebook_publication::TableOfContentsOrigin {
        self.inner.table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let mut section = self.inner.parse_section(index)?;
        let state = self
            .state
            .read()
            .map_err(|_| PublicationError::InvalidPublication("按句分段状态已损坏".to_owned()))?;
        let mut block_index = 0;
        while block_index < section.blocks.len() {
            let split_semicolons = !matches!(
                &section.blocks[block_index],
                Block::Quote(_)
                    | Block::Text(TextBlock {
                        kind: TextBlockKind::Blockquote,
                        ..
                    })
            );
            let active_text = match &mut section.blocks[block_index] {
                Block::Text(primary) => {
                    let active = text_structure_is_active(primary, index, &state);
                    if active && text_kind_is_structurable(primary.kind) {
                        apply_sentence_structure(primary, &self.language_hint);
                    }
                    active
                }
                Block::Figure(figure) => {
                    for caption in &mut figure.captions {
                        if text_structure_is_active(caption, index, &state)
                            && text_kind_is_structurable(caption.kind)
                        {
                            apply_sentence_structure(caption, &self.language_hint);
                        }
                    }
                    false
                }
                Block::Quote(quote) => {
                    let mut active_primary = false;
                    for body in &mut quote.body {
                        let active = text_structure_is_active(body, index, &state);
                        if (active || (body.source.is_none() && active_primary))
                            && text_kind_is_structurable(body.kind)
                        {
                            apply_sentence_structure_mode(body, &self.language_hint, false);
                        }
                        active_primary = active;
                    }
                    false
                }
                _ => false,
            };
            if active_text
                && let Some(Block::Text(companion)) = section.blocks.get_mut(block_index + 1)
                && companion.source.is_none()
                && text_kind_is_structurable(companion.kind)
            {
                apply_sentence_structure_mode(companion, &self.language_hint, split_semicolons);
            }
            block_index += 1;
        }
        Ok(section)
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.inner.resource(href)
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.inner.raster_resource(href)
    }

    fn fixed_page_dimensions(
        &self,
        section_index: usize,
    ) -> Result<Option<rebook_publication::FixedPageDimensions>, PublicationError> {
        self.inner.fixed_page_dimensions(section_index)
    }
}

#[cfg(test)]
fn paragraph_atoms_for_content(content: &[Inline], language_hint: &str) -> Vec<ParagraphAtom> {
    paragraph_atoms_for_content_mode(content, language_hint, true)
}

fn paragraph_atoms_for_content_mode(
    content: &[Inline],
    language_hint: &str,
    split_semicolons: bool,
) -> Vec<ParagraphAtom> {
    let text = inline_text(content);
    let mut cursor = 0;
    let mut protected = Vec::new();
    let mut footnotes = Vec::new();
    for inline in content {
        let len = match inline {
            Inline::Text(run) => {
                let len = run.text.chars().count();
                if is_focus_footnote(run) && len > 0 {
                    let range = cursor..cursor + len;
                    footnotes.push(range.clone());
                    protected.push(range);
                }
                len
            }
            Inline::Math(run) => {
                let len = run.latex.chars().count();
                if is_ocr_superscript_reference(run.display, &run.latex) && len > 0 {
                    footnotes.push(cursor..cursor + len);
                }
                if len > 0 {
                    protected.push(cursor..cursor + len);
                }
                len
            }
            Inline::Image(_) => 0,
            Inline::Break => 1,
        };
        cursor += len;
    }
    let atoms = paragraph_atoms_with_protected_ranges(&text, &protected, language_hint);
    let atoms = if split_semicolons && text.contains([';', '；']) {
        let chars = text.chars().collect::<Vec<_>>();
        let boundaries = atoms
            .iter()
            .take(atoms.len().saturating_sub(1))
            .map(|atom| atom.end)
            .chain(semicolon_boundaries(&chars, &protected));
        atoms_from_boundaries(boundaries, &chars, atoms.len())
    } else {
        atoms
    };
    let atoms = attach_paired_punctuation_atoms(atoms, &text);
    let atoms = merge_leading_continuation_punctuation_atoms(atoms, &text);
    attach_footnote_atoms(atoms, &text, &footnotes)
}

fn text_kind_is_structurable(kind: TextBlockKind) -> bool {
    matches!(
        kind,
        TextBlockKind::Paragraph
            | TextBlockKind::Blockquote
            | TextBlockKind::Caption
            | TextBlockKind::ListItem { .. }
    )
}

#[cfg(test)]
mod list_structure_tests {
    use super::*;
    #[test]
    fn sentence_splitting_preserves_list_number_depth_and_inline_content() {
        let kind = TextBlockKind::ListItem {
            ordered: true,
            ordinal: 3,
            depth: 2,
            marker_visible: true,
        };
        assert!(text_kind_is_structurable(kind));
        let mut block = TextBlock {
            kind,
            content: vec![Inline::Text(rebook_publication::TextRun {
                text: "First sentence. Second sentence.".into(),
                style: rebook_publication::TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        apply_sentence_structure(&mut block, "en");
        assert_eq!(block.kind, kind);
        assert_eq!(
            block
                .content
                .iter()
                .filter(|inline| matches!(inline, Inline::Break))
                .count(),
            1
        );
        assert!(block.style.sentence_indents);
    }
}

fn text_structure_is_active(
    text: &TextBlock,
    section_index: usize,
    state: &StructureState,
) -> bool {
    text.source.as_ref().is_some_and(|range| {
        state.active.get(&ParagraphStructureKey {
            section_index,
            node: range.start.node.clone(),
        }) == Some(&true)
    })
}

fn structurable_text_for_node<'a>(block: &'a Block, node: &str) -> Option<&'a TextBlock> {
    let matches = |text: &TextBlock| {
        text_kind_is_structurable(text.kind)
            && text
                .source
                .as_ref()
                .is_some_and(|range| range.start.node == node)
    };
    match block {
        Block::Text(text) => matches(text).then_some(text),
        Block::Figure(figure) => figure.captions.iter().find(|caption| matches(caption)),
        Block::Quote(quote) => quote.body.iter().find(|body| matches(body)),
        Block::Table(_)
        | Block::Image(_)
        | Block::Note(_)
        | Block::Separator(_)
        | Block::LineBreak
        | Block::PageBreak => None,
    }
}

fn is_focus_footnote(run: &TextRun) -> bool {
    run.style.inline_role == InlineRole::Footnote
        || (run
            .link
            .as_ref()
            .is_some_and(|target| target.fragment().is_some())
            && (run.style.link_role == LinkRole::FootnoteReference
                || (run.style.link_role == LinkRole::Normal
                    && run.style.baseline == TextBaseline::Superscript)))
}

fn is_ocr_superscript_reference(display: bool, latex: &str) -> bool {
    if display {
        return false;
    }
    let latex = latex.trim();
    let Some(marker) = latex
        .strip_prefix("^{")
        .and_then(|value| value.strip_suffix('}'))
    else {
        return false;
    };
    !marker.is_empty() && marker.chars().all(|character| character.is_ascii_digit())
}

fn attach_paired_punctuation_atoms(atoms: Vec<ParagraphAtom>, text: &str) -> Vec<ParagraphAtom> {
    let chars = text.chars().collect::<Vec<_>>();
    let pairs = paired_punctuation_ranges(&chars);
    if pairs.is_empty() {
        return atoms;
    }

    let boundaries = atoms
        .iter()
        .take(atoms.len().saturating_sub(1))
        .map(|atom| atom.end)
        .map(|mut boundary| {
            loop {
                let extended = pairs
                    .iter()
                    .filter(|range| range.start < boundary && boundary < range.end)
                    .map(|range| range.end)
                    .max()
                    .unwrap_or(boundary);
                if extended == boundary {
                    break boundary;
                }
                boundary = extended;
            }
        });
    let normalized = atoms_from_boundaries(boundaries, &chars, atoms.len());

    let mut attached: Vec<ParagraphAtom> = Vec::with_capacity(normalized.len());
    for atom in normalized {
        if is_parenthetical_annotation(&atom, &chars, &pairs)
            && let Some(previous) = attached.last_mut()
        {
            previous.text.push_str(&atom.text);
            previous.end = atom.end;
        } else {
            attached.push(atom);
        }
    }
    attach_leading_parenthetical_suffixes(attached, &chars, &pairs)
}

fn merge_leading_continuation_punctuation_atoms(
    atoms: Vec<ParagraphAtom>,
    text: &str,
) -> Vec<ParagraphAtom> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut merged: Vec<ParagraphAtom> = Vec::with_capacity(atoms.len());
    for atom in atoms {
        let first = (atom.start..atom.end)
            .find(|index| !chars[*index].is_whitespace())
            .and_then(|index| chars.get(index));
        if first.is_some_and(|character| is_sentence_continuation_punctuation(*character))
            && let Some(previous) = merged.last_mut()
        {
            previous.text.push_str(&atom.text);
            previous.end = atom.end;
        } else {
            merged.push(atom);
        }
    }
    merged
}

const fn is_sentence_continuation_punctuation(character: char) -> bool {
    matches!(character, '，' | '、' | '；' | '：' | ',' | ';' | ':')
}

fn attach_leading_parenthetical_suffixes(
    mut atoms: Vec<ParagraphAtom>,
    chars: &[char],
    pairs: &[std::ops::Range<usize>],
) -> Vec<ParagraphAtom> {
    for index in 1..atoms.len() {
        let previous_last = (atoms[index - 1].start..atoms[index - 1].end)
            .rev()
            .find(|position| !chars[*position].is_whitespace());
        let previous_ends_with_quote = previous_last
            .is_some_and(|position| matches!(chars[position], '”' | '’' | '」' | '』'));
        let current_start = atoms[index].start;
        let prefix_start = (current_start..atoms[index].end)
            .find(|position| !chars[*position].is_whitespace())
            .unwrap_or(atoms[index].end);
        let Some(prefix_end) = pairs
            .iter()
            .find(|range| {
                range.start == prefix_start
                    && matches!(chars.get(range.start), Some('（' | '(' | '【' | '[' | '〔'))
            })
            .map(|range| range.end)
        else {
            continue;
        };
        // A complete parenthetical sentence immediately following a sentence
        // is a trailing aside, not the beginning of the next sentence. Keep
        // noun prefixes such as “（新方法）可以...” with their own predicate.
        let sentence_aside = previous_last.is_some_and(|position| {
            matches!(chars[position], '。' | '！' | '？' | '.' | '!' | '?')
                && !chars[position + 1..prefix_start].contains(&'\n')
        }) && chars[prefix_start + 1..prefix_end - 1]
            .iter()
            .rev()
            .find(|character| {
                !character.is_whitespace() && !matches!(character, '”' | '’' | '」' | '』')
            })
            .is_some_and(|character| matches!(character, '。' | '！' | '？' | '.' | '!' | '?'));
        if !previous_ends_with_quote && !sentence_aside {
            continue;
        }
        move_atom_prefix_to_previous(&mut atoms, index, prefix_end, chars);
    }
    atoms
}

fn move_atom_prefix_to_previous(
    atoms: &mut [ParagraphAtom],
    index: usize,
    prefix_end: usize,
    chars: &[char],
) {
    let current_start = atoms[index].start;
    let current_end = atoms[index].end;
    if index == 0 || prefix_end <= current_start || prefix_end >= current_end {
        return;
    }
    let prefix = chars[current_start..prefix_end].iter().collect::<String>();
    let remainder = chars[prefix_end..current_end].iter().collect::<String>();
    atoms[index - 1].text.push_str(&prefix);
    atoms[index - 1].end = prefix_end;
    atoms[index].text = remainder;
    atoms[index].start = prefix_end;
}

fn semicolon_boundaries(chars: &[char], protected: &[std::ops::Range<usize>]) -> Vec<usize> {
    unquoted_punctuation_boundaries(chars, protected, &['；', ';'])
}

fn unquoted_punctuation_boundaries(
    chars: &[char],
    protected: &[std::ops::Range<usize>],
    terminators: &[char],
) -> Vec<usize> {
    let mut closers = Vec::new();
    let mut boundaries = Vec::new();
    for (index, character) in chars.iter().copied().enumerate() {
        if protected.iter().any(|range| range.contains(&index)) {
            continue;
        }
        if matches!(character, '\'' | '"') {
            let escaped = chars[..index]
                .iter()
                .rev()
                .take_while(|character| **character == '\\')
                .count()
                % 2
                == 1;
            let previous_is_word = index
                .checked_sub(1)
                .is_some_and(|previous| chars[previous].is_ascii_alphanumeric());
            let next_is_word = chars
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_alphanumeric());
            if escaped || (character == '\'' && previous_is_word && next_is_word) {
                continue;
            }
            if closers.last() == Some(&character) {
                closers.pop();
            } else if character == '"' || !previous_is_word {
                closers.push(character);
            }
            continue;
        }
        if let Some(closer) = paired_closer(character) {
            closers.push(closer);
            continue;
        }
        if paired_opener(character).is_some() {
            if let Some(position) = closers.iter().rposition(|closer| *closer == character) {
                closers.truncate(position);
            }
            continue;
        }
        if terminators.contains(&character) && closers.is_empty() {
            let mut end = index + 1;
            while chars.get(end).is_some_and(|character| {
                character.is_whitespace()
                    || matches!(character, '；' | ';' | '。' | '.' | '！' | '!' | '？' | '?')
            }) {
                end += 1;
            }
            boundaries.push(end);
        }
    }
    boundaries
}

fn paired_punctuation_ranges(chars: &[char]) -> Vec<std::ops::Range<usize>> {
    let mut stack = Vec::new();
    let mut ranges = Vec::new();
    for (index, character) in chars.iter().copied().enumerate() {
        if paired_closer(character).is_some() {
            stack.push((character, index));
            continue;
        }
        let Some(opener) = paired_opener(character) else {
            continue;
        };
        let Some(position) = stack
            .iter()
            .rposition(|(candidate, _)| *candidate == opener)
        else {
            continue;
        };
        let (_, start) = stack[position];
        stack.truncate(position);
        ranges.push(start..index + 1);
    }
    ranges
}

fn paired_closer(character: char) -> Option<char> {
    match character {
        '“' => Some('”'),
        '‘' => Some('’'),
        '《' => Some('》'),
        '〈' => Some('〉'),
        '（' => Some('）'),
        '(' => Some(')'),
        '【' => Some('】'),
        '[' => Some(']'),
        '〔' => Some('〕'),
        '「' => Some('」'),
        '『' => Some('』'),
        '{' => Some('}'),
        _ => None,
    }
}

fn paired_opener(character: char) -> Option<char> {
    match character {
        '”' => Some('“'),
        '’' => Some('‘'),
        '》' => Some('《'),
        '〉' => Some('〈'),
        '）' => Some('（'),
        ')' => Some('('),
        '】' => Some('【'),
        ']' => Some('['),
        '〕' => Some('〔'),
        '」' => Some('「'),
        '』' => Some('『'),
        '}' => Some('{'),
        _ => None,
    }
}

fn is_parenthetical_annotation(
    atom: &ParagraphAtom,
    chars: &[char],
    pairs: &[std::ops::Range<usize>],
) -> bool {
    let start = (atom.start..atom.end)
        .find(|index| !chars[*index].is_whitespace())
        .unwrap_or(atom.end);
    let end = (atom.start..atom.end)
        .rev()
        .find(|index| !chars[*index].is_whitespace())
        .map_or(start, |index| index + 1);
    matches!(chars.get(start), Some('（' | '(' | '【' | '[' | '〔'))
        && pairs
            .iter()
            .any(|range| range.start == start && range.end == end)
}

fn atoms_from_boundaries(
    boundaries: impl IntoIterator<Item = usize>,
    chars: &[char],
    capacity: usize,
) -> Vec<ParagraphAtom> {
    let mut boundaries = boundaries
        .into_iter()
        .filter(|boundary| *boundary > 0 && *boundary < chars.len())
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut atoms: Vec<ParagraphAtom> = Vec::with_capacity(capacity);
    let mut start = 0;
    for end in boundaries.into_iter().chain(std::iter::once(chars.len())) {
        if start >= end {
            continue;
        }
        let value = chars[start..end].iter().collect::<String>();
        if value.trim().is_empty() {
            if let Some(previous) = atoms.last_mut() {
                previous.text.push_str(&value);
                previous.end = end;
            }
        } else {
            atoms.push(ParagraphAtom {
                text: value,
                start,
                end,
            });
        }
        start = end;
    }
    atoms
}

fn attach_footnote_atoms(
    atoms: Vec<ParagraphAtom>,
    text: &str,
    footnotes: &[std::ops::Range<usize>],
) -> Vec<ParagraphAtom> {
    if footnotes.is_empty() {
        return atoms;
    }
    let chars = text.chars().collect::<Vec<_>>();
    let boundaries = atoms
        .iter()
        .take(atoms.len().saturating_sub(1))
        .map(|atom| atom.end)
        .map(|boundary| {
            footnotes
                .iter()
                .find(|range| range.start <= boundary && boundary < range.end)
                .map_or(boundary, |range| {
                    let mut end = range.end;
                    while chars
                        .get(end)
                        .is_some_and(|character| character.is_whitespace() && *character != '\n')
                    {
                        end += 1;
                    }
                    end
                })
        })
        .collect::<Vec<_>>();
    let normalized = atoms_from_boundaries(boundaries, &chars, atoms.len());

    let mut attached: Vec<ParagraphAtom> = Vec::with_capacity(normalized.len());
    for atom in normalized {
        let only_footnote = (atom.start..atom.end).any(|index| {
            !chars
                .get(index)
                .is_some_and(|character| character.is_whitespace())
                && footnotes.iter().any(|range| range.contains(&index))
        }) && (atom.start..atom.end).all(|index| {
            chars
                .get(index)
                .is_some_and(|character| character.is_whitespace())
                || footnotes.iter().any(|range| range.contains(&index))
        });
        if only_footnote && let Some(previous) = attached.last_mut() {
            previous.text.push_str(&atom.text);
            previous.end = atom.end;
        } else {
            attached.push(atom);
        }
    }
    attached
}

fn paragraph_atoms_with_protected_ranges(
    text: &str,
    protected_ranges: &[std::ops::Range<usize>],
    language: &str,
) -> Vec<ParagraphAtom> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut segmentation_chars = chars.clone();
    for range in protected_ranges {
        for index in range.clone() {
            if let Some(character) = segmentation_chars.get_mut(index) {
                *character = ' ';
            }
        }
    }
    let segmentation_text = segmentation_chars.iter().collect::<String>();
    let mut atoms: Vec<ParagraphAtom> = Vec::new();
    let mut start = 0;
    for range in sentence_char_ranges(&segmentation_text, language) {
        let end = range.end;
        let value = chars[start..end].iter().collect::<String>();
        if value.trim().is_empty() {
            if let Some(last) = atoms.last_mut() {
                last.text.push_str(&value);
                last.end = end;
                start = end;
            }
            continue;
        }
        atoms.push(ParagraphAtom {
            text: value,
            start,
            end,
        });
        start = end;
    }
    if start < chars.len() {
        let value = chars[start..].iter().collect::<String>();
        if let Some(last) = atoms.last_mut() {
            last.text.push_str(&value);
            last.end = chars.len();
        } else if !value.trim().is_empty() {
            atoms.push(ParagraphAtom {
                text: value,
                start,
                end: chars.len(),
            });
        }
    }
    let mut quote_boundaries = nested_quote_sentence_boundaries(&segmentation_chars);
    quote_boundaries.extend(cjk_boundaries_before_lowercase_words(&segmentation_chars));
    if quote_boundaries.is_empty() {
        return atoms;
    }
    let boundaries = atoms
        .iter()
        .take(atoms.len().saturating_sub(1))
        .map(|atom| atom.end)
        .chain(quote_boundaries);
    atoms_from_boundaries(boundaries, &chars, atoms.len())
}

fn cjk_boundaries_before_lowercase_words(chars: &[char]) -> Vec<usize> {
    // SentenceX can suppress a CJK terminator glued to a lowercase English
    // example. Share quotation/bracket protection with semicolon splitting;
    // leave ASCII periods (abbreviations/decimals) to the segmenter.
    unquoted_punctuation_boundaries(chars, &[], &['。', '！', '？'])
        .into_iter()
        .filter(|end| chars.get(*end).is_some_and(char::is_ascii_lowercase))
        .collect()
}

fn nested_quote_sentence_boundaries(chars: &[char]) -> Vec<usize> {
    let pairs = paired_punctuation_ranges(chars);
    let mut boundaries = Vec::new();
    for range in &pairs {
        if !matches!(chars.get(range.start), Some('“' | '‘' | '「' | '『'))
            || pairs
                .iter()
                .any(|parent| parent.start < range.start && range.end <= parent.end)
        {
            continue;
        }
        let mut index = range.end;
        let mut closers = 0;
        while index > range.start {
            match chars[index - 1] {
                '”' | '’' | '」' | '』' => {
                    closers += 1;
                    index -= 1;
                }
                character if character.is_whitespace() => index -= 1,
                _ => break,
            }
        }
        // SentenceX can hide the final terminator in nested quotes (e.g. 。’”).
        // Add only the boundary after the complete quotation, never inside it.
        if closers >= 2
            && index > range.start
            && matches!(chars[index - 1], '。' | '！' | '？' | '!' | '?')
        {
            let mut end = range.end;
            while chars
                .get(end)
                .is_some_and(|character| character.is_whitespace())
            {
                end += 1;
            }
            if end < chars.len() {
                boundaries.push(end);
            }
        }
    }
    boundaries
}

fn inline_text(content: &[Inline]) -> String {
    content
        .iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.as_str(),
            Inline::Math(run) => run.latex.as_str(),
            Inline::Image(_) => "",
            Inline::Break => "\n",
        })
        .collect()
}

fn apply_sentence_structure(block: &mut TextBlock, language_hint: &str) {
    apply_sentence_structure_mode(
        block,
        language_hint,
        block.kind != TextBlockKind::Blockquote,
    );
}

fn apply_sentence_structure_mode(
    block: &mut TextBlock,
    language_hint: &str,
    split_semicolons: bool,
) {
    let atoms = paragraph_atoms_for_content_mode(&block.content, language_hint, split_semicolons);
    if atoms.len() < 2 {
        return;
    }
    let original = std::mem::take(&mut block.content);
    block.style.preserve_sentence_prefix = original.iter().all(
        |inline| matches!(inline, Inline::Text(run) if !run.text.contains(['\n', '\r', '\t'])),
    );
    let mut content = Vec::new();
    for (index, atom) in atoms.iter().enumerate() {
        if index > 0 {
            content.push(Inline::Break);
        }
        content.extend(slice_inlines(&original, atom.start, atom.end));
    }
    block.content = content;
    block.style.sentence_indents = true;
}

fn slice_inlines(content: &[Inline], start: usize, end: usize) -> Vec<Inline> {
    let mut cursor = 0;
    let mut sliced = Vec::new();
    for inline in content {
        let len = match inline {
            Inline::Text(run) => run.text.chars().count(),
            Inline::Math(run) => run.latex.chars().count(),
            Inline::Image(_) => 0,
            Inline::Break => 1,
        };
        let inline_start = cursor;
        let inline_end = cursor + len;
        cursor = inline_end;
        if let Inline::Image(run) = inline {
            if inline_start >= start && inline_start < end {
                sliced.push(Inline::Image(run.clone()));
            }
            continue;
        }
        if inline_end <= start || inline_start >= end {
            continue;
        }
        match inline {
            Inline::Text(run) => {
                let local_start = start.saturating_sub(inline_start);
                let local_end = (end - inline_start).min(len);
                let text = run
                    .text
                    .chars()
                    .skip(local_start)
                    .take(local_end - local_start)
                    .collect::<String>();
                if !text.is_empty() {
                    sliced.push(Inline::Text(TextRun {
                        text,
                        style: run.style,
                        link: run.link.clone(),
                    }));
                }
            }
            Inline::Math(run) => sliced.push(Inline::Math(run.clone())),
            Inline::Image(_) => unreachable!("inline images are handled before range slicing"),
            Inline::Break => sliced.push(Inline::Break),
        }
    }
    sliced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semicolons_split_prose_but_preserve_quotations_and_brackets() {
        for (text, expected) in [
            (
                "先理解原文；再组织译文。",
                vec!["先理解原文；", "再组织译文。"],
            ),
            (
                "Read first; write later.",
                vec!["Read first;", "write later."],
            ),
            ("他说：“先理解；再表达。”", vec!["他说：“先理解；再表达。”"]),
            ("“先理解”；“再表达”。", vec!["“先理解”；", "“再表达”。"]),
            (
                "他说：“外层‘先甲；再乙’；继续。”；结束。",
                vec!["他说：“外层‘先甲；再乙’；继续。”；", "结束。"],
            ),
            (
                "He says \"read; write\"; continue.",
                vec!["He says \"read; write\";", "continue."],
            ),
            (
                "He says 'read; write'; continue.",
                vec!["He says 'read; write';", "continue."],
            ),
            (
                "He said \"Read first. Think; then write.\"; Continue.",
                vec!["He said \"Read first. Think; then write.\";", "Continue."],
            ),
            (
                "He says \"it's good; 'read; write'\"; continue.",
                vec!["He says \"it's good; 'read; write'\";", "continue."],
            ),
            (
                "Don't rush; think first.",
                vec!["Don't rush;", "think first."],
            ),
            (
                "James' notes; read them.",
                vec!["James' notes;", "read them."],
            ),
            (
                "参见文献（甲，2020；乙，2021）；继续。",
                vec!["参见文献（甲，2020；乙，2021）；", "继续。"],
            ),
            (
                "《甲；乙》；《丙；丁》。",
                vec!["《甲；乙》；", "《丙；丁》。"],
            ),
            (
                "先做；“未闭合；继续；尾部",
                vec!["先做；", "“未闭合；继续；尾部"],
            ),
            ("他说\"未闭合; 继续;尾部", vec!["他说\"未闭合; 继续;尾部"]),
            ("A;;B;", vec!["A;;", "B;"]),
        ] {
            let content = vec![Inline::Text(TextRun {
                text: text.into(),
                style: Default::default(),
                link: None,
            })];
            let atoms = paragraph_atoms_for_content(&content, "zh");
            assert_eq!(
                atoms
                    .iter()
                    .map(|atom| atom.text.trim())
                    .collect::<Vec<_>>(),
                expected,
                "{text}"
            );
            assert_eq!(
                atoms
                    .iter()
                    .map(|atom| atom.text.as_str())
                    .collect::<String>(),
                text
            );
        }
    }

    #[test]
    fn quotation_blocks_keep_semicolons_without_disabling_sentence_splitting() {
        let mut block = TextBlock {
            kind: TextBlockKind::Blockquote,
            content: vec![Inline::Text(TextRun {
                text: "甲；乙。丙；丁。".into(),
                style: Default::default(),
                link: None,
            })],
            style: Default::default(),
            source: None,
        };
        apply_sentence_structure(&mut block, "zh");
        assert_eq!(inline_text(&block.content), "甲；乙。\n丙；丁。");
        assert_eq!(
            paragraph_atoms_for_content_mode(&block.content, "zh", false).len(),
            2
        );
    }

    #[test]
    fn chinese_sentence_before_lowercase_english_starts_a_new_paragraph() {
        let first = "很多人把复数的名词也译成了“一个”或“一种”。";
        let second = "new methods译成了“一种新方法”，more difficult ways译成了“一个更艰难的途径”，不但不好，而且错了。";
        for separator in ["", " "] {
            let content = vec![Inline::Text(TextRun {
                text: format!("{first}{separator}{second}"),
                style: Default::default(),
                link: None,
            })];
            let atoms = paragraph_atoms_for_content(&content, "zh");
            assert_eq!(
                atoms
                    .iter()
                    .map(|atom| atom.text.trim())
                    .collect::<Vec<_>>(),
                vec![first, second]
            );
            assert_eq!(
                atoms
                    .iter()
                    .map(|atom| atom.text.as_str())
                    .collect::<String>(),
                format!("{first}{separator}{second}")
            );
        }
        for text in [
            "作者说：“很多人译成一种。new methods 也是这样译的。”",
            "作者说：\"很多人译成一种。new methods也是这样译的。\"",
            "这是一段说明（很多人译成一种。new methods 也是这样译的）。",
        ] {
            let quoted = vec![Inline::Text(TextRun {
                text: text.into(),
                style: Default::default(),
                link: None,
            })];
            let atoms = paragraph_atoms_for_content(&quoted, "zh");
            assert_eq!(
                atoms.len(),
                1,
                "must preserve quotation/parenthesis: {text}"
            );
            assert_eq!(atoms[0].text, text);
        }
    }

    #[test]
    fn complete_parenthetical_aside_stays_with_preceding_sentence() {
        let first = "“承认问题的重要性和迫切性”也不像话。";
        let second = "这句原文to acknowledge the importance and the urgency of the problem，译成“承认问题严重，也很迫切”就可以了。（比较起来，“连贯性”“长期性”等略微好些。）";
        let third = "这种用法是由英文的-ty（-ity、-ety）、-ness这些字尾构成的名词译来的。";
        let original = format!("{first}{second}{third}");
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: original.clone(),
                style: Default::default(),
                link: None,
            })],
            style: Default::default(),
            source: None,
        };
        let atoms = paragraph_atoms_for_content(&block.content, "zh");
        assert_eq!(
            atoms
                .iter()
                .map(|atom| atom.text.as_str())
                .collect::<Vec<_>>(),
            vec![first, second, third]
        );
        apply_sentence_structure(&mut block, "zh");
        assert_eq!(
            inline_text(&block.content),
            format!("{first}\n{second}\n{third}")
        );
        for (text, expected) in [
            (
                "前句结束。（新方法）可以提高效率。",
                vec!["前句结束。", "（新方法）可以提高效率。"],
            ),
            (
                "前句结束。（补充说明。）后句开始。",
                vec!["前句结束。（补充说明。）", "后句开始。"],
            ),
        ] {
            let content = vec![Inline::Text(TextRun {
                text: text.into(),
                style: Default::default(),
                link: None,
            })];
            let atoms = paragraph_atoms_for_content(&content, "zh");
            assert_eq!(
                atoms
                    .iter()
                    .map(|atom| atom.text.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn nested_quote_closers_leave_following_sentence_separate() {
        let text = concat!(
            "193 页，在“代名词”项下，作者讨论中译的另一个危机：“They are good questions, ",
            "because they call for thought-provoking answers 是平淡无奇的一句英文，但也很容易译得不像中文。",
            "（they 这个字是翻译海中的‘鲨鱼’，译者碰到了它就危险了……）",
            "就像‘它们是好的问题，因为它们需要对方做出激发思想的回答’，真再忠于原文也没有了。",
            "也不错，就是读者不知道那两个‘它们’是谁。如果是朗诵出来的，心中更想不起那批‘人’是谁。",
            "‘好的问题’‘做出……的回答’不像中国话。如果有这样一个意思要表达，而表达的人又没有看到英文，",
            "中国人会这样说：‘这些问题问得好，要回答就要好好动一下脑筋（思想一番）。’”",
            "这样的翻译才是活的译句，不是死的译字，才是变通，不是向英文投降。"
        );
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: text.into(),
                style: Default::default(),
                link: None,
            })],
            style: Default::default(),
            source: None,
        };
        let atoms = paragraph_atoms_for_content(&block.content, "zh");
        assert_eq!(
            atoms.len(),
            2,
            "long quotation collapsed to {} atoms: {:?}",
            atoms.len(),
            atoms.iter().map(|atom| &atom.text).collect::<Vec<_>>()
        );
        assert_eq!(
            atoms[1].text,
            "这样的翻译才是活的译句，不是死的译字，才是变通，不是向英文投降。"
        );
        assert!(atoms[0].text.ends_with("。’”"));
        assert_eq!(
            atoms
                .iter()
                .map(|atom| atom.text.as_str())
                .collect::<String>(),
            text
        );
        assert!(atoms.iter().any(|atom| {
            atom.text
                .contains("‘它们是好的问题，因为它们需要对方做出激发思想的回答’")
        }));
        assert!(atoms.iter().any(|atom| {
            atom.text
                .contains("（they 这个字是翻译海中的‘鲨鱼’，译者碰到了它就危险了……）")
        }));
        apply_sentence_structure(&mut block, "zh");
        assert_eq!(inline_text(&block.content).matches('\n').count(), 1);
        assert_eq!(inline_text(&block.content).replace('\n', ""), text);
    }

    #[test]
    fn nested_quote_boundary_keeps_continuations_and_citations_attached() {
        let text = "他说：“她喊‘快走！’”，随后大家离开。第二天又回来了。";
        let content = vec![Inline::Text(TextRun {
            text: text.into(),
            style: Default::default(),
            link: None,
        })];
        let atoms = paragraph_atoms_for_content(&content, "zh");
        assert_eq!(atoms.len(), 2);
        assert_eq!(atoms[0].text, "他说：“她喊‘快走！’”，随后大家离开。");
        let text = "他说：“她念‘第一句。第二句。’”（出处）后面的解释另起一句。";
        let content = vec![Inline::Text(TextRun {
            text: text.into(),
            style: Default::default(),
            link: None,
        })];
        let atoms = paragraph_atoms_for_content(&content, "zh");
        assert_eq!(atoms.len(), 2);
        assert_eq!(atoms[0].text, "他说：“她念‘第一句。第二句。’”（出处）");
        assert_eq!(atoms[1].text, "后面的解释另起一句。");
    }

    struct StaticSource {
        book: Book,
        section: Section,
    }

    impl BookSource for StaticSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            (index == 0)
                .then(|| self.section.clone())
                .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    fn source_range(
        spine: &rebook_publication::SpineItemId,
        node: &str,
        text: &str,
    ) -> rebook_publication::SourceRange {
        rebook_publication::SourceRange {
            start: rebook_publication::SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: rebook_publication::SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: u64::try_from(text.chars().count()).unwrap(),
            },
        }
    }

    #[test]
    fn atoms_cover_cjk_and_latin_sentences_without_rewriting() {
        let text = "首先，观察系统。其次，比较反馈；Finally, decide.";
        let atoms = paragraph_atoms_with_protected_ranges(text, &[], "zh");
        assert_eq!(
            atoms
                .iter()
                .map(|atom| atom.text.as_str())
                .collect::<String>(),
            text
        );
        assert_eq!(atoms.len(), 2);
        assert_eq!(atoms[1].text, "其次，比较反馈；Finally, decide.");
    }

    #[test]
    fn sentence_structure_keeps_spaced_dictionary_initials_together() {
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "这是前一句。C. ".into(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "O. D.对justify的解释是：".into(),
                    style: Default::default(),
                    link: None,
                }),
            ],
            style: Default::default(),
            source: None,
        };
        apply_sentence_structure(&mut block, "zh");
        assert_eq!(
            inline_text(&block.content),
            "这是前一句。\nC. O. D.对justify的解释是："
        );
    }

    #[test]
    fn sentence_structure_turns_each_sentence_into_a_subparagraph() {
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "第一句。第二句！第三句？".to_owned(),
                style: Default::default(),
                link: None,
            })],
            style: Default::default(),
            source: None,
        };
        apply_sentence_structure(&mut block, "zh");
        assert_eq!(inline_text(&block.content), "第一句。\n第二句！\n第三句？");
        assert!(block.style.sentence_indents);
    }

    #[test]
    fn captions_and_quote_bodies_support_sentence_structure() {
        let spine = rebook_publication::SpineItemId::new("chapter-1").unwrap();
        let href = PublicationUrl::parse("chapter-1.xhtml").unwrap();
        let figure_text = "Figure one. Second sentence.";
        let standalone_text = "Figure two. Another sentence.";
        let text_block = |kind, node: &str, text: &str| TextBlock {
            kind,
            content: vec![Inline::Text(TextRun {
                text: text.into(),
                style: Default::default(),
                link: None,
            })],
            style: Default::default(),
            source: Some(source_range(&spine, node, text)),
        };
        let section = Section {
            id: spine.clone(),
            href: href.clone(),
            blocks: vec![
                Block::Figure(rebook_publication::FigureBlock {
                    images: Vec::new(),
                    captions: vec![text_block(
                        TextBlockKind::Caption,
                        "figure-caption",
                        figure_text,
                    )],
                    caption_position: rebook_publication::CaptionPosition::After,
                    style: Default::default(),
                    source: Some(source_range(&spine, "figure", "")),
                }),
                Block::Text(text_block(
                    TextBlockKind::Caption,
                    "standalone-caption",
                    standalone_text,
                )),
                Block::Quote(rebook_publication::QuoteBlock {
                    body: vec![text_block(
                        TextBlockKind::Blockquote,
                        "quote-body",
                        "First sentence. Second sentence.",
                    )],
                    attribution: Some(text_block(
                        TextBlockKind::QuoteAttribution,
                        "quote-credit",
                        "Author. Book.",
                    )),
                    source: Some(source_range(&spine, "quote", "")),
                }),
                Block::Text(text_block(
                    TextBlockKind::Blockquote,
                    "legacy-quote",
                    "Legacy sentence. Another sentence.",
                )),
            ],
            anchors: Vec::new(),
        };
        let source = ParagraphStructureSource::new(Arc::new(StaticSource {
            book: Book {
                id: rebook_publication::PublicationId::new("caption-structure-test").unwrap(),
                metadata: rebook_publication::Metadata {
                    languages: vec!["en".into()],
                    ..Default::default()
                },
                cover: None,
                sections: vec![rebook_publication::SpineItem {
                    id: spine,
                    href,
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            section,
        }));
        let figure_key = ParagraphStructureKey {
            section_index: 0,
            node: "figure-caption".into(),
        };
        let standalone_key = ParagraphStructureKey {
            section_index: 0,
            node: "standalone-caption".into(),
        };

        assert!(source.can_structure(&figure_key).unwrap());
        assert!(source.can_structure(&standalone_key).unwrap());
        let quote_key = ParagraphStructureKey {
            section_index: 0,
            node: "quote-body".into(),
        };
        let legacy_key = ParagraphStructureKey {
            section_index: 0,
            node: "legacy-quote".into(),
        };
        assert!(source.can_structure(&quote_key).unwrap());
        assert!(source.can_structure(&legacy_key).unwrap());
        assert!(
            !source
                .can_structure(&ParagraphStructureKey {
                    section_index: 0,
                    node: "quote-credit".into()
                })
                .unwrap()
        );
        let original_quote = source.parse_section(0).unwrap().blocks[2].clone();
        source.set_active(quote_key.clone(), true).unwrap();
        source.set_active(legacy_key, true).unwrap();
        source.set_active(figure_key, true).unwrap();
        source.set_active(standalone_key, true).unwrap();
        let section = source.parse_section(0).unwrap();

        let Block::Figure(figure) = &section.blocks[0] else {
            panic!("expected figure");
        };
        assert_eq!(
            inline_text(&figure.captions[0].content),
            "Figure one. \nSecond sentence."
        );
        let Block::Text(caption) = &section.blocks[1] else {
            panic!("expected standalone caption");
        };
        assert_eq!(
            inline_text(&caption.content),
            "Figure two. \nAnother sentence."
        );
        let Block::Quote(quote) = &section.blocks[2] else {
            panic!("expected quote")
        };
        assert_eq!(
            inline_text(&quote.body[0].content),
            "First sentence. \nSecond sentence."
        );
        assert_eq!(quote.body[0].kind, TextBlockKind::Blockquote);
        assert!(quote.body[0].style.sentence_indents);
        let Block::Quote(original) = &original_quote else {
            unreachable!()
        };
        assert_eq!(quote.attribution, original.attribution);
        assert_eq!(quote.source, original.source);
        assert_eq!(quote.body[0].source, original.body[0].source);
        let Block::Text(legacy) = &section.blocks[3] else {
            panic!("expected legacy quote")
        };
        assert_eq!(
            inline_text(&legacy.content),
            "Legacy sentence. \nAnother sentence."
        );
        source.set_active(quote_key, false).unwrap();
        assert_eq!(source.parse_section(0).unwrap().blocks[2], original_quote);
    }

    #[test]
    fn sentence_leading_quoted_term_is_not_attached_to_the_previous_sentence() {
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "本书的核心关注点是现代纯粹数学，这一决定需要作一些说明。“现代”一词很简单，正如上文所述。然后继续。"
                    .to_owned(),
                style: Default::default(),
                link: None,
            })],
            style: Default::default(),
            source: None,
        };

        apply_sentence_structure(&mut block, "en");

        assert_eq!(
            inline_text(&block.content),
            "本书的核心关注点是现代纯粹数学，这一决定需要作一些说明。\n“现代”一词很简单，正如上文所述。\n然后继续。"
        );
    }

    #[test]
    fn quoted_exclamations_keep_comma_continuations_but_allow_outer_semicolons() {
        let text = concat!(
            "大喊一声“锤子！”，可能表示敲锤子或递锤子的意思；",
            "也有可能表示警告，“锤子要从屋顶上掉下来了，当心！”；",
            "此外，它还可能是提醒你买锤子或者不要忘记带锤子；等等。",
            "我们可以尽情地想象各种意义。"
        );
        let content = vec![Inline::Text(TextRun {
            text: text.to_owned(),
            style: Default::default(),
            link: None,
        })];

        let atoms = paragraph_atoms_for_content(&content, "zh");

        assert_eq!(
            atoms
                .iter()
                .map(|atom| atom.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "大喊一声“锤子！”，可能表示敲锤子或递锤子的意思；",
                "也有可能表示警告，“锤子要从屋顶上掉下来了，当心！”；",
                "此外，它还可能是提醒你买锤子或者不要忘记带锤子；",
                "等等。",
                "我们可以尽情地想象各种意义。",
            ]
        );
        assert!(atoms.iter().skip(1).all(|atom| {
            atom.text
                .trim_start()
                .chars()
                .next()
                .is_none_or(|character| !is_sentence_continuation_punctuation(character))
        }));
    }

    #[test]
    fn quoted_sentence_followed_by_ordinary_text_keeps_its_boundary() {
        let text = "他说：“快走！”第二天他们再次见面。";
        let atoms = paragraph_atoms_with_protected_ranges(text, &[], "zh");
        let atoms = attach_paired_punctuation_atoms(atoms, text);
        let atoms = merge_leading_continuation_punctuation_atoms(atoms, text);

        assert_eq!(atoms.len(), 2);
        assert_eq!(atoms[0].text, "他说：“快走！”");
        assert_eq!(atoms[1].text, "第二天他们再次见面。");
    }

    #[test]
    fn semicolon_splitting_preserves_formulas_and_attached_footnotes() {
        let target = PublicationUrl::parse("chapter.xhtml#note-1").unwrap();
        let formula = rebook_publication::MathRun {
            latex: "f(x;y)".into(),
            display: false,
            size_scale: 1.0,
        };
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "先列公式".into(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Math(formula.clone()),
                Inline::Text(TextRun {
                    text: "；".into(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "[1]".into(),
                    style: rebook_publication::TextStyle {
                        link_role: LinkRole::FootnoteReference,
                        ..Default::default()
                    },
                    link: Some(target.clone()),
                }),
                Inline::Text(TextRun {
                    text: "再解释；最后。".into(),
                    style: Default::default(),
                    link: None,
                }),
            ],
            style: Default::default(),
            source: None,
        };
        apply_sentence_structure(&mut block, "zh");
        assert_eq!(
            inline_text(&block.content),
            "先列公式f(x;y)；[1]\n再解释；\n最后。"
        );
        assert_eq!(
            block
                .content
                .iter()
                .filter(|inline| matches!(inline, Inline::Math(run) if run == &formula))
                .count(),
            1
        );
        assert_eq!(block.content.iter().filter(|inline| matches!(inline, Inline::Text(run) if run.link.as_ref() == Some(&target))).count(), 1);
        assert!(block.style.sentence_indents);
    }

    #[test]
    fn formula_punctuation_never_becomes_a_structure_boundary() {
        let content = vec![
            Inline::Text(TextRun {
                text: "公式".to_owned(),
                style: Default::default(),
                link: None,
            }),
            Inline::Math(rebook_publication::MathRun {
                latex: "f(x,y):=x+y".to_owned(),
                display: false,
                size_scale: 1.0,
            }),
            Inline::Text(TextRun {
                text: "。然后继续。".to_owned(),
                style: Default::default(),
                link: None,
            }),
        ];
        let atoms = paragraph_atoms_for_content(&content, "zh");
        assert_eq!(atoms.len(), 2);
        assert_eq!(atoms[0].text, "公式f(x,y):=x+y。");
    }

    #[test]
    fn trailing_footnote_stays_attached_to_the_preceding_subparagraph() {
        let target = PublicationUrl::parse("chapter.xhtml#note-54").unwrap();
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "第一句。第二句。".to_owned(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "54".to_owned(),
                    style: rebook_publication::TextStyle {
                        baseline: TextBaseline::Superscript,
                        link_role: LinkRole::FootnoteReference,
                        ..Default::default()
                    },
                    link: Some(target),
                }),
            ],
            style: Default::default(),
            source: None,
        };

        apply_sentence_structure(&mut block, "zh");

        assert_eq!(inline_text(&block.content), "第一句。\n第二句。54");
    }

    #[test]
    fn linked_footnote_between_sentences_is_not_duplicated_by_structure() {
        let target = PublicationUrl::parse("chapter.xhtml#note-8").unwrap();
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "分手时，她说：“朝朝暮暮，阳台之下。”".to_owned(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "【8】".to_owned(),
                    style: rebook_publication::TextStyle {
                        link_role: LinkRole::FootnoteReference,
                        ..Default::default()
                    },
                    link: Some(target),
                }),
                Inline::Text(TextRun {
                    text: "这里天地交媾的古老宇宙形象已经变成一个美丽的故事。不过应当注意。"
                        .to_owned(),
                    style: Default::default(),
                    link: None,
                }),
            ],
            style: Default::default(),
            source: None,
        };

        apply_sentence_structure(&mut block, "zh");

        assert_eq!(
            block
                .content
                .iter()
                .filter(|inline| matches!(inline, Inline::Text(run) if is_focus_footnote(run)))
                .count(),
            1
        );
        assert_eq!(inline_text(&block.content).matches("【8】").count(), 1);
        assert_eq!(
            inline_text(&block.content),
            "分手时，她说：“朝朝暮暮，阳台之下。”【8】\n这里天地交媾的古老宇宙形象已经变成一个美丽的故事。\n不过应当注意。"
        );
    }

    #[test]
    fn dialogue_and_its_parenthetical_citation_stay_intact() {
        let target = PublicationUrl::parse("chapter.xhtml#note-5").unwrap();
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "他说：“唯女子与小人为难养也。近之则不孙，远之则怨。”（《论语》卷十七）"
                        .to_owned(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "【5】".to_owned(),
                    style: rebook_publication::TextStyle {
                        link_role: LinkRole::FootnoteReference,
                        ..Default::default()
                    },
                    link: Some(target),
                }),
                Inline::Text(TextRun {
                    text: "话讲得机智却相当刻薄。无论如何，妇女的地位非常低下。".to_owned(),
                    style: Default::default(),
                    link: None,
                }),
            ],
            style: Default::default(),
            source: None,
        };

        apply_sentence_structure(&mut block, "zh");

        assert_eq!(
            inline_text(&block.content),
            "他说：“唯女子与小人为难养也。近之则不孙，远之则怨。”（《论语》卷十七）【5】\n话讲得机智却相当刻薄。\n无论如何，妇女的地位非常低下。"
        );
    }

    #[test]
    fn ocr_superscript_math_reference_stays_with_the_preceding_sentence() {
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "Literature creates, as Ryan puts it, “possible worlds.” ".to_owned(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Math(rebook_publication::MathRun {
                    latex: "^{11}".to_owned(),
                    display: false,
                    size_scale: 1.0,
                }),
                Inline::Text(TextRun {
                    text: " Kittler’s proposition follows. Another sentence follows.".to_owned(),
                    style: Default::default(),
                    link: None,
                }),
            ],
            style: Default::default(),
            source: None,
        };

        apply_sentence_structure(&mut block, "en");

        assert_eq!(
            inline_text(&block.content),
            "Literature creates, as Ryan puts it, “possible worlds.” ^{11} \nKittler’s proposition follows. \nAnother sentence follows."
        );
        let formula = block
            .content
            .iter()
            .position(|inline| matches!(inline, Inline::Math(_)))
            .unwrap();
        let first_break = block
            .content
            .iter()
            .position(|inline| matches!(inline, Inline::Break))
            .unwrap();
        assert!(formula < first_break);
        assert!(!is_ocr_superscript_reference(false, "x^{11}"));
        assert!(!is_ocr_superscript_reference(true, "^{11}"));
    }

    #[test]
    fn inline_footnote_sentences_do_not_become_separate_subparagraphs() {
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "正文。".to_owned(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "脚注第一句。脚注第二句。".to_owned(),
                    style: rebook_publication::TextStyle {
                        inline_role: InlineRole::Footnote,
                        ..Default::default()
                    },
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "下文。".to_owned(),
                    style: Default::default(),
                    link: None,
                }),
            ],
            style: Default::default(),
            source: None,
        };

        apply_sentence_structure(&mut block, "zh");

        assert_eq!(
            inline_text(&block.content),
            "正文。脚注第一句。脚注第二句。\n下文。"
        );
    }
}
