//! Frontend-neutral AI assistant state and request contracts.
//!
//! This crate deliberately excludes toolkit entities, window events, book
//! mutation implementations, and persistence. A frontend reduces user and
//! streaming events into [`ChatSession`], while the shared provider adapter and
//! tool-loop state machine execute the resulting [`ChatSubmission`].

mod book_tools;
mod citation;
mod json;
mod mutation;
mod rewrite;
mod search;
mod tool_loop;
mod translation;

pub use book_tools::BookSearchToolHost;
pub use citation::{
    CHAT_CITATION_INSTRUCTION, CHAT_CITATION_PREFIX, chat_citation_link, chat_citation_marker,
    chat_citation_marker_from_link, citations_for_model,
};
pub use json::parse_llm_json;
pub use mutation::{
    AssistantAnnotationAction, AssistantAnnotationTarget, AssistantMutationResolution,
    BlockRewrite, BlockTranslation, PendingAnnotationActions, TranslationBlockInput,
    TranslationMode, cancel_annotation_actions, confirm_annotation_actions,
};
pub use rewrite::{RewriteBookSource, RewriteTransaction};
pub use search::{
    BookSearchResult, search_book, search_section, section_title, text_block_kind, text_block_text,
};
pub use tool_loop::{
    AssistantToolCall, AssistantToolFuture, AssistantToolHost, AssistantToolResult, OpenAiToolLoop,
    ToolLoopStep,
};
pub use translation::TranslationBookSource;

use std::env;
use std::fs;
use std::time::Duration;

use directories::ProjectDirs;
use keyring::Entry;
use rebook_publication::SourceRange;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};

const SETTINGS_FILE: &str = "plugins.json";
const AI_CREDENTIAL_SERVICE: &str = "Rebook AI";
const DEFAULT_HISTORY_TURNS: u16 = 10;

/// Author of one durable chat turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    /// OpenAI-compatible role name used by the current provider adapter.
    #[must_use]
    pub const fn api_name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One completed turn. `display_content` may omit model-only context while the
/// request retains it separately in [`ChatSubmission::selection`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
    pub display_content: Option<String>,
}

/// Source-backed text supplied to the assistant without forcing a frontend to
/// render a visible citation badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatSelection {
    pub text: String,
    pub ranges: Vec<SourceRange>,
}

/// Pagination and semantic navigation context captured when a request starts.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatReadingContext {
    pub unit_index: usize,
    pub unit_id: Option<String>,
    pub unit_kind: String,
    pub unit_title: Option<String>,
    pub section_index: usize,
    pub section_id: Option<String>,
    pub section_title: Option<String>,
    pub toc_label: Option<String>,
    pub toc_href: Option<String>,
    pub section_fraction: f64,
    pub total_fraction: f64,
    pub segment_index: usize,
    pub segment_count: usize,
    pub page_index: usize,
    pub page_count: usize,
}

/// Stable identity of one request within an assistant session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChatRequestId(u64);

impl ChatRequestId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Immutable payload passed from a frontend reducer to a provider runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatSubmission {
    pub request_id: ChatRequestId,
    pub session_id: u64,
    /// Completed turns before the newly submitted user turn.
    pub history: Vec<ChatTurn>,
    pub question: String,
    pub selection: Option<ChatSelection>,
    pub current: ChatReadingContext,
    pub response_language: String,
}

/// Toolkit-independent activity exposed to any chat presentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ChatActivity {
    #[default]
    Idle,
    Pending {
        request_id: ChatRequestId,
    },
    Streaming {
        request_id: ChatRequestId,
        content: String,
    },
}

impl ChatActivity {
    #[must_use]
    pub const fn request_id(&self) -> Option<ChatRequestId> {
        match self {
            Self::Idle => None,
            Self::Pending { request_id } | Self::Streaming { request_id, .. } => Some(*request_id),
        }
    }

    #[must_use]
    pub fn streaming_content(&self) -> Option<&str> {
        match self {
            Self::Streaming { content, .. } => Some(content),
            Self::Idle | Self::Pending { .. } => None,
        }
    }
}

/// Rejection returned before a request leaves the deterministic reducer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatSubmitError {
    Empty,
    Busy,
}

