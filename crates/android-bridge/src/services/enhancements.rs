//! Optional presentation runs after base recognition/translation, without
//! holding the reading session mutex across a model request.
use super::*;
use plugins::semantic_layout::{self as semantic, Recognition};
use rebook_publication::Section;
use std::{collections::VecDeque, ops::Range};

const MAX_JOBS: usize = 32;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct State {
    pending: VecDeque<Job>,
    active: Option<String>,
    done: VecDeque<String>,
    pub(crate) dirty: bool,
}

#[derive(Clone)]
struct Job {
    key: String,
    configuration: String,
    book: String,
    index: usize,
    hash: String,
    section: Arc<Section>,
    range: Range<usize>,
    base: Recognition,
    settings: PluginSettings,
    bytes: usize,
}

fn settings(s: &ReaderState) -> Result<(PluginSettings, String, String)> {
    let mut settings = PluginSettings::load_default().map_err(error)?;
    // Manual page recognition is allowed with automatic layout disabled.
    settings.semantic_layout.enabled = true;
    settings.target_language = settings.resolved_target_language(
        rebook_app_core::preferences::AppLanguage::system_translation_target(),
    );
    let book = plugins::pdf_native::cache_identity(&s.book_id, s.pipeline.rewrite.as_ref());
    let key =
        semantic::configuration_key(&book, &settings).ok_or_else(|| error("排版模型未配置"))?;
    Ok((settings, book, key))
}

fn current(s: &ReaderState, job: &Job) -> bool {
    settings(s).is_ok_and(|(_, book, key)| book == job.book && key == job.configuration)
        && s.pipeline
            .rewrite
            .parse_section(job.index)
            .is_ok_and(|section| semantic::fingerprint(&section) == job.hash)
}

/// Group results before installation: recognition fingerprints cover the input
/// section, while installed scopes must carry their own exact fingerprint.
pub(crate) fn install(
    s: &ReaderState,
    index: usize,
    section: &Section,
    range: Range<usize>,
    result: Recognition,
) -> bool {
    let hash = semantic::fingerprint(section);
    let mut changed = false;
    for (range, result) in semantic::recognition_groups(section, range, result) {
        changed |= s
            .pipeline
            .semantic
            .install_prepared_scope(index, section, &hash, range, result);
    }
    changed
}

/// False means a completed enhancement already owns this scope; do not replace
/// it with a later cached base result and lose its chart/notes.
pub(crate) fn queue(
    s: &mut ReaderState,
    index: usize,
    section: Arc<Section>,
    range: Range<usize>,
    base: Recognition,
    settings: &PluginSettings,
) -> bool {
    if !semantic::needs_enhancement(&section, range.clone(), &base) {
        return true;
    }
    let book = plugins::pdf_native::cache_identity(&s.book_id, s.pipeline.rewrite.as_ref());
    let Some(configuration) = semantic::configuration_key(&book, settings) else {
        return true;
    };
    let hash = semantic::fingerprint(&section);
    let key = serde_json::json!([configuration, index, hash, range.start, range.end]).to_string();
    let state = &mut s.enhancements;
    if state.done.contains(&key) {
        return false;
    }
    if state.active.as_ref() == Some(&key) || state.pending.iter().any(|job| job.key == key) {
        return true;
    }
    let bytes = serde_json::to_vec(section.as_ref())
        .map_or(MAX_BYTES + 1, |v| v.len())
        .saturating_add(serde_json::to_vec(&base).map_or(MAX_BYTES + 1, |v| v.len()));
    if bytes > MAX_BYTES {
        return true;
    }
    while state.pending.len() >= MAX_JOBS
        || state.pending.iter().map(|job| job.bytes).sum::<usize>() + bytes > MAX_BYTES
    {
        state.pending.pop_front();
    }
    state.pending.push_back(Job {
        key,
        configuration,
        book,
        index,
        hash,
        section,
        range,
        base,
        settings: settings.clone(),
        bytes,
    });
    true
}

#[derive(uniffi::Record)]
pub struct EnhancementUpdate {
    pub page: Option<Arc<MobilePage>>,
    pub pending: bool,
    pub changed: bool,
}

#[uniffi::export]
impl MobileReader {
    pub async fn enhance_page_cancellable(
        self: Arc<Self>,
        request: Arc<chat::ChatRequest>,
    ) -> Result<EnhancementUpdate> {
        runtime()
            .spawn(async move { run_next(self, request).await })
            .await
            .map_err(error)?
    }
}

