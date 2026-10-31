//! Image-table recognition is an automatic semantic overlay, with no extra raster copy.
use super::*;

fn converted(
    image: &rebook_publication::ImageBlock,
    template: &rebook_publication::TableBlock,
) -> Option<Block> {
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
    Some(Block::Table(table))
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
                    *block = table;
                }
            }
            Block::Figure(figure) => {
                // Keep the enclosing block/translation index and authored captions stable.
                if figure.images.iter().any(|image| &image.href == href) {
                    let mut body = Vec::new();
                    for image in std::mem::take(&mut figure.images) {
                        body.push(if &image.href == href {
                            converted(&image, template).unwrap_or(Block::Image(image))
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
