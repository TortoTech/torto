use super::*;
use anyrender::ImageRenderer;
use rebook_publication::{Metadata, PublicationId, SpineItem, SpineItemId};

// The authored kana images and example words from Text Entry Systems, §10.2.
const HTML: &str = r#"<html><body><p><em>Syllabary</em>—Examples are the Japanese kana, Cree and Cherokee in North America, and Vai in Africa. In a syllabary, a character represents a syllable, but phonetically similar syllables do not have a similar graphical shape. For example, in Japanese kana, there is no graphical similarity between the syllables <img src="u10-02.jpg" alt="image"/> (ka) and <img src="u10-03.jpg" alt="image"/> (ki), even though they share the same consonant (/k/).</p><p>Stable text.</p></body></html>"#;
const TRANSLATED: &str = "<strong>音节文字</strong>：例如日本的假名、北美洲的克里文和切罗基文，以及非洲的瓦伊文。在音节文字中，一个字符代表一个音节，但发音相似的音节在字形上并不相似。例如，在日语假名中，音节<t-img-0/>(ka)和<em><t-img-1/></em>(ki)虽然共享同一个辅音(/k/)，但在字形上却没有任何相似之处。";

struct Source(Book, Section);
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.0
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        Ok(self.1.clone())
    }
    fn resource(&self, _: &PublicationUrl) -> Result<Resource, PublicationError> {
        panic!("translation placeholders must not load or decode images")
    }
}
fn source() -> Arc<Source> {
    let spine = SpineItem {
        id: SpineItemId::new("s").unwrap(),
        href: PublicationUrl::parse("s.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    let section = rebook_html::parse_section(HTML, &spine, |_| None).unwrap();
    Arc::new(Source(
        Book {
            id: PublicationId::new("inline-images").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![spine],
            table_of_contents: vec![],
        },
        section,
    ))
}
fn paragraph(source: &Source) -> &TextBlock {
    let Block::Text(text) = &source.1.blocks[0] else {
        panic!("paragraph")
    };
    text
}
fn flatten(content: &[Inline]) -> String {
    content
        .iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.clone(),
            Inline::Image(run) => {
                if run.image.href.path().ends_with("u10-02.jpg") {
                    "【ka】".into()
                } else {
                    "【ki】".into()
                }
            }
            Inline::Math(_) => "【math】".into(),
            _ => String::new(),
        })
        .collect::<String>()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[test]
fn kana_images_follow_their_examples_after_translation_reorders_the_clause() {
    let source = source();
    let original = paragraph(&source);
    let input = translation_text(original);
    assert!(input.contains("<t-img-0/> (ka) and <t-img-1/> (ki)"));
    assert_eq!(image_placeholder_indices(&input).unwrap(), [0, 1]);
    let content = replacement_content(TRANSLATED, TextStyle::default(), Some(&original.content));
    assert!(flatten(&content).contains("音节【ka】(ka)和【ki】(ki)虽然共享同一个辅音(/k/)"));
    let images = |content: &[Inline]| {
        content
            .iter()
            .filter(|i| matches!(i, Inline::Image(_)))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(images(&content), images(&original.content));
}

#[test]
fn recognized_character_examples_keep_unicode_and_accept_legacy_image_tags() {
    let mut source = source();
    let source = Arc::get_mut(&mut source).unwrap();
    let Block::Text(original) = &mut source.1.blocks[0] else {
        panic!()
    };
    let mut count = 0;
    for inline in &mut original.content {
        if let Inline::Image(run) = inline {
            run.image.ocr_text = Some(rebook_publication::ImageText {
                text: if count == 0 { "か" } else { "き" }.into(),
                literal: true,
                translated: None,
            });
            count += 1;
        }
    }
    let input = translation_text(original);
    assert!(input.contains("<t-img-0 keep=\"1\">か</t-img-0>"));
    let translated =
        "音节<t-img-0 keep=\"1\">か</t-img-0>(ka)和<t-img-1 keep=\"1\">き</t-img-1>(ki)。";
    assert!(validate_translation_image_placeholders(&input, translated).is_ok());
    assert!(
        validate_translation_image_placeholders(&input, &translated.replace("か", "カ")).is_err()
    );
    assert!(
        validate_translation_image_placeholders(&input, &translated.replace(" keep=\"1\"", ""))
            .is_err()
    );
    let restored = replacement_content(translated, TextStyle::default(), Some(&original.content));
    assert_eq!(
        restored
            .iter()
            .filter(|i| matches!(i, Inline::Image(_)))
            .count(),
        2
    );
    assert!(
        validate_translation_image_placeholders(
            "<t-img-0/><t-img-1/>",
            "<t-image-0/><torto-image-1/>"
        )
        .is_ok()
    );
    assert!(
        validate_translation_image_placeholders("<t-img-0/>", "<t-img-0/><t-image-0/>").is_err()
    );
    assert!(
        normalize_translation_spacing("x <t-img-0 keep=\"1\">か き</t-img-0> y").contains("か き")
    );
}

#[test]
fn recognized_prose_translates_without_losing_original_image_text() {
    let source = source();
    let mut original = paragraph(&source)
        .content
        .iter()
        .find(|i| matches!(i, Inline::Image(_)))
        .unwrap()
        .clone();
    let Inline::Image(image) = &mut original else {
        panic!()
    };
    image.image.ocr_text = Some(rebook_publication::ImageText {
        text: "Hello".into(),
        literal: false,
        translated: None,
    });
    let translated = replacement_content(
        "<t-img-0>你好</t-img-0>",
        TextStyle::default(),
        Some(std::slice::from_ref(&original)),
    );
    let Inline::Image(image) = &translated[0] else {
        panic!()
    };
    assert_eq!(image.image.ocr_text.as_ref().unwrap().text, "Hello");
    assert_eq!(
        image.image.ocr_text.as_ref().unwrap().display_text(),
        "你好"
    );
}

#[test]
fn missing_damaged_or_duplicate_image_ids_never_use_proportional_insertion() {
    let source = source();
    let original = paragraph(&source);
    let input = translation_text(original);
    for invalid in [
        "音节ka和ki共享辅音(/k/)",
        "<t-img-0/>",
        "<t-img-0/><t-img-0/>",
        "<t-img-0/><t-img-2/>",
        "<t-img-0/><t-img-1>",
        "<t-img-x/><t-img-1/>",
    ] {
        assert!(validate_translation_image_placeholders(&input, invalid).is_err());
        assert_eq!(
            replacement_content(invalid, TextStyle::default(), Some(&original.content)),
            original.content
        );
    }
    assert!(validate_translation_image_placeholders(&input, "<t-img-1/>ki，<t-img-0/>ka").is_ok());
    assert!(validate_translation_image_placeholders("plain", "<t-img-0/>").is_err());
}

#[test]
fn images_and_math_restore_in_translated_order_inside_styles() {
    let source = source();
    let mut original = paragraph(&source)
        .content
        .iter()
        .filter(|i| matches!(i, Inline::Image(_)))
        .cloned()
        .collect::<Vec<_>>();
    original.push(Inline::Math(rebook_publication::MathRun {
        original: None,
        latex: "x^2".into(),
        display: false,
        size_scale: 1.0,
    }));
    let content = replacement_content(
        "<strong><t-img-1/></strong>ki，<t-math-0/>，<em><t-img-0/></em>ka",
        TextStyle::default(),
        Some(&original),
    );
    assert_eq!(flatten(&content), "【ki】ki，【math】，【ka】ka");
    assert_eq!(content[0], original[1]);
}

#[test]
fn only_legacy_translations_with_inline_images_are_requested_again() {
    let source = source();
    let translated = TranslationBookSource::new(source.clone(), TranslationMode::Replace);
    translated.set_enabled(true).unwrap();
    translated.set_target_language("简体中文").unwrap();
    translated
        .store_batch(
            0,
            &[
                BlockTranslation {
                    block_index: 0,
                    segment_index: None,
                    text: "音节ka和ki共享辅音(/k/)".into(),
                },
                BlockTranslation {
                    block_index: 1,
                    segment_index: None,
                    text: "已缓存的普通译文。".into(),
                },
            ],
        )
        .unwrap();
    let prepared = prepare_translation_inputs(&source.1, false);
    let ranges = prepared.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>();
    let missing = translated
        .untranslated_prepared(0, &prepared, &ranges)
        .unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].block_index, 0);
    let old = translated.parse_section(0).unwrap();
    let Block::Text(old) = &old.blocks[0] else {
        panic!("paragraph")
    };
    assert_eq!(old.content, paragraph(&source).content);
    translated
        .store_batch(
            0,
            &[BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: TRANSLATED.into(),
            }],
        )
        .unwrap();
    assert!(
        translated
            .untranslated_prepared(0, &prepared, &ranges)
            .unwrap()
            .is_empty()
    );
    let rendered = translated.parse_section(0).unwrap();
    let Block::Text(text) = &rendered.blocks[0] else {
        panic!("paragraph")
    };
    assert!(flatten(&text.content).contains("音节【ka】(ka)和【ki】(ki)"));
}

