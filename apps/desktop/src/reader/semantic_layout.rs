use super::{DesktopReader, SnapshotEffects, TranslationTask};
use crate::platform::UserEvent;
use crate::plugins::BlockTranslation;
use crate::plugins::semantic_layout::{
    Recognition, block_ranges, empty_recognition, fingerprint, log_scope, needs_recognition,
    recognition_groups, recognize_visible, scope_section,
};
use rebook_publication::{RenditionLayout, Section, SourceRange};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod preparation;

const SETTLE: Duration = Duration::from_millis(200);
type Key = (usize, usize, Option<usize>);
type Demand = Vec<(usize, Vec<SourceRange>)>;
#[derive(Clone)]
struct Job {
    index: usize,
    offset: usize,
    target: std::ops::Range<usize>,
    section: Section,
    hash: String,
}
struct Group {
    index: usize,
    range: std::ops::Range<usize>,
    sources: Vec<SourceRange>,
    hash: String,
    result: Option<Recognition>,
}
#[derive(Default)]
pub(super) struct SemanticLayoutState {
    semantic_config: Option<(
        crate::plugins::semantic_layout::SemanticLayoutSettings,
        Option<crate::plugins::AiProvider>,
    )>,
    translation_config: Option<(
        String,
        String,
        crate::plugins::ReasoningEffort,
        Option<crate::plugins::AiProvider>,
    )>,
    demand: Demand,
    settled: Option<Instant>,
    next_poll: Option<Instant>,
    worker: Option<tokio::task::JoinHandle<()>>,
    active: Option<Job>,
    receiver: Option<mpsc::Receiver<(Job, Result<Recognition, String>)>>,
    done: HashMap<(usize, usize), String>,
    groups: Vec<Group>,
    translations: HashMap<Key, BlockTranslation>,
    failed: HashSet<Key>,
    originals: HashMap<usize, std::sync::Arc<Section>>,
    hashes: HashMap<usize, String>,
    inputs: HashMap<usize, Vec<(crate::plugins::TranslationBlockInput, SourceRange)>>,
    prepared_raw: Option<Demand>,
    prepared_with_translation: bool,
    expanded: Demand,
    prepare_worker: Option<tokio::task::JoinHandle<()>>,
    prepare_receiver: Option<mpsc::Receiver<preparation::Prepared>>,
    revision: u64,
    reflow_version: u64,
    reflow_dirty: bool,
    reflow_worker: Option<tokio::task::JoinHandle<()>>,
    reflow_receiver: Option<
        mpsc::Receiver<(
            u64,
            usize,
            rebook_layout::ReaderStyle,
            Result<rebook_reader::ReaderSession, rebook_reader::ReaderError>,
        )>,
    >,
}
impl Drop for SemanticLayoutState {
    fn drop(&mut self) {
        if let Some(worker) = self.reflow_worker.take() {
            worker.abort();
        }
        if let Some(worker) = self.prepare_worker.take() {
            worker.abort();
        }
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}
fn overlap(left: &SourceRange, right: &SourceRange) -> bool {
    left.start.spine == right.start.spine
        && (left.start.node == right.start.node
            || left.start.node == right.end.node
            || left.end.node == right.start.node)
}
fn relevant(index: usize, sources: &[SourceRange], demand: &Demand) -> bool {
    demand.iter().any(|(i, ranges)| {
        *i == index
            && sources
                .iter()
                .any(|source| ranges.iter().any(|range| overlap(source, range)))
    })
}
fn canonical(mut demand: Demand) -> Demand {
    for (_, ranges) in &mut demand {
        for range in ranges.iter_mut() {
            range.start.text_offset = 0;
            range.end.text_offset = 0;
        }
        ranges.dedup();
    }
    demand
}

fn demanded_sources(
    block: &rebook_publication::Block,
    visible: &[SourceRange],
) -> Vec<SourceRange> {
    let mut sources = block_ranges(block);
    // A Notes section is a container, not one atomic paragraph. A linked note
    // must not promote all other endnotes into translation dependencies.
    if matches!(block, rebook_publication::Block::Note(note) if note.kind == rebook_publication::NoteBlockKind::Section)
    {
        sources.retain(|source| visible.iter().any(|range| overlap(source, range)));
    }
    sources
}
impl DesktopReader {
    pub(super) fn current_content_request_ranges(
        &mut self,
    ) -> Result<Demand, rebook_reader::ReaderError> {
        if self.translation.enabled {
            self.current_translation_ranges()
        } else {
            self.current_visible_ai_ranges()
        }
    }
    fn stage_semantic_group(&mut self, mut group: Group) {
        let mut results = Vec::new();
        if let Some(result) = group.result.take() {
            results.push(result);
        }
        loop {
            let previous = self.semantic_layout.groups.iter().position(|other| {
                other.index == group.index
                    && other.hash == group.hash
                    && other.range.start < group.range.end
                    && group.range.start < other.range.end
            });
            let Some(position) = previous else {
                break;
            };
            let other = self.semantic_layout.groups.remove(position);
            group.range =
                other.range.start.min(group.range.start)..other.range.end.max(group.range.end);
            group.sources.extend(other.sources);
            if let Some(result) = other.result {
                results.push(result);
            }
        }
        if let Some(original) = self.semantic_layout.originals.get(&group.index) {
            group.result = Some(crate::plugins::semantic_layout::merge_recognition(
                original,
                group.range.clone(),
                results,
            ));
        } else {
            group.result = results.into_iter().next();
        }
        self.semantic_layout.groups.push(group);
    }
    fn cancel_semantic_request(&mut self) {
        if let Some(worker) = self.semantic_layout.worker.take() {
            worker.abort();
        }
        self.semantic_layout.active = None;
        self.semantic_layout.receiver = None;
    }
    pub(super) fn invalidate_semantic_plan(&mut self) {
        self.cancel_semantic_request();
        self.translation.task.cancel();
        self.semantic_layout.reflow_version += 1;
        self.semantic_layout.done.clear();
        self.semantic_layout.originals.clear();
        self.semantic_layout.hashes.clear();
        self.semantic_layout.inputs.clear();
        self.semantic_layout.prepared_raw = None;
        self.semantic_layout.expanded.clear();
        self.semantic_layout.prepare_receiver = None;
        if let Some(worker) = self.semantic_layout.prepare_worker.take() {
            worker.abort();
        }
        self.semantic_layout.groups.clear();
        self.semantic_layout.translations.clear();
        self.semantic_layout.failed.clear();
        self.semantic_layout.demand.clear();
        self.semantic_layout.settled = None;
    }
    pub(super) fn stage_translation_batch(&mut self, index: usize, batch: Vec<BlockTranslation>) {
        self.semantic_layout.next_poll = None;
        for translation in batch {
            self.semantic_layout.translations.insert(
                (index, translation.block_index, translation.segment_index),
                translation,
            );
        }
    }
    pub(super) fn fail_translation_batch(&mut self, task: &TranslationTask) {
        self.semantic_layout.next_poll = None;
        for block in &task.blocks {
            self.semantic_layout.failed.insert((
                task.section_index,
                block.block_index,
                block.segment_index,
            ));
        }
    }
    fn content_interacting(&self) -> bool {
        self.selection.is_some()
            || self.selection_anchor.is_some()
            || self.focus_selection_anchor.is_some()
            || self.ui.focus_scroll_motion.is_some()
            || self.annotation_note_draft.is_some()
    }
    fn semantic_enabled(&self) -> bool {
        self.plugin_settings.semantic_layout.enabled
            && self.source.book().metadata.layout != RenditionLayout::PrePaginated
    }
    fn sync_content_config(&mut self) -> bool {
        let mut refreshed = false;
        let provider = |id: &str| {
            self.plugin_settings
                .providers
                .iter()
                .find(|provider| provider.id == id)
        };
        let semantic = (
            self.plugin_settings.semantic_layout.clone(),
            provider(&self.plugin_settings.semantic_layout.provider).cloned(),
        );
        let target = self
            .plugin_settings
            .resolved_target_language(crate::preferences::AppLanguage::system_translation_target());
        let translation = (
            self.plugin_settings.translation_model.clone(),
            target.clone(),
            self.plugin_settings.translation_reasoning_effort,
            provider(&self.plugin_settings.translation_provider).cloned(),
        );
        if self.semantic_layout.semantic_config.as_ref() != Some(&semantic) {
            let had_config = self.semantic_layout.semantic_config.is_some();
            self.cancel_semantic_request();
            self.translation.task.cancel();
            self.semantic_layout.translations.clear();
            self.semantic_layout.failed.clear();
            self.semantic_layout.done.clear();
            self.semantic_layout.groups.clear();
            self.semantic_layout.semantic_config = Some(semantic);
            if had_config {
                self.semantic_source
                    .configure(&self.book_id, &self.plugin_settings);
                self.refresh_semantic_layout();
                refreshed = true;
            }
        }
        if self.semantic_layout.translation_config.as_ref() != Some(&translation) {
            self.translation.task.cancel();
            self.semantic_layout.translations.clear();
            self.semantic_layout.failed.clear();
            self.semantic_layout.translation_config = Some(translation);
            if let Err(error) = self.translation_source.set_target_language(&target) {
                self.translation.show_error(error, Instant::now());
            }
        }
        refreshed
    }
    pub(super) fn tick_semantic_layout(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        let revision = self.rewrite_source.revision();
        if self.semantic_layout.revision != revision {
            self.invalidate_semantic_plan();
            self.semantic_layout.revision = revision;
        }
        if self.sync_content_config() {
            let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
        }
        self.poll_semantic_reflow(runtime, proxy);
        let semantic = self.semantic_enabled();
        if !self.translation.enabled {
            self.translation.task.cancel();
        }
        if !semantic {
            self.cancel_semantic_request();
        }
        if !semantic && !self.translation.enabled {
            return;
        }
        let Ok(demand) = self.current_content_request_ranges() else {
            return;
        };
        let demand = canonical(demand);
        if demand != self.semantic_layout.demand {
            self.semantic_layout.next_poll = None;
            self.semantic_layout.demand = demand;
            self.semantic_layout.settled = Some(Instant::now() + SETTLE);
        }
        if let Some((job, result)) = self
            .semantic_layout
            .receiver
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.semantic_layout.next_poll = None;
            self.semantic_layout.worker = None;
            self.semantic_layout.active = None;
            self.semantic_layout.receiver = None;
            let result = result.unwrap_or_else(|error| {
                tracing::warn!(%error, "AI layout batch failed; releasing successful translations");
                self.notice_timer.show(
                    &mut self.notice,
                    self.language
                        .text(
                            "AI排版失败，已保留原排版",
                            "AI layout failed; original layout retained",
                        )
                        .into(),
                    Instant::now(),
                );
                empty_recognition(&job.section)
            });
            for (range, result) in recognition_groups(&job.section, job.target.clone(), result) {
                let sources = job.section.blocks[range.clone()]
                    .iter()
                    .flat_map(block_ranges)
                    .collect();
                let range = range.start + job.offset..range.end + job.offset;
                for block in range.clone() {
                    self.semantic_layout
                        .done
                        .insert((job.index, block), job.hash.clone());
                }
                self.stage_semantic_group(Group {
                    index: job.index,
                    range,
                    hash: job.hash.clone(),
                    sources,
                    result: Some(result),
                });
            }
        }
        if let Some(deadline) = self.semantic_layout.settled
            && Instant::now() < deadline
        {
            let _ = proxy.send_event(UserEvent::RepaintAfter(
                deadline.saturating_duration_since(Instant::now()),
            ));
            return;
        }
        if let Some(prepared) = self
            .semantic_layout
            .prepare_receiver
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.semantic_layout.prepare_worker = None;
            self.semantic_layout.prepare_receiver = None;
            self.semantic_layout.originals = prepared.originals;
            self.semantic_layout.hashes.extend(prepared.hashes);
            self.semantic_layout.inputs.extend(prepared.inputs);
            self.semantic_layout.expanded = prepared.demand;
            self.semantic_layout.prepared_raw = Some(prepared.raw);
            self.semantic_layout.prepared_with_translation = prepared.with_translation;
            self.semantic_layout.next_poll = None;
        }
        if self.semantic_layout.prepared_raw.as_ref() != Some(&self.semantic_layout.demand)
            || self.semantic_layout.prepared_with_translation != self.translation.enabled
        {
            if self.semantic_layout.prepare_worker.is_none() {
                let source = self.semantic_source.original();
                let raw = self.semantic_layout.demand.clone();
                let originals = self.semantic_layout.originals.clone();
                let fixed = self.source.book().metadata.layout == RenditionLayout::PrePaginated;
                let with_translation = self.translation.enabled;
                let proxy = proxy.clone();
                let (tx, rx) = mpsc::channel();
                self.semantic_layout.prepare_receiver = Some(rx);
                self.semantic_layout.prepare_worker = Some(runtime.spawn(async move {
                    if let Ok(prepared) = tokio::task::spawn_blocking(move || {
                        preparation::prepare(source, raw, originals, fixed, with_translation)
                    })
                    .await
                    {
                        let _ = tx.send(prepared);
                    }
                    let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
                }));
            }
            return;
        }
        if let Some(deadline) = self.semantic_layout.next_poll
            && Instant::now() < deadline
        {
            if self.semantic_layout.worker.is_some()
                || self.translation.task.is_pending()
                || !self.semantic_layout.groups.is_empty()
            {
                let _ = proxy.send_event(UserEvent::RepaintAfter(
                    deadline.saturating_duration_since(Instant::now()),
                ));
            }
            return;
        }
        self.semantic_layout.next_poll = Some(Instant::now() + SETTLE);
        let demand = self.semantic_layout.expanded.clone();
        if let Some(job) = &self.semantic_layout.active {
            let sources = job.section.blocks[job.target.clone()]
                .iter()
                .flat_map(block_ranges)
                .collect::<Vec<_>>();
            if !relevant(job.index, &sources, &demand) {
                self.cancel_semantic_request();
            }
        }
        // AI shares translation's page-level lookahead when both are enabled;
        // without translation only screen-visible original blocks are targeted.
        let original = self.semantic_source.original();
        let originals = self.semantic_layout.originals.clone();
        if semantic {
            'sections: for (index, ranges) in &demand {
                let Some(section) = originals.get(index) else {
                    continue;
                };
                let Some(hash) = self.semantic_layout.hashes.get(index).cloned() else {
                    continue;
                };
                let complete = self.semantic_source.has_recognition(*index, &hash);
                for start in 0..section.blocks.len() {
                    let sources = demanded_sources(&section.blocks[start], ranges);
                    if !sources
                        .iter()
                        .any(|source| ranges.iter().any(|range| overlap(source, range)))
                    {
                        continue;
                    }
                    if self.semantic_layout.done.get(&(*index, start)) == Some(&hash) {
                        if self
                            .semantic_layout
                            .translations
                            .keys()
                            .any(|(section, block, _)| *section == *index && *block == start)
                            && !self
                                .semantic_layout
                                .groups
                                .iter()
                                .any(|group| group.index == *index && group.range.contains(&start))
                        {
                            self.semantic_layout.groups.push(Group {
                                index: *index,
                                range: start..start + 1,
                                sources,
                                hash: hash.clone(),
                                result: None,
                            });
                        }
                        continue;
                    }
                    if complete
                        || !needs_recognition(section, start..start + 1)
                        || self.plugin_settings.semantic_layout_endpoint().is_err()
                    {
                        self.semantic_layout
                            .done
                            .insert((*index, start), hash.clone());
                        self.semantic_layout.groups.push(Group {
                            index: *index,
                            range: start..start + 1,
                            sources,
                            hash: hash.clone(),
                            result: None,
                        });
                        continue;
                    }
                    if self.semantic_layout.worker.is_some() {
                        continue;
                    }
                    let mut end = start + 1;
                    while end < section.blocks.len()
                        && end - start < 16
                        && needs_recognition(section, end..end + 1)
                        && self.semantic_layout.done.get(&(*index, end)) != Some(&hash)
                        && block_ranges(&section.blocks[end])
                            .iter()
                            .any(|source| ranges.iter().any(|range| overlap(source, range)))
                    {
                        end += 1;
                    }
                    let mut lo = start.saturating_sub(6);
                    let mut hi = (end + 6).min(section.blocks.len());
                    // TOC boundaries constrain context, never enlarge the target.
                    for item in self.reader.toc_items() {
                        if let Some(target) = &item.target
                            && target.path() == section.href.path()
                            && let Some(fragment) = target.fragment()
                            && let Some(anchor) = section
                                .anchors
                                .iter()
                                .find(|anchor| anchor.fragment == fragment)
                            && let Some(boundary) = section.blocks.iter().position(|block| {
                                block_ranges(block)
                                    .iter()
                                    .any(|range| range.start.node == anchor.source.node)
                            })
                        {
                            if boundary <= start {
                                lo = lo.max(boundary);
                            }
                            if boundary >= end {
                                hi = hi.min(boundary);
                            }
                        }
                    }
                    let job = Job {
                        index: *index,
                        offset: lo,
                        target: start - lo..end - lo,
                        section: scope_section(section, lo..hi),
                        hash,
                    };
                    let active = job.clone();
                    let settings = self.plugin_settings.clone();
                    let book = self.book_id.clone();
                    let source = original.clone();
                    let proxy = proxy.clone();
                    let (tx, rx) = mpsc::channel();
                    log_scope(&settings, *index, start, end, false);
                    self.semantic_layout.active = Some(active);
                    self.semantic_layout.receiver = Some(rx);
                    self.semantic_layout.worker = Some(runtime.spawn(async move {
                        let result = tokio::time::timeout(
                            Duration::from_secs(180),
                            recognize_visible(
                                &job.section,
                                job.target.clone(),
                                &book,
                                &settings,
                                source.as_ref(),
                            ),
                        )
                        .await
                        .unwrap_or_else(|_| Err("AI layout batch timed out".into()));
                        let _ = tx.send((job, result));
                        let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
                    }));
                    break 'sections;
                }
            }
        }
        let mut wanted = demand.clone();
        for group in &self.semantic_layout.groups {
            if relevant(group.index, &group.sources, &demand) {
                wanted.push((group.index, group.sources.clone()));
            }
        }
        let mut missing = Vec::new();
        for (index, ranges) in wanted.iter().filter(|_| self.translation.enabled) {
            if let Ok(inputs) = self.missing_prepared(*index, ranges) {
                for input in inputs {
                    let key = (*index, input.block_index, input.segment_index);
                    if !missing.iter().any(|(existing, _)| *existing == key) {
                        missing.push((key, input));
                    }
                }
            }
        }
        if let Some(task) = self.translation.task.active()
            && !task.blocks.iter().any(|input| {
                missing.iter().any(|(key, _)| {
                    *key == (task.section_index, input.block_index, input.segment_index)
                })
            })
        {
            self.translation.task.cancel();
        }
        if self.translation.enabled
            && !self.translation.task.is_pending()
            && self.plugin_settings.translation_endpoint().is_ok()
        {
            let available = missing
                .iter()
                .filter(|(key, _)| {
                    !self.semantic_layout.translations.contains_key(key)
                        && !self.semantic_layout.failed.contains(key)
                        && (!semantic || self.translation_semantics_ready(key.0, key.1))
                })
                .collect::<Vec<_>>();
            if let Some((first, _)) = available.first() {
                let index = first.0;
                let inputs = available
                    .into_iter()
                    .filter(|(key, _)| key.0 == index)
                    .map(|(_, input)| input.clone())
                    .collect::<Vec<_>>();
                if let Some(blocks) = crate::plugins::translation_batches(inputs, 2_000)
                    .into_iter()
                    .next()
                {
                    let mut settings = self.plugin_settings.clone();
                    settings.target_language = settings.resolved_target_language(
                        crate::preferences::AppLanguage::system_translation_target(),
                    );
                    self.translation.task.begin(TranslationTask {
                        section_index: index,
                        settings,
                        blocks,
                    });
                }
            }
        }
        if self.commit_ready_content(&demand, semantic) {
            let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
        }
    }

    fn missing_prepared(
        &self,
        index: usize,
        ranges: &[SourceRange],
    ) -> Result<Vec<crate::plugins::TranslationBlockInput>, String> {
        let inputs = self
            .semantic_layout
            .inputs
            .get(&index)
            .ok_or_else(|| "content snapshot pending".to_owned())?;
        let mut missing = self
            .translation_source
            .untranslated_prepared(index, inputs, ranges)?;
        if self.semantic_enabled()
            && let Some(original) = self.semantic_layout.originals.get(&index)
            && let Some(hash) = self.semantic_layout.hashes.get(&index)
        {
            for input in &mut missing {
                if !self.translation_semantics_ready(index, input.block_index) {
                    continue;
                }
                let mut section = scope_section(original, input.block_index..input.block_index + 1);
                self.semantic_source
                    .prepare_citation_input(index, hash, &mut section);
                for group in self
                    .semantic_layout
                    .groups
                    .iter()
                    .filter(|group| group.index == index && group.hash == *hash)
                {
                    if let Some(result) = &group.result {
                        crate::plugins::semantic_layout::apply_translation_citations(
                            &mut section,
                            result,
                        );
                    }
                }
                if let Some((prepared, _)) = crate::plugins::prepare_translation_inputs(
                    &section,
                    self.source.book().metadata.layout == RenditionLayout::PrePaginated,
                )
                .into_iter()
                .find(|(prepared, _)| prepared.segment_index == input.segment_index)
                {
                    input.text = prepared.text;
                }
            }
        }
        Ok(missing)
    }

    fn translation_semantics_ready(&self, index: usize, block: usize) -> bool {
        self.semantic_layout.hashes.get(&index).is_some_and(|hash| {
            self.semantic_layout.done.get(&(index, block)) == Some(hash)
                || self.semantic_source.has_recognition(index, hash)
        })
    }

    fn commit_ready_content(&mut self, demand: &Demand, semantic: bool) -> bool {
        let revision = self.rewrite_source.revision();
        if revision != self.semantic_layout.revision {
            self.invalidate_semantic_plan();
            self.semantic_layout.revision = revision;
            return false;
        }
        if !self.content_interacting() && self.semantic_layout.reflow_worker.is_none() {
            let mut changed = false;
            let mut retained = Vec::new();
            for group in std::mem::take(&mut self.semantic_layout.groups) {
                if !relevant(group.index, &group.sources, demand) {
                    retained.push(group);
                    continue;
                }
                let needed = self
                    .missing_prepared(group.index, &group.sources)
                    .unwrap_or_default();
                let ready = !self.translation.enabled
                    || self.plugin_settings.translation_endpoint().is_err()
                    || needed.iter().all(|input| {
                        let key = (group.index, input.block_index, input.segment_index);
                        self.semantic_layout.translations.contains_key(&key)
                            || self.semantic_layout.failed.contains(&key)
                    });
                if !ready {
                    retained.push(group);
                    continue;
                }
                if self.semantic_layout.hashes.get(&group.index) != Some(&group.hash) {
                    continue;
                }
                let translations = needed
                    .iter()
                    .filter_map(|input| {
                        self.semantic_layout.translations.remove(&(
                            group.index,
                            input.block_index,
                            input.segment_index,
                        ))
                    })
                    .collect::<Vec<_>>();
                self.semantic_source.commit_together(|| {
                    if !translations.is_empty() {
                        let _ = self
                            .translation_source
                            .store_batch(group.index, &translations);
                        changed = true;
                    }
                    if let Some(result) = group.result {
                        changed |= self.semantic_source.install_prepared_scope(
                            group.index,
                            &self.semantic_layout.originals[&group.index],
                            &group.hash,
                            group.range,
                            result,
                        );
                    }
                });
            }
            self.semantic_layout.groups = retained;
            // With AI disabled, each finished paragraph can publish immediately.
            if !semantic {
                let staged = std::mem::take(&mut self.semantic_layout.translations);
                for ((index, _, _), translation) in staged {
                    let _ = self.translation_source.store_batch(index, &[translation]);
                    changed = true;
                }
            }
            if changed {
                self.refresh_semantic_layout();
            }
            return changed;
        }
        false
    }
    fn refresh_semantic_layout(&mut self) {
        self.semantic_layout.reflow_version += 1;
        self.semantic_layout.reflow_dirty = true;
    }

    fn poll_semantic_reflow(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if self.content_interacting() {
            return;
        }
        if let Some((version, unit, style, result)) = self
            .semantic_layout
            .reflow_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.semantic_layout.reflow_worker = None;
            self.semantic_layout.reflow_receiver = None;
            if self.adopt_semantic_reflow(runtime, version, unit, style, result) {
                let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
            }
        }
        if self.semantic_layout.reflow_dirty && self.semantic_layout.reflow_worker.is_none() {
            self.semantic_layout.reflow_dirty = false;
            let version = self.semantic_layout.reflow_version;
            let unit = self.reader.reading_unit_location().index;
            let style = self.reader.style();
            let request = self
                .reader
                .prepare_refresh_request(self.progress_source_range());
            let proxy = proxy.clone();
            let (tx, rx) = mpsc::channel();
            self.semantic_layout.reflow_receiver = Some(rx);
            self.semantic_layout.reflow_worker = Some(runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || request.prepare())
                    .await
                    .unwrap_or_else(|error| {
                        Err(rebook_reader::ReaderError::Publication(
                            rebook_publication::PublicationError::InvalidPublication(
                                error.to_string(),
                            ),
                        ))
                    });
                let _ = tx.send((version, unit, style, result));
                let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
            }));
        }
    }

    fn adopt_semantic_reflow(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        version: u64,
        unit: usize,
        style: rebook_layout::ReaderStyle,
        result: Result<rebook_reader::ReaderSession, rebook_reader::ReaderError>,
    ) -> bool {
        match result {
            Ok(mut prepared)
                if version == self.semantic_layout.reflow_version
                    && unit == self.reader.reading_unit_location().index
                    && style == self.reader.style()
                    && prepared.viewport() == self.reader.viewport() =>
            {
                let scroll_source = self
                    .progress_source_range()
                    .or_else(|| self.reader.current_locator().source);
                if scroll_source
                    .as_ref()
                    .is_some_and(|range| !prepared.restore_cached_anchor(&range.start))
                {
                    runtime.spawn_blocking(move || drop(prepared));
                    self.semantic_layout.reflow_dirty = true;
                } else {
                    let anchor =
                        self.capture_focus_reflow_anchor(super::FocusReflowKind::DocumentLayout);
                    let old = std::mem::replace(&mut self.reader, prepared);
                    runtime.spawn_blocking(move || drop(old));
                    self.apply_snapshot(
                        self.reader.snapshot(),
                        SnapshotEffects::static_content_change(),
                    );
                    self.focus_reflow_anchor = anchor;
                    if self.is_scroll_mode() && !self.is_focus_mode() {
                        self.scroll_target_source = scroll_source;
                    }
                    return true;
                }
            }
            Ok(prepared) => {
                runtime.spawn_blocking(move || drop(prepared));
                self.semantic_layout.reflow_dirty = true;
            }
            Err(error) => tracing::warn!(%error, "background semantic reflow failed"),
        }
        false
    }

    #[cfg(test)]
    fn refresh_semantic_layout_sync(&mut self) {
        let scroll_source = self.progress_source_range();
        let focus_reflow_anchor =
            self.capture_focus_reflow_anchor(super::FocusReflowKind::DocumentLayout);
        match self.reader.refresh_source() {
            Ok(snapshot) => {
                self.apply_snapshot(snapshot, SnapshotEffects::static_content_change());
                self.focus_reflow_anchor = focus_reflow_anchor;
                if self.is_scroll_mode() && !self.is_focus_mode() {
                    self.scroll_target_source = scroll_source;
                }
            }
            Err(error) => tracing::warn!(%error, "failed to refresh semantic layout"),
        }
    }
}

#[cfg(test)]
mod tests;
