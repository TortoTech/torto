use super::*;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Clone, uniffi::Record)]
pub struct ChatSource {
    pub locator: String,
    pub title: String,
    pub excerpt: String,
}

pub(super) fn citation_links(content: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    rebook_app_core::chat_markdown::parse(content)
        .into_iter()
        .flat_map(|block| {
            block
                .runs
                .into_iter()
                .chain(block.rows.into_iter().flatten().flatten())
        })
        .filter_map(|run| {
            (rebook_app_core::chat_markdown::parse_chat_citation(&run.link).is_some()
                && seen.insert(run.link.clone()))
            .then_some(run.link)
        })
        .take(24)
        .collect()
}

pub(super) struct StreamState {
    started: Instant,
    pub content: String,
    pub reasoning: String,
    pub activity: String,
    progress: Vec<String>,
    reasoning_index: Option<usize>,
    tools: HashMap<String, usize>,
    thinking_seconds: Option<u64>,
}
impl Default for StreamState {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            content: String::new(),
            reasoning: String::new(),
            activity: String::new(),
            progress: vec![],
            reasoning_index: None,
            tools: HashMap::new(),
            thinking_seconds: None,
        }
    }
}
fn append_bounded(target: &mut String, text: &str) {
    let remaining = 100_000usize.saturating_sub(target.len());
    let mut end = text.len().min(remaining);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}
impl StreamState {
    pub fn update(&mut self, event: ChatStreamEvent) {
        match event {
            ChatStreamEvent::Content(content) => {
                if !content.is_empty() && self.thinking_seconds.is_none() {
                    self.thinking_seconds = Some(self.started.elapsed().as_secs());
                }
                self.content = content;
            }
            ChatStreamEvent::Reasoning(text) => {
                let before = self.reasoning.len();
                append_bounded(&mut self.reasoning, &text);
                if self.reasoning_index.is_none() && self.progress.len() < 128 {
                    self.reasoning_index = Some(self.progress.len());
                    self.progress.push(String::new());
                }
                if let Some(index) = self.reasoning_index {
                    append_bounded(&mut self.progress[index], &self.reasoning[before..]);
                }
            }
            ChatStreamEvent::Tool { id, text, done } => {
                self.reasoning_index = None;
                self.activity = text.chars().take(2000).collect();
                if self.progress.len() >= 128 && !self.tools.contains_key(&id) {
                    return;
                }
                let index = *self.tools.entry(id).or_insert_with(|| {
                    self.progress.push(String::new());
                    self.progress.len() - 1
                });
                self.progress[index] = format!(
                    "{} {}",
                    self.activity.trim_end(),
                    if done { "✓" } else { "…" }
                );
            }
        }
    }
}
pub(super) fn append_turn(
    s: &mut ReaderState,
    id: &str,
    question: String,
    images: Vec<crate::chat_media::ChatAttachment>,
    content: String,
    stream: &StreamState,
    stopped: bool,
) -> Result<()> {
    let mut messages = s.messages.clone();
    messages.push(ChatMessage {
        id: format!("{id}-question"),
        user: true,
        content: question,
        images,
        ..Default::default()
    });
    let citations = citation_links(&content);
    messages.push(ChatMessage {
        id: id.into(),
        content,
        progress: stream.progress.clone(),
        thinking_seconds: Some(
            stream
                .thinking_seconds
                .unwrap_or_else(|| stream.started.elapsed().as_secs()),
        ),
        stopped,
        citations,
        ..Default::default()
    });
    // Retain every existing turn for this reading session. Reaching the bounded
    // memory budget rejects the new turn rather than silently deleting history.
    if messages.len() > 1000
        || messages
            .iter()
            .map(|m| m.content.len() + m.progress.iter().map(String::len).sum::<usize>())
            .sum::<usize>()
            > 4 * 1024 * 1024
    {
        return Err(error("对话内容较多，请删除部分记录后重试"));
    }
    s.messages = messages;
    Ok(())
}

