use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use rebook_publication::{
    Block, BlockStyle, Book, BookSource, FigureBlock, FixedPageTextLayer, FixedPageTextRect,
    FixedPageTextReplacement, FixedPageTextReplacementSegment, Inline, InlineRole, LinkRole,
    PublicationError, PublicationUrl, RasterResource, RenditionLayout, Resource, Section,
    SourceRange, TextBaseline, TextBlock, TextBlockKind, TextRun, TextStyle,
};

use super::TranslationMode;
#[cfg(test)]
use super::search::text_block_text;

const MATH_PLACEHOLDER_PREFIX: &str = "<torto-math-";
const MATH_PLACEHOLDER_SUFFIX: &str = "/>";

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
                if let Some(text) = translatable_text(block) {
                    blocks.push(TranslationBlockInput {
                        block_index,
                        segment_index: None,
                        text,
                    });
                }
            }
            Block::Quote(quote) => {
                blocks.extend(
                    quote
                        .body
                        .iter()
                        .chain(quote.attribution.iter())
                        .enumerate()
                        .filter_map(|(segment_index, block)| {
                            translatable_text(block).map(|text| TranslationBlockInput {
                                block_index,
                                segment_index: Some(segment_index),
                                text,
                            })
                        }),
                );
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
                            translatable_text(&cell.text).map(|text| TranslationBlockInput {
                                block_index,
                                segment_index: Some(cell_index),
                                text,
                            })
                        }),
                );
            }
            Block::Figure(figure) => {
                blocks.extend(figure.captions.iter().enumerate().filter_map(
                    |(caption_index, caption)| {
                        translatable_text(caption).map(|text| TranslationBlockInput {
                            block_index,
                            segment_index: Some(caption_index),
                            text,
                        })
                    },
                ));
            }
            Block::Note(_) | Block::Separator(_) | Block::LineBreak | Block::PageBreak => {}
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
        Block::Quote(block) => block.source.as_ref(),
        Block::Table(block) => block.source.as_ref(),
        Block::Image(block) => block.source.as_ref(),
        Block::Figure(block) => block.source.as_ref(),
        Block::Note(block) => block.source.as_ref(),
        Block::Separator(_) | Block::LineBreak | Block::PageBreak => None,
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
    if let (Block::Figure(figure), Some(segment_index)) = (block, segment_index) {
        return figure
            .captions
            .get(segment_index)
            .and_then(|caption| caption.source.as_ref())
            .or(figure.source.as_ref());
    }
    if let (Block::Quote(quote), Some(segment_index)) = (block, segment_index) {
        return quote
            .body
            .iter()
            .chain(quote.attribution.iter())
            .nth(segment_index)
            .and_then(|block| block.source.as_ref())
            .or(quote.source.as_ref());
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
                            Inline::Math(_) | Inline::Image(_) | Inline::Break => None,
                        })
                        .unwrap_or_default();
                    match mode {
                        TranslationMode::Replace => {
                            original.content =
                                replacement_content(translation, style, Some(&original.content));
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
                Block::Quote(quote) if !translation.segments.is_empty() => {
                    rendered.push(Block::Quote(translated_quote(
                        quote,
                        &translation.segments,
                        mode,
                    )));
                }
                Block::Image(mut image) if image.text_layer.is_some() && is_pdf => {
                    apply_fixed_page_translation(&mut image, &translation.segments);
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
                    )));
                }
                Block::Figure(figure) if !translation.segments.is_empty() => {
                    rendered.push(Block::Figure(translated_figure(
                        figure,
                        &translation.segments,
                        mode,
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

    fn fixed_page_dimensions(
        &self,
        section_index: usize,
    ) -> Result<Option<rebook_publication::FixedPageDimensions>, PublicationError> {
        self.inner.fixed_page_dimensions(section_index)
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
                Inline::Math(_) | Inline::Image(_) | Inline::Break => None,
            })
            .unwrap_or_default();
        if mode == TranslationMode::Replace {
            let original = cell.text.content.clone();
            cell.text.content = replacement_content(translated, style, Some(&original));
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

fn translated_quote(
    mut quote: rebook_publication::QuoteBlock,
    translations: &HashMap<usize, String>,
    mode: TranslationMode,
) -> rebook_publication::QuoteBlock {
    let body_len = quote.body.len();
    quote.body = quote
        .body
        .into_iter()
        .enumerate()
        .flat_map(|(index, block)| translated_quote_text(block, translations.get(&index), mode))
        .collect();
    if let Some(attribution) = quote.attribution.take() {
        let mut translated = translated_quote_text(attribution, translations.get(&body_len), mode);
        quote.attribution = translated.pop();
        if !translated.is_empty() {
            quote.body.extend(translated);
        }
    }
    quote
}

fn translated_quote_text(
    mut original: TextBlock,
    translation: Option<&String>,
    mode: TranslationMode,
) -> Vec<TextBlock> {
    let Some(translation) = translation else {
        return vec![original];
    };
    let style = original
        .content
        .iter()
        .find_map(|inline| match inline {
            Inline::Text(run) => Some(run.style),
            Inline::Math(_) | Inline::Image(_) | Inline::Break => None,
        })
        .unwrap_or_default();
    if mode == TranslationMode::Replace {
        original.content = replacement_content(translation, style, Some(&original.content));
        return vec![original];
    }
    let mut translated = original.clone();
    let original_margin_after = original.style.margin_after;
    original.style.margin_after = original_margin_after.min(6.0);
    translated.content = replacement_content(translation, style, Some(&original.content));
    translated.source = None;
    translated.style.margin_before = 0.0;
    translated.style.margin_after = original_margin_after;
    vec![original, translated]
}

fn translated_figure(
    mut figure: FigureBlock,
    translations: &HashMap<usize, String>,
    mode: TranslationMode,
) -> FigureBlock {
    for (caption_index, caption) in figure.captions.iter_mut().enumerate() {
        let Some(translated) = translations.get(&caption_index) else {
            continue;
        };
        let style = caption
            .content
            .iter()
            .find_map(|inline| match inline {
                Inline::Text(run) => Some(run.style),
                Inline::Math(_) | Inline::Image(_) | Inline::Break => None,
            })
            .unwrap_or_default();
        let original = caption.content.clone();
        if mode == TranslationMode::Replace {
            caption.content = replacement_content(translated, style, Some(&original));
        } else {
            caption.content.push(Inline::Break);
            caption
                .content
                .extend(replacement_content(translated, style, Some(&original)));
        }
    }
    figure
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

fn translation_text(block: &TextBlock) -> String {
    let mut text = String::new();
    let mut math_index = 0;
    for inline in &block.content {
        match inline {
            Inline::Text(run) => push_translation_style_markup(&mut text, run),
            Inline::Math(_) => {
                push_math_placeholder(&mut text, math_index);
                math_index += 1;
            }
            Inline::Image(_) => {}
            Inline::Break => text.push('\n'),
        }
    }
    text
}

fn translatable_text(block: &TextBlock) -> Option<String> {
    block
        .content
        .iter()
        .any(|inline| {
            matches!(inline, Inline::Text(run) if run.text.chars().any(|character| !character.is_whitespace()))
        })
        .then(|| translation_text(block))
}

fn push_math_placeholder(output: &mut String, index: usize) {
    use std::fmt::Write as _;

    write!(
        output,
        "{MATH_PLACEHOLDER_PREFIX}{index}{MATH_PLACEHOLDER_SUFFIX}"
    )
    .expect("writing to a String should not fail");
}

fn math_placeholder_indices(text: &str) -> Result<Vec<usize>, ()> {
    let mut rest = text;
    let mut indices = Vec::new();
    while let Some(start) = rest.find(MATH_PLACEHOLDER_PREFIX) {
        rest = &rest[start + MATH_PLACEHOLDER_PREFIX.len()..];
        let digit_count = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 || !rest[digit_count..].starts_with(MATH_PLACEHOLDER_SUFFIX) {
            return Err(());
        }
        indices.push(rest[..digit_count].parse().map_err(|_| ())?);
        rest = &rest[digit_count + MATH_PLACEHOLDER_SUFFIX.len()..];
    }
    Ok(indices)
}

pub(super) fn validate_translation_math_placeholders(
    source: &str,
    translation: &str,
) -> Result<(), String> {
    let expected =
        math_placeholder_indices(source).map_err(|()| "原文包含无效的公式占位符".to_owned())?;
    let actual = math_placeholder_indices(translation)
        .map_err(|()| "译文修改了公式占位符格式".to_owned())?;
    let mut expected = expected;
    let mut actual = actual;
    expected.sort_unstable();
    actual.sort_unstable();
    if actual == expected {
        Ok(())
    } else {
        Err("译文必须完整保留每个公式占位符且各出现一次".to_owned())
    }
}

fn push_translation_style_markup(output: &mut String, run: &TextRun) {
    let mut closing = Vec::new();
    if run.style.inline_role == InlineRole::Footnote {
        output.push_str("<inlinefootnote>");
        closing.push("</inlinefootnote>");
    }
    match run.style.link_role {
        LinkRole::Normal => {}
        LinkRole::FootnoteReference => {
            output.push_str("<noteref>");
            closing.push("</noteref>");
        }
        LinkRole::FootnoteBacklink => {
            output.push_str("<noteback>");
            closing.push("</noteback>");
        }
    }
    if run.style.bold {
        output.push_str("<strong>");
        closing.push("</strong>");
    }
    if run.style.italic {
        output.push_str("<em>");
        closing.push("</em>");
    }
    if run.style.underline {
        output.push_str("<u>");
        closing.push("</u>");
    }
    match run.style.baseline {
        TextBaseline::Normal => {}
        TextBaseline::Superscript => {
            output.push_str("<sup>");
            closing.push("</sup>");
        }
        TextBaseline::Subscript => {
            output.push_str("<sub>");
            closing.push("</sub>");
        }
    }
    output.push_str(&run.text);
    for close in closing.into_iter().rev() {
        output.push_str(close);
    }
}

fn replacement_content(text: &str, style: TextStyle, original: Option<&[Inline]>) -> Vec<Inline> {
    let text = normalize_translation_spacing(text);
    let original = original.unwrap_or_default();
    let math = original
        .iter()
        .filter_map(|inline| match inline {
            Inline::Math(run) => Some(run.clone()),
            Inline::Text(_) | Inline::Image(_) | Inline::Break => None,
        })
        .collect::<Vec<_>>();
    if !math.is_empty() && validate_math_placeholder_count(&text, math.len()).is_err() {
        // Never degrade structured formulas into untranslated LaTeX text. This
        // also safely handles translations cached by versions that exposed
        // formulas to the model as `$...$`.
        return original.to_vec();
    }
    let style = neutral_translation_style(style, original);
    let styled = parse_inline_style_markup(&text, style)
        .unwrap_or_else(|| restore_original_baselines(&text, style, original));
    let mut content = Vec::new();
    for (text, style) in styled {
        append_translated_span(&mut content, &text, style, original, &math);
    }
    restore_inline_images(&mut content, original);
    content
}

fn restore_inline_images(content: &mut Vec<Inline>, original: &[Inline]) {
    let original_length = original.iter().map(inline_translation_units).sum::<usize>();
    let translated_length = content.iter().map(inline_translation_units).sum::<usize>();
    let mut cursor = 0usize;
    let mut images = Vec::new();
    for inline in original {
        if let Inline::Image(run) = inline {
            let target = if original_length == 0 {
                0
            } else {
                cursor.saturating_mul(translated_length) / original_length
            };
            images.push((target, run.clone()));
        } else {
            cursor += inline_translation_units(inline);
        }
    }
    for (target, image) in images.into_iter().rev() {
        insert_inline_at_offset(content, target, Inline::Image(image));
    }
}

fn inline_translation_units(inline: &Inline) -> usize {
    match inline {
        Inline::Text(run) => run.text.chars().count(),
        Inline::Math(_) | Inline::Break => 1,
        Inline::Image(_) => 0,
    }
}

fn insert_inline_at_offset(content: &mut Vec<Inline>, offset: usize, value: Inline) {
    let mut cursor = 0;
    for index in 0..content.len() {
        let len = inline_translation_units(&content[index]);
        if offset <= cursor {
            content.insert(index, value);
            return;
        }
        if let Inline::Text(run) = &content[index]
            && offset < cursor + len
        {
            let split = offset - cursor;
            let mut trailing = run.clone();
            let leading = run.text.chars().take(split).collect::<String>();
            trailing.text = run.text.chars().skip(split).collect();
            let mut replacement = Vec::with_capacity(3);
            if !leading.is_empty() {
                let mut leading_run = run.clone();
                leading_run.text = leading;
                replacement.push(Inline::Text(leading_run));
            }
            replacement.push(value);
            if !trailing.text.is_empty() {
                replacement.push(Inline::Text(trailing));
            }
            content.splice(index..=index, replacement);
            return;
        }
        cursor += len;
    }
    content.push(value);
}

fn validate_math_placeholder_count(text: &str, math_count: usize) -> Result<(), ()> {
    let mut actual = math_placeholder_indices(text)?;
    actual.sort_unstable();
    (actual == (0..math_count).collect::<Vec<_>>())
        .then_some(())
        .ok_or(())
}

fn append_translated_span(
    content: &mut Vec<Inline>,
    text: &str,
    style: TextStyle,
    original: &[Inline],
    math: &[rebook_publication::MathRun],
) {
    let mut rest = text;
    while let Some(start) = rest.find(MATH_PLACEHOLDER_PREFIX) {
        append_translated_text(content, &rest[..start], style, original);
        let token = &rest[start + MATH_PLACEHOLDER_PREFIX.len()..];
        let digit_count = token.bytes().take_while(u8::is_ascii_digit).count();
        let index = token[..digit_count]
            .parse::<usize>()
            .expect("validated formula placeholder should contain an index");
        content.push(Inline::Math(math[index].clone()));
        rest = &token[digit_count + MATH_PLACEHOLDER_SUFFIX.len()..];
    }
    append_translated_text(content, rest, style, original);
}

fn append_translated_text(
    content: &mut Vec<Inline>,
    text: &str,
    style: TextStyle,
    original: &[Inline],
) {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            content.push(Inline::Break);
        }
        if !line.is_empty() {
            content.push(Inline::Text(TextRun {
                text: line.to_owned(),
                style,
                link: translated_footnote_link(line, style, original),
            }));
        }
    }
}

fn translated_footnote_link(
    translated_marker: &str,
    style: TextStyle,
    original: &[Inline],
) -> Option<PublicationUrl> {
    if style.link_role == LinkRole::Normal && style.baseline != TextBaseline::Superscript {
        return None;
    }
    let translated_marker = translated_marker.trim();
    original.iter().find_map(|inline| {
        let Inline::Text(run) = inline else {
            return None;
        };
        ((run.style.link_role == style.link_role && run.style.link_role != LinkRole::Normal
            || style.link_role == LinkRole::Normal
                && run.style.baseline == TextBaseline::Superscript)
            && run.text.trim() == translated_marker)
            .then(|| run.link.clone())
            .flatten()
            .filter(|target| target.fragment().is_some())
    })
}

fn neutral_translation_style(fallback: TextStyle, original: &[Inline]) -> TextStyle {
    let mut style = original
        .iter()
        .find_map(|inline| match inline {
            Inline::Text(run) if run.style.baseline == TextBaseline::Normal => Some(run.style),
            Inline::Text(_) | Inline::Math(_) | Inline::Image(_) | Inline::Break => None,
        })
        .unwrap_or(fallback);
    style.bold = false;
    style.italic = false;
    style.underline = false;
    style.baseline = TextBaseline::Normal;
    style.link_role = LinkRole::Normal;
    style.inline_role = InlineRole::Normal;
    style
}

fn normalize_translation_spacing(text: &str) -> String {
    text.split('\n')
        .map(normalize_translation_line_spacing)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_translation_line_spacing(line: &str) -> String {
    let mut collapsed = String::with_capacity(line.len());
    let mut pending_space = false;
    for character in line.chars() {
        if character.is_whitespace() {
            pending_space = !collapsed.is_empty();
            continue;
        }
        if pending_space {
            collapsed.push(' ');
            pending_space = false;
        }
        collapsed.push(character);
    }

    let collapsed = compact_baseline_markup_spacing(collapsed);
    let cjk_count = collapsed
        .chars()
        .filter(|character| is_cjk(*character))
        .count();
    let mut latin_word_count = 0;
    let mut in_latin_word = false;
    for character in collapsed.chars() {
        if character.is_ascii_alphanumeric() {
            if !in_latin_word {
                latin_word_count += 1;
            }
            in_latin_word = true;
        } else {
            in_latin_word = false;
        }
    }
    if cjk_count == 0 || cjk_count < latin_word_count {
        return collapsed;
    }

    let characters = collapsed.chars().collect::<Vec<_>>();
    let mut normalized = String::with_capacity(collapsed.len());
    for (index, character) in characters.iter().copied().enumerate() {
        if character != ' ' {
            normalized.push(character);
            continue;
        }
        let previous = characters.get(index.wrapping_sub(1)).copied();
        let next = characters.get(index + 1).copied();
        if previous.is_some_and(is_cjk) || next.is_some_and(is_cjk) {
            continue;
        }
        // The layout engine caps ordinary space stretch before sharing the
        // remaining justification across CJK boundaries. Keep real Latin
        // word spaces so UAX #14 and hit testing see the source text.
        normalized.push(' ');
    }
    normalized
}

fn compact_baseline_markup_spacing(mut text: String) -> String {
    for (spaced, compact) in [
        (" <sup>", "<sup>"),
        (" <sub>", "<sub>"),
        ("<sup> ", "<sup>"),
        ("<sub> ", "<sub>"),
        (" </sup>", "</sup>"),
        (" </sub>", "</sub>"),
    ] {
        text = text.replace(spaced, compact);
    }
    text
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x2E80..=0x2FFF
            | 0x3040..=0x30FF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TranslationStyleTag {
    Bold,
    Italic,
    Underline,
    Superscript,
    Subscript,
    FootnoteReference,
    FootnoteBacklink,
    InlineFootnote,
}

fn parse_inline_style_markup(
    text: &str,
    default_style: TextStyle,
) -> Option<Vec<(String, TextStyle)>> {
    let mut rest = text;
    let mut styled = Vec::new();
    let mut stack = Vec::new();
    let mut style = default_style;
    let mut found = false;
    while let Some((start, tag, opening, token)) = next_translation_style_tag(rest) {
        if start > 0 {
            styled.push((rest[..start].to_owned(), style));
        }
        rest = &rest[start + token.len()..];
        if opening {
            stack.push((tag, style));
            style = apply_translation_style_tag(style, tag);
        } else {
            let (open_tag, previous) = stack.pop()?;
            if open_tag != tag {
                return None;
            }
            style = previous;
        }
        found = true;
    }
    if !rest.is_empty() {
        styled.push((rest.to_owned(), style));
    }
    (found && stack.is_empty()).then_some(styled)
}

fn next_translation_style_tag(
    text: &str,
) -> Option<(usize, TranslationStyleTag, bool, &'static str)> {
    [
        ("<strong>", TranslationStyleTag::Bold, true),
        ("</strong>", TranslationStyleTag::Bold, false),
        ("<b>", TranslationStyleTag::Bold, true),
        ("</b>", TranslationStyleTag::Bold, false),
        ("<em>", TranslationStyleTag::Italic, true),
        ("</em>", TranslationStyleTag::Italic, false),
        ("<i>", TranslationStyleTag::Italic, true),
        ("</i>", TranslationStyleTag::Italic, false),
        ("<u>", TranslationStyleTag::Underline, true),
        ("</u>", TranslationStyleTag::Underline, false),
        ("<sup>", TranslationStyleTag::Superscript, true),
        ("</sup>", TranslationStyleTag::Superscript, false),
        ("<sub>", TranslationStyleTag::Subscript, true),
        ("</sub>", TranslationStyleTag::Subscript, false),
        ("<noteref>", TranslationStyleTag::FootnoteReference, true),
        ("</noteref>", TranslationStyleTag::FootnoteReference, false),
        ("<noteback>", TranslationStyleTag::FootnoteBacklink, true),
        ("</noteback>", TranslationStyleTag::FootnoteBacklink, false),
        (
            "<inlinefootnote>",
            TranslationStyleTag::InlineFootnote,
            true,
        ),
        (
            "</inlinefootnote>",
            TranslationStyleTag::InlineFootnote,
            false,
        ),
    ]
    .into_iter()
    .filter_map(|(token, tag, opening)| text.find(token).map(|index| (index, tag, opening, token)))
    .min_by_key(|(index, _, opening, _)| (*index, !*opening))
}

fn apply_translation_style_tag(mut style: TextStyle, tag: TranslationStyleTag) -> TextStyle {
    match tag {
        TranslationStyleTag::Bold => style.bold = true,
        TranslationStyleTag::Italic => style.italic = true,
        TranslationStyleTag::Underline => style.underline = true,
        TranslationStyleTag::Superscript => {
            style = baseline_style(style, TextBaseline::Superscript);
        }
        TranslationStyleTag::Subscript => {
            style = baseline_style(style, TextBaseline::Subscript);
        }
        TranslationStyleTag::FootnoteReference => {
            style.link_role = LinkRole::FootnoteReference;
        }
        TranslationStyleTag::FootnoteBacklink => {
            style.link_role = LinkRole::FootnoteBacklink;
        }
        TranslationStyleTag::InlineFootnote => {
            style.inline_role = InlineRole::Footnote;
        }
    }
    style
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
            Inline::Math(_) | Inline::Image(_) => 0,
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
                (run.style.baseline != TextBaseline::Normal
                    || run.style.link_role != LinkRole::Normal
                    || run.style.inline_role != InlineRole::Normal)
                    .then_some((
                        run.text.as_str(),
                        run.style,
                        offset.saturating_mul(1_000) / original_length,
                    ))
                    .filter(|_| !run.text.is_empty())
            }
            Inline::Break => {
                original_offset += 1;
                None
            }
            Inline::Math(_) | Inline::Image(_) => None,
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
            let prefix = text[cursor..start].trim_end_matches(|character: char| {
                character.is_whitespace() || character == '\u{200b}'
            });
            if !prefix.is_empty() {
                styled.push((prefix.to_owned(), default_style));
            }
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
        BlockStyle, FigureBlock, FixedPageTextLayer, FixedPageTextRect, FixedPageTextSpan,
        ImageBlock, ImageStyle, Metadata, PublicationId, QuoteBlock, RenditionLayout, SourceAnchor,
        SourceRange, SpineItem, SpineItemId, TextBlock, TextBlockKind,
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
    fn translation_spacing_collapses_llm_whitespace_and_protects_latin_phrases() {
        assert_eq!(
            normalize_translation_spacing("OER    运动由 Dick      Durbin 提出"),
            "OER运动由Dick Durbin提出"
        );
        assert_eq!(
            normalize_translation_spacing("A long English sentence keeps normal spaces"),
            "A long English sentence keeps normal spaces"
        );
        assert_eq!(
            normalize_translation_spacing("主要优势。   <sup> 19 </sup>"),
            "主要优势。<sup>19</sup>"
        );
        assert_eq!(
            normalize_translation_spacing("word <sup>19</sup> next"),
            "word<sup>19</sup> next"
        );
        assert_eq!(
            normalize_translation_spacing("H <sub> 2 </sub> O"),
            "H<sub>2</sub> O"
        );
    }

    #[test]
    fn replacement_preserves_inline_heading_images_at_their_relative_position() {
        let image = rebook_publication::InlineImageRun {
            image: ImageBlock {
                href: PublicationUrl::parse("images/chapter-icon.jpg").unwrap(),
                alt: String::new(),
                style: ImageStyle::default(),
                source: None,
                text_layer: None,
            },
            size_scale: 1.0,
            presentation: true,
        };
        let original = vec![
            Inline::Image(Box::new(image.clone())),
            Inline::Text(TextRun {
                text: "Why Goal Setting Is Broken".into(),
                style: TextStyle::default(),
                link: None,
            }),
        ];

        assert_eq!(
            translation_text(&TextBlock {
                kind: TextBlockKind::Heading(1),
                content: original.clone(),
                style: BlockStyle::default(),
                source: None,
            }),
            "Why Goal Setting Is Broken"
        );
        let translated =
            replacement_content("目标设定的致命缺陷", TextStyle::default(), Some(&original));
        assert!(matches!(translated.first(), Some(Inline::Image(run)) if run.as_ref() == &image));
        assert!(
            matches!(translated.get(1), Some(Inline::Text(run)) if run.text == "目标设定的致命缺陷")
        );
    }

    #[test]
    fn translation_preserves_and_restores_baseline_markers() {
        let footnote_target = PublicationUrl::parse("notes.xhtml#note-4").unwrap();
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
                link: Some(footnote_target.clone()),
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
                    && note.link.as_ref() == Some(&footnote_target)
        ));

        let cached = replacement_content(
            "这是正文。4 然而继续。",
            TextStyle::default(),
            Some(&original),
        );
        assert!(cached.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "4"
                    && run.style.baseline == TextBaseline::Superscript
                    && run.link.as_ref() == Some(&footnote_target)
        )));

        let spaced_cache =
            replacement_content("这是主要优势。 4", TextStyle::default(), Some(&original));
        let rendered = spaced_cache
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) => Some(run.text.as_str()),
                Inline::Math(_) | Inline::Image(_) | Inline::Break => None,
            })
            .collect::<String>();
        assert_eq!(rendered, "这是主要优势。4");
        assert!(spaced_cache.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "4"
                    && run.style.baseline == TextBaseline::Superscript
                    && run.link.as_ref() == Some(&footnote_target)
        )));
    }

    #[test]
    fn translation_preserves_structured_math_without_exposing_latex() {
        let first = rebook_publication::MathRun {
            latex: r"E=mc^2".into(),
            display: false,
            size_scale: 1.0,
        };
        let second = rebook_publication::MathRun {
            latex: r"\int_0^1 x\,dx".into(),
            display: true,
            size_scale: 1.25,
        };
        let original = vec![
            Inline::Text(TextRun {
                text: "First ".into(),
                style: TextStyle::default(),
                link: None,
            }),
            Inline::Math(first.clone()),
            Inline::Text(TextRun {
                text: ", then ".into(),
                style: TextStyle::default(),
                link: None,
            }),
            Inline::Math(second.clone()),
        ];
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: original.clone(),
            style: BlockStyle::default(),
            source: None,
        };

        assert_eq!(
            translation_text(&block),
            "First <torto-math-0/>, then <torto-math-1/>"
        );
        assert!(!translation_text(&block).contains("E=mc"));

        let translated = replacement_content(
            "先计算 <torto-math-1/>，再使用 <strong><torto-math-0/></strong>。",
            TextStyle::default(),
            Some(&original),
        );
        let formulas = translated
            .iter()
            .filter_map(|inline| match inline {
                Inline::Math(run) => Some(run),
                Inline::Text(_) | Inline::Image(_) | Inline::Break => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(formulas, [&second, &first]);
    }

    #[test]
    fn invalid_math_translation_falls_back_to_original_structured_content() {
        let original = vec![
            Inline::Text(TextRun {
                text: "Energy is ".into(),
                style: TextStyle::default(),
                link: None,
            }),
            Inline::Math(rebook_publication::MathRun {
                latex: r"E=mc^2".into(),
                display: false,
                size_scale: 1.0,
            }),
        ];

        assert_eq!(
            replacement_content("能量是 $E=mc^2$", TextStyle::default(), Some(&original),),
            original
        );
    }

    #[test]
    fn formula_only_blocks_are_not_sent_for_translation() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Math(rebook_publication::MathRun {
                latex: r"\sum_{i=1}^n i".into(),
                display: true,
                size_scale: 1.0,
            })],
            style: BlockStyle::default(),
            source: None,
        };

        assert_eq!(translatable_text(&block), None);
    }

    #[test]
    fn translated_math_placeholders_must_be_complete_and_unique() {
        assert!(
            validate_translation_math_placeholders(
                "Use <torto-math-0/> and <torto-math-1/>.",
                "使用 <torto-math-1/> 和 <torto-math-0/>。"
            )
            .is_ok()
        );
        assert!(
            validate_translation_math_placeholders(
                "Use <torto-math-0/> and <torto-math-1/>.",
                "使用 <torto-math-0/>。"
            )
            .is_err()
        );
        assert!(
            validate_translation_math_placeholders(
                "Use <torto-math-0/> and <torto-math-1/>.",
                "使用 <torto-math-0/>、<torto-math-1/> 和 <torto-math-1/>。"
            )
            .is_err()
        );
        assert!(
            validate_translation_math_placeholders(
                "Use <torto-math-0/>.",
                "使用 <torto-math-0 />。"
            )
            .is_err()
        );
        assert!(
            validate_translation_math_placeholders("No formula.", "无公式 <torto-math-0/>。")
                .is_err()
        );
    }

    #[test]
    fn translation_preserves_semantic_baseline_footnote_links() {
        let target = PublicationUrl::parse("chapter.xhtml#note-3").unwrap();
        let original = vec![
            Inline::Text(TextRun {
                text: "Meaning".into(),
                style: TextStyle::default(),
                link: None,
            }),
            Inline::Text(TextRun {
                text: "【3】".into(),
                style: TextStyle {
                    link_role: LinkRole::FootnoteReference,
                    ..TextStyle::default()
                },
                link: Some(target.clone()),
            }),
        ];
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: original.clone(),
            style: BlockStyle::default(),
            source: None,
        };

        assert_eq!(translation_text(&block), "Meaning<noteref>【3】</noteref>");
        let translated = replacement_content(
            "含义<noteref>【3】</noteref>",
            TextStyle::default(),
            Some(&original),
        );
        assert!(translated.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "【3】"
                    && run.style.link_role == LinkRole::FootnoteReference
                    && run.style.baseline == TextBaseline::Normal
                    && run.link.as_ref() == Some(&target)
        )));
    }

    #[test]
    fn translation_preserves_inline_footnote_semantics() {
        let original = vec![
            Inline::Text(TextRun {
                text: "Body".into(),
                style: TextStyle::default(),
                link: None,
            }),
            Inline::Text(TextRun {
                text: "Inline note".into(),
                style: TextStyle {
                    inline_role: InlineRole::Footnote,
                    ..TextStyle::default()
                },
                link: None,
            }),
        ];
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: original.clone(),
            style: BlockStyle::default(),
            source: None,
        };

        assert_eq!(
            translation_text(&block),
            "Body<inlinefootnote>Inline note</inlinefootnote>"
        );
        let translated = replacement_content(
            "正文<inlinefootnote>行内脚注</inlinefootnote>",
            TextStyle::default(),
            Some(&original),
        );
        assert!(translated.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "行内脚注"
                    && run.style.inline_role == InlineRole::Footnote
                    && run.link.is_none()
        )));
    }

    #[test]
    fn translation_scopes_bold_and_italic_to_the_corresponding_text() {
        let emphasized = TextStyle {
            bold: true,
            italic: true,
            ..TextStyle::default()
        };
        let superscript = TextStyle {
            baseline: TextBaseline::Superscript,
            size_scale: 0.75,
            ..TextStyle::default()
        };
        let original = vec![
            Inline::Text(TextRun {
                text: "Look at this sentence".into(),
                style: emphasized,
                link: None,
            }),
            Inline::Text(TextRun {
                text: ". The rest is normal.".into(),
                style: TextStyle::default(),
                link: None,
            }),
            Inline::Text(TextRun {
                text: "54".into(),
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
        assert_eq!(
            translation_text(&block),
            "<strong><em>Look at this sentence</em></strong>. The rest is normal.<sup>54</sup>"
        );

        let translated = replacement_content(
            "<strong><em>看看这个句子</em></strong>。其余内容正常。<sup>54</sup>",
            emphasized,
            Some(&original),
        );
        assert!(translated.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "看看这个句子" && run.style.bold && run.style.italic
        )));
        assert!(translated.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "。其余内容正常。" && !run.style.bold && !run.style.italic
        )));
        assert!(translated.iter().any(|inline| matches!(
            inline,
            Inline::Text(run)
                if run.text == "54" && run.style.baseline == TextBaseline::Superscript
        )));

        let cached = replacement_content(
            "看看这个句子。其余内容正常。<sup>54</sup>",
            emphasized,
            Some(&original),
        );
        assert!(cached.iter().all(|inline| match inline {
            Inline::Text(run) => !run.style.bold && !run.style.italic,
            Inline::Math(_) | Inline::Image(_) | Inline::Break => true,
        }));
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
            Block::Text(block) if block.source.as_ref().unwrap().end.text_offset == 5
        ));

        source.set_mode(TranslationMode::Bilingual).unwrap();
        let bilingual = source.parse_section(0).unwrap();
        assert_eq!(bilingual.blocks.len(), 2);
        assert_eq!(block_text(&bilingual.blocks[0]), "Hello");
        assert_eq!(block_text(&bilingual.blocks[1]), "你好");
        assert!(matches!(&bilingual.blocks[1], Block::Text(block) if block.source.is_none()));
    }

    #[test]
    fn quote_translation_preserves_body_and_attribution_roles() {
        let block = |kind, value: &str| TextBlock {
            kind,
            content: vec![Inline::Text(TextRun {
                text: value.into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle::default(),
            source: None,
        };
        let quote = QuoteBlock {
            body: vec![block(TextBlockKind::Blockquote, "Quoted prose")],
            attribution: Some(block(TextBlockKind::QuoteAttribution, "The source")),
            source: None,
        };
        let translations = HashMap::from([(0, "引用正文".into()), (1, "引用来源".into())]);

        let translated = translated_quote(quote, &translations, TranslationMode::Replace);

        assert_eq!(translation_text(&translated.body[0]), "引用正文");
        let attribution = translated
            .attribution
            .expect("attribution should remain attached");
        assert_eq!(attribution.kind, TextBlockKind::QuoteAttribution);
        assert_eq!(translation_text(&attribution), "引用来源");
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
    fn figure_captions_are_translated_without_detaching_them_from_the_image() {
        let figure = FigureBlock {
            images: Vec::new(),
            captions: vec![TextBlock {
                kind: TextBlockKind::Caption,
                content: vec![Inline::Text(TextRun {
                    text: "Original caption".into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: None,
            }],
            caption_position: Default::default(),
            style: BlockStyle::default(),
            source: None,
        };
        let section = Section {
            id: SpineItemId::new("figure-section").unwrap(),
            href: PublicationUrl::parse("figure.xhtml").unwrap(),
            blocks: vec![Block::Figure(figure.clone())],
            anchors: Vec::new(),
        };
        assert_eq!(
            translatable_blocks(&section, false),
            vec![TranslationBlockInput {
                block_index: 0,
                segment_index: Some(0),
                text: "Original caption".into(),
            }]
        );

        let translations = HashMap::from([(0, "翻译图注".to_owned())]);
        let replaced = translated_figure(figure.clone(), &translations, TranslationMode::Replace);
        assert_eq!(text_block_text(&replaced.captions[0]), "翻译图注");

        let bilingual = translated_figure(figure, &translations, TranslationMode::Bilingual);
        assert_eq!(
            text_block_text(&bilingual.captions[0]),
            "Original caption\n翻译图注"
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
            Block::Image(image) if image.source.as_ref().unwrap().end.text_offset == 9
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
