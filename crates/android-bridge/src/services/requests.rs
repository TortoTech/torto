//! Snapshot/network/commit boundaries. No await of a model owns ReaderState.
use super::*;
use rebook_app_core::reading_ai::{self, Kind, Task};
use rebook_publication::Section;

#[derive(Clone)]
pub(crate) struct Target {
    pub index: usize,
    pub ranges: Vec<SourceRange>,
    pub prefetch: bool,
    pub defer: bool,
}
#[derive(Clone)]
pub(crate) struct Staged {
    index: usize,
    hash: String,
    revision: u64,
    section: Arc<Section>,
    range: std::ops::Range<usize>,
    result: plugins::semantic_layout::Recognition,
    settings: PluginSettings,
}
pub(crate) fn target(s: &mut ReaderState) -> Result<Target> {
    let mut ranges = s
        .session
        .current_visible_content_sources()
        .map_err(error)?
        .into_iter()
        .flat_map(|(_, r)| r)
        .collect::<Vec<_>>();
    ranges.extend(scroll::visible_ranges(s));
    // Math and images can have geometry without a text fragment.
    let spread = s.session.current_spread().map_err(error)?;
    ranges.extend(spread.primary.geometry_sources().cloned());
    if let Some(page) = spread.secondary {
        ranges.extend(page.geometry_sources().cloned());
    }
    Ok(Target {
        index: s.session.snapshot().location.section_index,
        ranges,
        prefetch: false,
        defer: false,
    })
}
struct Snapshot {
    index: usize,
    section: Arc<Section>,
    hash: String,
    revision: u64,
    book: String,
    settings: PluginSettings,
    source: Arc<plugins::RewriteBookSource>,
    translation: Arc<plugins::TranslationBookSource>,
    semantic: Arc<plugins::semantic_layout::SemanticLayoutSource>,
    toc: Vec<rebook_reader::TocViewItem>,
    staged: Vec<Staged>,
}
fn resolved() -> Result<PluginSettings> {
    let mut settings = PluginSettings::load_default().map_err(error)?;
    settings.target_language = settings.resolved_target_language(
        rebook_app_core::preferences::AppLanguage::system_translation_target(),
    );
    Ok(settings)
}
async fn snapshot(reader: Arc<MobileReader>, index: usize) -> Result<Snapshot> {
    work(move || {
        let (source, translation, semantic, book, toc, staged) = reader.with(|s| {
            Ok((
                s.pipeline.rewrite.clone(),
                s.pipeline.translation.clone(),
                s.pipeline.semantic.clone(),
                s.book_id.clone(),
                s.session.toc_items().to_vec(),
                s.staged_layout.clone(),
            ))
        })?;
        let revision = source.revision();
        let prepared = reader
            .ai_snapshots
            .load(index, revision, source.as_ref(), &toc)
            .map_err(error)?;
        let section = prepared.section.clone();
        let hash = prepared.hash.clone();
        Ok(Snapshot {
            index,
            section,
            hash,
            revision,
            book,
            settings: resolved()?,
            source,
            translation,
            semantic,
            toc,
            staged,
        })
    })
    .await
}
fn valid(s: &ReaderState, snapshot: &Snapshot, layout: bool) -> bool {
    if s.book_id != snapshot.book || s.pipeline.rewrite.revision() != snapshot.revision {
        return false;
    }
    let Ok(mut settings) = resolved() else {
        return false;
    };
    if layout {
        settings.semantic_layout.enabled = true;
        plugins::semantic_layout::configuration_key(&snapshot.book, &settings)
            == plugins::semantic_layout::configuration_key(&snapshot.book, &snapshot.settings)
    } else {
        let translation = rebook_app_core::translation_cache::configuration_key(&settings).ok()
            == rebook_app_core::translation_cache::configuration_key(&snapshot.settings).ok();
        let mut previous = snapshot.settings.clone();
        previous.semantic_layout.enabled = true;
        settings.semantic_layout.enabled = true;
        translation
            && plugins::semantic_layout::configuration_key(&snapshot.book, &settings)
                == plugins::semantic_layout::configuration_key(&snapshot.book, &previous)
    }
}
fn check(request: &chat::ChatRequest) -> Result<()> {
    if request.scheduling.is_cancelled() {
        Err(error("已停止处理"))
    } else {
        Ok(())
    }
}

