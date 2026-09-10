use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderName, HeaderValue, IF_MATCH,
    IF_NONE_MATCH, LOCATION, RANGE,
};
use reqwest::{Client, Method, StatusCode, Url};

use super::SyncResult;
use super::settings::SyncSettings;
use super::store::SyncStore;

const DEPTH: HeaderName = HeaderName::from_static("depth");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_mins(30);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_mins(2);
const PROGRESS_REFRESH: Duration = Duration::from_millis(100);
const UPLOAD_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct RemoteObject {
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}

#[derive(Clone)]
pub(crate) struct WebDavClient {
    client: Client,
    download_client: Client,
    root: Url,
    username: String,
    password: String,
    cstcloud_compatibility: bool,
    cache: Option<(SyncStore, String)>,
}

impl WebDavClient {
    pub(crate) fn new(settings: &SyncSettings, password: String) -> SyncResult<Self> {
        settings.validate()?;
        if password.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "请输入 WebDAV 密码").into());
        }
        let mut root = Url::parse(&settings.base_url)?;
        root.path_segments_mut()
            .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "WebDAV 地址不能作为目录"))?
            .pop_if_empty()
            .extend(["Rebook", "v1"]);
        if !root.path().ends_with('/') {
            root.set_path(&format!("{}/", root.path()));
        }
        let allowed_origin = root.origin();
        let download_allowed_origin = allowed_origin.clone();
        let mut client_builder = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT);
        let mut download_client_builder = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(DOWNLOAD_READ_TIMEOUT);
        let compatibility_user_agent = settings.user_agent();
        if let Some(user_agent) = compatibility_user_agent {
            client_builder = client_builder.user_agent(user_agent);
            download_client_builder = download_client_builder.user_agent(user_agent);
        }
        let client = client_builder
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.url().origin() == allowed_origin {
                    attempt.follow()
                } else {
                    attempt.error("WebDAV 重定向到了不同来源，已拒绝发送凭据")
                }
            }))
            .build()?;
        let download_client = download_client_builder
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.url().origin() == download_allowed_origin {
                    attempt.follow()
                } else {
                    attempt.error("WebDAV download redirected to a different origin")
                }
            }))
            .build()?;
        Ok(Self {
            client,
            download_client,
            root,
            username: settings.username.clone(),
            password,
            cstcloud_compatibility: compatibility_user_agent.is_some(),
            cache: None,
        })
    }

    pub(crate) fn with_store(mut self, store: SyncStore, account: String) -> Self {
        self.cache = Some((store, account));
        self
    }

    pub(crate) fn cache_get(&self, key: &str) -> SyncResult<Option<Vec<u8>>> {
        self.cache
            .as_ref()
            .map_or(Ok(None), |(store, account)| store.cache_get(account, key))
    }

    pub(crate) fn cache_set(&self, key: &str, value: &[u8]) -> SyncResult<()> {
        self.cache.as_ref().map_or(Ok(()), |(store, account)| {
            store.cache_set(account, key, value)
        })
    }

    pub(crate) fn invalidate_object(&self, path: &str) -> SyncResult<()> {
        self.cache.as_ref().map_or(Ok(()), |(store, account)| {
            store.cache_remove(account, &format!("object:{path}"))
        })
    }

    fn cached_object(&self, path: &str) -> SyncResult<Option<RemoteObject>> {
        let Some(bytes) = self.cache_get(&format!("object:{path}"))? else {
            return Ok(None);
        };
        let Some(split) = bytes.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let etag = std::str::from_utf8(&bytes[..split])?.to_owned();
        Ok(Some(RemoteObject {
            bytes: bytes[split + 1..].to_vec(),
            etag: (!etag.is_empty()).then_some(etag),
        }))
    }

    fn remember_object(&self, path: &str, object: &RemoteObject) -> SyncResult<()> {
        if self.cache.is_none() {
            return Ok(());
        }
        let mut bytes = object
            .etag
            .as_deref()
            .unwrap_or_default()
            .as_bytes()
            .to_vec();
        bytes.push(b'\n');
        bytes.extend_from_slice(&object.bytes);
        self.cache_set(&format!("object:{path}"), &bytes)
    }

    async fn repair_parent(&self, path: &str) -> SyncResult<()> {
        self.create_collection_absolute(self.root.join("../")?)
            .await?;
        self.create_collection_absolute(self.root.clone()).await?;
        let mut current = self.root.clone();
        if let Some((parent, _)) = path.rsplit_once('/') {
            for segment in parent.split('/').filter(|segment| !segment.is_empty()) {
                current = current.join(&format!("{segment}/"))?;
                self.create_collection_absolute(current.clone()).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn ensure_base_layout(&self) -> SyncResult<()> {
        self.ensure_collection_absolute(self.root.join("../")?)
            .await?;
        self.ensure_collection_absolute(self.root.clone()).await?;
        for path in [
            "library/",
            "library/devices/",
            "books/",
            "state/",
            "derived/",
            "tmp/",
        ] {
            self.ensure_collection(path).await?;
        }
        Ok(())
    }

    pub(crate) async fn ensure_collection(&self, path: &str) -> SyncResult<()> {
        let mut current = self.root.clone();
        for segment in path.trim_matches('/').split('/') {
            if segment.is_empty() {
                continue;
            }
            current = current.join(&format!("{segment}/"))?;
            self.ensure_collection_absolute(current.clone()).await?;
        }
        Ok(())
    }

    pub(crate) async fn get_optional(&self, path: &str) -> SyncResult<Option<RemoteObject>> {
        let cached = self.cached_object(path)?;
        let mut request = self.request(Method::GET, self.url(path)?);
        if let Some(etag) = cached.as_ref().and_then(|object| object.etag.as_ref()) {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return cached.map(Some).ok_or_else(|| {
                io::Error::other("WebDAV returned 304 without cached content").into()
            });
        }
        if response.status() == StatusCode::NOT_FOUND {
            self.invalidate_object(path)?;
            return Ok(None);
        }
        let response = response.error_for_status()?;
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let object = RemoteObject {
            bytes: response.bytes().await?.to_vec(),
            etag,
        };
        self.remember_object(path, &object)?;
        Ok(Some(object))
    }

    pub(crate) async fn download_to_file<F>(
        &self,
        path: &str,
        destination: &Path,
        expected_length: u64,
        mut progress: F,
    ) -> SyncResult<bool>
    where
        F: FnMut(u64),
    {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut downloaded = fs::metadata(destination)
            .map(|metadata| metadata.len())
            .unwrap_or_default();
        if downloaded > expected_length {
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(destination)?;
            downloaded = 0;
        }
        if downloaded == expected_length {
            progress(downloaded);
            return Ok(true);
        }

        let mut request = self
            .download_client
            .request(Method::GET, self.url(path)?)
            .basic_auth(&self.username, Some(&self.password));
        if downloaded > 0 {
            request = request.header(RANGE, format!("bytes={downloaded}-"));
        }
        let response = request.send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }

        let append = downloaded > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
        if append {
            let range_start = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(content_range_start);
            if range_start != Some(downloaded) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WebDAV server returned an invalid Content-Range",
                )
                .into());
            }
        } else if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
            if downloaded == expected_length {
                progress(downloaded);
                return Ok(true);
            }
            fs::remove_file(destination).ok();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WebDAV server rejected the saved download range",
            )
            .into());
        } else {
            downloaded = 0;
            progress(0);
        }

        let mut response = response.error_for_status()?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(destination)?;
        let mut last_update = Instant::now();
        while let Some(chunk) = response.chunk().await? {
            let chunk_length = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            let next = downloaded.saturating_add(chunk_length);
            if next > expected_length {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "WebDAV download is larger than its manifest",
                )
                .into());
            }
            file.write_all(&chunk)?;
            downloaded = next;
            if last_update.elapsed() >= PROGRESS_REFRESH || downloaded == expected_length {
                progress(downloaded);
                last_update = Instant::now();
            }
        }
        file.flush()?;
        if downloaded != expected_length {
            progress(downloaded);
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("WebDAV download ended at {downloaded} of {expected_length} bytes"),
            )
            .into());
        }
        progress(downloaded);
        Ok(true)
    }

    pub(crate) async fn put_immutable(
        &self,
        path: &str,
        bytes: Vec<u8>,
        content_type: &'static str,
    ) -> SyncResult<bool> {
        self.put_immutable_with_progress(path, bytes, content_type, |_| {})
            .await
    }

    pub(crate) async fn put_immutable_with_progress(
        &self,
        path: &str,
        bytes: Vec<u8>,
        content_type: &'static str,
        progress: impl FnMut(u64),
    ) -> SyncResult<bool> {
        if self.cstcloud_compatibility && self.get_optional(path).await?.is_some() {
            return Ok(false);
        }
        self.put_with_progress(path, bytes, content_type, true, progress)
            .await
    }

    pub(crate) async fn put_mutable_bytes_with_progress(
        &self,
        path: &str,
        bytes: Vec<u8>,
        content_type: &'static str,
        progress: impl FnMut(u64),
    ) -> SyncResult<()> {
        self.put_with_progress(path, bytes, content_type, false, progress)
            .await?;
        Ok(())
    }

    async fn put_with_progress(
        &self,
        path: &str,
        bytes: Vec<u8>,
        content_type: &'static str,
        immutable: bool,
        mut progress: impl FnMut(u64),
    ) -> SyncResult<bool> {
        let bytes = bytes::Bytes::from(bytes);
        let length = bytes.len() as u64;
        let mut url = self.url(path)?;
        let mut last_reported = 0;
        progress(0);
        // Streaming bodies cannot be replayed by reqwest's redirect policy.
        // Replay our shared buffer explicitly, retaining same-origin authentication.
        for _ in 0..10 {
            let consumed = Arc::new(AtomicU64::new(0));
            let counter = Arc::clone(&consumed);
            let body = futures_util::stream::unfold(bytes.clone(), move |mut remaining| {
                let counter = Arc::clone(&counter);
                async move {
                    if remaining.is_empty() {
                        return None;
                    }
                    let chunk = remaining.split_to(remaining.len().min(UPLOAD_CHUNK_SIZE));
                    counter.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    Some((Ok::<_, io::Error>(chunk), remaining))
                }
            });
            let mut request = self
                .request(Method::PUT, url.clone())
                .header(CONTENT_TYPE, content_type)
                .header(CONTENT_LENGTH, length)
                .body(reqwest::Body::wrap_stream(body));
            if immutable && !self.cstcloud_compatibility {
                request = request.header(IF_NONE_MATCH, "*");
            }
            let mut pending = Box::pin(request.send());
            let response = loop {
                match futures_util::future::select(
                    pending,
                    Box::pin(tokio::time::sleep(PROGRESS_REFRESH)),
                )
                .await
                {
                    futures_util::future::Either::Left((result, _)) => break result?,
                    futures_util::future::Either::Right((_, request)) => {
                        pending = request;
                        // Body consumption includes transport buffering. Reserve completion
                        // until the server acknowledges the request successfully.
                        let current = consumed
                            .load(Ordering::Relaxed)
                            .min(length.saturating_sub(1));
                        if current > last_reported {
                            progress(current);
                            last_reported = current;
                        }
                    }
                }
            };
            if matches!(
                response.status(),
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
            ) {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or_else(|| io::Error::other("WebDAV upload redirect has no location"))?
                    .to_str()?;
                let next = response.url().join(location)?;
                if next.origin() != self.root.origin() {
                    return Err(
                        io::Error::other("WebDAV upload redirected to a different origin").into(),
                    );
                }
                url = next;
                continue;
            }
            if immutable && response.status() == StatusCode::PRECONDITION_FAILED {
                return Ok(false);
            }
            if matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::CONFLICT
            ) {
                self.repair_parent(path).await?;
                continue;
            }
            if !response.status().is_success() {
                response.error_for_status()?;
                return Err(io::Error::other("Unexpected WebDAV upload response").into());
            }
            progress(length);
            if content_type == "application/json" {
                let etag = response
                    .headers()
                    .get(ETAG)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                self.remember_object(
                    path,
                    &RemoteObject {
                        bytes: bytes.to_vec(),
                        etag,
                    },
                )?;
            }
            return Ok(true);
        }
        Err(io::Error::other("Too many WebDAV upload redirects").into())
    }

    pub(crate) async fn put_mutable_json<T: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        value: &T,
    ) -> SyncResult<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        let mut existing = match self.cached_object(path)? {
            Some(object) => Some(object),
            None => self.get_optional(path).await?,
        };
        for _ in 0..3 {
            if existing
                .as_ref()
                .is_some_and(|object| object.bytes == bytes)
            {
                return Ok(());
            }
            let mut request = self
                .request(Method::PUT, self.url(path)?)
                .header(CONTENT_TYPE, "application/json")
                .body(bytes.clone());
            if !self.cstcloud_compatibility {
                request = if let Some(etag) = existing
                    .as_ref()
                    .and_then(|object| object.etag.as_ref())
                    .filter(|etag| !etag.is_empty())
                {
                    request.header(IF_MATCH, etag)
                } else if existing.is_none() {
                    request.header(IF_NONE_MATCH, "*")
                } else {
                    request
                };
            }
            let response = request.send().await?;
            if matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::CONFLICT
            ) {
                self.repair_parent(path).await?;
                self.invalidate_object(path)?;
                existing = None;
                continue;
            }
            if response.status() == StatusCode::PRECONDITION_FAILED {
                self.invalidate_object(path)?;
                existing = self.get_optional(path).await?;
                continue;
            }
            let response = response.error_for_status()?;
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            self.remember_object(path, &RemoteObject { bytes, etag })?;
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("WebDAV file changed during upload: {path}"),
        )
        .into())
    }

    pub(crate) async fn list_json_files(&self, path: &str) -> SyncResult<Vec<String>> {
        let method = Method::from_bytes(b"PROPFIND")?;
        let body = r#"<?xml version="1.0" encoding="utf-8" ?>
            <d:propfind xmlns:d="DAV:"><d:prop><d:getetag/></d:prop></d:propfind>"#;
        let response = self
            .request(method, self.url(path)?)
            .header(DEPTH, HeaderValue::from_static("1"))
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(body)
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let response = response.error_for_status()?;
        let xml = response.text().await?;
        let mut names = parse_propfind_hrefs(&xml, &self.root)?
            .into_iter()
            .filter_map(|href| href.path_segments()?.next_back().map(str::to_owned))
            .filter_map(|name| self.logical_json_file_name(name))
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .basic_auth(&self.username, Some(&self.password))
    }

    fn url(&self, path: &str) -> SyncResult<Url> {
        let path = path.trim_start_matches('/');
        if !self.cstcloud_compatibility || path.ends_with('/') {
            return Ok(self.root.join(path)?);
        }
        let lower = path.to_ascii_lowercase();
        let mapped = if lower.ends_with(".prop") || lower.ends_with(".zip") {
            path.to_owned()
        } else if lower.ends_with(".json") {
            format!("{path}.prop")
        } else {
            format!("{path}.zip")
        };
        Ok(self.root.join(&mapped)?)
    }

    fn logical_json_file_name(&self, name: String) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        if self.cstcloud_compatibility && lower.ends_with(".json.prop") {
            return Some(name[..name.len() - ".prop".len()].to_owned());
        }
        std::path::Path::new(&name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            .then_some(name)
    }

    async fn ensure_collection_absolute(&self, url: Url) -> SyncResult<()> {
        let key = format!("directory:{url}");
        if self.cache_get(&key)?.is_some() {
            return Ok(());
        }
        if let Err(error) = self.create_collection_absolute(url.clone()).await {
            let missing_parent = error
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::status)
                .is_some_and(|status| {
                    matches!(status, StatusCode::NOT_FOUND | StatusCode::CONFLICT)
                });
            if !missing_parent {
                return Err(error);
            }
            if let Some(relative) = url.path().strip_prefix(self.root.path()) {
                self.repair_parent(&format!("{relative}__directory__"))
                    .await?;
            } else {
                self.create_collection_absolute(self.root.join("../")?)
                    .await?;
            }
            self.create_collection_absolute(url).await?;
        }
        self.cache_set(&key, b"1")
    }

    async fn create_collection_absolute(&self, url: Url) -> SyncResult<()> {
        let response = self
            .request(Method::from_bytes(b"MKCOL")?, url)
            .send()
            .await?;
        match response.status() {
            StatusCode::CREATED | StatusCode::METHOD_NOT_ALLOWED | StatusCode::OK => Ok(()),
            _ => {
                response.error_for_status()?;
                Ok(())
            }
        }
    }
}

