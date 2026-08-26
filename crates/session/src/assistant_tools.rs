use std::sync::Arc;

use rebook_assistant::{
    AssistantAnnotationAction, AssistantToolCall, AssistantToolFuture, AssistantToolHost,
    BookSearchToolHost, ChatSelection, PendingAnnotationActions, chat_citation_link,
    citations_for_model,
};
use rebook_publication::{BookSource, SourceRange};
use rebook_sync::StoredHighlight;
use serde_json::{Value, json};

/// Shared document capabilities for assistant surfaces that may propose
/// annotation mutations.
///
/// Search and citation lookup read the normalized publication immediately.
/// Annotation tools only mutate this host's working copy and append to a
/// [`PendingAnnotationActions`] batch. No persistence is touched until a
/// frontend explicitly confirms that batch through its durable mutation target.
pub struct DocumentAssistantToolHost {
    source: Arc<dyn BookSource>,
    search: BookSearchToolHost,
    book_id: String,
    selection: Option<ChatSelection>,
    baseline_annotations: Vec<StoredHighlight>,
    annotations: Vec<StoredHighlight>,
    pending: PendingAnnotationActions<StoredHighlight>,
}

impl DocumentAssistantToolHost {
    #[must_use]
    pub fn new(
        source: Arc<dyn BookSource>,
        current_section: usize,
        book_id: impl Into<String>,
        selection: Option<ChatSelection>,
        annotations: Vec<StoredHighlight>,
    ) -> Self {
        Self {
            search: BookSearchToolHost::new(Arc::clone(&source), current_section),
            source,
            book_id: book_id.into(),
            selection,
            baseline_annotations: annotations.clone(),
            annotations,
            pending: PendingAnnotationActions::new(),
        }
    }

    #[must_use]
    pub const fn pending_annotation_actions(&self) -> &PendingAnnotationActions<StoredHighlight> {
        &self.pending
    }

    #[must_use]
    pub fn into_pending_annotation_actions(self) -> PendingAnnotationActions<StoredHighlight> {
        self.pending
    }

    fn execute_document_tool(&mut self, name: &str, arguments: &Value) -> Value {
        match name {
            "getCurrentSelection" => self.selection.as_ref().map_or_else(
                || json!({ "error": "当前没有可用的阅读器选区。请让用户先选择原文。" }),
                |selection| {
                    json!({
                        "text": selection.text,
                        "hrefs": selection
                            .ranges
                            .iter()
                            .filter_map(|range| source_range_link(self.source.as_ref(), range))
                            .collect::<Vec<_>>(),
                    })
                },
            ),
            "listAnnotations" => {
                let limit = read_usize(arguments, "limit", 50).clamp(1, 100);
                json!({
                    "items": self
                        .annotations
                        .iter()
                        .take(limit)
                        .map(|annotation| compact_annotation(self.source.as_ref(), annotation))
                        .collect::<Vec<_>>(),
                })
            }
            "searchAnnotations" => {
                let query = arguments
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                if query.is_empty() {
                    return json!({ "error": "query 不能为空" });
                }
                let limit = read_usize(arguments, "limit", 20).clamp(1, 100);
                json!({
                    "items": self
                        .annotations
                        .iter()
                        .filter(|annotation| {
                            annotation.quote.to_lowercase().contains(&query)
                                || annotation.note.as_deref().is_some_and(|note| {
                                    note.to_lowercase().contains(&query)
                                })
                        })
                        .take(limit)
                        .map(|annotation| compact_annotation(self.source.as_ref(), annotation))
                        .collect::<Vec<_>>(),
                })
            }
            "createAnnotation" => self.create_annotation(arguments),
            "updateAnnotation" => self.update_annotation(arguments),
            "deleteAnnotation" => self.delete_annotation(arguments),
            _ => json!({ "error": format!("未知书籍工具：{name}") }),
        }
    }

    fn create_annotation(&mut self, arguments: &Value) -> Value {
        let Some(selection) = self.selection.as_ref() else {
            return json!({ "error": "当前没有选区。请让用户先选择原文。" });
        };
        let annotation = StoredHighlight::with_note(
            self.book_id.clone(),
            selection.ranges.clone(),
            selection.text.clone(),
            normalized_optional_text(arguments.get("note").and_then(Value::as_str)),
        );
        self.annotations.insert(0, annotation.clone());
        self.pending
            .push(AssistantAnnotationAction::Create(annotation.clone()));
        json!({
            "status": "pending_confirmation",
            "annotation": compact_annotation(self.source.as_ref(), &annotation),
        })
    }

