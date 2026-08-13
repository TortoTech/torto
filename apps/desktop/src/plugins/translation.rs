use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use rebook_publication::{
    Block, BlockStyle, Book, BookSource, FixedPageTextLayer, FixedPageTextRect,
    FixedPageTextReplacement, FixedPageTextReplacementSegment, Inline, PublicationError,
    PublicationUrl, RasterResource, RenditionLayout, Resource, Section, SectionAnchor, SourceRange,
    TextBaseline, TextBlock, TextBlockKind, TextRun, TextStyle,
};

use super::TranslationMode;
#[cfg(test)]
use super::search::text_block_text;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationBlockInput {
    pub block_index: usize,
    pub segment_index: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTranslation {
    pub block_index: usize,
    pub segment_index: Option<usize>,
    pub text: String,
}

#[derive(Default)]
struct StoredBlockTranslation {
    whole: Option<String>,
    segments: HashMap<usize, String>,
}

#[derive(Default)]
struct TranslationState {
    enabled: bool,
    mode: TranslationMode,
    sections: HashMap<usize, HashMap<usize, StoredBlockTranslation>>,
}

/// An in-memory view of a publication that overlays translated text blocks.
/// The canonical source remains unchanged, so translation can be toggled off
/// without reopening the book.
pub struct TranslationBookSource {
    inner: Arc<dyn BookSource>,
    fixed_page_replacement_only: AtomicBool,
    state: RwLock<TranslationState>,
}

impl TranslationBookSource {
    pub fn new(inner: Arc<dyn BookSource>, mode: TranslationMode) -> Self {
        let fixed_page_replacement_only =
            inner.book().metadata.layout == RenditionLayout::PrePaginated;
        Self::with_fixed_page_policy(inner, mode, fixed_page_replacement_only)
    }

    pub fn new_fixed_page(inner: Arc<dyn BookSource>, mode: TranslationMode) -> Self {
        Self::with_fixed_page_policy(inner, mode, true)
    }

    fn with_fixed_page_policy(
        inner: Arc<dyn BookSource>,
        mode: TranslationMode,
        fixed_page_replacement_only: bool,
    ) -> Self {
        let mode = normalized_translation_mode(fixed_page_replacement_only, mode);
        Self {
            inner,
            fixed_page_replacement_only: AtomicBool::new(fixed_page_replacement_only),
            state: RwLock::new(TranslationState {
                mode,
                ..TranslationState::default()
            }),
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        self.state
            .write()
            .map_err(|_| "正文翻译状态已损坏".to_owned())?
            .enabled = enabled;
        Ok(())
    }

    pub fn set_mode(&self, mode: TranslationMode) -> Result<(), String> {
        let mode = normalized_translation_mode(self.fixed_page_replacement_only(), mode);
        self.state
            .write()
            .map_err(|_| "正文翻译状态已损坏".to_owned())?
            .mode = mode;
        Ok(())
    }

    pub fn set_fixed_page_replacement_only(&self, fixed_page: bool) {
        self.fixed_page_replacement_only
            .store(fixed_page, Ordering::Release);
    }

    fn fixed_page_replacement_only(&self) -> bool {
        self.fixed_page_replacement_only.load(Ordering::Acquire)
    }

    pub fn clear(&self) -> Result<(), String> {
        self.state
            .write()
            .map_err(|_| "正文翻译状态已损坏".to_owned())?
            .sections
            .clear();
        Ok(())
    }

    #[cfg(test)]
    pub fn translatable_blocks(
        &self,
        section_index: usize,
    ) -> Result<Vec<TranslationBlockInput>, String> {
        let section = self
            .inner
            .parse_section(section_index)
            .map_err(|error| format!("解析第 {} 节失败：{error}", section_index + 1))?;
        Ok(translatable_blocks(
            &section,
            self.fixed_page_replacement_only(),
        ))
    }

    pub fn untranslated_blocks_for_ranges(
        &self,
        section_index: usize,
        visible_ranges: &[SourceRange],
    ) -> Result<Vec<TranslationBlockInput>, String> {
        let section = self
            .inner
            .parse_section(section_index)
            .map_err(|error| format!("解析第 {} 节失败：{error}", section_index + 1))?;
        let state = self
            .state
            .read()
            .map_err(|_| "正文翻译状态已损坏".to_owned())?;
        let stored = state.sections.get(&section_index);
        Ok(
            translatable_blocks(&section, self.fixed_page_replacement_only())
                .into_iter()
                .filter(|input| {
                    section
                        .blocks
                        .get(input.block_index)
                        .and_then(|block| {
                            translation_input_source_range(block, input.segment_index)
                        })
                        .is_some_and(|source| {
                            visible_ranges
                                .iter()
                                .any(|visible| source_range_nodes_overlap(source, visible))
                        })
                        && !stored.is_some_and(|translations| {
                            translations
                                .get(&input.block_index)
                                .is_some_and(|translation| {
                                    input.segment_index.map_or_else(
                                        || translation.whole.is_some(),
                                        |segment| translation.segments.contains_key(&segment),
                                    )
                                })
                        })
                })
                .collect(),
        )
    }

    pub fn store_batch(
        &self,
        section_index: usize,
        translations: &[BlockTranslation],
    ) -> Result<(), String> {
        let mut state = self
            .state
            .write()
            .map_err(|_| "正文翻译状态已损坏".to_owned())?;
        let values = state.sections.entry(section_index).or_default();
        merge_translations(values, translations);
        Ok(())
    }

    #[cfg(test)]
    pub fn store_section(
        &self,
        section_index: usize,
        translations: &[BlockTranslation],
    ) -> Result<(), String> {
        let mut values = HashMap::<usize, StoredBlockTranslation>::new();
        merge_translations(&mut values, translations);
        self.state
            .write()
            .map_err(|_| "正文翻译状态已损坏".to_owned())?
            .sections
            .insert(section_index, values);
        Ok(())
    }
}

fn translatable_blocks(section: &Section, is_pdf: bool) -> Vec<TranslationBlockInput> {
    let mut blocks = Vec::new();
    for (block_index, block) in section.blocks.iter().enumerate() {
        match block {
            Block::Text(block) => {
                let text = translation_text(block);
                if !text.trim().is_empty() {
                    blocks.push(TranslationBlockInput {
                        block_index,
                        segment_index: None,
                        text,
                    });
                }
            }
            Block::Image(image) if is_pdf => {
                let Some(layer) = image.text_layer.as_ref() else {
                    continue;
                };
                blocks.extend(
                    fixed_page_text_groups(layer)
                        .into_iter()
                        .filter(|segment| fixed_page_group_is_translatable(&segment.text))
                        .map(|segment| TranslationBlockInput {
                            block_index,
                            segment_index: Some(segment.index),
                            text: segment.text,
                        }),
                );
            }
            Block::Image(image) => {
                let text = image
                    .text_layer
                    .as_ref()
                    .map(|layer| layer.text.clone())
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    blocks.push(TranslationBlockInput {
                        block_index,
                        segment_index: None,
                        text,
                    });
                }
            }
            Block::Table(table) => {
                blocks.extend(
                    table
                        .rows
                        .iter()
                        .flat_map(|row| &row.cells)
                        .enumerate()
                        .filter_map(|(cell_index, cell)| {
                            let text = translation_text(&cell.text);
                            (!text.trim().is_empty()).then_some(TranslationBlockInput {
                                block_index,
                                segment_index: Some(cell_index),
                                text,
                            })
                        }),
                );
            }
            Block::Separator | Block::PageBreak => {}
        }
    }
    blocks
}

fn merge_translations(
    values: &mut HashMap<usize, StoredBlockTranslation>,
    translations: &[BlockTranslation],
) {
    for translation in translations
        .iter()
        .filter(|translation| !translation.text.trim().is_empty())
    {
        let stored = values.entry(translation.block_index).or_default();
        if let Some(segment_index) = translation.segment_index {
            stored
                .segments
                .insert(segment_index, translation.text.clone());
        } else {
            stored.whole = Some(translation.text.clone());
        }
    }
}

fn block_source_range(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(block) => block.source.as_ref(),
        Block::Table(block) => block.source.as_ref(),
        Block::Image(block) => block.source.as_ref(),
        Block::Separator | Block::PageBreak => None,
    }
}

