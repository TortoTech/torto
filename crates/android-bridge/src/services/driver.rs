//! Android's demand/result adapter. All model admission, priorities, cancellation
//! and deadlines belong to app-core::reading_ai; Kotlin never chains requests.
use super::*;
use rebook_app_core::reading_ai::{self, Kind};
use std::collections::{HashMap, VecDeque};

#[derive(Default)]
pub(crate) struct State {
    context: String,
    demand: reading_ai::Demand,
    active: bool,
    interacting: bool,
    popup: bool,
    epoch: u64,
    version: u64,
    view_dirty: bool,
    replan: bool,
    manual_layout: bool,
    listener: Option<Arc<dyn ReaderAiListener>>,
    requests: HashMap<String, Running>,
    done: VecDeque<String>,
    error: Option<String>,
    notice: Option<String>,
    document_revision: u64,
    error_serial: u64,
    error_kind: String,
}
struct Running {
    request: Arc<chat::ChatRequest>,
    kind: Kind,
    index: usize,
    sources: Vec<SourceRange>,
}
#[derive(Clone, uniffi::Record)]
pub struct ReaderAiActivity {
    pub active: bool,
    pub interacting: bool,
    pub manual_layout: bool,
    pub ui_epoch: u64,
}
#[derive(uniffi::Record)]
pub struct ReaderAiUpdate {
    pub version: u64,
    pub ui_epoch: u64,
    pub processing: bool,
    pub enhancing: bool,
    pub page: Option<Arc<MobilePage>>,
    pub toc: Option<Vec<TocItem>>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub document_revision: u64,
    pub error_serial: u64,
    pub error_kind: String,
}
#[uniffi::export(with_foreign)]
pub trait ReaderAiListener: Send + Sync {
    fn update(&self, update: ReaderAiUpdate);
}

#[uniffi::export]
impl MobileReader {
    pub async fn update_ai_demand(
        self: Arc<Self>,
        activity: ReaderAiActivity,
        listener: Arc<dyn ReaderAiListener>,
    ) -> Result<()> {
        runtime()
            .spawn(update(self, activity, listener))
            .await
            .map_err(error)?
    }
    pub fn suspend_ai(&self) {
        self.scheduler.pause();
        if let Ok(mut state) = self.ai.lock() {
            state.interacting = true;
        }
    }
    pub fn stop_ai(&self) {
        self.scheduler.invalidate();
        if let Ok(mut state) = self.ai.lock() {
            state.active = false;
            state.replan = false;
            state.listener = None;
            for work in state.requests.values() {
                work.request.cancel();
            }
            state.requests.clear();
        }
    }
    pub fn retry_ai(&self) {
        self.stop_ai();
        if let Ok(mut state) = self.ai.lock() {
            state.done.clear();
            state.error = None;
            state.context.clear();
        }
    }
}