    fn update_annotation(&mut self, arguments: &Value) -> Value {
        let annotation_id = arguments
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let Some(annotation) = self
            .annotations
            .iter_mut()
            .find(|annotation| annotation.id == annotation_id)
        else {
            return json!({ "error": "批注不存在。" });
        };
        annotation.note = normalized_optional_text(arguments.get("note").and_then(Value::as_str));
        let annotation = annotation.clone();
        self.pending
            .push(AssistantAnnotationAction::Update(annotation.clone()));
        json!({
            "status": "pending_confirmation",
            "annotation": compact_annotation(self.source.as_ref(), &annotation),
        })
    }

    fn delete_annotation(&mut self, arguments: &Value) -> Value {
        let annotation_id = arguments
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let Some(index) = self
            .annotations
            .iter()
            .position(|annotation| annotation.id == annotation_id)
        else {
            return json!({ "error": "批注不存在。" });
        };
        self.annotations.remove(index);
        self.pending.push(AssistantAnnotationAction::Delete {
            annotation_id: annotation_id.to_owned(),
        });
        json!({ "status": "pending_confirmation" })
    }
}

impl AssistantToolHost for DocumentAssistantToolHost {
    fn definitions(&self) -> Value {
        let mut definitions = self
            .search
            .definitions()
            .as_array()
            .cloned()
            .unwrap_or_default();
        definitions.extend(annotation_tool_definitions());
        Value::Array(definitions)
    }

    fn execute(&mut self, call: AssistantToolCall) -> AssistantToolFuture<'_> {
        if call.name() == "searchBook" {
            return self.search.execute(call);
        }
        let result = match call.arguments() {
            Ok(arguments) if arguments.is_object() => {
                citations_for_model(self.execute_document_tool(call.name(), arguments))
            }
            Ok(_) => json!({ "error": "工具参数必须是 JSON 对象。" }),
            Err(error) => json!({ "error": format!("工具参数 JSON 无效：{error}") }),
        };
        Box::pin(async move { result })
    }

    fn rollback(&mut self) -> Result<(), String> {
        self.annotations.clone_from(&self.baseline_annotations);
        self.pending.cancel();
        Ok(())
    }
}

fn annotation_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "getCurrentSelection",
                "description": "获取当前选区文字及 citation。创建批注前先调用。",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "listAnnotations",
                "description": "列出当前书籍的用户高亮和批注，包括本轮尚待用户确认的变更。",
                "parameters": {
                    "type": "object",
                    "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 50 } },
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "searchAnnotations",
                "description": "在当前书籍的高亮原文和批注内容中搜索。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "createAnnotation",
                "description": "基于当前选区创建高亮或批注。动作只会暂存，必须由用户明确确认后才写入。",
                "parameters": {
                    "type": "object",
                    "properties": { "note": { "type": "string" } },
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "updateAnnotation",
                "description": "修改已有批注文字。动作只会暂存，必须由用户明确确认后才写入。",
                "parameters": {
                    "type": "object",
                    "properties": { "id": { "type": "string" }, "note": { "type": "string" } },
                    "required": ["id"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "deleteAnnotation",
                "description": "删除已有高亮或批注。动作只会暂存，必须由用户明确确认后才写入。",
                "parameters": {
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"],
                    "additionalProperties": false
                }
            }
        }),
    ]
}

fn normalized_optional_text(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        (!value.is_empty()).then_some(value)
    })
}