fn translation_input_source_range(
    block: &Block,
    segment_index: Option<usize>,
) -> Option<&SourceRange> {
    if let (Block::Table(table), Some(segment_index)) = (block, segment_index) {
        return table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .nth(segment_index)
            .and_then(|cell| cell.text.source.as_ref());
    }
    block_source_range(block)
}

fn source_range_nodes_overlap(source: &SourceRange, visible: &SourceRange) -> bool {
    source.start.spine == visible.start.spine
        && (source.start.node == visible.start.node
            || source.start.node == visible.end.node
            || source.end.node == visible.start.node
            || source.end.node == visible.end.node)
}

impl BookSource for TranslationBookSource {
    fn book(&self) -> &Book {
        self.inner.book()
    }

    fn table_of_contents_origin(&self) -> rebook_publication::TableOfContentsOrigin {
        self.inner.table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let mut section = self.inner.parse_section(index)?;
        let is_pdf = self.fixed_page_replacement_only();
        let state = self
            .state
            .read()
            .map_err(|_| PublicationError::InvalidPublication("正文翻译状态已损坏".to_owned()))?;
        let Some(translations) = active_translations(&state, index) else {
            return Ok(section);
        };
        let mode = normalized_translation_mode(is_pdf, state.mode);
        let mut rendered = Vec::with_capacity(section.blocks.len() * 2);
        for (block_index, block) in section.blocks.into_iter().enumerate() {
            let Some(translation) = translations.get(&block_index) else {
                rendered.push(block);
                continue;
            };
            match block {
                Block::Text(mut original) => {
                    let Some(translation) = translation.whole.as_deref() else {
                        rendered.push(Block::Text(original));
                        continue;
                    };
                    let style = original
                        .content
                        .iter()
                        .find_map(|inline| match inline {
                            Inline::Text(run) => Some(run.style),
                            Inline::Math(_) | Inline::Break => None,
                        })
                        .unwrap_or_default();
                    match mode {
                        TranslationMode::Replace => {
                            original.content =
                                replacement_content(translation, style, Some(&original.content));
                            update_translated_source(
                                original.source.as_mut(),
                                translation,
                                &mut section.anchors,
                            );
                            rendered.push(Block::Text(original));
                        }
                        TranslationMode::Bilingual => {
                            let mut translated = original.clone();
                            let original_margin_after = original.style.margin_after;
                            original.style.margin_after = original_margin_after.min(6.0);
                            translated.content =
                                replacement_content(translation, style, Some(&original.content));
                            translated.source = None;
                            translated.style.margin_before = 0.0;
                            translated.style.margin_after = original_margin_after;
                            rendered.push(Block::Text(original));
                            rendered.push(Block::Text(translated));
                        }
                    }
                }
                Block::Image(mut image) if image.text_layer.is_some() && is_pdf => {
                    apply_fixed_page_translation(
                        &mut image,
                        &translation.segments,
                        &mut section.anchors,
                    );
                    rendered.push(Block::Image(image));
                }
                Block::Image(image) if image.text_layer.is_some() => {
                    let Some(translation) = translation.whole.as_deref() else {
                        rendered.push(Block::Image(image));
                        continue;
                    };
                    let translated = translated_fixed_page_block(
                        translation,
                        (mode == TranslationMode::Replace)
                            .then(|| image.source.clone())
                            .flatten(),
                    );
                    if mode == TranslationMode::Replace {
                        let mut translated = translated;
                        update_translated_source(
                            translated.source.as_mut(),
                            translation,
                            &mut section.anchors,
                        );
                        rendered.push(Block::Text(translated));
                    } else {
                        rendered.push(Block::Image(image));
                        rendered.push(Block::Text(translated));
                    }
                }
                Block::Table(table) if !translation.segments.is_empty() => {
                    rendered.push(Block::Table(translated_table(
                        table,
                        &translation.segments,
                        mode,
                        &mut section.anchors,
                    )));
                }
                other => rendered.push(other),
            }
        }
        section.blocks = rendered;
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
}

fn active_translations(
    state: &TranslationState,
    section_index: usize,
) -> Option<&HashMap<usize, StoredBlockTranslation>> {
    state
        .enabled
        .then(|| state.sections.get(&section_index))
        .flatten()
}

fn translated_table(
    mut table: rebook_publication::TableBlock,
    translations: &HashMap<usize, String>,
    mode: TranslationMode,
    anchors: &mut [SectionAnchor],
) -> rebook_publication::TableBlock {
    for (cell_index, cell) in table
        .rows
        .iter_mut()
        .flat_map(|row| &mut row.cells)
        .enumerate()
    {
        let Some(translated) = translations.get(&cell_index) else {
            continue;
        };
        let style = cell
            .text
            .content
            .iter()
            .find_map(|inline| match inline {
                Inline::Text(run) => Some(run.style),
                Inline::Math(_) | Inline::Break => None,
            })
            .unwrap_or_default();
        if mode == TranslationMode::Replace {
            let original = cell.text.content.clone();
            cell.text.content = replacement_content(translated, style, Some(&original));
            update_translated_source(cell.text.source.as_mut(), translated, anchors);
        } else {
            let original = cell.text.content.clone();
            cell.text.content.push(Inline::Break);
            cell.text
                .content
                .extend(replacement_content(translated, style, Some(&original)));
        }
    }
    table
}

fn normalized_translation_mode(
    fixed_page_replacement_only: bool,
    mode: TranslationMode,
) -> TranslationMode {
    if fixed_page_replacement_only {
        TranslationMode::Replace
    } else {
        mode
    }
}

fn apply_fixed_page_translation(
    image: &mut rebook_publication::ImageBlock,
    translations: &HashMap<usize, String>,
    anchors: &mut [SectionAnchor],
) {
    let Some(layer) = image.text_layer.as_mut() else {
        return;
    };
    let mut source_offset = 0_u64;
    let segments = fixed_page_text_groups(layer)
        .into_iter()
        .filter_map(|segment| {
            let text = translations.get(&segment.index)?.trim().to_owned();
            if text.is_empty() {
                return None;
            }
            let replacement = FixedPageTextReplacementSegment {
                text: text.clone(),
                rect: segment.rect,
                source_offset,
            };
            source_offset = source_offset
                .saturating_add(u64::try_from(text.chars().count()).unwrap_or(u64::MAX))
                .saturating_add(1);
            Some(replacement)
        })
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return;
    }
    let translated_page = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    update_translated_source(image.source.as_mut(), &translated_page, anchors);
    layer.replacement = Some(FixedPageTextReplacement { segments });
}