async fn run_next(
    reader: Arc<MobileReader>,
    request: Arc<chat::ChatRequest>,
) -> Result<EnhancementUpdate> {
    let _claim = reader.enhancement_claim.lock().await;
    if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(error("已停止处理"));
    }
    let host = reader.clone();
    let job = work(move || {
        host.with(|s| {
            if s.enhancements.active.is_some() {
                return Ok(None);
            }
            // Prefer current visible work over prefetched scopes.
            let index = s.session.snapshot().location.section_index;
            let mut visible = s
                .session
                .current_visible_content_sources()
                .map_err(error)?
                .into_iter()
                .flat_map(|(_, ranges)| ranges)
                .collect::<Vec<_>>();
            visible.extend(scroll::visible_ranges(s));
            if let Some(position) = s.enhancements.pending.iter().position(|j| {
                j.index == index
                    && j.section.blocks[j.range.clone()]
                        .iter()
                        .flat_map(semantic::block_ranges)
                        .any(|source| {
                            visible
                                .iter()
                                .any(|range| plugins::source_range_nodes_overlap(&source, range))
                        })
            }) {
                let job = s.enhancements.pending.remove(position).unwrap();
                s.enhancements.pending.push_front(job);
            }
            while let Some(job) = s.enhancements.pending.pop_front() {
                if current(s, &job) {
                    s.enhancements.active = Some(job.key.clone());
                    return Ok(Some(job));
                }
            }
            Ok(None)
        })
    })
    .await?;
    let Some(job) = job else {
        return Ok(EnhancementUpdate {
            page: None,
            pending: false,
            changed: false,
        });
    };
    let descriptor = rebook_app_core::reading_ai::Task {
        key: job.key.clone(),
        kind: rebook_app_core::reading_ai::Kind::Enhancement,
        section: job.index,
        sources: job.section.blocks[job.range.clone()]
            .iter()
            .flat_map(semantic::block_ranges)
            .collect(),
        prefetch: false,
    };
    let result = reader
        .scheduler
        .run(
            descriptor,
            &request.scheduling,
            semantic::enhance_visible(
                &job.section,
                job.range.clone(),
                &job.book,
                &job.settings,
                job.base.clone(),
            ),
        )
        .await;
    let reader = reader.clone();
    work(move || {
        reader.with(|s| {
            s.enhancements.active = None;
            let cancelled = request.cancelled.load(std::sync::atomic::Ordering::Acquire)
                || result
                    .as_ref()
                    .is_err_and(|e| rebook_app_core::reading_ai::interrupted(e));
            if !current(s, &job) {
                return Ok(EnhancementUpdate {
                    page: None,
                    pending: !s.enhancements.pending.is_empty(),
                    changed: false,
                });
            }
            if cancelled {
                // Resume the same job after a gesture/background interruption.
                if s.enhancements.pending.len() < MAX_JOBS
                    && s.enhancements
                        .pending
                        .iter()
                        .map(|j| j.bytes)
                        .sum::<usize>()
                        + job.bytes
                        <= MAX_BYTES
                {
                    s.enhancements.pending.push_front(job);
                }
                return Err(error("已停止处理"));
            }
            let changed = match result {
                Ok(Some(result)) => install(s, job.index, &job.section, job.range.clone(), result),
                Ok(None) => false,
                Err(_) => {
                    tracing::warn!("optional mobile AI enhancement failed");
                    false
                }
            };
            s.enhancements.done.push_back(job.key);
            while s.enhancements.done.len() > 256 {
                s.enhancements.done.pop_front();
            }
            s.enhancements.dirty |= changed;
            // An open popup or selection retains its geometry. Its next ordinary
            // render consumes the dirty source, without resetting the reading anchor.
            let page = if changed
                && s.selected.is_none()
                && s.document.is_none()
                && s.session.snapshot().location.section_index == job.index
                && super::driver::visible(
                    &reader,
                    job.index,
                    &job.section.blocks[job.range.clone()]
                        .iter()
                        .flat_map(semantic::block_ranges)
                        .collect::<Vec<_>>(),
                ) {
                Some(render(s)?)
            } else {
                None
            };
            Ok(EnhancementUpdate {
                page,
                pending: !s.enhancements.pending.is_empty(),
                changed,
            })
        })
    })
    .await
}