fn content_range_start(value: &str) -> Option<u64> {
    value
        .strip_prefix("bytes ")?
        .split_once('-')?
        .0
        .parse()
        .ok()
}

fn parse_propfind_hrefs(xml: &str, base: &Url) -> SyncResult<Vec<Url>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut hrefs = Vec::new();
    let mut in_href = false;
    loop {
        match reader.read_event()? {
            Event::Start(element) if element.local_name().as_ref() == b"href" => in_href = true,
            Event::End(element) if element.local_name().as_ref() == b"href" => in_href = false,
            Event::Text(text) if in_href => {
                let decoded = text.decode()?;
                let unescaped = quick_xml::escape::unescape(&decoded)?;
                if let Ok(url) = Url::parse(&unescaped).or_else(|_| base.join(&unescaped)) {
                    hrefs.push(url);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(hrefs)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::sync::CloudProviderKind;

    fn upload_case(status: u16, redirect: bool, immutable: bool) -> (Vec<u64>, SyncResult<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let payload = vec![42; 2 * 1024 * 1024];
        let expected = payload.clone();
        let server = thread::spawn(move || {
            for attempt in 0..=usize::from(redirect) {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 16 * 1024];
                let header_end = loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                assert!(headers.starts_with("put "));
                assert!(headers.contains(&format!("content-length: {}", expected.len())));
                assert!(headers.contains("authorization: basic "));
                assert_eq!(headers.contains("if-none-match: *"), immutable);
                assert!(!headers.contains("transfer-encoding: chunked"));
                while request.len() - header_end < expected.len() {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    thread::sleep(Duration::from_millis(2));
                }
                assert_eq!(&request[header_end..], expected);
                // Allow progress polling while the server has not acknowledged the PUT.
                thread::sleep(Duration::from_millis(150));
                let response = if redirect && attempt == 0 {
                    "307 Temporary Redirect\r\nLocation: /redirected-upload".to_owned()
                } else {
                    format!("{status} Test")
                };
                write!(
                    socket,
                    "HTTP/1.1 {response}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        });
        let mut settings = SyncSettings::new_device();
        settings.provider = CloudProviderKind::Custom;
        settings.base_url = format!("http://{address}");
        settings.username = "reader".into();
        let client = WebDavClient::new(&settings, "secret".into()).unwrap();
        let mut updates = Vec::new();
        let result = tokio::runtime::Runtime::new().unwrap().block_on(async {
            if immutable {
                client
                    .put_immutable_with_progress(
                        "book.epub",
                        payload,
                        "application/octet-stream",
                        |sent| updates.push(sent),
                    )
                    .await
            } else {
                client
                    .put_mutable_bytes_with_progress(
                        "ocr.zip",
                        payload,
                        "application/zip",
                        |sent| updates.push(sent),
                    )
                    .await
                    .map(|()| true)
            }
        });
        server.join().unwrap();
        assert_eq!(updates.first(), Some(&0));
        assert!(
            updates
                .iter()
                .any(|&sent| sent > 0 && sent < 2 * 1024 * 1024)
        );
        assert!(updates.windows(2).all(|pair| pair[0] <= pair[1]));
        (updates, result)
    }

    #[test]
    fn streamed_book_and_ocr_uploads_report_progress_before_completion() {
        for immutable in [true, false] {
            let (updates, result) = upload_case(201, false, immutable);
            assert!(result.unwrap());
            assert_eq!(updates.last(), Some(&(2 * 1024 * 1024)));
        }
    }

    #[test]
    fn failed_or_already_existing_upload_does_not_report_full_transfer() {
        for status in [500, 412] {
            let (updates, result) = upload_case(status, false, true);
            if status == 500 {
                assert!(result.is_err());
            } else {
                assert!(!result.unwrap());
            }
            assert!(updates.iter().all(|&sent| sent < 2 * 1024 * 1024));
        }
    }

    #[test]
    fn streamed_upload_replays_body_on_same_origin_redirect() {
        let (updates, result) = upload_case(201, true, true);
        assert!(result.unwrap());
        assert_eq!(updates.last(), Some(&(2 * 1024 * 1024)));
    }

    fn captured_request_headers(provider: CloudProviderKind, download: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            String::from_utf8(request).unwrap()
        });
        let mut settings = SyncSettings::new_device();
        settings.provider = provider;
        settings.base_url = format!("http://{address}");
        settings.username = "reader".into();
        let client = WebDavClient::new(&settings, "secret".into()).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            if download {
                client
                    .download_client
                    .get(client.root.clone())
                    .basic_auth(&client.username, Some(&client.password))
                    .send()
                    .await
                    .unwrap();
            } else {
                client.get_optional("probe").await.unwrap();
            }
        });
        server.join().unwrap()
    }

    #[test]
    fn propfind_parser_handles_namespaces_and_escaped_paths() {
        let base = Url::parse("https://dav.example.test/root/Rebook/v1/").unwrap();
        let xml = r#"<?xml version="1.0"?>
          <d:multistatus xmlns:d="DAV:">
            <d:response><d:href>/root/Rebook/v1/library/devices/device-a.json</d:href></d:response>
            <d:response><d:href>/root/Rebook/v1/library/devices/a%20b.json</d:href></d:response>
          </d:multistatus>"#;
        let hrefs = parse_propfind_hrefs(xml, &base).unwrap();
        assert_eq!(hrefs.len(), 2);
        assert!(hrefs[0].path().ends_with("device-a.json"));
        assert!(hrefs[1].path().ends_with("a%20b.json"));
    }

    #[test]
    fn parses_content_range_start() {
        assert_eq!(
            content_range_start("bytes 262144-524287/1048576"),
            Some(262_144)
        );
        assert_eq!(content_range_start("bytes */1048576"), None);
    }

    #[test]
    fn cstcloud_requests_include_the_required_compatibility_user_agent() {
        let expected = concat!("Torto/", env!("CARGO_PKG_VERSION"), " Zotero/7.0");
        let expected_header = format!("user-agent: {expected}");

        for headers in [
            captured_request_headers(CloudProviderKind::CstCloud, false),
            captured_request_headers(CloudProviderKind::CstCloud, true),
        ] {
            assert!(headers.lines().any(|line| {
                line.trim_end_matches('\r')
                    .eq_ignore_ascii_case(&expected_header)
            }));
        }
    }

    #[test]
    fn custom_webdav_requests_do_not_impersonate_a_compatibility_client() {
        let headers = captured_request_headers(CloudProviderKind::Custom, false);
        assert!(
            !headers
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
        );
    }

    #[test]
    fn cstcloud_checks_before_using_an_unconditional_create() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for status in ["404 Not Found", "201 Created"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                requests.push(String::from_utf8(request).unwrap());
            }
            requests
        });

        let mut settings = SyncSettings::new_device();
        settings.provider = CloudProviderKind::CstCloud;
        settings.base_url = format!("http://{address}");
        settings.username = "reader".into();
        let client = WebDavClient::new(&settings, "secret".into()).unwrap();
        let created = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(client.put_immutable_with_progress(
                "probe.json",
                b"payload".to_vec(),
                "application/json",
                |_| {},
            ))
            .unwrap();
        assert!(created);

        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /Rebook/v1/probe.json.prop HTTP/1.1"));
        assert!(requests[1].starts_with("PUT /Rebook/v1/probe.json.prop HTTP/1.1"));
        assert!(!requests[1].to_ascii_lowercase().contains("if-none-match:"));
    }

    #[test]
    fn cstcloud_maps_logical_files_to_its_allowed_extensions() {
        let mut settings = SyncSettings::new_device();
        settings.provider = CloudProviderKind::CstCloud;
        settings.base_url = "http://127.0.0.1:1".into();
        settings.username = "reader".into();
        let client = WebDavClient::new(&settings, "secret".into()).unwrap();

        assert!(
            client
                .url("protocol.json")
                .unwrap()
                .path()
                .ends_with("protocol.json.prop")
        );
        assert!(
            client
                .url("books/id/content.epub")
                .unwrap()
                .path()
                .ends_with("content.epub.zip")
        );
        assert!(
            client
                .url("derived/id/ocr.zip")
                .unwrap()
                .path()
                .ends_with("ocr.zip")
        );
        assert!(
            client
                .url("library/devices/")
                .unwrap()
                .path()
                .ends_with("library/devices/")
        );
        assert_eq!(
            client.logical_json_file_name("device.json.prop".into()),
            Some("device.json".into())
        );
        assert_eq!(client.logical_json_file_name("content.zip".into()), None);
    }
}