struct FixedPageSourceSegment {
    index: usize,
    text: String,
    rect: FixedPageTextRect,
}

fn fixed_page_text_groups(layer: &FixedPageTextLayer) -> Vec<FixedPageSourceSegment> {
    let mut groups = Vec::<Vec<FixedPageSourceSegment>>::new();
    for segment in fixed_page_text_segments(layer) {
        let belongs_to_previous = groups
            .last()
            .is_some_and(|group| fixed_page_lines_share_paragraph(group, &segment));
        if !belongs_to_previous {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(segment);
    }

    groups
        .into_iter()
        .enumerate()
        .filter_map(|(index, group)| {
            group.first()?;
            let text = join_fixed_page_lines(group.iter().map(|segment| segment.text.as_str()));
            if text.is_empty() {
                return None;
            }
            let (x0, y0, x1, y1) = group.iter().fold(
                (
                    f32::INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::NEG_INFINITY,
                ),
                |bounds, segment| {
                    (
                        bounds.0.min(segment.rect.x),
                        bounds.1.min(segment.rect.y),
                        bounds.2.max(segment.rect.x + segment.rect.width),
                        bounds.3.max(segment.rect.y + segment.rect.height),
                    )
                },
            );
            Some(FixedPageSourceSegment {
                index,
                text,
                rect: FixedPageTextRect {
                    x: x0,
                    y: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                },
            })
        })
        .collect()
}

fn fixed_page_lines_share_paragraph(
    group: &[FixedPageSourceSegment],
    current: &FixedPageSourceSegment,
) -> bool {
    let Some(previous) = group.last() else {
        return false;
    };
    let line_height = previous.rect.height.max(current.rect.height).max(1.0);
    let vertical_step = current.rect.y - previous.rect.y;
    let vertical_gap = current.rect.y - (previous.rect.y + previous.rect.height);
    if vertical_step <= line_height * 0.25 || vertical_gap > line_height * 0.45 {
        return false;
    }
    let height_ratio = previous.rect.height.min(current.rect.height) / line_height;
    if height_ratio < 0.68 {
        return false;
    }
    let first = &group[0];
    if looks_like_section_heading(&first.text) {
        return false;
    }
    if current.rect.x > first.rect.x + line_height * 2.5 {
        return false;
    }
    if current.rect.x + line_height * 4.0 < first.rect.x
        && current.rect.width > first.rect.width * 1.25
    {
        return false;
    }
    if current.text.chars().count() < 20
        && current.rect.width < previous.rect.width * 0.35
        && previous.text.chars().count() > 40
    {
        return false;
    }
    if group.len() > 1
        && current.rect.width > first.rect.width * 1.35
        && previous.rect.width < current.rect.width * 0.8
    {
        return false;
    }
    let overlap = (previous.rect.x + previous.rect.width).min(current.rect.x + current.rect.width)
        - previous.rect.x.max(current.rect.x);
    let minimum_width = previous.rect.width.min(current.rect.width).max(1.0);
    overlap / minimum_width >= 0.35 || (current.rect.x - previous.rect.x).abs() <= line_height * 2.5
}

fn looks_like_section_heading(text: &str) -> bool {
    let Some(number) = text.split_whitespace().next() else {
        return false;
    };
    text.chars().count() < 80
        && number.contains('.')
        && number
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
}

fn fixed_page_group_is_translatable(text: &str) -> bool {
    text.chars()
        .filter(|character| character.is_alphabetic())
        .take(2)
        .count()
        >= 2
}

fn join_fixed_page_lines<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let mut joined = String::new();
    for line in lines {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(&line);
            continue;
        }
        let joins_hyphenated_word =
            joined.ends_with('-') && line.chars().next().is_some_and(char::is_lowercase);
        if joins_hyphenated_word {
            joined.pop();
        } else {
            joined.push(' ');
        }
        joined.push_str(&line);
    }
    joined
}