/// Frontend-neutral chat state machine shared by egui and GPUI presentations.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatSession {
    session_id: u64,
    next_request_id: u64,
    turns: Vec<ChatTurn>,
    draft: String,
    selection: Option<ChatSelection>,
    activity: ChatActivity,
    error: Option<String>,
}

impl ChatSession {
    #[must_use]
    pub const fn new(session_id: u64) -> Self {
        Self {
            session_id,
            next_request_id: 1,
            turns: Vec::new(),
            draft: String::new(),
            selection: None,
            activity: ChatActivity::Idle,
            error: None,
        }
    }

    #[must_use]
    pub const fn session_id(&self) -> u64 {
        self.session_id
    }

    #[must_use]
    pub fn turns(&self) -> &[ChatTurn] {
        &self.turns
    }

    #[must_use]
    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn set_draft(&mut self, draft: impl Into<String>) {
        self.draft = draft.into();
        self.error = None;
    }

    pub fn set_selection(&mut self, selection: Option<ChatSelection>) {
        self.selection = selection;
    }

    #[must_use]
    pub const fn activity(&self) -> &ChatActivity {
        &self.activity
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[must_use]
    pub const fn is_pending(&self) -> bool {
        !matches!(self.activity, ChatActivity::Idle)
    }

    /// Captures one immutable provider request and immediately records the user
    /// turn, so later streaming events cannot observe a mutated draft/context.
    pub fn submit(
        &mut self,
        current: ChatReadingContext,
        response_language: impl Into<String>,
    ) -> Result<ChatSubmission, ChatSubmitError> {
        if self.is_pending() {
            return Err(ChatSubmitError::Busy);
        }
        let question = self.draft.trim().to_owned();
        if question.is_empty() && self.selection.is_none() {
            return Err(ChatSubmitError::Empty);
        }
        let history = self.turns.clone();
        let request_id = ChatRequestId(self.next_request_id);
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.turns.push(ChatTurn {
            role: ChatRole::User,
            content: question.clone(),
            display_content: None,
        });
        self.draft.clear();
        self.error = None;
        self.activity = ChatActivity::Pending { request_id };
        Ok(ChatSubmission {
            request_id,
            session_id: self.session_id,
            history,
            question,
            selection: self.selection.clone(),
            current,
            response_language: response_language.into(),
        })
    }

    /// Replaces the currently streamed snapshot. Provider adapters may call this
    /// with cumulative text without making the reducer provider-specific.
    pub fn update_stream(&mut self, request_id: ChatRequestId, content: impl Into<String>) -> bool {
        if self.activity.request_id() != Some(request_id) {
            return false;
        }
        self.activity = ChatActivity::Streaming {
            request_id,
            content: content.into(),
        };
        true
    }

    pub fn complete(&mut self, request_id: ChatRequestId, content: impl Into<String>) -> bool {
        if self.activity.request_id() != Some(request_id) {
            return false;
        }
        self.turns.push(ChatTurn {
            role: ChatRole::Assistant,
            content: content.into(),
            display_content: None,
        });
        self.activity = ChatActivity::Idle;
        self.error = None;
        true
    }

    pub fn fail(&mut self, request_id: ChatRequestId, error: impl Into<String>) -> bool {
        if self.activity.request_id() != Some(request_id) {
            return false;
        }
        self.activity = ChatActivity::Idle;
        self.error = Some(error.into());
        true
    }
}

/// Validated OpenAI-compatible endpoint selected from Torto's existing plugin
/// settings. It is intentionally read-only so both desktop frontends consume a
/// single persisted configuration without introducing a second settings file.
#[derive(Clone)]
pub struct AssistantEndpoint {
    base_url: String,
    api_key: String,
    model: String,
    history_turns: usize,
}

impl AssistantEndpoint {
    pub fn load_default() -> Result<Self, String> {
        let project = ProjectDirs::from("com", "Rebook", "Rebook")
            .ok_or_else(|| "无法确定插件配置目录".to_owned())?;
        let path = project.config_dir().join(SETTINGS_FILE);
        let settings = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoredAssistantSettings>(&bytes)
                .map(StoredAssistantSettings::with_defaults)
                .map_err(|error| format!("读取 AI 配置失败：{error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                StoredAssistantSettings::default().with_defaults()
            }
            Err(error) => return Err(format!("读取 AI 配置失败：{error}")),
        };
        Self::from_stored(settings)
    }

