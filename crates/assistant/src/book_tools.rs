use std::sync::Arc;

use rebook_publication::BookSource;
use serde_json::{Value, json};

use crate::{
    AssistantToolCall, AssistantToolFuture, AssistantToolHost, citations_for_model, search_book,
    search_section,
};

const DEFAULT_SEARCH_RESULTS: usize = 20;
const MAX_SEARCH_RESULTS: usize = 20;

/// Read-only, frontend-neutral book capabilities available to lightweight
/// assistant surfaces such as the GPUI focus chat.
///
/// The host reads normalized Reading IR directly and returns source-backed
/// citations. Mutation tools deliberately remain outside this type until their
/// confirmation and transaction contracts are shared as well.
pub struct BookSearchToolHost {
    source: Arc<dyn BookSource>,
    current_section: usize,
}

impl BookSearchToolHost {
    #[must_use]
    pub const fn new(source: Arc<dyn BookSource>, current_section: usize) -> Self {
        Self {
            source,
            current_section,
        }
    }

    fn execute_search(&self, arguments: &Value) -> Value {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if query.is_empty() {
            return json!({ "error": "query 不能为空" });
        }
        let max_results = arguments
            .get("maxResults")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(DEFAULT_SEARCH_RESULTS)
            .clamp(1, MAX_SEARCH_RESULTS);
        let scope = arguments
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("book");
        let results = match scope {
            "book" => search_book(self.source.as_ref(), query, max_results),
            "unit" => {
                let section_index = arguments
                    .get("unit")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(self.current_section);
                search_section(self.source.as_ref(), query, section_index, max_results)
            }
            _ => return json!({ "error": "scope 只能是 book 或 unit" }),
        };
        match results {
            Ok(results) => citations_for_model(json!({
                "results": results.into_iter().map(|result| json!({
                    "unit": result.section_index,
                    "title": result.section_title,
                    "id": result.range.start.node,
                    "type": result.block_kind,
                    "text": result.excerpt,
                    "match": result.matched_text,
                    "href": crate::chat_citation_link(
                        result.section_index,
                        Some(&result.range.start.node),
                    ),
                })).collect::<Vec<_>>()
            })),
            Err(error) => json!({ "error": error }),
        }
    }
}

impl AssistantToolHost for BookSearchToolHost {
    fn definitions(&self) -> Value {
        json!([{
            "type": "function",
            "function": {
                "name": "searchBook",
                "description": "搜索当前书籍的规范化正文，返回匹配文字及可验证 citation。需要书中未附在当前段落里的事实时使用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "scope": {
                            "type": "string",
                            "enum": ["book", "unit"],
                            "default": "book"
                        },
                        "unit": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "scope=unit 时的内容单元；不填使用当前单元。"
                        },
                        "maxResults": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_SEARCH_RESULTS,
                            "default": DEFAULT_SEARCH_RESULTS
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        }])
    }

    fn execute(&mut self, call: AssistantToolCall) -> AssistantToolFuture<'_> {
        let result = match call.arguments() {
            Ok(arguments) if call.name() == "searchBook" => self.execute_search(arguments),
            Ok(_) => json!({ "error": format!("未知书籍工具：{}", call.name()) }),
            Err(error) => json!({ "error": format!("工具参数不是有效 JSON：{error}") }),
        };
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use rebook_publication::{
        Block, BlockStyle, Book, Inline, Metadata, PublicationError, PublicationId, PublicationUrl,
        Resource, Section, SourceAnchor, SourceRange, SpineItem, SpineItemId, TextBlock,
        TextBlockKind, TextRun, TextStyle,
    };

    use crate::{OpenAiToolLoop, ToolLoopStep};

    use super::*;

    struct ToolSource {
        book: Book,
        section: Section,
    }

    impl BookSource for ToolSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            (index == 0)
                .then(|| self.section.clone())
                .ok_or_else(|| PublicationError::ResourceNotFound(index.to_string()))
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    fn source() -> Arc<dyn BookSource> {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let href = PublicationUrl::parse("chapter-1.xhtml").unwrap();
        let text = "A distributed system has many cooperating parts.";
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph one".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph one".into(),
                text_offset: u64::try_from(text.chars().count()).unwrap(),
            },
        };
        Arc::new(ToolSource {
            book: Book {
                id: PublicationId::new("tool-book").unwrap(),
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
                        text: text.into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(source),
                })],
                anchors: Vec::new(),
            },
        })
    }

    fn call(arguments: &str) -> AssistantToolCall {
        let mut state = OpenAiToolLoop::new(Vec::new(), 1);
        let ToolLoopStep::CallTools(mut calls) = state
            .accept_assistant_message(json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "searchBook", "arguments": arguments }
                }]
            }))
            .unwrap()
        else {
            panic!("expected one search call");
        };
        calls.remove(0)
    }

    fn execute(host: &mut BookSearchToolHost, call: AssistantToolCall) -> Value {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(host.execute(call))
    }

    #[test]
    fn searches_normalized_reading_ir_and_returns_source_citations() {
        let mut host = BookSearchToolHost::new(source(), 0);
        let result = execute(&mut host, call("{'query':'SYSTEM',}"));
        assert_eq!(result["results"][0]["match"], "system");
        assert_eq!(
            result["results"][0]["citation"],
            "【0/paragraph%20one†source】"
        );
        assert!(result["results"][0].get("href").is_none());
    }

    #[test]
    fn unit_scope_uses_the_current_section_by_default() {
        let mut host = BookSearchToolHost::new(source(), 0);
        let result = execute(&mut host, call(r#"{"query":"parts","scope":"unit"}"#));
        assert_eq!(result["results"].as_array().unwrap().len(), 1);
    }
}