fn fixed_page_text_segments(layer: &FixedPageTextLayer) -> Vec<FixedPageSourceSegment> {
    let mut groups = Vec::<Vec<&rebook_publication::FixedPageTextSpan>>::new();
    for span in layer
        .spans
        .iter()
        .filter(|span| valid_fixed_rect(span.rect))
    {
        let starts_new_group =
            groups
                .last()
                .and_then(|group| group.last())
                .is_some_and(|previous| {
                    let between =
                        char_slice(&layer.text, previous.char_range.end, span.char_range.start);
                    let horizontal_gap = span.rect.x - (previous.rect.x + previous.rect.width);
                    let height = span.rect.height.max(previous.rect.height).max(1.0);
                    let vertical_shift = (span.rect.y - previous.rect.y).abs();
                    between.contains('\n')
                        || horizontal_gap > height * 2.5
                        || horizontal_gap < -height * 2.5
                        || vertical_shift > height * 0.75
                });
        if starts_new_group || groups.is_empty() {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(span);
    }

    groups
        .into_iter()
        .enumerate()
        .filter_map(|(index, group)| {
            let first = group.first()?;
            let last = group.last()?;
            let text = char_slice(&layer.text, first.char_range.start, last.char_range.end)
                .trim()
                .to_owned();
            if text.is_empty() {
                return None;
            }
            let (x0, y0, x1, y1) = group.iter().fold(
                (
                    f32::INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::NEG_INFINITY,
                ),
                |bounds, span| {
                    (
                        bounds.0.min(span.rect.x),
                        bounds.1.min(span.rect.y),
                        bounds.2.max(span.rect.x + span.rect.width),
                        bounds.3.max(span.rect.y + span.rect.height),
                    )
                },
            );
            let padding = 1.5;
            let x = (x0 - padding).max(0.0);
            let y = (y0 - padding).max(0.0);
            let right = (x1 + padding).min(layer.width);
            let bottom = (y1 + padding).min(layer.height);
            (right > x && bottom > y).then_some(FixedPageSourceSegment {
                index,
                text,
                rect: FixedPageTextRect {
                    x,
                    y,
                    width: right - x,
                    height: bottom - y,
                },
            })
        })
        .collect()
}

fn valid_fixed_rect(rect: FixedPageTextRect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
}

fn char_slice(text: &str, start: u64, end: u64) -> &str {
    let start = usize::try_from(start).unwrap_or(usize::MAX);
    let end = usize::try_from(end).unwrap_or(usize::MAX);
    let start = text
        .char_indices()
        .nth(start)
        .map_or(text.len(), |(index, _)| index);
    let end = text
        .char_indices()
        .nth(end)
        .map_or(text.len(), |(index, _)| index);
    text.get(start.min(end)..end).unwrap_or_default()
}

fn translated_fixed_page_block(text: &str, source: Option<SourceRange>) -> TextBlock {
    TextBlock {
        kind: TextBlockKind::Paragraph,
        content: replacement_content(text, TextStyle::default(), None),
        style: BlockStyle {
            margin_before: 16.0,
            margin_after: 16.0,
            ..BlockStyle::default()
        },
        source,
    }
}

fn update_translated_source(
    source: Option<&mut SourceRange>,
    translation: &str,
    anchors: &mut [SectionAnchor],
) {
    let Some(source) = source else {
        return;
    };
    source.end.spine = source.start.spine.clone();
    source.end.node.clone_from(&source.start.node);
    source.end.text_offset = source.start.text_offset.saturating_add(
        u64::try_from(translation_visible_char_count(translation)).unwrap_or(u64::MAX),
    );
    for anchor in anchors {
        if anchor.source.spine == source.start.spine && anchor.source.node == source.start.node {
            anchor.source.text_offset = anchor
                .source
                .text_offset
                .clamp(source.start.text_offset, source.end.text_offset);
        }
    }
}

fn translation_text(block: &TextBlock) -> String {
    let mut text = String::new();
    for inline in &block.content {
        match inline {
            Inline::Text(run) => match run.style.baseline {
                TextBaseline::Normal => text.push_str(&run.text),
                TextBaseline::Superscript => {
                    text.push_str("<sup>");
                    text.push_str(&run.text);
                    text.push_str("</sup>");
                }
                TextBaseline::Subscript => {
                    text.push_str("<sub>");
                    text.push_str(&run.text);
                    text.push_str("</sub>");
                }
            },
            Inline::Math(run) => {
                text.push('$');
                text.push_str(&run.latex);
                text.push('$');
            }
            Inline::Break => text.push('\n'),
        }
    }
    text
}

fn translation_visible_char_count(text: &str) -> usize {
    text.replace("<sup>", "")
        .replace("</sup>", "")
        .replace("<sub>", "")
        .replace("</sub>", "")
        .chars()
        .count()
}

fn replacement_content(text: &str, style: TextStyle, original: Option<&[Inline]>) -> Vec<Inline> {
    let styled = parse_baseline_markup(text, style)
        .unwrap_or_else(|| restore_original_baselines(text, style, original.unwrap_or_default()));
    let mut content = Vec::new();
    for (text, style) in styled {
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                content.push(Inline::Break);
            }
            if !line.is_empty() {
                content.push(Inline::Text(TextRun {
                    text: line.to_owned(),
                    style,
                    link: None,
                }));
            }
        }
    }
    content
}