    fn from_stored(settings: StoredAssistantSettings) -> Result<Self, String> {
        let configured = settings
            .providers
            .iter()
            .find(|provider| provider.id == settings.chat_provider)
            .or_else(|| settings.providers.first());
        let provider_id = configured
            .map(|provider| provider.id.trim())
            .filter(|id| !id.is_empty())
            .unwrap_or("openai")
            .to_owned();
        let provider_name = configured
            .map(|provider| provider.name.trim())
            .filter(|name| !name.is_empty())
            .unwrap_or("AI Provider")
            .to_owned();
        let base_url = env_non_empty("REBOOK_AI_BASE_URL")
            .or_else(|| configured.map(|provider| provider.base_url.trim().to_owned()))
            .unwrap_or_default();
        if base_url.is_empty() {
            return Err(format!("{provider_name} 的 API 地址不能为空"));
        }
        if !base_url.starts_with("https://") && !base_url.starts_with("http://") {
            return Err(format!(
                "{provider_name} 的 API 地址必须使用 http:// 或 https://"
            ));
        }
        let model = env_non_empty("REBOOK_AI_MODEL").unwrap_or(settings.chat_model);
        let model = model.trim().to_owned();
        if model.is_empty() {
            return Err(format!(
                "请先在“设置 → AI Chat”中选择 {provider_name} 下的模型"
            ));
        }
        let api_key = match env_non_empty("REBOOK_AI_API_KEY") {
            Some(api_key) => api_key,
            None => match Entry::new(AI_CREDENTIAL_SERVICE, &provider_id)
                .map_err(|error| format!("读取 AI 凭据失败：{error}"))?
                .get_password()
            {
                Ok(api_key) => api_key,
                Err(keyring::Error::NoEntry) => String::new(),
                Err(error) => return Err(format!("读取 AI 凭据失败：{error}")),
            },
        };
        if api_key.trim().is_empty() {
            return Err(format!(
                "请先在“设置 → AI”中填写 {provider_name} 的 API Key"
            ));
        }
        Ok(Self {
            base_url,
            api_key,
            model,
            history_turns: usize::from(settings.chat_history_turns.clamp(1, 50)),
        })
    }

    #[cfg(test)]
    fn test(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: "secret".into(),
            model: "test-model".into(),
            history_turns: 10,
        }
    }
}

/// Provider executor shared by toolkit frontends. Frontends supply neutral
/// capability hosts; the runtime owns provider messages and tool-loop protocol
/// without depending on an application toolkit.
pub struct AssistantRuntime {
    endpoint: AssistantEndpoint,
    client: Client,
}

impl AssistantRuntime {
    pub fn load_default() -> Result<Self, String> {
        Self::new(AssistantEndpoint::load_default()?)
    }

