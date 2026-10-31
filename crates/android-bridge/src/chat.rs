use crate::*;
use rebook_app_core::{
    highlights::HighlightRepository,
    plugins::{self, ChatRole, ChatStreamEvent, ChatTurn},
};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct PendingChanges {
    pub response: plugins::ChatResponse,
    pub rewrites: Vec<plugins::BlockRewrite>,
    pub replace_rewrites: bool,
}

#[derive(Clone, Serialize, Deserialize, uniffi::Record)]
pub struct ChatMessage {
    pub user: bool,
    pub content: String,
    #[serde(default)]
    pub images: Vec<crate::chat_media::ChatAttachment>,
}
#[derive(Clone, uniffi::Record)]
pub struct ChatChange {
    pub kind: String,
    pub text: String,
}
#[derive(uniffi::Record)]
pub struct ChatReply {
    pub messages: Vec<ChatMessage>,
    pub changes: Vec<ChatChange>,
}
#[derive(uniffi::Object)]
pub struct ChatRequest {
    pub(crate) cancelled: AtomicBool,
    pub(crate) notify: Arc<tokio::sync::Notify>,
    progress: Mutex<Option<Arc<dyn AiProgressListener>>>,
}
#[uniffi::export]
impl ChatRequest {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            notify: Arc::new(tokio::sync::Notify::new()),
            progress: Mutex::new(None),
        })
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_one();
    }
}
#[uniffi::export(with_foreign)]
pub trait AiProgressListener: Send + Sync {
    fn update(&self, stage: String, completed: u32, total: u32);
}
#[uniffi::export]
impl ChatRequest {
    pub fn observe_ai(&self, listener: Arc<dyn AiProgressListener>) {
        if let Ok(mut value) = self.progress.lock() {
            *value = Some(listener);
        }
    }
}
impl ChatRequest {
    pub(crate) fn report(&self, stage: &str, completed: usize, total: usize) {
        let listener = self.progress.lock().ok().and_then(|v| v.clone());
        if !self.cancelled.load(Ordering::Acquire) {
            if let Some(listener) = listener {
                listener.update(stage.into(), completed as u32, total as u32);
            }
        }
    }
}
#[uniffi::export(with_foreign)]
pub trait ChatListener: Send + Sync {
    fn update(&self, content: String, reasoning: String, activity: String);
}
pub(crate) fn load_messages(path: &std::path::Path) -> Vec<ChatMessage> {
    std::fs::File::open(path)
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            file.take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() <= 4 * 1024 * 1024).then_some(bytes)
        })
        .and_then(|b| serde_json::from_slice::<Vec<ChatMessage>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .rev()
        .take(60)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
#[uniffi::export]
impl MobileReader {
    pub async fn chat_history(self: Arc<Self>) -> Result<Vec<ChatMessage>> {
        work(move || self.with(|s| Ok(s.messages.clone()))).await
    }
    pub async fn clear_chat(self: Arc<Self>) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.messages.clear();
                s.pending = None;
                rebook_app_core::persistence::write_json_atomic(&s.chat_path, &s.messages)
                    .map_err(error)?;
                let images = s.chat_path.with_extension("images");
                if images.exists() {
                    std::fs::remove_dir_all(images).map_err(error)?;
                }
                Ok(())
            })
        })
        .await
    }
    pub async fn chat(
        self: Arc<Self>,
        question: String,
        request: Arc<ChatRequest>,
        listener: Arc<dyn ChatListener>,
    ) -> Result<ChatReply> {
        self.chat_with_images(question, vec![], request, listener)
            .await
    }
    pub async fn chat_with_images(
        self: Arc<Self>,
        question: String,
        images: Vec<crate::chat_media::ChatAttachment>,
        request: Arc<ChatRequest>,
        listener: Arc<dyn ChatListener>,
    ) -> Result<ChatReply> {
        work(move || {
            self.with(|s| {
                if (question.trim().is_empty() && images.is_empty())
                    || question.chars().count() > 16_000
                {
                    return Err(error("请输入不超过 16000 字的问题"));
                }
                if s.pending.is_some() {
                    return Err(error("请先确认或放弃上次助手提出的修改"));
                }
                if request.cancelled.load(Ordering::Acquire) {
                    return Err(error("已停止生成"));
                }
                let snapshot = s.session.snapshot();
                let location = snapshot.location;
                let section = s
                    .source
                    .parse_section(location.section_index)
                    .map_err(error)?;
                let title = plugins::section_title(
                    s.source.as_ref(),
                    location.section_index,
                    &section.blocks,
                );
                let context = plugins::ChatReadingContext {
                    unit_index: location.section_index,
                    unit_id: Some(section.id.as_str().to_owned()),
                    unit_kind: "section".into(),
                    unit_title: Some(title.clone()),
                    section_index: location.section_index,
                    section_id: Some(section.id.as_str().to_owned()),
                    section_title: Some(title),
                    toc_label: None,
                    toc_href: Some(section.href.to_string()),
                    section_fraction: location.page_index as f64
                        / location.page_count.max(1) as f64,
                    total_fraction: snapshot.total_progression,
                    segment_index: location.segment_index,
                    segment_count: location.segment_count,
                    page_index: location.page_index,
                    page_count: location.page_count,
                };
                let mut attached = images
                    .iter()
                    .map(|a| crate::chat_media::load(s, a))
                    .collect::<Result<Vec<_>>>()?;
                if let Some(selected) = &s.selected {
                    attached.extend(
                        plugins::chat_media::capture_images(s.source.as_ref(), &selected.ranges)
                            .map_err(error)?,
                    );
                }
                plugins::chat_media::validate_image_count(attached.len()).map_err(error)?;
                let attachments = attached
                    .iter()
                    .map(|image| crate::chat_media::store(s, image))
                    .collect::<Result<Vec<_>>>()?;
                let mut budget = 20usize * 1024 * 1024;
                for image in &attached {
                    budget = budget.saturating_sub(image.png_bytes().map_err(error)?.len());
                }
                let history_images = crate::chat_media::message_images(s, &s.messages, &mut budget);
                let selection = if s.selected.is_some() || !attached.is_empty() {
                    Some(plugins::ChatSelection {
                        images: attached,
                        text: s
                            .selected
                            .as_ref()
                            .map_or_else(String::new, |v| v.text.clone()),
                        ranges: s
                            .selected
                            .as_ref()
                            .map_or_else(Vec::new, |v| v.ranges.clone()),
                    })
                } else {
                    None
                };
                let settings = plugins::PluginSettings::load_default().map_err(error)?;
                let language = settings.resolved_target_language("简体中文");
                let history = s
                    .messages
                    .iter()
                    .zip(history_images)
                    .map(|(m, images)| ChatTurn {
                        thinking_seconds: None,
                        progress: vec![],
                        images,
                        role: if m.user {
                            ChatRole::User
                        } else {
                            ChatRole::Assistant
                        },
                        content: m.content.clone(),
                        display_content: None,
                    })
                    .collect();
                let mut content = String::new();
                let mut reasoning = String::new();
                let mut activity = String::new();
                let before = s.pipeline.rewrite.list_rewrites(None).map_err(error)?;
                let mut response = runtime()
                    .block_on(plugins::chat_with_book(
                        s.source.clone(),
                        s.format,
                        plugins::ChatRequestKind::Normal,
                        s.pipeline.rewrite.clone(),
                        s.book_id.clone(),
                        selection,
                        s.store.highlights_for_book(&s.book_id).map_err(error)?,
                        settings,
                        history,
                        question.clone(),
                        context,
                        language,
                        request.notify.clone(),
                        |event| {
                            match event {
                                ChatStreamEvent::Content(v) => content = v,
                                ChatStreamEvent::Reasoning(v) => reasoning = v,
                                ChatStreamEvent::Tool { text, .. } => activity = text,
                            }
                            listener.update(content.clone(), reasoning.clone(), activity.clone());
                        },
                    ))
                    .map_err(error)?;
                let after = s.pipeline.rewrite.list_rewrites(None).map_err(error)?;
                let replace_rewrites = before != after;
                // Tools may temporarily stage rewrites to inspect their own output.
                // Roll these back before returning; Android requires explicit review.
                for transaction in std::mem::take(&mut response.rewrite_transactions)
                    .into_iter()
                    .rev()
                {
                    s.pipeline.rewrite.rollback(transaction).map_err(error)?;
                }
                if request.cancelled.load(Ordering::Acquire) {
                    return Err(error("已停止生成"));
                }
                let mut changes = after
                    .iter()
                    .filter(|r| !before.contains(r))
                    .map(|r| ChatChange {
                        kind: "改写正文".into(),
                        text: r.text.clone(),
                    })
                    .chain(response.annotation_actions.iter().map(|a| {
                        match a {
                            plugins::ChatAnnotationAction::Create(a) => ChatChange {
                                kind: "新增批注".into(),
                                text: format!("{}\n{}", a.quote, a.note.as_deref().unwrap_or("")),
                            },
                            plugins::ChatAnnotationAction::Update(a) => ChatChange {
                                kind: "修改批注".into(),
                                text: format!("{}\n{}", a.quote, a.note.as_deref().unwrap_or("")),
                            },
                            plugins::ChatAnnotationAction::Delete { annotation_id } => ChatChange {
                                kind: "删除批注".into(),
                                text: s
                                    .store
                                    .highlights_for_book(&s.book_id)
                                    .unwrap_or_default()
                                    .into_iter()
                                    .find(|a| a.id == *annotation_id)
                                    .map_or_else(|| annotation_id.clone(), |a| a.quote),
                            },
                        }
                    }))
                    .collect::<Vec<_>>();
                for removed in before.iter().filter(|old| {
                    !after.iter().any(|new| {
                        new.section_index == old.section_index && new.block_id == old.block_id
                    })
                }) {
                    changes.push(ChatChange {
                        kind: "还原正文".into(),
                        text: format!("将以下临时改写还原为原文：\n{}", removed.text),
                    });
                }
                s.messages.push(ChatMessage {
                    user: true,
                    content: question,
                    images: attachments,
                });
                s.messages.push(ChatMessage {
                    user: false,
                    content: response.content.clone(),
                    images: vec![],
                });
                if s.messages.len() > 60 {
                    s.messages.drain(..s.messages.len() - 60);
                }
                rebook_app_core::persistence::write_json_atomic(&s.chat_path, &s.messages)
                    .map_err(error)?;
                if !changes.is_empty() {
                    s.pending = Some(PendingChanges {
                        response,
                        rewrites: after,
                        replace_rewrites,
                    });
                }
                Ok(ChatReply {
                    messages: s.messages.clone(),
                    changes,
                })
            })
        })
        .await
    }
    pub async fn resolve_chat_changes(self: Arc<Self>, accept: bool) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                if let Some(pending) = s.pending.as_ref().filter(|_| accept) {
                    let response = &pending.response;
                    // Keep the proposal on failure so it can be reviewed/retried.
                    for action in &response.annotation_actions {
                        match action {
                            plugins::ChatAnnotationAction::Create(a) => {
                                s.store.insert_highlight(a).map_err(error)?
                            }
                            plugins::ChatAnnotationAction::Update(a) => {
                                s.store.update_highlight(a).map_err(error)?;
                            }
                            plugins::ChatAnnotationAction::Delete { annotation_id } => {
                                s.store.remove_highlight(annotation_id).map_err(error)?;
                            }
                        }
                    }
                    if pending.replace_rewrites {
                        let (rollback, _) =
                            s.pipeline.rewrite.clear_rewrites(None).map_err(error)?;
                        if let Err(err) = s.pipeline.rewrite.apply_rewrites(&pending.rewrites) {
                            s.pipeline.rewrite.rollback(rollback).map_err(error)?;
                            return Err(error(err));
                        }
                    }
                }
                s.pending = None;
                s.session.refresh_source().map_err(error)?;
                render(s)
            })
        })
        .await
    }
    pub async fn undo_rewrites(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                s.pipeline.rewrite.clear_rewrites(None).map_err(error)?;
                s.session.refresh_source().map_err(error)?;
                render(s)
            })
        })
        .await
    }
}
