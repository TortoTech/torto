//! Asynchronous HTTP bytes loader for egui images using Torto's existing
//! `reqwest`/Tokio network stack.

use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};
use egui::mutex::Mutex;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode};
use tokio::runtime::Handle;

#[derive(Clone)]
struct RemoteFile {
    bytes: Arc<[u8]>,
    mime: Option<String>,
}

type Entry = Poll<Result<RemoteFile, String>>;

struct ReqwestBytesLoader {
    cache: Arc<Mutex<HashMap<String, Entry>>>,
    client: Result<Client, String>,
    runtime: Handle,
}

impl ReqwestBytesLoader {
    const ID: &'static str = egui::generate_loader_id!(ReqwestBytesLoader);

    fn new(runtime: Handle) -> Self {
        Self {
            cache: Arc::default(),
            client: crate::http::builder()
                .timeout(Duration::from_secs(90))
                .build()
                .map_err(|error| error.to_string()),
            runtime,
        }
    }
}

pub(super) fn install(ctx: &egui::Context, runtime: &Handle) {
    if !ctx.is_loader_installed(ReqwestBytesLoader::ID) {
        ctx.add_bytes_loader(Arc::new(ReqwestBytesLoader::new(runtime.clone())));
    }
}

impl BytesLoader for ReqwestBytesLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        if !uri.starts_with("http://") && !uri.starts_with("https://") {
            return Err(LoadError::NotSupported);
        }

        let mut cache = self.cache.lock();
        if let Some(entry) = cache.get(uri).cloned() {
            return match entry {
                Poll::Ready(Ok(file)) => Ok(BytesPoll::Ready {
                    size: None,
                    bytes: Bytes::Shared(file.bytes),
                    mime: file.mime,
                }),
                Poll::Ready(Err(error)) => Err(LoadError::Loading(error)),
                Poll::Pending => Ok(BytesPoll::Pending { size: None }),
            };
        }

        let client = self
            .client
            .as_ref()
            .map_err(|error| LoadError::Loading(format!("failed to create HTTP client: {error}")))?
            .clone();
        let uri = uri.to_owned();
        cache.insert(uri.clone(), Poll::Pending);
        drop(cache);

        let ctx = ctx.clone();
        let cache = Arc::clone(&self.cache);
        self.runtime.spawn(async move {
            let result = fetch(&client, &uri).await;
            let repaint = {
                let mut cache = cache.lock();
                if let std::collections::hash_map::Entry::Occupied(mut entry) =
                    cache.entry(uri.clone())
                {
                    *entry.get_mut() = Poll::Ready(result);
                    true
                } else {
                    false
                }
            };
            if repaint {
                ctx.request_repaint();
            }
        });

        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.cache.lock().remove(uri);
    }

    fn forget_all(&self) {
        self.cache.lock().clear();
    }

    fn byte_size(&self) -> usize {
        self.cache
            .lock()
            .values()
            .map(|entry| match entry {
                Poll::Ready(Ok(file)) => {
                    file.bytes.len() + file.mime.as_ref().map_or(0, String::len)
                }
                Poll::Ready(Err(error)) => error.len(),
                Poll::Pending => 0,
            })
            .sum()
    }

    fn has_pending(&self) -> bool {
        self.cache.lock().values().any(Poll::is_pending)
    }
}

async fn fetch(client: &Client, uri: &str) -> Result<RemoteFile, String> {
    let response = client
        .get(uri)
        .send()
        .await
        .map_err(|error| format!("failed to load {uri:?}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(uri, status));
    }
    let mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read {uri:?}: {error}"))?;

    Ok(RemoteFile {
        bytes: Arc::from(bytes.as_ref()),
        mime,
    })
}

fn http_status_error(uri: &str, status: StatusCode) -> String {
    format!("failed to load {uri:?}: HTTP {status}")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    use super::*;

    #[test]
    fn ignores_non_http_uris() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let loader = ReqwestBytesLoader::new(runtime.handle().clone());

        assert!(matches!(
            loader.load(&egui::Context::default(), "bytes://cover.png"),
            Err(LoadError::NotSupported)
        ));
    }

    #[test]
    fn loads_and_caches_http_bytes_with_the_response_mime_type() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = b"image-bytes";
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: image/png; charset=binary\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let loader = ReqwestBytesLoader::new(runtime.handle().clone());
        let ctx = egui::Context::default();
        let uri = format!("http://{address}/cover.png");
        assert!(matches!(
            loader.load(&ctx, &uri).unwrap(),
            BytesPoll::Pending { .. }
        ));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match loader.load(&ctx, &uri).unwrap() {
                BytesPoll::Ready {
                    bytes: Bytes::Shared(bytes),
                    mime,
                    ..
                } => {
                    assert_eq!(&*bytes, body);
                    assert_eq!(mime.as_deref(), Some("image/png"));
                    break;
                }
                BytesPoll::Ready { .. } => panic!("HTTP bytes must use shared storage"),
                BytesPoll::Pending { .. } if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                BytesPoll::Pending { .. } => panic!("HTTP image load timed out"),
            }
        }

        server.join().unwrap();
        assert!(!loader.has_pending());
        assert!(loader.byte_size() >= body.len() + "image/png".len());
    }
}