    pub fn new(endpoint: AssistantEndpoint) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .map_err(|error| format!("创建 AI 客户端失败：{error}"))?;
        Ok(Self { endpoint, client })
    }

    pub async fn complete(&self, submission: &ChatSubmission) -> Result<String, String> {
        let messages = chat_messages(&self.endpoint, submission);
        let message = self.request_completion_message(&messages, None).await?;
        message
            .get("content")
            .and_then(message_content)
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| "AI 返回了空内容".to_owned())
    }

    /// Runs the same provider loop for any application-owned, toolkit-neutral
    /// tool host. Book parsing and mutations remain behind the host boundary.
    pub async fn complete_with_tool_host<H>(
        &self,
        submission: &ChatSubmission,
        host: &mut H,
        max_tool_steps: usize,
    ) -> Result<String, String>
    where
        H: AssistantToolHost,
    {
        let tools = host.definitions();
        if !tools.is_array() {
            return Err("AI 工具定义必须是数组".into());
        }
        let mut tool_loop =
            OpenAiToolLoop::new(chat_messages(&self.endpoint, submission), max_tool_steps);
        loop {
            let request_messages = match tool_loop.request_messages() {
                Ok(messages) => messages,
                Err(error) => return Err(rollback_tool_host(host, error)),
            };
            let message = match self
                .request_completion_message(request_messages, Some(&tools))
                .await
            {
                Ok(message) => message,
                Err(error) => return Err(rollback_tool_host(host, error)),
            };
            let calls = match tool_loop.accept_assistant_message(message) {
                Ok(ToolLoopStep::Complete(content)) => return Ok(content),
                Ok(ToolLoopStep::CallTools(calls)) => calls,
                Err(error) => return Err(rollback_tool_host(host, error)),
            };
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                let call_id = call.id().to_owned();
                let content = host.execute(call).await;
                results.push(AssistantToolResult::new(call_id, content));
            }
            if let Err(error) = tool_loop.apply_tool_results(results) {
                return Err(rollback_tool_host(host, error));
            }
        }
    }

    async fn request_completion_message(
        &self,
        messages: &[Value],
        tools: Option<&Value>,
    ) -> Result<Value, String> {
        let mut body = json!({
            "model": self.endpoint.model,
            "messages": messages,
            "temperature": 0.2,
            "stream": false,
        });
        if let Some(tools) = tools {
            body["tools"] = tools.clone();
        }
        let response = self
            .client
            .post(chat_completions_url(&self.endpoint.base_url))
            .bearer_auth(self.endpoint.api_key.trim())
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("AI 请求失败：{error}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("读取 AI 响应失败：{error}"))?;
        let payload = serde_json::from_str::<Value>(&text)
            .map_err(|error| format!("AI 响应不是有效 JSON：{error}"))?;
        if !status.is_success() {
            let message = payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or(&text);
            return Err(format!("AI 服务返回 {status}：{message}"));
        }
        payload
            .pointer("/choices/0/message")
            .cloned()
            .ok_or_else(|| "AI 响应缺少 assistant message".to_owned())
    }

    /// Runs the async provider on an isolated Tokio runtime. GPUI calls this on
    /// its background executor, so no HTTP reactor or blocking work leaks onto
    /// the main-thread event loop.
    pub fn complete_blocking(&self, submission: &ChatSubmission) -> Result<String, String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("创建 AI 运行时失败：{error}"))?
            .block_on(self.complete(submission))
    }

    /// Blocking adapter for a capability-backed completion. This is intended
    /// for a frontend background executor, never its UI event thread.
    pub fn complete_with_tool_host_blocking<H>(
        &self,
        submission: &ChatSubmission,
        host: &mut H,
        max_tool_steps: usize,
    ) -> Result<String, String>
    where
        H: AssistantToolHost,
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("创建 AI 运行时失败：{error}"))?
            .block_on(self.complete_with_tool_host(submission, host, max_tool_steps))
    }
}