async fn update(
    reader: Arc<MobileReader>,
    activity: ReaderAiActivity,
    listener: Arc<dyn ReaderAiListener>,
) -> Result<()> {
    if !activity.active {
        reader.stop_ai();
        return Ok(());
    }
    let r = reader.clone();
    let (target, source, book, toc, settings, translated, revision, changed, popup) =
        work(move || {
            r.with(|s| {
                let mut settings = PluginSettings::load_default().map_err(error)?;
                settings.target_language = settings.resolved_target_language(
                    rebook_app_core::preferences::AppLanguage::system_translation_target(),
                );
                let reading = settings::load_reading(&s.root);
                let changed = s.options.translated != reading.translated;
                s.options.translated = reading.translated;
                s.pipeline
                    .translation
                    .set_enabled(reading.translated)
                    .map_err(error)?;
                if changed {
                    s.enhancements.dirty = true;
                }
                let popup = s.document.is_some();
                let target = documents::translation_target(s).map(Ok).unwrap_or_else(|| requests::target(s))?;
                Ok((
                    target,
                    s.pipeline.rewrite.clone(),
                    s.book_id.clone(),
                    s.session.toc_items().to_vec(),
                    settings,
                    reading.translated,
                    s.pipeline.rewrite.revision(),
                    changed,
                    popup,
                ))
            })
        })
        .await?;
    let context = blake3::hash(
        &serde_json::to_vec(&serde_json::json!([book, settings, translated, revision])).unwrap(),
    )
    .to_hex()
    .to_string();
    let original_demand = vec![(target.index, target.ranges.clone())];
    let demand = if translated && !popup && !activity.interacting {
        let source = source.clone();
        let mut demand = original_demand.clone();
        work(move || {
            rebook_app_core::reading_text::append_linked_footnote_translation_ranges(source.as_ref(), &mut demand);
            Ok(demand)
        }).await?
    } else { original_demand.clone() };
    let reset = {
        let mut state = reader.ai.lock().map_err(error)?;
        let reset = state.context != context;
        if state.context != context {
            reader.scheduler.invalidate();
            for work in state.requests.values() {
                work.request.cancel();
            }
            state.requests.clear();
            state.done.clear();
            state.replan = false;
            state.error = None;
            state.context = context.clone();
        }
        let resume =
            (state.interacting && !activity.interacting) || state.epoch != activity.ui_epoch || state.popup != popup;
        state.view_dirty |= resume || changed;
        state.listener = Some(listener);
        state.active = true;
        state.interacting = activity.interacting;
        state.popup = popup;
        state.manual_layout = activity.manual_layout;
        state.epoch = activity.ui_epoch;
        state.demand = demand.clone();
        let obsolete = state.requests.iter().filter(|(key,work)| !activity.interacting && key.contains("\"linked-note\"") && !reading_ai::relevant(work.index,&work.sources,&demand)).map(|(key,_)|key.clone()).collect::<Vec<_>>();
        for key in obsolete {if let Some(work)=state.requests.remove(&key){work.request.cancel();}}
        reset
    };
    if reset {
        let r = reader.clone();
        work(move || {
            r.with(|s| {
                s.staged_layout.clear();
                Ok(())
            })
        })
        .await?;
    }
    reader.scheduler.update(demand.clone(), activity.interacting);
    // Resuming interaction may publish results already marked dirty by a
    // worker; it must not invent a source change and redraw unchanged pixels.
    if changed {
        content_changed(&reader).await;
    }
    if activity.interacting {
        emit(&reader, None, None);
        return Ok(());
    }
    if !settings.semantic_layout.enabled && !translated && !activity.manual_layout {
        content_changed(&reader).await;
        return Ok(());
    }
    // Linked notes share desktop demand expansion, cache and translation retry.
    // A popup updates its document only, without rebuilding the body viewport.
    if translated {
        for (index, ranges) in &demand {
            let linked = if popup { ranges.clone() } else {
                ranges.iter().filter(|range| !original_demand.iter().any(|(i,rs)| i==index && rs.contains(range))).cloned().collect()
            };
            if !linked.is_empty() {
                start_note_translation(&reader, &context, requests::Target {index:*index, ranges:linked, prefetch:false, defer:false});
            }
        }
    }
    if popup {
        content_changed(&reader).await;
        return Ok(());
    }
    // Planning and source parsing are worker work, and never own ReaderState.
    let prepared = work({
        let r = reader.clone();
        move || {
            r.ai_snapshots
                .load(target.index, revision, source.as_ref(), &toc)
                .map_err(error)
        }
    })
    .await?;
    let plans = prepared.plan.select(&target.ranges);
    // As on desktop, current-unit structure warming is automatic. The shared
    // scheduler defers these background batches behind visible work.
    for batch in plans {
        let index = batch.index;
        let sources = prepared.section.blocks[batch.target.clone()]
            .iter()
            .flat_map(plugins::semantic_layout::block_ranges)
            .collect::<Vec<_>>();
        let flow = reading_ai::Flow::scope(
            settings.semantic_layout.enabled || (activity.manual_layout && batch.visible),
            translated,
            batch.visible,
            true,
        );
        let layout = flow.structure;
        let scoped_translation = flow.translation;
        if !layout && !scoped_translation {
            continue;
        }
        let key = serde_json::json!([
            context,
            index,
            batch.target.start,
            batch.target.end,
            layout,
            scoped_translation
        ])
        .to_string();
        let request = {
            let mut state = reader.ai.lock().map_err(error)?;
            if !state.active || state.context != context {
                return Ok(());
            }
            if state.done.contains(&key) || state.requests.contains_key(&key) {
                continue;
            }
            // A prefetch graph may own this same fixed batch already. Let it
            // finish, then replan visible translation from its cached structure.
            // Starting another graph would lose the handoff to deduplication.
            if state.requests.values().any(|w| {
                matches!(w.kind, Kind::Structure | Kind::Translation)
                    && w.index == index
                    && w.sources == sources
            }) {
                state.replan |= batch.visible;
                continue;
            }
            // Bounded prepared work; network queue admission/eviction remains core-owned.
            if state.requests.len() >= reading_ai::MAX_QUEUED {
                let candidate = reading_ai::Task {
                    key: key.clone(),
                    kind: if layout {
                        Kind::Structure
                    } else {
                        Kind::Translation
                    },
                    section: index,
                    sources: sources.clone(),
                    prefetch: !batch.visible,
                };
                let worst = state
                    .requests
                    .iter()
                    .filter(|(_, w)| matches!(w.kind, Kind::Structure | Kind::Translation))
                    .map(|(key, w)| {
                        let task = reading_ai::Task {
                            key: key.clone(),
                            kind: w.kind,
                            section: w.index,
                            sources: w.sources.clone(),
                            prefetch: true,
                        };
                        (key.clone(), task.rank(&state.demand))
                    })
                    .max_by_key(|(_, rank)| *rank);
                if let Some((old, _)) =
                    worst.filter(|(_, rank)| *rank > candidate.rank(&state.demand))
                {
                    if let Some(old) = state.requests.remove(&old) {
                        old.request.cancel();
                    }
                } else {
                    continue;
                }
            }
            let request = chat::ChatRequest::new();
            state.requests.insert(
                key.clone(),
                Running {
                    request: request.clone(),
                    kind: if layout {
                        Kind::Structure
                    } else {
                        Kind::Translation
                    },
                    index,
                    sources: sources.clone(),
                },
            );
            request
        };
        let reader = reader.clone();
        let context = context.clone();
        let target = requests::Target {
            index,
            ranges: sources,
            prefetch: !batch.visible,
            defer: scoped_translation,
        };
        runtime().spawn(async move {
            let result = async {
                for stage in flow.stages() {
                    phase(&reader, &key, stage);
                    let result = match stage {
                        Kind::Structure => {
                            requests::recognize(reader.clone(), target.clone(), request.clone())
                                .await
                        }
                        Kind::Translation => {
                            requests::translate(
                                reader.clone(),
                                target.clone(),
                                true,
                                request.clone(),
                            )
                            .await
                        }
                        _ => unreachable!(),
                    };
                    if let Err(failure) = result {
                        match reading_ai::failure(stage, &failure.to_string()) {
                            reading_ai::Failure::KeepOriginal => {
                                if let Ok(mut state) = reader.ai.lock() {
                                    state.notice = Some("AI 排版失败，已保留原排版".into());
                                }
                                emit(&reader, None, None);
                            }
                            reading_ai::Failure::StopBatch => {
                                let _ = requests::release_staged(
                                    reader.clone(),
                                    target.clone(),
                                    request.clone(),
                                )
                                .await;
                                return Err(failure);
                            }
                            reading_ai::Failure::Interrupted => return Err(failure),
                        }
                    }
                }
                Ok::<(), CoreError>(())
            }
            .await;
            finish(&reader, &context, &key, &request, result);
            ensure_enhancements(&reader);
            content_changed(&reader).await;
            replan(&reader);
        });
    }
    if translated && settings.translate_toc {
        let key = format!("{context}:contents");
        let request = {
            let mut state = reader.ai.lock().map_err(error)?;
            if state.done.contains(&key) || state.requests.contains_key(&key) {
                None
            } else {
                let request = chat::ChatRequest::new();
                state.requests.insert(
                    key.clone(),
                    Running {
                        request: request.clone(),
                        kind: Kind::Contents,
                        index: usize::MAX,
                        sources: vec![],
                    },
                );
                Some(request)
            }
        };
        if let Some(request) = request {
            let reader = reader.clone();
            let context = context.clone();
            runtime().spawn(async move {
                let result = requests::contents(reader.clone(), request.clone()).await;
                let toc = result.as_ref().ok().cloned();
                finish(&reader, &context, &key, &request, result.map(|_| ()));
                emit_at(&reader, None, toc, None, Some(&context));
            });
        }
    }
    content_changed(&reader).await;
    ensure_enhancements(&reader);
    Ok(())
}