#[test]
#[ignore = "requires TORTO_INLINE_IMAGE_BOOK, optionally TORTO_INLINE_IMAGE_PNG"]
fn local_book_kana_translation() {
    let book =
        rebook_formats::open_file(std::env::var("TORTO_INLINE_IMAGE_BOOK").unwrap()).unwrap();
    let source = book.source();
    for index in 0..source.book().sections.len() {
        let section = source.parse_section(index).unwrap();
        for (block, item) in section.blocks.iter().enumerate() {
            let Block::Text(text) = item else { continue };
            let plain = text
                .content
                .iter()
                .flat_map(Inline::text_runs)
                .map(|r| r.text.as_str())
                .collect::<String>();
            if !plain.contains("Cree and Cherokee") {
                continue;
            }
            let input = translation_text(text);
            assert_eq!(image_placeholder_indices(&input).unwrap(), [0, 1]);
            assert!(input.contains("<t-img-0/> (ka) and <t-img-1/> (ki)"));
            println!("section={index}, block={block}, input={input}");
            let translated = TranslationBookSource::new(source.clone(), TranslationMode::Replace);
            translated.set_enabled(true).unwrap();
            translated.set_target_language("简体中文").unwrap();
            translated
                .store_batch(
                    index,
                    &[BlockTranslation {
                        block_index: block,
                        segment_index: None,
                        text: TRANSLATED.into(),
                    }],
                )
                .unwrap();
            let section = translated.parse_section(index).unwrap();
            let item = &section.blocks[block];
            let Block::Text(text) = item else {
                panic!("paragraph")
            };
            let images = text
                .content
                .iter()
                .filter_map(|inline| {
                    if let Inline::Image(image) = inline {
                        Some(image.image.href.path().to_owned())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert!(images[0].contains("u10-02"));
            assert!(images[1].contains("u10-03"));
            let mut style = rebook_layout::ReaderStyle::default();
            style.typesetting.mode = rebook_layout::TypesettingMode::Unified;
            let layout = rebook_layout::LayoutEngine::with_fonts(
                crate::fonts::embedded_reader_fonts().iter().cloned(),
            )
            .layout_blocks(
                &translated,
                std::slice::from_ref(item),
                rebook_layout::LayoutViewport::new(1100, 600).unwrap(),
                &style,
            )
            .unwrap();
            let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
            if let Ok(path) = std::env::var("TORTO_INLINE_IMAGE_PNG") {
                let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(1100, 600);
                let mut pixels = Vec::new();
                renderer.render_to_vec(|scene| display.paint(scene), &mut pixels);
                image::RgbaImage::from_raw(1100, 600, pixels)
                    .unwrap()
                    .save(path)
                    .unwrap();
            }
            return;
        }
    }
    panic!("sample paragraph missing");
}