#[uniffi::export]
impl MobileReader {
    pub async fn delete_chat_turn(self: Arc<Self>, id: String) -> Result<Vec<ChatMessage>> {
        work(move || {
            self.with(|s| {
                let Some(index) = s.messages.iter().position(|m| m.id == id && !m.user) else {
                    return Ok(s.messages.clone());
                };
                let start = if index > 0 && s.messages[index - 1].user {
                    index - 1
                } else {
                    index
                };
                let latest = index + 1 == s.messages.len();
                let mut messages = s.messages.clone();
                messages.drain(start..=index);
                s.messages = messages;
                if latest {
                    s.pending = None;
                }
                Ok(s.messages.clone())
            })
        })
        .await
    }
    pub async fn chat_sources(
        self: Arc<Self>,
        locators: Vec<String>,
    ) -> Result<Vec<super::ChatSource>> {
        work(move || {
            self.with(|s| {
                let mut sections = HashMap::new();
                let mut seen = HashSet::new();
                let mut sources = vec![];
                for locator in locators.into_iter().take(24) {
                    if !seen.insert(locator.clone()) {
                        continue;
                    }
                    let Some(citation) =
                        rebook_app_core::chat_markdown::parse_chat_citation(&locator)
                    else {
                        continue;
                    };
                    if citation.section_index >= s.source.book().sections.len() {
                        continue;
                    }
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        sections.entry(citation.section_index)
                    {
                        let Ok(section) = s.source.parse_section(citation.section_index) else {
                            continue;
                        };
                        entry.insert(section);
                    }
                    let section = &sections[&citation.section_index];
                    let title = plugins::section_title(
                        s.source.as_ref(),
                        citation.section_index,
                        &section.blocks,
                    );
                    let excerpt = if let Some(node) = citation.node {
                        section.blocks.iter().find_map(|block| {
                            block
                                .text_blocks()
                                .find(|text| {
                                    text.source.as_ref().is_some_and(|r| r.start.node == node)
                                })
                                .map(rebook_app_core::reading_text::text_block_focus_text)
                                .or_else(|| {
                                    rebook_app_core::reading_text::block_source_range(block)
                                        .filter(|r| r.start.node == node)
                                        .map(|_| {
                                            rebook_app_core::reading_text::block_focus_text(block)
                                        })
                                })
                        })
                    } else {
                        Some(
                            section
                                .blocks
                                .iter()
                                .filter(|block| !matches!(block,rebook_publication::Block::Text(text) if text.kind.is_heading()))
                                .map(rebook_app_core::reading_text::block_focus_text)
                                .filter(|t| !t.trim().is_empty())
                                .take(3)
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                    };
                    let Some(excerpt) = excerpt else {
                        continue;
                    };
                    sources.push(ChatSource {
                        locator,
                        title,
                        excerpt: excerpt
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(240)
                            .collect(),
                    });
                }
                Ok(sources)
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_accumulates_chunks_and_updates_tool_entries_in_place() {
        let mut stream = StreamState::default();
        stream.update(ChatStreamEvent::Reasoning("第一".into()));
        stream.update(ChatStreamEvent::Reasoning("段".into()));
        stream.update(ChatStreamEvent::Tool {
            id: "search".into(),
            text: "检索原文".into(),
            done: false,
        });
        stream.update(ChatStreamEvent::Tool {
            id: "search".into(),
            text: "检索原文".into(),
            done: true,
        });
        stream.update(ChatStreamEvent::Reasoning("第二段".into()));
        assert_eq!(stream.reasoning, "第一段第二段");
        assert_eq!(stream.progress, vec!["第一段", "检索原文 ✓", "第二段"]);
        assert_eq!(
            citation_links(
                "[1](link://j/0/n1) [2](link://j/0/n1) [3](https://example.com) `link://j/2/n3`"
            ),
            vec!["link://j/0/n1"]
        );
    }
    #[test]
    fn reasoning_budget_spans_tool_boundaries_without_splitting_unicode() {
        let mut stream = StreamState::default();
        stream.update(ChatStreamEvent::Reasoning("文".repeat(40_000)));
        stream.update(ChatStreamEvent::Tool {
            id: "search".into(),
            text: "Search".into(),
            done: true,
        });
        stream.update(ChatStreamEvent::Reasoning("abcdef".into()));
        assert_eq!(stream.reasoning.len(), 100_000);
        assert_eq!(stream.progress[0].len(), 99_999);
        assert_eq!(stream.progress[2], "a");
    }
}
