use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use crate::parse_llm_json;

/// One provider-neutral tool invocation decoded from an OpenAI-compatible
/// assistant message. The raw argument string is retained so the transcript can
/// round-trip the provider response while callers consume repaired JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantToolCall {
    id: String,
    name: String,
    raw_arguments: String,
    arguments: Result<Value, String>,
}

impl AssistantToolCall {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn raw_arguments(&self) -> &str {
        &self.raw_arguments
    }

    pub fn arguments(&self) -> Result<&Value, &str> {
        self.arguments.as_ref().map_err(String::as_str)
    }
}

/// JSON result produced by the application-owned implementation of one tool.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantToolResult {
    call_id: String,
    content: Value,
}

impl AssistantToolResult {
    #[must_use]
    pub fn new(call_id: impl Into<String>, content: Value) -> Self {
        Self {
            call_id: call_id.into(),
            content,
        }
    }
}

/// Decision emitted after accepting one assistant message.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolLoopStep {
    Complete(String),
    CallTools(Vec<AssistantToolCall>),
}

/// Future returned by an application-owned assistant tool host.
pub type AssistantToolFuture<'a> = Pin<Box<dyn Future<Output = Value> + Send + 'a>>;

/// Application capabilities available to the provider tool loop.
///
/// Implementations may read a publication and stage reversible mutations, but
/// they must not depend on an egui or GPUI presentation. The shared runtime
/// always invokes tools sequentially so a host can keep transaction order.
pub trait AssistantToolHost {
    /// OpenAI-compatible function definitions advertised for this request.
    fn definitions(&self) -> Value;

    /// Executes one already-decoded call. Invalid repaired arguments should be
    /// returned as a JSON error value rather than panicking.
    fn execute(&mut self, call: AssistantToolCall) -> AssistantToolFuture<'_>;