fn parse_baseline_markup(text: &str, default_style: TextStyle) -> Option<Vec<(String, TextStyle)>> {
    let mut rest = text;
    let mut styled = Vec::new();
    let mut found = false;
    while let Some((start, baseline, open, close)) = next_baseline_tag(rest) {
        if start > 0 {
            styled.push((rest[..start].to_owned(), default_style));
        }
        let value_start = start + open.len();
        let relative_end = rest[value_start..].find(close)?;
        let value_end = value_start + relative_end;
        styled.push((
            rest[value_start..value_end].to_owned(),
            baseline_style(default_style, baseline),
        ));
        rest = &rest[value_end + close.len()..];
        found = true;
    }
    if !rest.is_empty() {
        styled.push((rest.to_owned(), default_style));
    }
    found.then_some(styled)
}

fn next_baseline_tag(text: &str) -> Option<(usize, TextBaseline, &'static str, &'static str)> {
    let superscript = text
        .find("<sup>")
        .map(|index| (index, TextBaseline::Superscript, "<sup>", "</sup>"));
    let subscript = text
        .find("<sub>")
        .map(|index| (index, TextBaseline::Subscript, "<sub>", "</sub>"));
    match (superscript, subscript) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn baseline_style(mut style: TextStyle, baseline: TextBaseline) -> TextStyle {
    style.baseline = baseline;
    style.size_scale *= 0.75;
    style
}

fn restore_original_baselines(
    text: &str,
    default_style: TextStyle,
    original: &[Inline],
) -> Vec<(String, TextStyle)> {
    let original_length = original
        .iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.chars().count(),
            Inline::Break => 1,
            Inline::Math(_) => 0,
        })
        .sum::<usize>()
        .max(1);
    let mut original_offset = 0;
    let markers = original
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(run) => {
                let offset = original_offset;
                original_offset += run.text.chars().count();
                (run.style.baseline != TextBaseline::Normal && !run.text.is_empty()).then_some((
                    run.text.as_str(),
                    run.style,
                    offset.saturating_mul(1_000) / original_length,
                ))
            }
            Inline::Break => {
                original_offset += 1;
                None
            }
            Inline::Math(_) => None,
        })
        .collect::<Vec<_>>();
    if markers.is_empty() {
        return vec![(text.to_owned(), default_style)];
    }

    let target_length = text.chars().count().max(1);
    let mut cursor = 0;
    let mut styled = Vec::new();
    for (marker, marker_style, relative_offset) in markers {
        let Some(start) = best_marker_match(text, marker, cursor, relative_offset, target_length)
        else {
            continue;
        };
        if start > cursor {
            styled.push((text[cursor..start].to_owned(), default_style));
        }
        let end = start + marker.len();
        styled.push((text[start..end].to_owned(), marker_style));
        cursor = end;
    }
    if cursor < text.len() {
        styled.push((text[cursor..].to_owned(), default_style));
    }
    if styled.is_empty() {
        vec![(text.to_owned(), default_style)]
    } else {
        styled
    }
}

