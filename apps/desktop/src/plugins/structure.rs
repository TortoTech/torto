use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rebook_publication::{
    Block, Book, BookSource, Inline, InlineRole, LinkRole, PublicationError, PublicationUrl,
    RasterResource, Resource, Section, TextBaseline, TextBlock, TextBlockKind, TextRun,
};
use rebook_reader::sentence_byte_ranges;

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
    state: RwLock<StructureState>,
}

impl ParagraphStructureSource {
    pub(crate) fn new(inner: Arc<dyn BookSource>) -> Self {
        Self {
            inner,
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
        let Some(primary) = section.blocks.iter().find_map(|block| {
            matches!(block, Block::Text(text) if text.source.as_ref().is_some_and(|range| range.start.node == key.node))
                .then_some(block)
                .and_then(|block| match block {
                    Block::Text(text) => Some(text),
                    _ => None,
                })
        }) else {
            return Ok(false);
        };
        if primary.kind != TextBlockKind::Paragraph {
            return Ok(false);
        }
        Ok(paragraph_atoms_for_content(&primary.content).len() >= 2)
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
            let active = match &section.blocks[block_index] {
                Block::Text(text) => text.source.as_ref().and_then(|range| {
                    let key = ParagraphStructureKey {
                        section_index: index,
                        node: range.start.node.clone(),
                    };
                    state.active.get(&key).copied()
                }),
                _ => None,
            }
            .unwrap_or(false);
            if !active {
                block_index += 1;
                continue;
            }
            if let Block::Text(primary) = &mut section.blocks[block_index]
                && primary.kind == TextBlockKind::Paragraph
            {
                apply_sentence_structure(primary);
            }
            if let Some(Block::Text(companion)) = section.blocks.get_mut(block_index + 1)
                && companion.source.is_none()
                && companion.kind == TextBlockKind::Paragraph
            {
                apply_sentence_structure(companion);
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

fn paragraph_atoms_for_content(content: &[Inline]) -> Vec<ParagraphAtom> {
    let text = inline_text(content);
    let mut cursor = 0;
    let mut protected = Vec::new();
    let mut footnotes = Vec::new();
    for inline in content {
        let len = match inline {
            Inline::Text(run) => {
                let len = run.text.chars().count();
                if is_focus_footnote(run) && len > 0 {
                    footnotes.push(cursor..cursor + len);
                }
                len
            }
            Inline::Math(run) => {
                let len = run.latex.chars().count();
                if is_ocr_superscript_reference(run.display, &run.latex) && len > 0 {
                    footnotes.push(cursor..cursor + len);
                }
                protected.extend((cursor + 1)..(cursor + len));
                len
            }
            Inline::Image(_) => 0,
            Inline::Break => 1,
        };
        cursor += len;
    }
    let atoms = paragraph_atoms_with_protected_boundaries(&text, &protected);
    let atoms = attach_paired_punctuation_atoms(atoms, &text);
    attach_footnote_atoms(atoms, &text, &footnotes)
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
    attached
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

fn paragraph_atoms_with_protected_boundaries(
    text: &str,
    protected_boundaries: &[usize],
) -> Vec<ParagraphAtom> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut atoms: Vec<ParagraphAtom> = Vec::new();
    let mut start = 0;
    let mut end = 0;
    for range in sentence_byte_ranges(text) {
        end += text[range].chars().count();
        if protected_boundaries.contains(&end) {
            continue;
        }
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
    atoms
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

fn apply_sentence_structure(block: &mut TextBlock) {
    let atoms = paragraph_atoms_for_content(&block.content);
    if atoms.len() < 2 {
        return;
    }
    let original = std::mem::take(&mut block.content);
    let mut content = Vec::new();
    for (index, atom) in atoms.iter().enumerate() {
        if index > 0 {
            content.push(Inline::Break);
            content.push(Inline::Break);
        }
        content.extend(slice_inlines(&original, atom.start, atom.end));
    }
    block.content = content;
    block.style.subparagraph_gap_em = Some(0.3);
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
    fn atoms_cover_cjk_and_latin_sentences_without_rewriting() {
        let text = "首先，观察系统。其次，比较反馈；Finally, decide.";
        let atoms = paragraph_atoms_with_protected_boundaries(text, &[]);
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
        apply_sentence_structure(&mut block);
        assert_eq!(
            inline_text(&block.content),
            "第一句。\n\n第二句！\n\n第三句？"
        );
        assert_eq!(block.style.subparagraph_gap_em, Some(0.3));
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
        let atoms = paragraph_atoms_for_content(&content);
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

        apply_sentence_structure(&mut block);

        assert_eq!(inline_text(&block.content), "第一句。\n\n第二句。54");
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

        apply_sentence_structure(&mut block);

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
            "分手时，她说：“朝朝暮暮，阳台之下。”【8】\n\n这里天地交媾的古老宇宙形象已经变成一个美丽的故事。\n\n不过应当注意。"
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

        apply_sentence_structure(&mut block);

        assert_eq!(
            inline_text(&block.content),
            "他说：“唯女子与小人为难养也。近之则不孙，远之则怨。”（《论语》卷十七）【5】\n\n话讲得机智却相当刻薄。\n\n无论如何，妇女的地位非常低下。"
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

        apply_sentence_structure(&mut block);

        assert_eq!(
            inline_text(&block.content),
            "Literature creates, as Ryan puts it, “possible worlds.” ^{11} \n\nKittler’s proposition follows. \n\nAnother sentence follows."
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

        apply_sentence_structure(&mut block);

        assert_eq!(
            inline_text(&block.content),
            "正文。脚注第一句。脚注第二句。\n\n下文。"
        );
    }
}