fn staged_for(snapshot: &Snapshot, target: &Target) -> Vec<Staged> {
    snapshot
        .staged
        .iter()
        .filter(|g| {
            g.index == snapshot.index
                && g.revision == snapshot.revision
                && g.hash == snapshot.hash
                && g.section.blocks[g.range.clone()]
                    .iter()
                    .flat_map(plugins::semantic_layout::block_ranges)
                    .any(|s| target.ranges.iter().any(|r| reading_ai::overlap(&s, r)))
        })
        .cloned()
        .collect()
}
fn unify(s: &mut ReaderState) -> Result<()> {
    if !s.focus {
        s.classic_typesetting =
            rebook_app_core::reading::typesetting(&s.classic_typesetting, TypesettingMode::Unified);
    }
    // Adopt style and source together in the next anchored presentation pass.
    s.enhancements.dirty = true;
    Ok(())
}
fn install_staged(s: &mut ReaderState, staged: &[Staged]) -> bool {
    let mut changed = false;
    for group in staged {
        if enhancements::queue(
            s,
            group.index,
            group.section.clone(),
            group.range.clone(),
            group.result.clone(),
            &group.settings,
        ) {
            changed |= enhancements::install(
                s,
                group.index,
                &group.section,
                group.range.clone(),
                group.result.clone(),
            );
        }
        s.staged_layout.retain(|g| {
            !(g.index == group.index && g.hash == group.hash && g.range == group.range)
        });
    }
    changed
}
pub(crate) async fn translate(
    reader: Arc<MobileReader>,
    mut target: Target,
    enabled: bool,
    request: Arc<chat::ChatRequest>,
) -> Result<()> {
    check(&request)?;
    let snapshot = snapshot(reader.clone(), target.index).await?;
    if !enabled {
        return work(move || {
            reader.with(|s| {
                check(&request)?;
                s.options.translated = false;
                s.pipeline.translation.set_enabled(false).map_err(error)?;
                s.enhancements.dirty = true;
                Ok(())
            })
        })
        .await;
    }
    let (snapshot, target, mut cache, batches, mut translated, staged) = {
        let reader = reader.clone();
        let request = request.clone();
        work(move || {
            let staged = staged_for(&snapshot, &target);
            let mut section = (*snapshot.section).clone();
            snapshot
                .semantic
                .prepare_citation_input(snapshot.index, &snapshot.hash, &mut section);
            for group in &staged {
                plugins::semantic_layout::apply_translation_citations(&mut section, &group.result);
                target.ranges.extend(
                    group.section.blocks[group.range.clone()]
                        .iter()
                        .flat_map(plugins::semantic_layout::block_ranges),
                );
            }
            let prepared = plugins::prepare_translation_inputs(
                &section,
                snapshot.source.book().metadata.layout
                    == rebook_publication::RenditionLayout::PrePaginated,
            );
            let cache = rebook_app_core::translation_cache::TranslationCache::open(
                &snapshot.book,
                snapshot.index,
                &snapshot.settings,
                &prepared,
            )
            .map_err(error)?;
            let all_cached = cache.load();
            let wanted =
                plugins::TranslationBookSource::inputs_for_ranges(&prepared, &target.ranges)
                    .into_iter()
                    .map(|(input, _)| (input.block_index, input.segment_index))
                    .collect::<std::collections::HashSet<_>>();
            let cached = all_cached
                .into_iter()
                .filter(|t| wanted.contains(&(t.block_index, t.segment_index)))
                .collect::<Vec<_>>();
            let keys = cached
                .iter()
                .map(|t| (t.block_index, t.segment_index))
                .collect::<std::collections::HashSet<_>>();
            let inputs = reader.with(|s| {
                check(&request)?;
                if !valid(s, &snapshot, false) {
                    return Err(error("已停止处理：配置或原文已变化"));
                }
                let config =
                    rebook_app_core::translation_cache::configuration_key(&snapshot.settings)
                        .map_err(error)?;
                if s.translation_config.as_ref() != Some(&config) {
                    snapshot.translation.clear().map_err(error)?;
                    s.translation_config = Some(config);
                }
                snapshot
                    .translation
                    .set_mode(snapshot.settings.translation_mode)
                    .map_err(error)?;
                snapshot
                    .translation
                    .set_target_language(&snapshot.settings.target_language)
                    .map_err(error)?;
                for (block, (raw, prepared)) in snapshot
                    .section
                    .blocks
                    .iter()
                    .zip(&section.blocks)
                    .enumerate()
                {
                    snapshot.translation.remember_translation_input(
                        snapshot.index,
                        block,
                        raw,
                        prepared,
                    );
                }
                if !target.prefetch {
                    s.options.translated = true;
                    snapshot.translation.set_enabled(true).map_err(error)?;
                }
                Ok(snapshot
                    .translation
                    .untranslated_prepared(snapshot.index, &prepared, &target.ranges)
                    .map_err(error)?
                    .into_iter()
                    .filter(|b| !keys.contains(&(b.block_index, b.segment_index)))
                    .collect::<Vec<_>>())
            })?;
            Ok((
                snapshot,
                target,
                cache,
                plugins::translation_batches(inputs, reading_ai::TRANSLATION_BATCH_CHARS),
                cached,
                staged,
            ))
        })
        .await?
    };
    let total = batches.iter().map(Vec::len).sum::<usize>();
    let mut completed = 0;
    request.report("translation", 0, total);
    for blocks in batches {
        check(&request)?;
        let descriptor = Task {
            key: reading_ai::translation_key(
                &snapshot.book,
                snapshot.index,
                &snapshot.settings,
                &blocks,
            ),
            kind: Kind::Translation,
            section: snapshot.index,
            sources: target.ranges.clone(),
            prefetch: target.prefetch,
        };
        let glossary = plugins::glossary::Context::new(
            &snapshot.book,
            &snapshot.settings.target_language,
            Some(snapshot.index),
        );
        let mut result = Vec::new();
        reader
            .scheduler
            .run(
                descriptor,
                &request.scheduling,
                plugins::translate_blocks_with_glossary(
                    snapshot.settings.clone(),
                    blocks,
                    Some(glossary),
                    |batch| result.extend(batch),
                ),
            )
            .await
            .map_err(error)?;
        completed += result.len();
        request.report("translation", completed, total);
        let (next_cache, result) = work(move || {
            if cache.store(&result).is_err() {
                tracing::warn!("translation cache could not be persisted");
            }
            Ok((cache, result))
        })
        .await?;
        cache = next_cache;
        translated.extend(result);
    }
    // Each fixed semantic batch publishes atomically. A large table can require
    // multiple HTTP batches without exposing a half-translated table/code run.
    work({
        let reader = reader.clone();
        move || {
            reader.with(|s| {
                check(&request)?;
                if !valid(s, &snapshot, false) {
                    return Err(error("已停止处理：配置或原文已变化"));
                }
                let source = snapshot.semantic.clone();
                source.commit_together(|| {
                    snapshot
                        .translation
                        .store_batch(snapshot.index, &translated)
                        .map_err(error)?;
                    install_staged(s, &staged);
                    Ok::<(), CoreError>(())
                })?;
                if !target.prefetch && !staged.is_empty() {
                    unify(s)?;
                }
                s.enhancements.dirty = true;
                Ok(())
            })
        }
    })
    .await?;
    super::driver::scope_changed(&reader, &target).await;
    Ok(())
}