fn best_marker_match(
    text: &str,
    marker: &str,
    cursor: usize,
    relative_offset: usize,
    target_length: usize,
) -> Option<usize> {
    text[cursor..]
        .match_indices(marker)
        .map(|(offset, _)| cursor + offset)
        .min_by(|left, right| {
            let score = |byte_index: usize| {
                let char_index = text[..byte_index].chars().count();
                char_index
                    .saturating_mul(1_000)
                    .abs_diff(relative_offset.saturating_mul(target_length))
            };
            score(*left).cmp(&score(*right))
        })
}

#[cfg(test)]
mod tests {
    use rebook_publication::{
        BlockStyle, FixedPageTextLayer, FixedPageTextRect, FixedPageTextSpan, ImageBlock,
        ImageStyle, Metadata, PublicationId, RenditionLayout, SourceAnchor, SourceRange, SpineItem,
        SpineItemId, TextBlock, TextBlockKind,
    };

    use super::*;

    struct TestSource {
        book: Book,
        section: Section,
    }

    impl BookSource for TestSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            Ok(self.section.clone())
        }

        fn resource(&self, _href: &PublicationUrl) -> Result<Resource, PublicationError> {
            unreachable!()
        }
    }

    fn source() -> Arc<dyn BookSource> {
        let spine = SpineItemId::new("chapter").unwrap();
        let href = PublicationUrl::parse("chapter.xhtml").unwrap();
        Arc::new(TestSource {
            book: Book {
                id: PublicationId::new("translation-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: vec![SpineItem {
                    id: spine.clone(),
                    href: href.clone(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            section: Section {
                id: spine.clone(),
                href,
                blocks: vec![Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: "Hello".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(SourceRange {
                        start: SourceAnchor {
                            spine: spine.clone(),
                            node: "p-1".into(),
                            text_offset: 0,
                        },
                        end: SourceAnchor {
                            spine,
                            node: "p-1".into(),
                            text_offset: 5,
                        },
                    }),
                })],
                anchors: Vec::new(),
            },
        })
    }

    fn fixed_page_source() -> Arc<dyn BookSource> {
        let spine = SpineItemId::new("pdf-page").unwrap();
        let href = PublicationUrl::parse("page.xhtml").unwrap();
        let image_href = PublicationUrl::parse("page.png").unwrap();
        Arc::new(TestSource {
            book: Book {
                id: PublicationId::new("translation-pdf-test").unwrap(),
                metadata: Metadata {
                    layout: RenditionLayout::PrePaginated,
                    ..Metadata::default()
                },
                cover: None,
                sections: vec![SpineItem {
                    id: spine.clone(),
                    href: href.clone(),
                    // PdfPublication reuses DirectBookSource, whose fixed-page
                    // descriptors are XHTML even though the opened format is PDF.
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            section: Section {
                id: spine.clone(),
                href,
                blocks: vec![Block::Image(ImageBlock {
                    href: image_href,
                    alt: "PDF page".into(),
                    style: ImageStyle::default(),
                    source: Some(SourceRange {
                        start: SourceAnchor {
                            spine: spine.clone(),
                            node: "pdf-page-text".into(),
                            text_offset: 0,
                        },
                        end: SourceAnchor {
                            spine,
                            node: "pdf-page-text".into(),
                            text_offset: 9,
                        },
                    }),
                    text_layer: Some(FixedPageTextLayer {
                        width: 100.0,
                        height: 100.0,
                        text: "Hello PDF".into(),
                        spans: vec![FixedPageTextSpan {
                            char_range: 0..9,
                            rect: FixedPageTextRect {
                                x: 10.0,
                                y: 10.0,
                                width: 60.0,
                                height: 12.0,
                            },
                        }],
                        replacement: None,
                    }),
                })],
                anchors: Vec::new(),
            },
        })
    }

    fn block_text(block: &Block) -> String {
        let Block::Text(block) = block else {
            panic!("expected text block");
        };
        text_block_text(block)
    }

    #[test]
    fn translation_preserves_and_restores_baseline_markers() {
        let superscript = TextStyle {
            baseline: TextBaseline::Superscript,
            size_scale: 0.75,
            ..TextStyle::default()
        };
        let original = vec![
            Inline::Text(TextRun {
                text: "Meaning".into(),
                style: TextStyle::default(),
                link: None,
            }),
            Inline::Text(TextRun {
                text: "4".into(),
                style: superscript,
                link: None,
            }),
        ];
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: original.clone(),
            style: BlockStyle::default(),
            source: None,
        };

        assert_eq!(translation_text(&block), "Meaning<sup>4</sup>");
        let marked = replacement_content("含义<sup>4</sup>", TextStyle::default(), Some(&original));
        assert!(matches!(
            marked.as_slice(),
            [Inline::Text(body), Inline::Text(note)]
                if body.text == "含义"
                    && note.text == "4"
                    && note.style.baseline == TextBaseline::Superscript
        ));

        let cached = replacement_content(
            "这是正文。4 然而继续。",
            TextStyle::default(),
            Some(&original),
        );
        assert!(cached.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "4" && run.style.baseline == TextBaseline::Superscript
        )));
    }

    #[test]
    fn toggles_between_original_replace_and_bilingual_views() {
        let source = TranslationBookSource::new(source(), TranslationMode::Replace);
        source
            .store_section(
                0,
                &[BlockTranslation {
                    block_index: 0,
                    segment_index: None,
                    text: "你好".into(),
                }],
            )
            .unwrap();

        assert_eq!(
            block_text(&source.parse_section(0).unwrap().blocks[0]),
            "Hello"
        );

        source.set_enabled(true).unwrap();
        let replaced = source.parse_section(0).unwrap();
        assert_eq!(replaced.blocks.len(), 1);
        assert_eq!(block_text(&replaced.blocks[0]), "你好");
        assert!(matches!(&replaced.blocks[0], Block::Text(block) if block.source.is_some()));
        assert!(matches!(
            &replaced.blocks[0],
            Block::Text(block) if block.source.as_ref().unwrap().end.text_offset == 2
        ));

        source.set_mode(TranslationMode::Bilingual).unwrap();
        let bilingual = source.parse_section(0).unwrap();
        assert_eq!(bilingual.blocks.len(), 2);
        assert_eq!(block_text(&bilingual.blocks[0]), "Hello");
        assert_eq!(block_text(&bilingual.blocks[1]), "你好");
        assert!(matches!(&bilingual.blocks[1], Block::Text(block) if block.source.is_none()));
    }

    #[test]
    fn visible_window_requests_only_missing_blocks_and_batches_merge_immediately() {
        let original = source();
        let book = original.book().clone();
        let mut section = original.parse_section(0).unwrap();
        let mut hidden = section.blocks[0].clone();
        let Block::Text(hidden) = &mut hidden else {
            unreachable!();
        };
        hidden.content = vec![Inline::Text(TextRun {
            text: "Hidden".into(),
            style: TextStyle::default(),
            link: None,
        })];
        let hidden_source = hidden.source.as_mut().unwrap();
        hidden_source.start.node = "p-2".into();
        hidden_source.end.node = "p-2".into();
        hidden_source.end.text_offset = 6;
        section.blocks.push(Block::Text(hidden.clone()));
        let source = TranslationBookSource::new(
            Arc::new(TestSource { book, section }),
            TranslationMode::Replace,
        );
        let visible = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: "p-1".into(),
                text_offset: 1,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: "p-1".into(),
                text_offset: 4,
            },
        };

        let pending = source
            .untranslated_blocks_for_ranges(0, std::slice::from_ref(&visible))
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].block_index, 0);
        source
            .store_batch(
                0,
                &[BlockTranslation {
                    block_index: 0,
                    segment_index: None,
                    text: "你好".into(),
                }],
            )
            .unwrap();

        assert!(
            source
                .untranslated_blocks_for_ranges(0, &[visible])
                .unwrap()
                .is_empty()
        );
        source.set_enabled(true).unwrap();
        assert_eq!(
            block_text(&source.parse_section(0).unwrap().blocks[0]),
            "你好"
        );
    }

    #[test]
    fn translates_fixed_page_text_layers_for_text_based_pdfs() {
        let source =
            TranslationBookSource::new_fixed_page(fixed_page_source(), TranslationMode::Bilingual);
        let blocks = source.translatable_blocks(0).unwrap();
        assert_eq!(
            blocks,
            [TranslationBlockInput {
                block_index: 0,
                segment_index: Some(0),
                text: "Hello PDF".into(),
            }]
        );
        source
            .store_section(
                0,
                &[BlockTranslation {
                    block_index: 0,
                    segment_index: Some(0),
                    text: "你好，PDF".into(),
                }],
            )
            .unwrap();
        source.set_enabled(true).unwrap();

        let replaced = source.parse_section(0).unwrap();
        assert_eq!(replaced.blocks.len(), 1);
        assert!(matches!(
            &replaced.blocks[0],
            Block::Image(image)
                if image.text_layer.as_ref().unwrap().replacement.as_ref().unwrap().segments[0].text == "你好，PDF"
        ));
        assert!(matches!(
            &replaced.blocks[0],
            Block::Image(image) if image.source.as_ref().unwrap().end.text_offset == 6
        ));

        source.set_mode(TranslationMode::Bilingual).unwrap();
        let bilingual = source.parse_section(0).unwrap();
        assert_eq!(bilingual.blocks.len(), 1);
        assert!(matches!(&bilingual.blocks[0], Block::Image(_)));
        assert!(matches!(
            &bilingual.blocks[0],
            Block::Image(image)
                if image.text_layer.as_ref().unwrap().replacement.as_ref().unwrap().segments[0].text == "你好，PDF"
        ));
    }

    #[test]
    fn translates_ocr_reflow_table_cells_and_matches_their_visible_ranges() {
        let base = source();
        let book = base.book().clone();
        let descriptor = book.sections[0].clone();
        let section = rebook_html::parse_section(
            "<html><body><table><tr><td>Hello</td><td>World</td></tr></table></body></html>",
            &descriptor,
            |_| None,
        )
        .unwrap();
        let visible = match &section.blocks[0] {
            Block::Table(table) => table.rows[0].cells[0].text.source.clone().unwrap(),
            _ => panic!("expected table"),
        };
        let source = TranslationBookSource::new(
            Arc::new(TestSource { book, section }),
            TranslationMode::Replace,
        );
        let pending = source
            .untranslated_blocks_for_ranges(0, &[visible])
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].segment_index, Some(0));
        source
            .store_section(
                0,
                &[BlockTranslation {
                    block_index: 0,
                    segment_index: Some(0),
                    text: "你好".into(),
                }],
            )
            .unwrap();
        source.set_enabled(true).unwrap();
        let section = source.parse_section(0).unwrap();
        let Block::Table(table) = &section.blocks[0] else {
            panic!("expected translated table");
        };
        assert_eq!(text_block_text(&table.rows[0].cells[0].text), "你好");
        assert_eq!(text_block_text(&table.rows[0].cells[1].text), "World");
    }

    #[test]
    fn fixed_page_segments_split_lines_and_large_horizontal_gaps() {
        let layer = FixedPageTextLayer {
            width: 200.0,
            height: 100.0,
            text: "left right\nnext".into(),
            spans: vec![
                FixedPageTextSpan {
                    char_range: 0..4,
                    rect: FixedPageTextRect {
                        x: 10.0,
                        y: 10.0,
                        width: 20.0,
                        height: 10.0,
                    },
                },
                FixedPageTextSpan {
                    char_range: 5..10,
                    rect: FixedPageTextRect {
                        x: 120.0,
                        y: 10.0,
                        width: 25.0,
                        height: 10.0,
                    },
                },
                FixedPageTextSpan {
                    char_range: 11..15,
                    rect: FixedPageTextRect {
                        x: 10.0,
                        y: 30.0,
                        width: 24.0,
                        height: 10.0,
                    },
                },
            ],
            replacement: None,
        };

        let segments = fixed_page_text_segments(&layer);

        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].text, "left");
        assert_eq!(segments[1].text, "right");
        assert_eq!(segments[2].text, "next");
        assert!(segments[0].rect.x + segments[0].rect.width < segments[1].rect.x);
    }

    #[test]
    fn fixed_page_groups_merge_wrapped_lines_and_restore_hyphenation() {
        let text = "The result is rough-\nly correct.\nNext heading";
        let first_end = text.find('\n').unwrap();
        let second_start = first_end + 1;
        let second_end = text[second_start..].find('\n').unwrap() + second_start;
        let third_start = second_end + 1;
        let first_end_offset = u64::try_from(first_end).unwrap();
        let second_start_offset = u64::try_from(second_start).unwrap();
        let second_end_offset = u64::try_from(second_end).unwrap();
        let third_start_offset = u64::try_from(third_start).unwrap();
        let text_end_offset = u64::try_from(text.len()).unwrap();
        let layer = FixedPageTextLayer {
            width: 240.0,
            height: 140.0,
            text: text.into(),
            spans: vec![
                FixedPageTextSpan {
                    char_range: 0..first_end_offset,
                    rect: FixedPageTextRect {
                        x: 20.0,
                        y: 20.0,
                        width: 150.0,
                        height: 10.0,
                    },
                },
                FixedPageTextSpan {
                    char_range: second_start_offset..second_end_offset,
                    rect: FixedPageTextRect {
                        x: 20.0,
                        y: 33.0,
                        width: 82.0,
                        height: 10.0,
                    },
                },
                FixedPageTextSpan {
                    char_range: third_start_offset..text_end_offset,
                    rect: FixedPageTextRect {
                        x: 20.0,
                        y: 70.0,
                        width: 90.0,
                        height: 10.0,
                    },
                },
            ],
            replacement: None,
        };

        let groups = fixed_page_text_groups(&layer);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].text, "The result is roughly correct.");
        assert_eq!(groups[1].text, "Next heading");
        assert!(groups[0].rect.height > 20.0);
    }

    #[test]
    fn fixed_page_groups_do_not_span_across_figure_wraps() {
        let text = "Narrow first line\nnarrow second line\nfull width after figure";
        let first_end = text.find('\n').unwrap();
        let second_start = first_end + 1;
        let second_end = text[second_start..].find('\n').unwrap() + second_start;
        let third_start = second_end + 1;
        let first_end_offset = u64::try_from(first_end).unwrap();
        let second_start_offset = u64::try_from(second_start).unwrap();
        let second_end_offset = u64::try_from(second_end).unwrap();
        let third_start_offset = u64::try_from(third_start).unwrap();
        let text_end_offset = u64::try_from(text.len()).unwrap();
        let layer = FixedPageTextLayer {
            width: 240.0,
            height: 120.0,
            text: text.into(),
            spans: vec![
                FixedPageTextSpan {
                    char_range: 0..first_end_offset,
                    rect: FixedPageTextRect {
                        x: 20.0,
                        y: 20.0,
                        width: 90.0,
                        height: 10.0,
                    },
                },
                FixedPageTextSpan {
                    char_range: second_start_offset..second_end_offset,
                    rect: FixedPageTextRect {
                        x: 20.0,
                        y: 33.0,
                        width: 92.0,
                        height: 10.0,
                    },
                },
                FixedPageTextSpan {
                    char_range: third_start_offset..text_end_offset,
                    rect: FixedPageTextRect {
                        x: 20.0,
                        y: 46.0,
                        width: 190.0,
                        height: 10.0,
                    },
                },
            ],
            replacement: None,
        };

        let groups = fixed_page_text_groups(&layer);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].text, "Narrow first line narrow second line");
        assert_eq!(groups[1].text, "full width after figure");
    }
}
