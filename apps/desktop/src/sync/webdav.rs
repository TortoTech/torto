use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::header::{
    CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderName, HeaderValue, IF_MATCH, IF_NONE_MATCH, RANGE,
};
use reqwest::{Client, Method, StatusCode, Url};

use super::SyncResult;
use super::settings::SyncSettings;

const DEPTH: HeaderName = HeaderName::from_static("depth");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_mins(30);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_mins(2);
const DOWNLOAD_PROGRESS_INTERVAL: u64 = 256 * 1024;

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
        if let Some(user_agent) = settings.provider.user_agent() {
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
        })
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
        let response = self.request(Method::GET, self.url(path)?).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response.error_for_status()?;
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(Some(RemoteObject {
            bytes: response.bytes().await?.to_vec(),
            etag,
        }))
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
        let mut last_reported = downloaded;
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
            if downloaded.saturating_sub(last_reported) >= DOWNLOAD_PROGRESS_INTERVAL
                || downloaded == expected_length
            {
                progress(downloaded);
                last_reported = downloaded;
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
        let response = self
            .request(Method::PUT, self.url(path)?)
            .header(IF_NONE_MATCH, "*")
            .header(CONTENT_TYPE, content_type)
            .body(bytes)
            .send()
            .await?;
        if response.status() == StatusCode::PRECONDITION_FAILED {
            return Ok(false);
        }
        response.error_for_status()?;
        Ok(true)
    }

    pub(crate) async fn put_mutable_bytes(
        &self,
        path: &str,
        bytes: Vec<u8>,
        content_type: &'static str,
    ) -> SyncResult<()> {
        self.request(Method::PUT, self.url(path)?)
            .header(CONTENT_TYPE, content_type)
            .body(bytes)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub(crate) async fn put_mutable_json<T: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        value: &T,
    ) -> SyncResult<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        for _ in 0..2 {
            let existing = self.get_optional(path).await?;
            let mut request = self
                .request(Method::PUT, self.url(path)?)
                .header(CONTENT_TYPE, "application/json")
                .body(bytes.clone());
            request = if let Some(etag) = existing.and_then(|object| object.etag) {
                request.header(IF_MATCH, etag)
            } else {
                request.header(IF_NONE_MATCH, "*")
            };
            let response = request.send().await?;
            if response.status() == StatusCode::PRECONDITION_FAILED {
                continue;
            }
            response.error_for_status()?;
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("WebDAV 文件在写入时被其他客户端修改：{path}"),
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
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            })
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
        Ok(self.root.join(path.trim_start_matches('/'))?)
    }

    async fn ensure_collection_absolute(&self, url: Url) -> SyncResult<()> {
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
}
