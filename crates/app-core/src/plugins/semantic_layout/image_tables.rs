//! Image-table recognition is an automatic semantic overlay, with no extra raster copy.
use super::*;

fn converted(
    image: &rebook_publication::ImageBlock,
    template: &rebook_publication::TableBlock,
) -> Option<rebook_publication::TableBlock> {
    let mut table = template.clone();
    let source = image.source.as_ref()?;
    table.source = Some(source.clone());
    for (row, value) in table.rows.iter_mut().enumerate() {
        for (col, cell) in value.cells.iter_mut().enumerate() {
            identify(&mut cell.text, source, &format!("r{row}-c{col}"));
        }
    }
    for (index, text) in table.before.iter_mut().chain(&mut table.after).enumerate() {
        identify(text, source, &format!("caption-{index}"));
    }
    Some(table)
}

fn move_authored_captions(
    table: &mut rebook_publication::TableBlock,
    figure: &mut rebook_publication::FigureBlock,
) {
    use rebook_publication::CaptionPosition;
    let key = |text: &TextBlock| {
        crate::media::caption_text(text).chars()
            .filter(|c| !c.is_whitespace() && !matches!(c, '\u{200B}' | '\u{2060}'))
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let authored = figure.captions.iter().map(key).collect::<HashSet<_>>();
    for side in [&mut table.before, &mut table.after] {
        side.retain(|text| !authored.contains(&key(text)));
    }
    let split = match figure.caption_position {
        CaptionPosition::Before => figure.captions.len(),
        CaptionPosition::After => 0,
        CaptionPosition::Split(n) => n.min(figure.captions.len()),
    };
    let authored_after = figure.captions.split_off(split);
    table.before.splice(0..0, std::mem::take(&mut figure.captions));
    table.after.extend(authored_after);
}

fn identify(text: &mut TextBlock, image: &SourceRange, suffix: &str) {
    let mut source = image.clone();
    source.start.node = format!("{}-table-{suffix}", image.start.node);
    source.start.text_offset = 0;
    source.end = source.start.clone();
    source.end.text_offset = text
        .content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|run| run.text.chars().count() as u64)
        .sum();
    text.source = Some(source);
}

pub(super) fn compose(
    blocks: &mut [Block],
    href: &PublicationUrl,
    template: &rebook_publication::TableBlock,
) {
    for block in blocks {
        match block {
            Block::Image(image) if &image.href == href => {
                if let Some(table) = converted(image, template) {
                    *block = Block::Table(table);
                }
            }
            Block::Figure(figure) => {
                // A single image with captions is now an ordinary table. Keep its
                // image and caption sources and the enclosing block's index, but
                // drop the figure padding around the grid.
                if figure.body.is_empty()
                    && let [image] = figure.images.as_slice()
                    && &image.href == href
                    && let Some(mut table) = converted(image, template)
                {
                    move_authored_captions(&mut table, figure);
                    *block = Block::Table(table);
                    continue;
                }
                if figure.images.iter().any(|image| &image.href == href) {
                    let mut body = Vec::new();
                    for image in std::mem::take(&mut figure.images) {
                        body.push(if &image.href == href {
                            converted(&image, template).map(Block::Table).unwrap_or(Block::Image(image))
                        } else {
                            Block::Image(image)
                        });
                    }
                    body.append(&mut figure.body);
                    figure.body = body;
                }
                compose(&mut figure.body, href, template);
            }
            Block::Note(note) => compose(&mut note.blocks, href, template),
            // Already translated table cells must never be overwritten during final composition.
            _ => {}
        }
    }
}

pub(super) fn prepare(section: &mut Section, recognition: &Recognition) {
    for annotation in &recognition.annotations {
        if let Annotation::ImageTable { href, table } = annotation {
            compose(&mut section.blocks, href, table);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{image, original_source, section};
    use super::*;
    use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};

    #[test]
    fn recognized_image_table_uses_one_authored_caption_and_no_figure_panel() {
        let descriptor = rebook_publication::SpineItem {
            id: rebook_publication::SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        };
        let mut section = rebook_html::parse_section(
            "<html><body><p>Leading prose.</p><p>表C-1　测试表</p><img src='table.png'/><p>Following prose.</p></body></html>",
            &descriptor, |_| None,
        ).unwrap();
        let Block::Figure(original) = &section.blocks[1] else { panic!("expected image table"); };
        let image = original.images[0].clone();
        let authored = original.captions[0].source.clone();
        let template = rebook_html::parse_recognized_table(
            "<table><caption>表C-1 测试表</caption><tr><td>A</td><td>B</td></tr><tr><td>1</td><td>2</td></tr></table>"
        ).unwrap();
        compose(&mut section.blocks, &image.href, &template);
        let Block::Table(table) = &section.blocks[1] else { panic!("expected a regular table flow block"); };
        assert_eq!(table.source, image.source);
        assert_eq!(table.before.len(), 1);
        assert_eq!(table.before[0].source, authored);
        let target = crate::media::targets(&section, 0).into_iter().find(|target| target.block == 1).unwrap();
        let preview = crate::media::preview_blocks(&section, &target).unwrap();
        let captions = preview.iter().flat_map(Block::text_blocks)
            .filter(|text| text.kind == rebook_publication::TextBlockKind::Caption).count();
        assert_eq!(captions, 1);
    }

    #[test]
    fn recognized_tables_translate_cells_and_final_composition_keeps_translations() {
        let raw = section(vec![image("table-image")]);
        let original = original_source(raw.clone());
        let translation = TranslationBookSource::new(original, TranslationMode::Replace);
        translation.set_enabled(true).unwrap();
        let href = PublicationUrl::parse("image.png").unwrap();
        let html = "<table><tr><th>Year</th><th>Amount</th></tr><tr><td>2024</td><td>12.5</td></tr></table>";
        let template = rebook_html::parse_recognized_table(html).unwrap();
        let mut prepared = raw.clone();
        compose(&mut prepared.blocks, &href, &template);
        assert!(matches!(prepared.blocks[0], Block::Table(_)));
        translation.remember_translation_input(0, 0, &raw.blocks[0], &prepared.blocks[0]);
        let inputs = crate::plugins::prepare_translation_inputs(&prepared, false);
        assert_eq!(inputs.len(), 4);
        let visible = source(&raw.blocks[0]).unwrap();
        assert_eq!(
            TranslationBookSource::inputs_for_ranges(&inputs, &[visible.clone()]).len(),
            4
        );
        translation
            .store_batch(
                0,
                &inputs
                    .iter()
                    .map(|(input, _)| BlockTranslation {
                        block_index: 0,
                        segment_index: input.segment_index,
                        text: input.text.replace("Year", "年份").replace("Amount", "金额"),
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let mut translated = translation.parse_section(0).unwrap();
        compose(&mut translated.blocks, &href, &template);
        let Block::Table(table) = &translated.blocks[0] else {
            panic!("table missing")
        };
        assert_eq!(
            table.rows[0].cells[0]
                .text
                .content
                .iter()
                .flat_map(Inline::text_runs)
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "年份"
        );
        assert!(matches!(raw.blocks[0], Block::Image(_)));
    }
}