    /// Reverts staged mutations after provider or protocol failure.
    fn rollback(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Deterministic OpenAI-compatible tool-loop transcript.
///
/// HTTP, book access, rewrite transactions, and annotation persistence remain
/// outside this type. It only owns request ordering, repaired arguments, tool
/// result correlation, and the maximum number of model/tool rounds.
#[derive(Clone, Debug)]
pub struct OpenAiToolLoop {
    messages: Vec<Value>,
    pending_call_ids: Vec<String>,
    completed_tool_steps: usize,
    max_tool_steps: usize,
}

impl OpenAiToolLoop {
    #[must_use]
    pub fn new(messages: Vec<Value>, max_tool_steps: usize) -> Self {
        Self {
            messages,
            pending_call_ids: Vec::new(),
            completed_tool_steps: 0,
            max_tool_steps,
        }
    }

    pub fn request_messages(&self) -> Result<&[Value], String> {
        if !self.pending_call_ids.is_empty() {
            return Err("必须先提交当前工具调用结果".into());
        }
        if self.completed_tool_steps >= self.max_tool_steps {
            return Err("AI 工具调用次数过多，请缩小问题范围后重试".into());
        }
        Ok(&self.messages)
    }

    pub fn accept_assistant_message(&mut self, message: Value) -> Result<ToolLoopStep, String> {
        if !self.pending_call_ids.is_empty() {
            return Err("上一批工具调用尚未完成".into());
        }
        let calls = decode_tool_calls(&message)?;
        if calls.is_empty() {
            let content = message
                .get("content")
                .and_then(message_content)
                .filter(|content| !content.trim().is_empty())
                .ok_or_else(|| "AI 返回了空内容".to_owned())?;
            return Ok(ToolLoopStep::Complete(content));
        }
        self.completed_tool_steps = self.completed_tool_steps.saturating_add(1);
        self.pending_call_ids = calls.iter().map(|call| call.id.clone()).collect();
        self.messages.push(message);
        Ok(ToolLoopStep::CallTools(calls))
    }

    pub fn apply_tool_results(&mut self, results: Vec<AssistantToolResult>) -> Result<(), String> {
        if self.pending_call_ids.is_empty() {
            return Err("当前没有等待结果的工具调用".into());
        }
        let result_ids = results
            .iter()
            .map(|result| result.call_id.as_str())
            .collect::<Vec<_>>();
        let pending_ids = self
            .pending_call_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if result_ids != pending_ids {
            return Err("工具结果必须与调用按相同顺序一一对应".into());
        }
        for result in results {
            let content = serde_json::to_string(&result.content)
                .map_err(|error| format!("序列化工具结果失败：{error}"))?;
            self.messages.push(json!({
                "role": "tool",
                "tool_call_id": result.call_id,
                "content": content,
            }));
        }
        self.pending_call_ids.clear();
        Ok(())
    }
}

fn decode_tool_calls(message: &Value) -> Result<Vec<AssistantToolCall>, String> {
    let Some(calls) = message.get("tool_calls") else {
        return Ok(Vec::new());
    };
    if calls.is_null() {
        return Ok(Vec::new());
    }
    let calls = calls
        .as_array()
        .ok_or_else(|| "AI 工具调用必须是数组".to_owned())?;
    calls
        .iter()
        .map(|call| {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| "AI 工具调用缺少 id".to_owned())?
                .to_owned();
            let function = call
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| "AI 工具调用缺少 function".to_owned())?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| "AI 工具调用缺少函数名".to_owned())?
                .to_owned();
            let raw_arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_owned();
            let arguments = parse_llm_json::<Value>(&raw_arguments);
            Ok(AssistantToolCall {
                id,
                name,
                raw_arguments,
                arguments,
            })
        })
        .collect()
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
    use super::*;

    fn tool_message(arguments: &str) -> Value {
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "searchBook",
                    "arguments": arguments,
                }
            }]
        })
    }

    #[test]
    fn repairs_arguments_and_correlates_results_in_the_transcript() {
        let mut state = OpenAiToolLoop::new(vec![json!({ "role": "user", "content": "q" })], 2);
        let ToolLoopStep::CallTools(calls) = state
            .accept_assistant_message(tool_message("{'query':'term',}"))
            .unwrap()
        else {
            panic!("expected tool calls");
        };
        assert_eq!(calls[0].name(), "searchBook");
        assert_eq!(calls[0].arguments().unwrap()["query"], "term");

        state
            .apply_tool_results(vec![AssistantToolResult::new(
                calls[0].id(),
                json!({ "results": [] }),
            )])
            .unwrap();
        let messages = state.request_messages().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call-1");
    }

    #[test]
    fn requires_results_before_the_next_provider_request() {
        let mut state = OpenAiToolLoop::new(Vec::new(), 2);
        state.accept_assistant_message(tool_message("{}")).unwrap();
        assert_eq!(
            state.request_messages().unwrap_err(),
            "必须先提交当前工具调用结果"
        );
    }

    #[test]
    fn enforces_the_model_tool_round_limit_after_completed_results() {
        let mut state = OpenAiToolLoop::new(Vec::new(), 1);
        let ToolLoopStep::CallTools(calls) =
            state.accept_assistant_message(tool_message("{}")).unwrap()
        else {
            panic!("expected tool calls");
        };
        state
            .apply_tool_results(vec![AssistantToolResult::new(calls[0].id(), json!({}))])
            .unwrap();
        assert_eq!(
            state.request_messages().unwrap_err(),
            "AI 工具调用次数过多，请缩小问题范围后重试"
        );
    }

    #[test]
    fn accepts_a_final_text_response_without_mutating_the_transcript() {
        let mut state = OpenAiToolLoop::new(Vec::new(), 2);
        assert_eq!(
            state
                .accept_assistant_message(json!({
                    "role": "assistant",
                    "content": [{ "type": "text", "text": "answer" }]
                }))
                .unwrap(),
            ToolLoopStep::Complete("answer".into())
        );
    }

    #[test]
    fn null_tool_calls_are_a_normal_final_response() {
        let mut state = OpenAiToolLoop::new(Vec::new(), 2);
        assert_eq!(
            state
                .accept_assistant_message(json!({
                    "role": "assistant",
                    "content": "answer",
                    "tool_calls": null,
                }))
                .unwrap(),
            ToolLoopStep::Complete("answer".into())
        );
    }
}
