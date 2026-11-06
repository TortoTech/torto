use super::*;
use rebook_app_core::plugins::{self, TranslationMode};

fn screen_y(s: &ReaderState, anchor: &Anchor) -> f32 {
    s.scroll_slices
        .iter()
        .filter_map(|slice| {
            let y = if anchor.baseline {
                slice.page.source_text_baseline(&anchor.source)
            } else {
                slice
                    .page
                    .source_content_bounds(std::slice::from_ref(&anchor.source))
                    .map(|r| r.y0 as f32)
            }?;
            Some(y + slice.top - slice.origin)
        })
        .min_by(|a, b| {
            (a - anchor.screen_y)
                .abs()
                .total_cmp(&(b - anchor.screen_y).abs())
        })
        .expect("the visible source must remain represented after reflow")
}

pub(crate) async fn verify(core: &Arc<MobileCore>, root: &std::path::Path) {
    let original = plugins::PluginSettings::load_default().unwrap();
    let reading = core.clone().reading_options().await.unwrap();
    core.clone()
        .save_reading_options(settings::ReadingOptions {
            translated: false,
            sentence_split: false,
            ..reading.clone()
        })
        .await
        .unwrap();
    let mut settings = plugins::PluginSettings::default();
    settings.semantic_layout.enabled = false;
    settings.target_language = "简体中文".into();
    settings.translation_provider = settings.providers[0].id.clone();
    settings.providers[0].kind = plugins::AiProviderKind::OpenAi;
    settings.providers[0].api_key.clear(); // A cache miss must fail, never use a paid service.
    for repeats in [2, 10] {
        let path = root.join(format!("reflow-{repeats}.fb2"));
        let body = (0..36)
            .map(|i| {
                format!(
                    "<p>Paragraph {i}: {}</p>",
                    "Stable source positions should survive a translated paragraph. "
                        .repeat(repeats)
                )
            })
            .collect::<String>();
        std::fs::write(&path,format!(r#"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Reflow {repeats}</book-title><lang>en</lang></title-info></description><body><section>{body}</section></body></FictionBook>"#)).unwrap();
        let book = core
            .clone()
            .import_book(path.to_string_lossy().into_owned())
            .await
            .unwrap()
            .into_iter()
            .find(|b| b.title == format!("Reflow {repeats}"))
            .unwrap();
        for mode in [TranslationMode::Replace, TranslationMode::Bilingual] {
            for long in [false, true] {
                settings.translation_mode = mode;
                settings.translation_model = format!("reflow-{repeats}-{mode:?}-{long}");
                settings.providers[0].models = vec![plugins::AiModelConfig::language(
                    &settings.translation_model,
                )];
                settings.save_default().unwrap();
                let reader = core
                    .clone()
                    .open_book(
                        book.id.clone(),
                        Viewport {
                            width: 360,
                            height: 640,
                            scale: 1.0,
                        },
                    )
                    .await
                    .unwrap();
                reader.clone().configure(20.0, true, false).await.unwrap();
                reader.clone().focus_mode(true).await.unwrap();
                let source = reader
                    .clone()
                    .search("Paragraph 12:".into())
                    .await
                    .unwrap()
                    .remove(0)
                    .source;
                reader.clone().go_to_source(source).await.unwrap();
                reader.clone().scroll_by(110.0).await.unwrap();
                let anchor = reader.with(|s| Ok(Anchor::capture(s).unwrap())).unwrap();
                reader
                    .with(|s| {
                        let section = s.pipeline.rewrite.parse_section(0).map_err(error)?;
                        let inputs = plugins::prepare_translation_inputs(&section, false);
                        let mut resolved =
                            plugins::PluginSettings::load_default().map_err(error)?;
                        resolved.target_language = resolved.resolved_target_language("简体中文");
                        let mut cache = rebook_app_core::translation_cache::TranslationCache::open(
                            &book.id, 0, &resolved, &inputs,
                        )
                        .map_err(error)?;
                        cache
                            .store(
                                &inputs
                                    .iter()
                                    .map(|(input, _)| plugins::BlockTranslation {
                                        block_index: input.block_index,
                                        segment_index: input.segment_index,
                                        text: if long {
                                            "这是一段更长的译文，重排后仍需要保持当前阅读行的位置。"
                                                .repeat(40)
                                        } else {
                                            "这是一段简短译文。".into()
                                        },
                                    })
                                    .collect::<Vec<_>>(),
                            )
                            .map_err(error)?;
                        Ok(())
                    })
                    .unwrap();
                reader.clone().translate_page(true).await.unwrap();
                reader.with(|s| {
                    assert_eq!(s.focus_anchor.as_ref().unwrap().start.node,anchor.focus.start.node,
                        "{repeats}/{mode:?}/{long}: translation must not focus a preceding paragraph");
                    assert!((screen_y(s,&anchor)-anchor.screen_y).abs()<0.1,
                        "{repeats}/{mode:?}/{long}: the reading line must not jump");
                    Ok(())
                }).unwrap();
                // A metadata-only refresh uses the same anchor, including while
                // the visible paragraph has both original and translated text.
                let after = reader.with(|s| Ok(Anchor::capture(s).unwrap())).unwrap();
                reader
                    .with(|s| {
                        s.enhancements.dirty = true;
                        render(s)?;
                        assert!((screen_y(s, &after) - after.screen_y).abs() < 0.1);
                        Ok(())
                    })
                    .unwrap();
                if let Some(control) = reader
                    .clone()
                    .page()
                    .await
                    .unwrap()
                    .info()
                    .controls
                    .into_iter()
                    .find(|c| c.kind == "sentences")
                {
                    let before = reader.with(|s| Ok(Anchor::capture(s).unwrap())).unwrap();
                    reader
                        .clone()
                        .toggle_reading_control(control)
                        .await
                        .unwrap();
                    reader
                        .with(|s| {
                            assert_eq!(
                                s.focus_anchor.as_ref().unwrap().start.node,
                                before.focus.start.node
                            );
                            assert!(
                                (screen_y(s, &before) - before.screen_y).abs() < 0.1,
                                "sentence structure must preserve the currently read line"
                            );
                            Ok(())
                        })
                        .unwrap();
                }
                reader.shutdown().await.unwrap();
            }
        }
    }
    core.clone().save_reading_options(reading).await.unwrap();
    original.save_default().unwrap();
}

pub(crate) async fn media(reader: &Arc<MobileReader>) {
    let controls = reader.clone().page().await.unwrap().info().controls;
    let chart = controls.iter().find(|c| c.kind == "chart").unwrap();
    let formula = controls.iter().find(|c| c.kind == "formula").unwrap();
    let table_source = chart.source.clone();
    let formula_sources: Vec<SourceRange> = serde_json::from_str(&formula.source).unwrap();
    reader.clone().focus_mode(true).await.unwrap();
    for (source, kind) in [
        (table_source, "chart"),
        (
            serde_json::to_string(&formula_sources[0]).unwrap(),
            "formula",
        ),
    ] {
        reader.clone().go_to_source(source).await.unwrap();
        for _ in 0..2 {
            let before = reader.with(|s| Ok(Anchor::capture(s).unwrap())).unwrap();
            let control = if kind == "chart" {
                chart.clone()
            } else {
                formula.clone()
            };
            reader
                .clone()
                .toggle_reading_control(control)
                .await
                .unwrap();
            reader
                .with(|s| {
                    assert_eq!(
                        s.focus_anchor.as_ref().unwrap().start.node,
                        before.focus.start.node
                    );
                    assert!(
                        (screen_y(s, &before) - before.screen_y).abs() < 0.1,
                        "{kind} changes must not shift the viewport"
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}