pub(crate) async fn recognize(
    reader: Arc<MobileReader>,
    target: Target,
    request: Arc<chat::ChatRequest>,
) -> Result<()> {
    check(&request)?;
    let mut snapshot = snapshot(reader.clone(), target.index).await?;
    snapshot.settings.semantic_layout.enabled = true;
    snapshot
        .settings
        .semantic_layout_endpoint()
        .map_err(error)?;
    let plan = reading_ai::batching::SectionPlan::new(
        snapshot.index,
        snapshot.section.clone(),
        &snapshot.toc,
    );
    let batches = plan
        .select(&target.ranges)
        .into_iter()
        .filter(|batch| batch.visible)
        .collect::<Vec<_>>();
    let total = batches.len();
    request.report("layout", 0, total);
    for (completed, batch) in batches.into_iter().enumerate() {
        let section =
            plugins::semantic_layout::scope_section(&snapshot.section, batch.context.clone());
        let range =
            batch.target.start - batch.context.start..batch.target.end - batch.context.start;
        let book = plugins::pdf_native::cache_identity(&snapshot.book, snapshot.source.as_ref());
        let descriptor = Task {
            key: serde_json::json!([
                "structure",
                book,
                snapshot.index,
                batch.target.start,
                batch.target.end,
                snapshot.hash,
                plugins::semantic_layout::configuration_key(&snapshot.book, &snapshot.settings)
            ])
            .to_string(),
            kind: Kind::Structure,
            section: snapshot.index,
            sources: target.ranges.clone(),
            prefetch: target.prefetch,
        };
        let result = reader
            .scheduler
            .run(
                descriptor,
                &request.scheduling,
                plugins::semantic_layout::recognize_visible(
                    &section,
                    range,
                    &book,
                    &snapshot.settings,
                    snapshot.source.as_ref(),
                ),
            )
            .await
            .map_err(error)?;
        let r = reader.clone();
        let q = request.clone();
        snapshot = work(move || {
            r.with(|s| {
                check(&q)?;
                if !valid(s, &snapshot, true) {
                    return Err(error("已停止处理：配置或原文已变化"));
                }
                s.pipeline.semantic.configure_presentation(
                    &plugins::pdf_native::cache_identity(&snapshot.book, snapshot.source.as_ref()),
                    &snapshot.settings,
                    true,
                );
                if target.defer {
                    s.staged_layout
                        .retain(|g| !(g.index == snapshot.index && g.range == batch.target));
                    if s.staged_layout.len() >= 32 {
                        s.staged_layout.remove(0);
                    }
                    s.staged_layout.push(Staged {
                        index: snapshot.index,
                        hash: snapshot.hash.clone(),
                        revision: snapshot.revision,
                        section: snapshot.section.clone(),
                        range: batch.target,
                        result,
                        settings: snapshot.settings.clone(),
                    });
                } else {
                    if enhancements::queue(
                        s,
                        snapshot.index,
                        snapshot.section.clone(),
                        batch.target.clone(),
                        result.clone(),
                        &snapshot.settings,
                    ) {
                        enhancements::install(
                            s,
                            snapshot.index,
                            &snapshot.section,
                            batch.target,
                            result,
                        );
                    }
                    if !target.prefetch {
                        unify(s)?;
                    }
                    s.enhancements.dirty = true;
                }
                Ok(())
            })?;
            Ok(snapshot)
        })
        .await?;
        request.report("layout", completed + 1, total);
        if !target.defer {
            super::driver::scope_changed(&reader, &target).await;
        }
    }
    Ok(())
}

