//! Shared source-note presentation for the reader and enlarged media preview.
use super::*;

pub(super) fn neutral_source(block: &mut TextBlock) {
    let neutral = |run: &mut rebook_publication::TextRun| {
        run.style.bold = false;
        run.style.italic = false;
        run.style.emphasis = false;
        run.style.alternate_voice = false;
        run.style.citation = false;
    };
    for inline in &mut block.content {
        match inline {
            Inline::Text(run) => neutral(run),
            Inline::Ruby(ruby) => {
                ruby.base.iter_mut().for_each(neutral);
                ruby.annotation.iter_mut().for_each(neutral);
            }
            _ => {}
        }
    }
}

impl LayoutEngine {
    pub(super) fn align_media_sources(
        &mut self,
        source: &dyn BookSource,
        captions: &mut [(PreparedText, Cow<'_, TextBlock>)],
        style: &ReaderStyle,
        content_width: f32,
        media_width: f32,
    ) -> Result<(), LayoutError> {
        let media_width = media_width.clamp(1.0, content_width.max(1.0));
        for (prepared, block) in captions {
            if !is_media_source(block) {
                continue;
            }
            let multiline = content_visible_lines(prepared).take(2).count() > 1;
            let fits_media = prepared.layout.width() <= media_width;
            let (align, width, offset) = if multiline {
                (TextAlignment::Start, content_width, 0.0)
            } else if fits_media {
                (
                    TextAlignment::End,
                    media_width,
                    (content_width - media_width) * 0.5,
                )
            } else {
                (TextAlignment::Center, content_width, 0.0)
            };
            let block = block.to_mut();
            block.style.align = align;
            block.style.logical_alignment = false;
            *prepared = self.shape_text_from_source(source, block, style, width)?;
            prepared.lines = edge_content_lines(prepared);
            prepared.start_offset += offset;
        }
        Ok(())
    }

    /// Shape preview captions with the same source-note rules as their media.
    pub fn layout_media_captions(
        &mut self,
        source: &dyn BookSource,
        captions: &[TextBlock],
        viewport: LayoutViewport,
        style: &ReaderStyle,
        media_width: f32,
    ) -> Result<SectionLayout, LayoutError> {
        self.publication_languages
            .clone_from(&source.book().metadata.languages);
        let geometry = resolve_page_geometry(viewport.width as f32, viewport.height as f32, style);
        let width = geometry.width;
        let visible_pages = geometry.visible_pages;
        let continuation_offset_x = geometry.continuation_offset_x;
        let mut paginator = Paginator::new(
            viewport,
            style.background,
            geometry,
            false,
            style.minimum_paragraph_gap,
        );
        let unified = style.typesetting.mode == TypesettingMode::Unified;
        let mut shaped = self.shape_figure_captions(source, captions, style, width, unified)?;
        if unified {
            self.align_media_sources(source, &mut shaped, style, width, media_width)?;
        }
        for (text, block) in shaped {
            paginator.push_text(&text, &block)?;
        }
        Ok(SectionLayout {
            pages: paginator.finish(),
            visible_pages,
            continuation_offset_x,
        })
    }
}