fn rollback_tool_host(host: &mut impl AssistantToolHost, error: String) -> String {
    match host.rollback() {
        Ok(()) => error,
        Err(rollback_error) => format!("{error}；回滚工具事务也失败：{rollback_error}"),
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct StoredAssistantSettings {
    providers: Vec<StoredProvider>,
    chat_provider: String,
    chat_model: String,
    chat_history_turns: u16,
}

impl StoredAssistantSettings {
    fn with_defaults(mut self) -> Self {
        if self.chat_history_turns == 0 {
            self.chat_history_turns = DEFAULT_HISTORY_TURNS;
        }
        self
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct StoredProvider {
    id: String,
    name: String,
    base_url: String,
}

fn env_non_empty(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
fn chat_request_body(endpoint: &AssistantEndpoint, submission: &ChatSubmission) -> Value {
    json!({
        "model": endpoint.model,
        "messages": chat_messages(endpoint, submission),
        "temperature": 0.2,
        "stream": false,
    })
}

fn chat_messages(endpoint: &AssistantEndpoint, submission: &ChatSubmission) -> Vec<Value> {
    let mut messages = vec![json!({
        "role": "system",
        "content": focus_system_prompt(submission),
    })];
    let history_start = submission
        .history
        .len()
        .saturating_sub(endpoint.history_turns);
    messages.extend(submission.history[history_start..].iter().map(|turn| {
        json!({
            "role": turn.role.api_name(),
            "content": turn.content,
        })
    }));
    messages.push(json!({
        "role": "user",
        "content": focus_user_prompt(submission),
    }));
    messages
}

fn focus_system_prompt(submission: &ChatSubmission) -> String {
    let title = submission
        .current
        .unit_title
        .as_deref()
        .or(submission.current.section_title.as_deref())
        .unwrap_or("当前章节");
    format!(
        "你是 Torto 阅读助手。围绕用户当前阅读内容准确、简洁地回答；不要声称看不到已经提供的段落。需要当前段落之外的书籍事实时调用 searchBook。回答语言：{}。当前阅读单元：{}。\n\n{}",
        submission.response_language, title, CHAT_CITATION_INSTRUCTION
    )
}

fn focus_user_prompt(submission: &ChatSubmission) -> String {
    let question = if submission.question.trim().is_empty() {
        "请解释这段内容。"
    } else {
        submission.question.trim()
    };
    submission.selection.as_ref().map_or_else(
        || question.to_owned(),
        |selection| {
            format!(
                "<current-paragraph>\n{}\n</current-paragraph>\n\n<question>\n{}\n</question>",
                selection.text.trim(),
                question
            )
        },
    )
}

fn chat_completions_url(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/chat/completions") {
        base_url.to_owned()
    } else {
        format!("{base_url}/chat/completions")
    }
}

fn message_content(value: &Value) -> Option<String> {
    if let Some(content) = value.as_str() {
        return Some(content.to_owned());
    }
    value.as_array().map(|parts| {
        parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.as_str())
            })
            .collect::<Vec<_>>()
            .join("")
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use super::*;
    use rebook_publication::{SourceAnchor, SpineItemId};

    fn context() -> ChatReadingContext {
        ChatReadingContext {
            unit_index: 1,
            unit_id: Some("chapter-2".into()),
            unit_kind: "section".into(),
            unit_title: Some("Chapter 2".into()),
            section_index: 1,
            section_id: Some("chapter-2".into()),
            section_title: Some("Chapter 2".into()),
            toc_label: Some("Chapter 2".into()),
            toc_href: Some("chapter-2.xhtml".into()),
            section_fraction: 0.25,
            total_fraction: 0.5,
            segment_index: 0,
            segment_count: 1,
            page_index: 2,
            page_count: 8,
        }
    }

    fn selection() -> ChatSelection {
        let spine = SpineItemId::new("chapter-2").unwrap();
        ChatSelection {
            text: "Current paragraph".into(),
            ranges: vec![SourceRange {
                start: SourceAnchor {
                    spine: spine.clone(),
                    node: "paragraph-4".into(),
                    text_offset: 0,
                },
                end: SourceAnchor {
                    spine,
                    node: "paragraph-4".into(),
                    text_offset: 17,
                },
            }],
        }
    }

    #[test]
    fn submission_captures_hidden_source_context_and_pre_submit_history() {
        let mut session = ChatSession::new(7);
        session.set_selection(Some(selection()));
        session.set_draft("  explain this  ");

        let submission = session.submit(context(), "简体中文").unwrap();

        assert_eq!(submission.session_id, 7);
        assert_eq!(submission.question, "explain this");
        assert_eq!(submission.selection.unwrap().text, "Current paragraph");
        assert!(submission.history.is_empty());
        assert_eq!(session.turns()[0].content, "explain this");
        assert!(session.draft().is_empty());
        assert!(session.is_pending());
    }

    #[test]
    fn streaming_and_completion_ignore_stale_request_ids() {
        let mut session = ChatSession::new(3);
        session.set_draft("question");
        let request = session.submit(context(), "English").unwrap();
        let stale = ChatRequestId(request.request_id.get() + 1);

        assert!(!session.update_stream(stale, "stale"));
        assert!(session.update_stream(request.request_id, "partial"));
        assert_eq!(session.activity().streaming_content(), Some("partial"));
        assert!(!session.complete(stale, "wrong"));
        assert!(session.complete(request.request_id, "answer"));
        assert_eq!(session.turns().len(), 2);
        assert_eq!(session.turns()[1].role, ChatRole::Assistant);
        assert!(!session.is_pending());
    }

    #[test]
    fn empty_and_busy_submissions_are_rejected_without_mutating_turns() {
        let mut session = ChatSession::new(1);
        assert_eq!(
            session.submit(context(), "English"),
            Err(ChatSubmitError::Empty)
        );
        session.set_draft("first");
        let request = session.submit(context(), "English").unwrap();
        session.set_draft("second");
        assert_eq!(
            session.submit(context(), "English"),
            Err(ChatSubmitError::Busy)
        );
        assert!(session.fail(request.request_id, "offline"));
        assert_eq!(session.error(), Some("offline"));
        assert_eq!(session.turns().len(), 1);
    }

    #[test]
    fn provider_payload_includes_hidden_paragraph_without_a_ui_reference_label() {
        let mut session = ChatSession::new(9);
        session.set_selection(Some(selection()));
        session.set_draft("What does this mean?");
        let submission = session.submit(context(), "English").unwrap();

        let body = chat_request_body(
            &AssistantEndpoint::test("https://example.com/v1"),
            &submission,
        );
        let prompt = body
            .pointer("/messages/1/content")
            .and_then(Value::as_str)
            .unwrap();

        assert!(prompt.contains("<current-paragraph>\nCurrent paragraph"));
        assert!(prompt.contains("What does this mean?"));
        assert!(!prompt.contains("引用"));
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn completion_url_accepts_provider_roots_and_prebuilt_endpoints() {
        assert_eq!(
            chat_completions_url("https://example.com/v1/"),
            "https://example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://example.com/v1/chat/completions"),
            "https://example.com/v1/chat/completions"
        );
    }

    struct TestToolHost {
        calls: usize,
    }

    impl AssistantToolHost for TestToolHost {
        fn definitions(&self) -> Value {
            json!([{
                "type": "function",
                "function": {
                    "name": "searchBook",
                    "description": "search",
                    "parameters": { "type": "object" },
                }
            }])
        }

        fn execute(&mut self, call: AssistantToolCall) -> AssistantToolFuture<'_> {
            Box::pin(async move {
                self.calls += 1;
                assert_eq!(call.name(), "searchBook");
                assert_eq!(call.arguments().unwrap()["query"], "term");
                json!({ "results": ["hit"] })
            })
        }
    }

    #[test]
    fn shared_runtime_executes_a_repaired_tool_round_before_the_final_answer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let first_body = read_http_json(&mut first);
            assert_eq!(first_body["tools"][0]["function"]["name"], "searchBook");
            write_http_json(
                &mut first,
                &json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call-1",
                                "type": "function",
                                "function": {
                                    "name": "searchBook",
                                    "arguments": "{'query':'term',}",
                                }
                            }]
                        }
                    }]
                }),
            );

            let (mut second, _) = listener.accept().unwrap();
            let second_body = read_http_json(&mut second);
            let messages = second_body["messages"].as_array().unwrap();
            assert_eq!(messages.last().unwrap()["role"], "tool");
            assert_eq!(messages.last().unwrap()["tool_call_id"], "call-1");
            write_http_json(
                &mut second,
                &json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": "done" }
                    }]
                }),
            );
        });

        let mut session = ChatSession::new(11);
        session.set_draft("find it");
        let submission = session.submit(context(), "English").unwrap();
        let runtime =
            AssistantRuntime::new(AssistantEndpoint::test(format!("http://{address}/v1"))).unwrap();
        let mut host = TestToolHost { calls: 0 };
        let answer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(runtime.complete_with_tool_host(&submission, &mut host, 2))
            .unwrap();

        assert_eq!(answer, "done");
        assert_eq!(host.calls, 1);
        server.join().unwrap();
    }

    fn read_http_json(stream: &mut TcpStream) -> Value {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "request ended before its JSON body");
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap();
            let body_start = header_end + 4;
            if request.len() >= body_start + content_length {
                return serde_json::from_slice(
                    &request[body_start..body_start.saturating_add(content_length)],
                )
                .unwrap();
            }
        }
    }

    fn write_http_json(stream: &mut TcpStream, body: &Value) {
        let body = serde_json::to_vec(body).unwrap();
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();
    }
}
