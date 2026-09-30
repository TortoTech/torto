//! Tolerate legacy OpenAI-compatible gateways without implementing an SSE
//! parser. Real event streams are passed untouched to Rig.
use bytes::Bytes;
use futures_util::StreamExt;
use rig_core::http_client::{
    self as http, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default)]
pub(super) struct CompatHttp(
    pub reqwest_rig::Client,
    pub Vec<Value>,
    pub Option<std::sync::Arc<std::sync::Mutex<Vec<crate::plugins::web_search::WebSource>>>>,
);

impl CompatHttp {
    fn prepare<T: Into<Bytes>>(&self, request: Request<T>) -> Request<Bytes> {
        let (parts, body) = request.into_parts();
        let mut bytes = body.into();
        if !self.1.is_empty()
            && let Ok(mut payload) = serde_json::from_slice::<Value>(&bytes)
        {
            if !payload["tools"].is_array() {
                payload["tools"] = json!([]);
            }
            payload["tools"]
                .as_array_mut()
                .unwrap()
                .extend(self.1.iter().cloned());
            bytes = Bytes::from(payload.to_string());
        }
        #[cfg(debug_assertions)]
        if let Ok(payload) = serde_json::from_slice::<Value>(&bytes) {
            // Capture the final wire declarations after Rig serialization and merging.
            // Never include headers, credentials, messages or function arguments.
            let declarations = payload
                .get("tools")
                .and_then(Value::as_array)
                .map(|tools| {
                    tools
                        .iter()
                        .map(|tool| {
                            if tool["type"] == "function" {
                                json!({"type":"function", "name":tool["function"]["name"]})
                            } else {
                                tool.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let declarations = serde_json::to_string(&declarations).unwrap_or_default();
            crate::diagnostics::log(
                "llm.wire.tools",
                &[
                    crate::diagnostics::Field::Detail("path", parts.uri.path()),
                    crate::diagnostics::Field::Detail(
                        "model",
                        payload["model"].as_str().unwrap_or_default(),
                    ),
                    crate::diagnostics::Field::Detail("tools", &declarations),
                ],
            );
        }
        let mut request = Request::from_parts(parts, bytes);
        request.headers_mut().remove("content-length");
        request
    }
}

fn normalize(mut payload: Value) -> Value {
    if !payload["choices"].is_array() {
        return payload;
    }
    for (key, value) in [
        ("id", json!("")),
        ("model", json!("")),
        ("object", json!("chat.completion")),
        ("created", json!(0)),
    ] {
        if payload.get(key).is_none() {
            payload[key] = value;
        }
    }
    for (index, choice) in payload["choices"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        if choice.get("index").is_none() {
            choice["index"] = json!(index);
        }
        if choice.get("finish_reason").is_none() {
            choice["finish_reason"] = json!(if choice["message"]["tool_calls"].is_array() {
                "tool_calls"
            } else {
                "stop"
            });
        }
        if choice["message"].is_object() && choice["message"].get("role").is_none() {
            choice["message"]["role"] = json!("assistant");
        }
        // Rig parses tool arguments while decoding the response. Repair our
        // output-only tool before that boundary; actual agent tools stay strict.
        if let Some(calls) = choice["message"]["tool_calls"].as_array_mut() {
            for call in calls {
                if call["function"]["name"] == super::OUTPUT_TOOL
                    && let Some(raw) = call["function"]["arguments"].as_str()
                    && serde_json::from_str::<Value>(raw).is_err()
                    && let Ok(value) = crate::plugins::llm_json::parse::<Value>(raw)
                {
                    call["function"]["arguments"] = Value::String(value.to_string());
                }
            }
        }
        // Some gateways emit both aliases. Serde treats them as the same field
        // and rejects the whole response, including otherwise valid tool calls.
        if let Some(message) = choice["message"].as_object_mut()
            && message.contains_key("reasoning_content")
            && let Some(alias) = message.remove("reasoning")
            && message["reasoning_content"]
                .as_str()
                .is_none_or(str::is_empty)
        {
            message.insert("reasoning_content".into(), alias);
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_reasoning_aliases_preserve_tools_and_signed_details() {
        let payload = json!({"choices":[{"message":{"role":"assistant","content":"Looking at pages",
            "reasoning":"think","reasoning_content":"think",
            "reasoning_details":[{"type":"reasoning.text","text":"think","signature":"signed","index":0}],
            "tool_calls":[{"id":"c1","index":0,"type":"function","function":{"name":"overview_pages","arguments":"{\"pages\":[1]}"}}]}}]});
        let normalized = normalize(payload);
        let parsed: rig_core::providers::openai::completion::CompletionResponse =
            serde_json::from_value(normalized.clone()).unwrap();
        assert_eq!(
            normalized["choices"][0]["message"]["reasoning_content"],
            "think"
        );
        assert_eq!(
            normalized["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "overview_pages"
        );
        assert_eq!(
            normalized["choices"][0]["message"]["reasoning_details"][0]["signature"],
            "signed"
        );
        drop(parsed);
    }
}

impl HttpClientExt for CompatHttp {
    fn send<T, U>(
        &self,
        request: Request<T>,
    ) -> impl std::future::Future<Output = http::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let request = self.prepare(request);
        let compatible = request.uri().path().ends_with("/chat/completions");
        let pending = self.0.send::<Bytes, Bytes>(request);
        let sources = self.2.clone();
        async move {
            let response = pending.await?;
            let success = response.status().is_success();
            let (mut parts, body) = response.into_parts();
            parts.headers.remove("content-length");
            let body: LazyBody<U> = Box::pin(async move {
                let bytes = body.await?;
                if compatible
                    && success
                    && let Ok(payload) = serde_json::from_slice::<Value>(&bytes)
                {
                    if let Some(sources) = sources {
                        crate::plugins::web_search::collect_sources(
                            &payload,
                            &mut sources
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner),
                        );
                    }
                    return Ok(U::from(Bytes::from(normalize(payload).to_string())));
                }
                Ok(U::from(bytes))
            });
            Ok(Response::from_parts(parts, body))
        }
    }

    fn send_multipart<U>(
        &self,
        request: Request<MultipartForm>,
    ) -> impl std::future::Future<Output = http::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        self.0.send_multipart(request)
    }

    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl std::future::Future<Output = http::Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let request = self.prepare(request);
        let compatible = request.uri().path().ends_with("/chat/completions");
        let sources = self.2.clone();
        async move {
            let response = self.0.send_streaming(request).await?;
            let is_json = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h.contains("application/json"));
            if !compatible || !is_json || !response.status().is_success() {
                if response.status().is_success()
                    && let Some(sources) = sources
                {
                    let (parts, stream) = response.into_parts();
                    let mut pending = Vec::new();
                    let stream: http::sse::BoxedStream = Box::pin(stream.map(move |chunk| {
                        if let Ok(bytes) = &chunk {
                            if pending.len() + bytes.len() <= 2 * 1024 * 1024 {
                                pending.extend_from_slice(bytes);
                            } else {
                                pending.clear();
                            }
                            while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                                let line: Vec<_> = pending.drain(..=end).collect();
                                if let Some(data) = line.strip_prefix(b"data:")
                                    && let Ok(value) = serde_json::from_slice::<Value>(data)
                                {
                                    crate::plugins::web_search::collect_sources(
                                        &value,
                                        &mut sources
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                                    );
                                }
                            }
                        }
                        chunk
                    }));
                    return Ok(Response::from_parts(parts, stream));
                }
                return Ok(response);
            }
            let (mut parts, mut stream) = response.into_parts();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                bytes.extend_from_slice(&chunk?);
            }
            let payload: Value =
                serde_json::from_slice(&bytes).map_err(|e| http::Error::Instance(Box::new(e)))?;
            if let Some(sources) = sources {
                crate::plugins::web_search::collect_sources(
                    &payload,
                    &mut sources
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            }
            let mut payload = normalize(payload);
            let choices = payload["choices"]
                .as_array_mut()
                .ok_or_else(|| http::Error::Instance("AI 鍝嶅簲缂哄皯 choices".into()))?;
            for choice in choices {
                let mut delta = choice
                    .as_object_mut()
                    .and_then(|c| c.remove("message"))
                    .unwrap_or_default();
                if let Some(calls) = delta["tool_calls"].as_array_mut() {
                    for (index, call) in calls.iter_mut().enumerate() {
                        call["index"] = json!(index);
                    }
                }
                choice["delta"] = delta;
            }
            parts.headers.insert(
                "content-type",
                http::HeaderValue::from_static("text/event-stream"),
            );
            parts.headers.remove("content-length");
            let event = Bytes::from(format!("data: {payload}\n\ndata: [DONE]\n\n"));
            let stream: http::sse::BoxedStream =
                Box::pin(futures_util::stream::once(async move { Ok(event) }));
            Ok(Response::from_parts(parts, stream))
        }
    }
}