fn start_note_translation(reader: &Arc<MobileReader>, context: &str, target: requests::Target) {
    let key = serde_json::json!([context,"linked-note",target.index,target.ranges]).to_string();
    let request = {
        let Ok(mut state) = reader.ai.lock() else {return;};
        if !state.active || state.context != context || state.done.contains(&key) || state.requests.contains_key(&key) {return;}
        if state.requests.len() >= reading_ai::MAX_QUEUED {state.replan=true;return;}
        let request = chat::ChatRequest::new();
        state.requests.insert(key.clone(), Running {request:request.clone(),kind:Kind::Translation,index:target.index,sources:target.ranges.clone()});
        request
    };
    let reader=reader.clone();let context=context.to_owned();
    runtime().spawn(async move {
        let result = requests::translate(reader.clone(),target,true,request.clone()).await;
        finish(&reader,&context,&key,&request,result);
        content_changed(&reader).await;
        replan(&reader);
    });
}

fn replan(reader: &Arc<MobileReader>) {
    let activity = {
        let Ok(mut state) = reader.ai.lock() else {
            return;
        };
        if !state.replan || !state.active || state.interacting {
            return;
        }
        let Some(listener) = state.listener.clone() else {
            return;
        };
        state.replan = false;
        (
            ReaderAiActivity {
                active: true,
                interacting: false,
                manual_layout: state.manual_layout,
                ui_epoch: state.epoch,
            },
            listener,
        )
    };
    let reader = reader.clone();
    let task: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            let _ = update(reader, activity.0, activity.1).await;
        });
    runtime().spawn(task);
}