fn read_usize(arguments: &Value, key: &str, default: usize) -> usize {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn source_range_link(source: &dyn BookSource, range: &SourceRange) -> Option<String> {
    let section_index = source
        .book()
        .sections
        .iter()
        .position(|section| section.id == range.start.spine)?;
    Some(chat_citation_link(section_index, Some(&range.start.node)))
}

fn compact_annotation(source: &dyn BookSource, annotation: &StoredHighlight) -> Value {
    json!({
        "id": annotation.id,
        "quote": annotation.quote,
        "note": annotation.note,
        "href": annotation
            .ranges
            .first()
            .and_then(|range| source_range_link(source, range)),
        "createdAt": annotation.created_at,
    })
}

#[cfg(test)]
mod tests {
    use rebook_assistant::{OpenAiToolLoop, ToolLoopStep};
    use rebook_publication::{
        Block, BlockStyle, Book, Inline, Metadata, PublicationError, PublicationId, PublicationUrl,
        Resource, Section, SourceAnchor, SpineItem, SpineItemId, TextBlock, TextBlockKind, TextRun,
        TextStyle,
    };

    use super::*;

    struct TestSource {
        book: Book,
        section: Section,
    }

    impl BookSource for TestSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            Ok(self.section.clone())
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    fn source() -> Arc<dyn BookSource> {
        let spine = SpineItemId::new("chapter").unwrap();
        let href = PublicationUrl::parse("chapter.xhtml").unwrap();
        let range = range();
        Arc::new(TestSource {
            book: Book {
                id: PublicationId::new("assistant-host").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: vec![SpineItem {
                    id: spine.clone(),
                    href: href.clone(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            section: Section {
                id: spine,
                href,
                blocks: vec![Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: "A normalized searchable paragraph.".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(range),
                })],
                anchors: Vec::new(),
            },
        })
    }

    fn range() -> SourceRange {
        let spine = SpineItemId::new("chapter").unwrap();
        SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 2,
            },
            end: SourceAnchor {
                spine,
                node: "paragraph-1".into(),
                text_offset: 15,
            },
        }
    }

    fn selection() -> ChatSelection {
        ChatSelection {
            text: "selected source".into(),
            ranges: vec![range()],
        }
    }

    fn call(name: &str, arguments: &str) -> AssistantToolCall {
        let mut state = OpenAiToolLoop::new(Vec::new(), 1);
        let ToolLoopStep::CallTools(mut calls) = state
            .accept_assistant_message(json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]
            }))
            .unwrap()
        else {
            panic!("expected one tool call");
        };
        calls.remove(0)
    }

    fn execute(host: &mut DocumentAssistantToolHost, call: AssistantToolCall) -> Value {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(host.execute(call))
    }

    #[test]
    fn advertises_search_and_explicit_confirmation_mutations() {
        let host = DocumentAssistantToolHost::new(source(), 0, "book", None, Vec::new());
        let definitions = host.definitions();
        let names = definitions
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|definition| definition.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "searchBook",
                "getCurrentSelection",
                "listAnnotations",
                "searchAnnotations",
                "createAnnotation",
                "updateAnnotation",
                "deleteAnnotation",
            ]
        );
    }

    #[test]
    fn create_is_staged_with_exact_selection_and_source_citation() {
        let expected_range = range();
        let mut host =
            DocumentAssistantToolHost::new(source(), 0, "book", Some(selection()), Vec::new());

        let result = execute(
            &mut host,
            call("createAnnotation", r#"{"note":" remember "}"#),
        );

        assert_eq!(result["status"], "pending_confirmation");
        assert_eq!(result["annotation"]["citation"], "【0/paragraph-1†source】");
        let [AssistantAnnotationAction::Create(annotation)] =
            host.pending_annotation_actions().actions()
        else {
            panic!("expected one staged create");
        };
        assert_eq!(annotation.ranges, vec![expected_range]);
        assert_eq!(annotation.note.as_deref(), Some("remember"));
    }

    #[test]
    fn sequential_update_and_delete_use_the_working_annotation_view() {
        let annotation = StoredHighlight::with_note(
            "book".into(),
            vec![range()],
            "quote".into(),
            Some("old".into()),
        );
        let id = annotation.id.clone();
        let mut host = DocumentAssistantToolHost::new(source(), 0, "book", None, vec![annotation]);

        let update = execute(
            &mut host,
            call(
                "updateAnnotation",
                &json!({ "id": id, "note": "new" }).to_string(),
            ),
        );
        assert_eq!(update["annotation"]["note"], "new");
        let delete = execute(
            &mut host,
            call("deleteAnnotation", &json!({ "id": id }).to_string()),
        );
        assert_eq!(delete["status"], "pending_confirmation");
        assert_eq!(host.pending_annotation_actions().len(), 2);
    }

    #[test]
    fn provider_failure_rolls_back_only_the_in_memory_proposal() {
        let mut host =
            DocumentAssistantToolHost::new(source(), 0, "book", Some(selection()), Vec::new());
        let _ = execute(&mut host, call("createAnnotation", "{}"));
        assert_eq!(host.pending_annotation_actions().len(), 1);

        host.rollback().unwrap();

        assert!(host.pending_annotation_actions().is_empty());
        let listed = execute(&mut host, call("listAnnotations", "{}"));
        assert!(listed["items"].as_array().unwrap().is_empty());
    }
}
