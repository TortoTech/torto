//! Adapt OpenAI-compatible gateways without implementing an SSE
//! parser. Real event streams are passed untouched to Rig.
use crate::plugins::web_search;
use bytes::Bytes;
use futures_util::StreamExt;
use rig_core::http_client::{
    self as http, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Preserve JSON replies from gateways that ignore the streaming flag, and
/// capture hosted-search sources. Rig handles message decoding and tool merging.
#[derive(Clone)]
pub(super) struct CompatHttp {
    pub inner: rig_reqwest::ReqwestClient,
    pub sources: Option<Arc<Mutex<Vec<web_search::WebSource>>>>,
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
        let compatible = request.uri().path().ends_with("/chat/completions");
        let pending = self.inner.send::<T, Bytes>(request);
        let sources = self.sources.clone();
        async move {
            let response = pending.await?;
            let success = response.status().is_success();
            let (mut parts, body) = response.into_parts();
            parts.headers.remove("content-length");
            let body: LazyBody<U> = Box::pin(async move {
                let bytes = body.await?;
                if compatible
                    && success
                    && let Some(sources) = sources
                    && let Ok(payload) = serde_json::from_slice::<Value>(&bytes)
                {
                    crate::plugins::web_search::collect_sources(
                        &payload,
                        &mut sources
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
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
        self.inner.send_multipart(request)
    }

    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl std::future::Future<Output = http::Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let compatible = request.uri().path().ends_with("/chat/completions");
        let sources = self.sources.clone();
        async move {
            let response = self.inner.send_streaming(request).await?;
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
                    let stream: http::BoxedStream = Box::pin(stream.map(move |chunk| {
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
            let mut payload = payload;
            payload["object"] = json!("chat.completion.chunk");
            let choices = payload["choices"]
                .as_array_mut()
                .ok_or_else(|| http::Error::Instance("AI 响应缺少 choices".into()))?;
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
            let stream: http::BoxedStream =
                Box::pin(futures_util::stream::once(async move { Ok(event) }));
            Ok(Response::from_parts(parts, stream))
        }
    }
}