fn phase(reader: &MobileReader, key: &str, kind: Kind) {
    if let Ok(mut state) = reader.ai.lock() {
        if let Some(work) = state.requests.get_mut(key) {
            work.kind = kind;
        }
    }
    emit(reader, None, None);
}
fn finish(
    reader: &MobileReader,
    context: &str,
    key: &str,
    request: &Arc<chat::ChatRequest>,
    result: Result<()>,
) {
    if let Ok(mut state) = reader.ai.lock() {
        if state.context != context
            || state
                .requests
                .get(key)
                .is_none_or(|r| !Arc::ptr_eq(&r.request, request))
        {
            return;
        }
        let kind = state.requests.remove(key).unwrap().kind;
        match result {
            Err(error) if reading_ai::interrupted(&error.to_string()) => {}
            Err(error) => {
                if kind != Kind::Enhancement {
                    tracing::warn!(kind=?kind, error=%error, "Reader AI request failed after shared retries");
                    state.error = Some(error.to_string());
                    state.error_serial = state.error_serial.wrapping_add(1);
                    state.error_kind = if matches!(kind,Kind::Translation|Kind::Contents) {"translation"} else {"layout"}.into();
                }
                state.done.push_back(key.into());
            }
            Ok(()) => state.done.push_back(key.into()),
        }
        while state.done.len() > 256 {
            state.done.pop_front();
        }
    }
    emit(reader, None, None);
}
fn emit(reader: &MobileReader, page: Option<Arc<MobilePage>>, toc: Option<Vec<TocItem>>) {
    emit_at(reader, page, toc, None, None);
}
fn emit_at(
    reader: &MobileReader,
    page: Option<Arc<MobilePage>>,
    toc: Option<Vec<TocItem>>,
    epoch: Option<u64>,
    context: Option<&str>,
) -> bool {
    // Serialize callback publication, not model work. Otherwise a later status
    // could arrive before an earlier page and make the UI discard that page.
    let Ok(_delivery) = reader.ai_delivery.lock() else {
        return false;
    };
    let event = {
        let Ok(mut state) = reader.ai.lock() else {
            return false;
        };
        if !state.active {
            return false;
        }
        let Some(listener) = state.listener.clone() else {
            return false;
        };
        if epoch.is_some_and(|e| e != state.epoch)
            || context.is_some_and(|c| c != state.context)
            || (page.is_some() && state.interacting)
        {
            return false;
        }
        state.version = state.version.wrapping_add(1);
        let processing = state.requests.values().any(|w| {
            w.kind != Kind::Enhancement
                && (w.kind == Kind::Contents
                    || reading_ai::relevant(w.index, &w.sources, &state.demand))
        });
        let enhancing = state.requests.values().any(|w| w.kind == Kind::Enhancement);
        let notice = state.notice.take();
        (
            listener,
            ReaderAiUpdate {
                version: state.version,
                ui_epoch: state.epoch,
                processing,
                enhancing,
                page,
                toc,
                error: state.error.clone(),
                notice,
                document_revision:state.document_revision,
                error_serial:state.error_serial,
                error_kind:state.error_kind.clone(),
            },
        )
    };
    event.0.update(event.1);
    true
}
fn ensure_enhancements(reader: &Arc<MobileReader>) {
    let registered = {
        let Ok(mut state) = reader.ai.lock() else {
            return;
        };
        if !state.active || state.interacting {
            return;
        }
        let key = format!("{}:enhancements", state.context);
        if state.requests.contains_key(&key) {
            return;
        }
        let request = chat::ChatRequest::new();
        let context = state.context.clone();
        state.requests.insert(
            key.clone(),
            Running {
                request: request.clone(),
                kind: Kind::Enhancement,
                index: usize::MAX,
                sources: vec![],
            },
        );
        (key, request, context)
    };
    let reader = reader.clone();
    runtime().spawn(async move {
        let (key, request, context) = registered;
        loop {
            let epoch = reader.ai.lock().map(|s| s.epoch).unwrap_or(0);
            match reader
                .clone()
                .enhance_page_cancellable(request.clone())
                .await
            {
                Ok(update) => {
                    if let Some(page) = update.page {
                        if !emit_at(&reader, Some(page), None, Some(epoch), Some(&context)) {
                            mark_dirty(&reader).await;
                        }
                    } else if update.changed {
                        content_changed(&reader).await;
                    }
                    if !update.pending {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if let Ok(mut state) = reader.ai.lock() {
            if state.context == context
                && state
                    .requests
                    .get(&key)
                    .is_some_and(|w| Arc::ptr_eq(&w.request, &request))
            {
                state.requests.remove(&key);
            }
        }
        emit(&reader, None, None);
    });
}
pub(crate) async fn content_changed(reader: &Arc<MobileReader>) {
    let (epoch, context, dirty) = {
        let Ok(mut state) = reader.ai.lock() else {
            return;
        };
        if !state.active || state.interacting {
            return;
        }
        let dirty = state.view_dirty;
        state.view_dirty = false;
        (state.epoch, state.context.clone(), dirty)
    };
    if !dirty {
        emit(reader, None, None);
        return;
    }
    let r = reader.clone();
    let (page, document_changed) = work(move || {
        r.with(|s| {
            if s.document.is_some() {
                return documents::refresh(s).map(|changed| (None, changed));
            }
            if s.enhancements.dirty && s.selected.is_none() && s.document.is_none() {
                render(s).map(|page| (Some(page), false))
            } else {
                Ok((None, false))
            }
        })
    })
    .await
    .unwrap_or((None,false));
    if document_changed {
        if let Ok(mut state)=reader.ai.lock() {
            if state.epoch==epoch && state.context==context {state.document_revision=state.document_revision.wrapping_add(1);}
        }
    }
    if !emit_at(reader, page, None, Some(epoch), Some(&context)) {
        mark_dirty(reader).await;
    }
}
async fn mark_dirty(reader: &Arc<MobileReader>) {
    if let Ok(mut state) = reader.ai.lock() {
        state.view_dirty = true;
    }
    let r = reader.clone();
    let _ = work(move || {
        r.with(|s| {
            s.enhancements.dirty = true;
            Ok(())
        })
    })
    .await;
}
pub(crate) async fn scope_changed(reader: &Arc<MobileReader>, target: &requests::Target) {
    if let Ok(mut state) = reader.ai.lock() {
        if reading_ai::relevant(target.index, &target.ranges, &state.demand) {
            state.view_dirty = true;
        }
    }
    content_changed(reader).await;
}
pub(crate) fn visible(reader: &MobileReader, index: usize, sources: &[SourceRange]) -> bool {
    reader.ai.lock().is_ok_and(|state| {
        !state.active || (!state.interacting && reading_ai::relevant(index, sources, &state.demand))
    })
}
