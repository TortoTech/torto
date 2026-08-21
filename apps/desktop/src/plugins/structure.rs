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
                protected.extend((cursor + 1)..(cursor + len));
                len
            }
            Inline::Break => 1,
        };
        cursor += len;
    }
    attach_footnote_atoms(
        paragraph_atoms_with_protected_boundaries(&text, &protected),
        &text,
        &footnotes,
    )
}

fn is_focus_footnote(run: &TextRun) -> bool {
    run.style.inline_role == InlineRole::Footnote
        || (run
            .link
            .as_ref()
            .is_some_and(|target| target.fragment().is_some())
            && (run.style.link_role == LinkRole::FootnoteReference
                || run.style.baseline == TextBaseline::Superscript))
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
    let mut attached: Vec<ParagraphAtom> = Vec::with_capacity(atoms.len());
    for atom in atoms {
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
            Inline::Break => 1,
        };
        let inline_start = cursor;
        let inline_end = cursor + len;
        cursor = inline_end;
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