pub(crate) async fn release_staged(
    reader: Arc<MobileReader>,
    target: Target,
    request: Arc<chat::ChatRequest>,
) -> Result<()> {
    let snapshot = snapshot(reader.clone(), target.index).await?;
    let staged = staged_for(&snapshot, &target);
    work(move || {
        reader.with(|s| {
            check(&request)?;
            if !valid(s, &snapshot, true) {
                return Err(error("已停止处理：配置或原文已变化"));
            }
            let source = snapshot.semantic.clone();
            source.commit_together(|| install_staged(s, &staged));
            if !target.prefetch && !staged.is_empty() {
                unify(s)?;
            }
            s.enhancements.dirty = true;
            Ok(())
        })
    })
    .await
}

pub(crate) async fn contents(
    reader: Arc<MobileReader>,
    request: Arc<chat::ChatRequest>,
) -> Result<Vec<TocItem>> {
    check(&request)?;
    let r = reader.clone();
    let (book, settings, rows, enabled) = work(move || {
        r.with(|s| {
            Ok((
                s.book_id.clone(),
                resolved()?,
                s.session.toc_items().to_vec(),
                s.options.translated,
            ))
        })
    })
    .await?;
    if !enabled || !settings.translate_toc {
        return work(move || {
            reader.with(|s| {
                s.toc_labels.clear();
                Ok(toc_items(s))
            })
        })
        .await;
    }
    let inputs = rows
        .iter()
        .enumerate()
        .map(|(index, t)| plugins::TranslationBlockInput {
            block_index: index,
            segment_index: None,
            text: t.label.clone(),
        })
        .collect::<Vec<_>>();
    let (mut cache, mut translations, missing) = work({
        let book = book.clone();
        let settings = settings.clone();
        move || {
            let cache = rebook_app_core::translation_cache::TranslationCache::open_text_inputs(
                &format!("{book}-toc"),
                &settings,
                &inputs,
            )
            .map_err(error)?;
            let translations = cache.load();
            let cached = translations
                .iter()
                .map(|t| t.block_index)
                .collect::<std::collections::HashSet<_>>();
            Ok((
                cache,
                translations,
                inputs
                    .into_iter()
                    .filter(|t| !t.text.trim().is_empty() && !cached.contains(&t.block_index))
                    .collect::<Vec<_>>(),
            ))
        }
    })
    .await?;
    let total = missing.len();
    let mut completed = 0;
    request.report("toc", 0, total);
    for blocks in plugins::translation_batches(missing, reading_ai::TRANSLATION_BATCH_CHARS) {
        let descriptor = Task {
            key: reading_ai::translation_key(&book, usize::MAX, &settings, &blocks),
            kind: Kind::Contents,
            section: usize::MAX,
            sources: vec![],
            prefetch: false,
        };
        let glossary = plugins::glossary::Context::new(&book, &settings.target_language, None);
        let mut batch = Vec::new();
        reader
            .scheduler
            .run(
                descriptor,
                &request.scheduling,
                plugins::translate_blocks_with_glossary(
                    settings.clone(),
                    blocks,
                    Some(glossary),
                    |result| batch.extend(result),
                ),
            )
            .await
            .map_err(error)?;
        completed += batch.len();
        request.report("toc", completed, total);
        let (next_cache, batch) = work(move || {
            cache.store(&batch).map_err(error)?;
            Ok((cache, batch))
        })
        .await?;
        cache = next_cache;
        translations.extend(batch);
    }
    work(move || {
        reader.with(|s| {
            check(&request)?;
            if !s.options.translated
                || rebook_app_core::translation_cache::configuration_key(&resolved()?)
                    .map_err(error)?
                    != rebook_app_core::translation_cache::configuration_key(&settings)
                        .map_err(error)?
            {
                return Err(error("已停止处理：配置已变化"));
            }
            s.toc_labels.clear();
            for translation in translations {
                if let Some(row) = rows.get(translation.block_index) {
                    s.toc_labels.insert(
                        row.id.clone(),
                        if settings.translation_mode == plugins::TranslationMode::Bilingual {
                            format!("{}\n{}", row.label, translation.text)
                        } else {
                            translation.text
                        },
                    );
                }
            }
            s.toc_config = Some(
                rebook_app_core::translation_cache::configuration_key(&settings).map_err(error)?,
            );
            Ok(toc_items(s))
        })
    })
    .await
}
