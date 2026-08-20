use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Cursor, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use directories::ProjectDirs;
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
use quick_xml::escape::unescape;
use rebook_publication::{
    Book, BookSource, PublicationError, PublicationUrl, RasterResource, RenditionLayout, Resource,
    Section, SpineItem, SpineItemId, TableOfContentsOrigin, TocEntry,
};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use zip::ZipArchive;

use super::{MINERU_API_URL, PADDLE_OCR_JOBS_URL, PdfOcrProviderKind, PluginSettings};
use crate::generated_toc::GeneratedTocEntry;
use crate::persistence::write_bytes_atomic;
use crate::persistence::write_json_atomic;

const PDF_OCR_VERSION: u8 = 1;
const PDF_OCR_DIRECTORY: &str = "pdf-ocr";
const DOCUMENT_FILE: &str = "document.json";
const VIEW_MODE_FILE: &str = "view-mode.json";
const PAGE_ROLES_FILE: &str = "page-roles.json";
const PAGE_ROLES_VERSION: u8 = 1;
const PADDLE_JOB_FILE: &str = "paddle-job.json";
const PADDLE_JOB_VERSION: u8 = 1;
const PADDLE_PAGE_CHUNK_SIZE: usize = 100;
const MAX_RESULT_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const PDF_PAGE_ANCHOR_PREFIX: &str = "pdf-page-";

type SharedOcrResult = Result<(), String>;
type SharedOcrSender = watch::Sender<Option<SharedOcrResult>>;

static PDF_OCR_TASKS: OnceLock<Mutex<HashMap<String, SharedOcrSender>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PdfOcrViewMode {
    #[default]
    Original,
    Reflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PdfOcrPageRole {
    Cover,
    TitlePage,
    BackCover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PdfOcrPageRoleAssignment {
    /// One-based physical PDF page.
    pub(crate) physical_page: usize,
    pub(crate) role: PdfOcrPageRole,
}

#[derive(Serialize, Deserialize)]
struct StoredPdfOcrPageRoles {
    version: u8,
    book_id: String,
    roles: Vec<PdfOcrPageRoleAssignment>,
}

pub(crate) struct PdfOcrLoadedSource {
    pub(crate) source: Arc<dyn BookSource>,
    pub(crate) controller: Option<Arc<PdfOcrSourceController>>,
    pub(crate) available: bool,
    pub(crate) mode: PdfOcrViewMode,
}

pub(crate) struct PdfOcrSourceController {
    original: Arc<dyn BookSource>,
    reflow: Arc<dyn BookSource>,
    original_page_targets: Vec<PublicationUrl>,
    reflow_page_targets: Vec<PublicationUrl>,
    reflow_enabled: AtomicBool,
}

impl PdfOcrSourceController {
    fn new(
        original: Arc<dyn BookSource>,
        reflow: Arc<dyn BookSource>,
        reflow_page_targets: Vec<PublicationUrl>,
        mode: PdfOcrViewMode,
    ) -> Self {
        let original_page_targets = original
            .book()
            .sections
            .iter()
            .map(|section| section.href.clone())
            .collect();
        Self {
            original,
            reflow,
            original_page_targets,
            reflow_page_targets,
            reflow_enabled: AtomicBool::new(mode == PdfOcrViewMode::Reflow),
        }
    }

    pub(crate) fn set_mode(&self, mode: PdfOcrViewMode) {
        self.reflow_enabled
            .store(mode == PdfOcrViewMode::Reflow, Ordering::Release);
    }

    pub(crate) fn is_reflow_enabled(&self) -> bool {
        self.reflow_enabled.load(Ordering::Acquire)
    }

    pub(crate) fn original_source(&self) -> Arc<dyn BookSource> {
        Arc::clone(&self.original)
    }

    pub(crate) fn reflow_target_for_page(&self, page_index: usize) -> Option<PublicationUrl> {
        self.reflow_page_targets.get(page_index).cloned()
    }

    pub(crate) fn original_target_for_page(&self, page_index: usize) -> Option<PublicationUrl> {
        self.original_page_targets.get(page_index).cloned()
    }

    pub(crate) fn original_target_for_reflow_anchor(
        &self,
        fragment: &str,
    ) -> Option<PublicationUrl> {
        let page_number = fragment
            .strip_prefix(PDF_PAGE_ANCHOR_PREFIX)?
            .parse::<usize>()
            .ok()?;
        self.original_target_for_page(page_number.checked_sub(1)?)
    }

    fn active(&self) -> &Arc<dyn BookSource> {
        if self.reflow_enabled.load(Ordering::Acquire) {
            &self.reflow
        } else {
            &self.original
        }
    }
}

impl BookSource for PdfOcrSourceController {
    fn book(&self) -> &Book {
        self.active().book()
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.active().table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        self.active().parse_section(index)
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.active().resource(href)
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.active().raster_resource(href)
    }

    fn fixed_page_dimensions(
        &self,
        section_index: usize,
    ) -> Result<Option<rebook_publication::FixedPageDimensions>, PublicationError> {
        self.active().fixed_page_dimensions(section_index)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredPdfOcrDocument {
    version: u8,
    book_id: String,
    provider: PdfOcrProviderKind,
    model: String,
    view_mode: PdfOcrViewMode,
    pages: Vec<StoredOcrPage>,
    resources: Vec<StoredOcrResource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    page_roles: Vec<PdfOcrPageRoleAssignment>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StoredOcrPage {
    markdown: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredOcrResource {
    href: String,
    file_name: String,
    media_type: String,
}

pub(crate) struct PdfOcrSyncData {
    pub(crate) document: Vec<u8>,
    pub(crate) resources: Vec<(String, Vec<u8>)>,
}

struct OcrResourceData {
    source: String,
    href: String,
    file_name: String,
    media_type: String,
    bytes: Vec<u8>,
}

struct ParsedOcrDocument {
    provider: PdfOcrProviderKind,
    model: String,
    pages: Vec<StoredOcrPage>,
    resources: Vec<OcrResourceData>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredPaddleJob {
    version: u8,
    book_id: String,
    model: String,
    page_count: usize,
    chunks: Vec<StoredPaddleChunk>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredPaddleChunk {
    start_page: usize,
    end_page: usize,
    job_id: Option<String>,
}

enum ConfiguredPdfOcrProvider<'a> {
    PaddleOcr(&'a PluginSettings),
    MinerU(&'a PluginSettings),
}

impl<'a> ConfiguredPdfOcrProvider<'a> {
    fn new(settings: &'a PluginSettings) -> Self {
        match settings.pdf_ocr_provider {
            PdfOcrProviderKind::PaddleOcr => Self::PaddleOcr(settings),
            PdfOcrProviderKind::MinerU => Self::MinerU(settings),
        }
    }

    async fn recognize<F>(
        &self,
        client: &Client,
        path: &Path,
        book_id: &str,
        page_count: usize,
        progress: &mut F,
    ) -> Result<ParsedOcrDocument, String>
    where
        F: FnMut(String) + Send,
    {
        match self {
            Self::PaddleOcr(settings) => {
                recognize_with_paddle(client, path, book_id, page_count, settings, progress).await
            }
            Self::MinerU(settings) => {
                recognize_with_mineru(client, path, book_id, settings, progress).await
            }
        }
    }
}

struct OcrReflowBookSource {
    inner: Arc<dyn BookSource>,
    book: Book,
    pages: Vec<StoredOcrPage>,
    page_ranges: Vec<Range<usize>>,
    page_targets: Vec<PublicationUrl>,
    toc_anchors: Vec<Vec<OcrTocAnchor>>,
    page_roles: HashMap<usize, PdfOcrPageRole>,
    resources: HashMap<String, StoredResourceLocation>,
}

#[derive(Clone, Debug)]
struct OcrTocAnchor {
    fragment: String,
    label: String,
}

struct ContinuousReflowSections {
    page_ranges: Vec<Range<usize>>,
    page_targets: Vec<PublicationUrl>,
    toc_anchors: Vec<Vec<OcrTocAnchor>>,
}

struct StoredResourceLocation {
    path: PathBuf,
    media_type: String,
}

pub(crate) fn load_pdf_ocr_source(
    source: Arc<dyn BookSource>,
    reflow_content: bool,
) -> io::Result<PdfOcrLoadedSource> {
    let book_id = source.book().id.to_string();
    let Some(document) = load_document(&book_id)? else {
        return Ok(PdfOcrLoadedSource {
            source,
            controller: None,
            available: false,
            mode: PdfOcrViewMode::Original,
        });
    };
    let mode = load_pdf_ocr_view_mode(&book_id, document.view_mode)?;
    let reflow = Arc::new(OcrReflowBookSource::new(
        Arc::clone(&source),
        document,
        reflow_content,
    )?);
    let reflow_page_targets = reflow.page_targets.clone();
    let reflow_source: Arc<dyn BookSource> = reflow;
    let controller = Arc::new(PdfOcrSourceController::new(
        source,
        reflow_source,
        reflow_page_targets,
        mode,
    ));
    Ok(PdfOcrLoadedSource {
        source: controller.clone(),
        controller: Some(controller),
        available: true,
        mode,
    })
}

pub(crate) fn set_pdf_ocr_view_mode(book_id: &str, mode: PdfOcrViewMode) -> io::Result<()> {
    if !document_path(book_id)?.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "PDF OCR result is unavailable",
        ));
    }
    write_json_atomic(&view_mode_path(book_id)?, &mode)
}

pub(crate) fn has_pending_pdf_ocr_task(
    book_id: &str,
    settings: &PluginSettings,
) -> io::Result<bool> {
    if settings.pdf_ocr_provider != PdfOcrProviderKind::PaddleOcr {
        return Ok(false);
    }
    let job_path = paddle_job_path(book_id)?;
    let document_path = document_path(book_id)?;
    if document_path.try_exists()?
        && job_path.try_exists()?
        && fs::metadata(&document_path)?.modified()? >= fs::metadata(&job_path)?.modified()?
    {
        return Ok(false);
    }
    Ok(load_paddle_job(book_id)?.is_some_and(|job| {
        job.version == PADDLE_JOB_VERSION
            && job.book_id == book_id
            && job.model == settings.paddle_ocr_model.trim()
    }))
}

pub(crate) async fn recognize_pdf<F>(
    path: PathBuf,
    book_id: String,
    page_count: usize,
    settings: PluginSettings,
    mut progress: F,
) -> Result<(), String>
where
    F: FnMut(String) + Send,
{
    let follower = {
        let tasks = PDF_OCR_TASKS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut tasks = tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sender) = tasks.get(&book_id) {
            Some(sender.subscribe())
        } else {
            let (sender, _receiver) = watch::channel(None);
            tasks.insert(book_id.clone(), sender);
            None
        }
    };
    if let Some(mut receiver) = follower {
        progress("该 PDF 正在识别，正在等待现有任务…".into());
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            if receiver.changed().await.is_err() {
                return Err("现有 PDF OCR 任务意外结束".into());
            }
        }
    }

    let result = recognize_pdf_inner(path, &book_id, page_count, settings, &mut progress).await;
    let sender = PDF_OCR_TASKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&book_id);
    if let Some(sender) = sender {
        sender.send_replace(Some(result.clone()));
    }
    result
}

async fn recognize_pdf_inner<F>(
    path: PathBuf,
    book_id: &str,
    page_count: usize,
    settings: PluginSettings,
    progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(String) + Send,
{
    if !settings.pdf_ocr_enabled {
        return Err("请先在“设置 → OCR”中启用正文识别".into());
    }
    progress("正在上传 PDF…".into());
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_mins(3))
        .build()
        .map_err(|error| format!("创建 OCR HTTP 客户端失败：{error}"))?;
    let provider = ConfiguredPdfOcrProvider::new(&settings);
    let mut parsed = provider
        .recognize(&client, &path, book_id, page_count, progress)
        .await?;
    normalize_page_count(&mut parsed.pages, page_count);
    progress("正在生成可重排正文…".into());
    save_document(book_id, parsed).map_err(|error| format!("保存 PDF OCR 结果失败：{error}"))?;
    if settings.pdf_ocr_provider == PdfOcrProviderKind::PaddleOcr
        && let Err(error) = clear_paddle_job(book_id)
    {
        tracing::warn!(%error, %book_id, "failed to clear completed PaddleOCR task");
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the asynchronous provider protocol is clearest as one submit-poll-download flow"
)]
async fn recognize_with_paddle<F>(
    client: &Client,
    path: &Path,
    book_id: &str,
    page_count: usize,
    settings: &PluginSettings,
    progress: &mut F,
) -> Result<ParsedOcrDocument, String>
where
    F: FnMut(String),
{
    let token = settings.paddle_ocr_token.trim();
    if token.is_empty() {
        return Err("请填写 PaddleOCR Access Token".into());
    }
    let endpoint = PADDLE_OCR_JOBS_URL;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document.pdf")
        .to_owned();
    let bytes = Bytes::from(fs::read(path).map_err(|error| format!("读取 PDF 失败：{error}"))?);
    let optional_payload = paddle_optional_payload(settings.paddle_ocr_model.trim());
    let model = settings.paddle_ocr_model.trim();
    let mut stored_job = load_or_create_paddle_job(book_id, model, page_count)
        .map_err(|error| format!("保存 PaddleOCR 任务状态失败：{error}"))?;
    let mut pages = Vec::new();
    let mut resources = Vec::new();
    let chunk_count = stored_job.chunks.len();
    for chunk_index in 0..chunk_count {
        let chunk = stored_job.chunks[chunk_index].clone();
        let result_urls = loop {
            let job_id = if let Some(job_id) = stored_job.chunks[chunk_index].job_id.clone() {
                progress(paddle_resume_message(chunk_index, chunk_count));
                job_id
            } else {
                let job_id = submit_paddle_chunk(
                    client,
                    endpoint,
                    token,
                    model,
                    &optional_payload,
                    &file_name,
                    bytes.clone(),
                    &chunk,
                    chunk_index,
                    chunk_count,
                )
                .await?;
                stored_job.chunks[chunk_index].job_id = Some(job_id.clone());
                save_paddle_job(&stored_job)
                    .map_err(|error| format!("保存 PaddleOCR jobId 失败：{error}"))?;
                job_id
            };
            match poll_paddle_job(
                client,
                endpoint,
                token,
                &job_id,
                &chunk,
                page_count,
                chunk_index,
                chunk_count,
                progress,
            )
            .await?
            {
                PaddlePollResult::Done(urls) => break urls,
                PaddlePollResult::Resubmit => {
                    stored_job.chunks[chunk_index].job_id = None;
                    save_paddle_job(&stored_job)
                        .map_err(|error| format!("更新 PaddleOCR 任务状态失败：{error}"))?;
                }
                PaddlePollResult::Failed(error) => {
                    stored_job.chunks[chunk_index].job_id = None;
                    save_paddle_job(&stored_job)
                        .map_err(|save_error| format!("{error}；保存重试状态失败：{save_error}"))?;
                    return Err(error);
                }
            }
        };
        progress(paddle_download_message(chunk_index, chunk_count));
        let mut chunk_pages = Vec::new();
        let mut chunk_resources = Vec::new();
        let namespace = format!("pages-{}-{}", chunk.start_page, chunk.end_page);
        if let Some(url) = result_urls.get("jsonUrl").and_then(Value::as_str) {
            let text = download_text(client, url, "PaddleOCR JSON").await?;
            parse_paddle_jsonl(
                client,
                &text,
                &namespace,
                &mut chunk_pages,
                &mut chunk_resources,
            )
            .await?;
        }
        if chunk_pages.is_empty()
            && let Some(url) = result_urls.get("markdownUrl").and_then(Value::as_str)
        {
            chunk_pages.push(StoredOcrPage {
                markdown: download_text(client, url, "PaddleOCR Markdown").await?,
            });
        }
        if chunk_pages.is_empty() {
            return Err(format!(
                "PaddleOCR 第 {}-{} 页没有返回可读正文",
                chunk.start_page, chunk.end_page
            ));
        }
        normalize_page_range(&mut chunk_pages, chunk.start_page, chunk.end_page);
        pages.extend(chunk_pages);
        resources.extend(chunk_resources);
    }
    if pages.is_empty() {
        return Err("PaddleOCR 返回的结果中没有可读正文".into());
    }
    Ok(ParsedOcrDocument {
        provider: PdfOcrProviderKind::PaddleOcr,
        model: model.to_owned(),
        pages,
        resources,
    })
}

enum PaddlePollResult {
    Done(Value),
    Resubmit,
    Failed(String),
}

#[allow(clippy::too_many_arguments)]
async fn submit_paddle_chunk(
    client: &Client,
    endpoint: &str,
    token: &str,
    model: &str,
    optional_payload: &Value,
    file_name: &str,
    bytes: Bytes,
    chunk: &StoredPaddleChunk,
    chunk_index: usize,
    chunk_count: usize,
) -> Result<String, String> {
    let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let part = reqwest::multipart::Part::stream_with_length(bytes, byte_count)
        .file_name(file_name.to_owned())
        .mime_str("application/pdf")
        .map_err(|error| format!("创建 PDF 上传内容失败：{error}"))?;
    let form = reqwest::multipart::Form::new()
        .text("model", model.to_owned())
        .text("optionalPayload", optional_payload.to_string())
        .text(
            "pageRanges",
            format!("{}-{}", chunk.start_page, chunk.end_page),
        )
        .part("file", part);
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await
        .map_err(|error| format!("提交 PaddleOCR 任务失败：{error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("解析 PaddleOCR 响应失败：{error}"))?;
    if status.is_success() && api_code(&body).is_none_or(|code| code == 0) {
        return body
            .pointer("/data/jobId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "PaddleOCR 没有返回 jobId".to_owned());
    }
    if api_code(&body) == Some(10010) {
        let chunk = paddle_chunk_label(chunk_index, chunk_count);
        return Err(format!("PaddleOCR 服务队列已满{chunk}，请稍后手动重试"));
    }
    Err(api_error("PaddleOCR", status.as_u16(), &body))
}

#[allow(clippy::too_many_arguments)]
async fn poll_paddle_job<F>(
    client: &Client,
    endpoint: &str,
    token: &str,
    job_id: &str,
    chunk: &StoredPaddleChunk,
    page_count: usize,
    chunk_index: usize,
    chunk_count: usize,
    progress: &mut F,
) -> Result<PaddlePollResult, String>
where
    F: FnMut(String),
{
    let poll_url = format!("{}/{}", endpoint.trim_end_matches('/'), job_id);
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let response = client
            .get(&poll_url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| format!("查询 PaddleOCR 任务失败：{error}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("解析 PaddleOCR 任务状态失败：{error}"))?;
        if !status.is_success() || api_code(&body).is_some_and(|code| code != 0) {
            if matches!(api_code(&body), Some(11001 | 11002)) {
                return Ok(PaddlePollResult::Resubmit);
            }
            return Err(api_error("PaddleOCR", status.as_u16(), &body));
        }
        let state = body
            .pointer("/data/state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if let Some(extracted) = body
            .pointer("/data/extractProgress/extractedPages")
            .and_then(value_as_usize)
        {
            let completed = chunk.start_page.saturating_sub(1);
            let overall = completed.saturating_add(extracted).min(page_count);
            progress(format!("正在解析 {overall}/{page_count} 页…"));
        } else {
            progress(paddle_pending_message(chunk_index, chunk_count));
        }
        match state {
            "done" => {
                return Ok(PaddlePollResult::Done(body["data"]["resultUrl"].clone()));
            }
            "failed" => {
                return Ok(PaddlePollResult::Failed(
                    body.pointer("/data/errorMsg")
                        .and_then(Value::as_str)
                        .unwrap_or("PaddleOCR 解析失败")
                        .to_owned(),
                ));
            }
            _ => {}
        }
    }
}

fn paddle_resume_message(chunk_index: usize, chunk_count: usize) -> String {
    let chunk = paddle_chunk_label(chunk_index, chunk_count);
    format!("正在恢复{chunk}识别任务…")
}

fn paddle_pending_message(chunk_index: usize, chunk_count: usize) -> String {
    let chunk = paddle_chunk_label(chunk_index, chunk_count);
    format!("{chunk}正在排队解析…")
}

fn paddle_download_message(chunk_index: usize, chunk_count: usize) -> String {
    let chunk = paddle_chunk_label(chunk_index, chunk_count);
    format!("正在下载{chunk}结构化结果…")
}

fn paddle_chunk_label(chunk_index: usize, chunk_count: usize) -> String {
    if chunk_count > 1 {
        format!("第 {}/{} 段", chunk_index + 1, chunk_count)
    } else {
        String::new()
    }
}

fn paddle_page_chunks(page_count: usize) -> Vec<StoredPaddleChunk> {
    let page_count = page_count.max(1);
    (1..=page_count)
        .step_by(PADDLE_PAGE_CHUNK_SIZE)
        .map(|start_page| StoredPaddleChunk {
            start_page,
            end_page: start_page
                .saturating_add(PADDLE_PAGE_CHUNK_SIZE - 1)
                .min(page_count),
            job_id: None,
        })
        .collect()
}

fn load_or_create_paddle_job(
    book_id: &str,
    model: &str,
    page_count: usize,
) -> io::Result<StoredPaddleJob> {
    let expected_chunks = paddle_page_chunks(page_count);
    let stored = match load_paddle_job(book_id) {
        Ok(job) => job,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            clear_paddle_job(book_id)?;
            None
        }
        Err(error) => return Err(error),
    };
    if let Some(job) = stored
        && job.version == PADDLE_JOB_VERSION
        && job.book_id == book_id
        && job.model == model
        && job.page_count == page_count
        && job.chunks.len() == expected_chunks.len()
        && job
            .chunks
            .iter()
            .zip(&expected_chunks)
            .all(|(stored, expected)| {
                stored.start_page == expected.start_page && stored.end_page == expected.end_page
            })
    {
        return Ok(job);
    }
    let job = StoredPaddleJob {
        version: PADDLE_JOB_VERSION,
        book_id: book_id.to_owned(),
        model: model.to_owned(),
        page_count,
        chunks: expected_chunks,
    };
    save_paddle_job(&job)?;
    Ok(job)
}

fn load_paddle_job(book_id: &str) -> io::Result<Option<StoredPaddleJob>> {
    let bytes = match fs::read(paddle_job_path(book_id)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn save_paddle_job(job: &StoredPaddleJob) -> io::Result<()> {
    write_json_atomic(&paddle_job_path(&job.book_id)?, job)
}

fn clear_paddle_job(book_id: &str) -> io::Result<()> {
    match fs::remove_file(paddle_job_path(book_id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn paddle_optional_payload(model: &str) -> Value {
    if model.starts_with("PaddleOCR-VL") {
        json!({
            "useDocOrientationClassify": true,
            "useDocUnwarping": false,
            "useLayoutDetection": true,
            "useChartRecognition": true,
            "prettifyMarkdown": true,
        })
    } else {
        json!({
            "useDocOrientationClassify": true,
            "useDocUnwarping": false,
            "useTextlineOrientation": true,
            "useTableRecognition": true,
            "useFormulaRecognition": true,
            "useChartRecognition": false,
            "prettifyMarkdown": true,
        })
    }
}

async fn parse_paddle_jsonl(
    client: &Client,
    text: &str,
    resource_namespace: &str,
    pages: &mut Vec<StoredOcrPage>,
    resources: &mut Vec<OcrResourceData>,
) -> Result<(), String> {
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let root: Value = serde_json::from_str(line)
            .map_err(|error| format!("解析 PaddleOCR JSONL 失败：{error}"))?;
        let result = root.get("result").unwrap_or(&root);
        if let Some(entries) = result.get("layoutParsingResults").and_then(Value::as_array) {
            for entry in entries {
                let mut markdown = entry
                    .pointer("/markdown/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let page_index = pages.len();
                if let Some(images) = entry.pointer("/markdown/images").and_then(Value::as_object) {
                    for (source, value) in images {
                        let Some(data) = value.as_str() else {
                            continue;
                        };
                        let resource =
                            paddle_resource(client, source, data, resource_namespace).await?;
                        markdown = replace_resource_reference(&markdown, source, &resource.href);
                        resources.push(resource);
                    }
                }
                if markdown.trim().is_empty() {
                    markdown = plain_paddle_text(entry);
                }
                if markdown.trim().is_empty() {
                    pages.push(StoredOcrPage {
                        markdown: format!("## 第 {} 页\n\n本页未识别到正文。", page_index + 1),
                    });
                } else {
                    pages.push(StoredOcrPage { markdown });
                }
            }
        } else if let Some(entries) = result.get("ocrResults").and_then(Value::as_array) {
            for entry in entries {
                pages.push(StoredOcrPage {
                    markdown: plain_paddle_text(entry),
                });
            }
        }
    }
    Ok(())
}

fn plain_paddle_text(value: &Value) -> String {
    value
        .pointer("/prunedResult/rec_texts")
        .or_else(|| value.pointer("/rec_texts"))
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .filter(|line| !line.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

#[allow(
    clippy::too_many_lines,
    reason = "the asynchronous provider protocol is clearest as one submit-upload-poll flow"
)]
async fn recognize_with_mineru<F>(
    client: &Client,
    path: &Path,
    book_id: &str,
    settings: &PluginSettings,
    progress: &mut F,
) -> Result<ParsedOcrDocument, String>
where
    F: FnMut(String),
{
    let token = settings.mineru_token.trim();
    if token.is_empty() {
        return Err("请填写 MinerU API Token".into());
    }
    let base = MINERU_API_URL.trim_end_matches('/').to_owned();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document.pdf")
        .to_owned();
    let response = client
        .post(format!("{base}/file-urls/batch"))
        .bearer_auth(token)
        .json(&json!({
            "files": [{
                "name": file_name,
                "data_id": book_id,
                "is_ocr": true,
            }],
            "model_version": settings.mineru_model,
            "language": "ch",
            "enable_formula": true,
            "enable_table": true,
        }))
        .send()
        .await
        .map_err(|error| format!("申请 MinerU 上传地址失败：{error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("解析 MinerU 响应失败：{error}"))?;
    if !status.is_success() || api_code(&body).is_some_and(|code| code != 0) {
        return Err(api_error("MinerU", status.as_u16(), &body));
    }
    let batch_id = body
        .pointer("/data/batch_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "MinerU 没有返回 batch_id".to_owned())?;
    let upload_url = body
        .pointer("/data/file_urls/0")
        .and_then(Value::as_str)
        .ok_or_else(|| "MinerU 没有返回文件上传地址".to_owned())?;
    let bytes = fs::read(path).map_err(|error| format!("读取 PDF 失败：{error}"))?;
    let response = client
        .put(upload_url)
        .body(bytes)
        .send()
        .await
        .map_err(|error| format!("上传 PDF 到 MinerU 失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "上传 PDF 到 MinerU 失败：HTTP {}",
            response.status()
        ));
    }

    let poll_url = format!("{base}/extract-results/batch/{batch_id}");
    let zip_url = loop {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let response = client
            .get(&poll_url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| format!("查询 MinerU 任务失败：{error}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("解析 MinerU 任务状态失败：{error}"))?;
        if !status.is_success() || api_code(&body).is_some_and(|code| code != 0) {
            return Err(api_error("MinerU", status.as_u16(), &body));
        }
        let result = body
            .pointer("/data/extract_result/0")
            .ok_or_else(|| "MinerU 任务结果为空".to_owned())?;
        let state = result
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if let Some(extracted) = result
            .pointer("/extract_progress/extracted_pages")
            .and_then(value_as_usize)
        {
            let total = result
                .pointer("/extract_progress/total_pages")
                .and_then(value_as_usize)
                .unwrap_or(extracted);
            progress(format!("正在解析 {extracted}/{total} 页…"));
        } else {
            progress("正在排队解析…".into());
        }
        match state {
            "done" => {
                break result
                    .get("full_zip_url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "MinerU 没有返回结果下载地址".to_owned())?
                    .to_owned();
            }
            "failed" => {
                return Err(result
                    .get("err_msg")
                    .and_then(Value::as_str)
                    .unwrap_or("MinerU 解析失败")
                    .to_owned());
            }
            _ => {}
        }
    };
    progress("正在下载结构化结果…".into());
    let bytes = download_bytes(client, &zip_url, "MinerU ZIP").await?;
    let (pages, resources) = parse_mineru_zip(bytes)?;
    if pages.is_empty() {
        return Err("MinerU 返回的结果中没有可读正文".into());
    }
    Ok(ParsedOcrDocument {
        provider: PdfOcrProviderKind::MinerU,
        model: settings.mineru_model.clone(),
        pages,
        resources,
    })
}

fn parse_mineru_zip(bytes: Vec<u8>) -> Result<(Vec<StoredOcrPage>, Vec<OcrResourceData>), String> {
    if bytes.len() > MAX_RESULT_BYTES {
        return Err("MinerU 结果压缩包过大".into());
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("打开 MinerU ZIP 失败：{error}"))?;
    let mut entries = Vec::new();
    let mut total_size = 0_usize;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| format!("读取 MinerU ZIP 条目失败：{error}"))?;
        if file.is_dir() || !safe_archive_name(file.name()) {
            continue;
        }
        let size = usize::try_from(file.size()).unwrap_or(usize::MAX);
        total_size = total_size.saturating_add(size);
        if total_size > MAX_RESULT_BYTES {
            return Err("MinerU 解压结果过大".into());
        }
        let mut content = Vec::with_capacity(size.min(8 * 1024 * 1024));
        file.read_to_end(&mut content)
            .map_err(|error| format!("解压 MinerU 结果失败：{error}"))?;
        entries.push((file.name().replace('\\', "/"), content));
    }

    let mut resources = Vec::new();
    for (name, content) in &entries {
        if image_media_type(name).is_some() {
            resources.push(resource_from_bytes(name, content.clone()));
        }
    }
    let structured = entries
        .iter()
        .find(|(name, _)| name.ends_with("content_list_v2.json"))
        .or_else(|| {
            entries
                .iter()
                .find(|(name, _)| name.ends_with("content_list.json"))
        });
    let mut pages = if let Some((_, content)) = structured {
        let value: Value = serde_json::from_slice(content)
            .map_err(|error| format!("解析 MinerU 结构化 JSON 失败：{error}"))?;
        mineru_pages_from_value(&value)
    } else {
        Vec::new()
    };
    if pages.is_empty()
        && let Some((_, content)) = entries.iter().find(|(name, _)| name.ends_with("full.md"))
    {
        pages.push(StoredOcrPage {
            markdown: String::from_utf8_lossy(content).into_owned(),
        });
    }
    for page in &mut pages {
        for resource in &resources {
            page.markdown =
                replace_resource_reference(&page.markdown, &resource.source, &resource.href);
        }
    }
    Ok((pages, resources))
}

fn mineru_pages_from_value(value: &Value) -> Vec<StoredOcrPage> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    if items.first().is_some_and(Value::is_array) {
        return items
            .iter()
            .map(|page| StoredOcrPage {
                markdown: page
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(mineru_item_markdown)
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default(),
            })
            .collect();
    }
    let mut pages = BTreeMap::<usize, Vec<String>>::new();
    for item in items {
        let page = item
            .get("page_idx")
            .and_then(value_as_usize)
            .unwrap_or_default();
        if let Some(markdown) = mineru_item_markdown(item) {
            pages.entry(page).or_default().push(markdown);
        }
    }
    let page_count = pages.keys().next_back().map_or(0, |page| page + 1);
    (0..page_count)
        .map(|page| StoredOcrPage {
            markdown: pages.remove(&page).unwrap_or_default().join("\n\n"),
        })
        .collect()
}

fn mineru_item_markdown(item: &Value) -> Option<String> {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("text");
    let content = preferred_text(item).trim().to_owned();
    match kind {
        "title" => {
            let level = item
                .get("text_level")
                .or_else(|| item.pointer("/content/level"))
                .and_then(value_as_usize)
                .unwrap_or(2)
                .clamp(1, 6);
            (!content.is_empty()).then(|| format!("{} {content}", "#".repeat(level)))
        }
        "image" | "chart" => {
            let path = item
                .get("img_path")
                .or_else(|| item.get("image_path"))
                .or_else(|| item.pointer("/content/img_path"))
                .or_else(|| item.pointer("/content/image_path"))
                .and_then(Value::as_str);
            path.map(|path| {
                let caption = item
                    .get("image_caption")
                    .or_else(|| item.get("chart_caption"))
                    .map(preferred_text)
                    .unwrap_or_default();
                format!("![{}]({path})", caption.trim())
            })
            .or_else(|| (!content.is_empty()).then_some(content))
        }
        "table" => {
            let table = item
                .get("table_body")
                .or_else(|| item.pointer("/content/table_body"))
                .map(preferred_text)
                .unwrap_or(content);
            (!table.trim().is_empty()).then_some(table)
        }
        "equation" | "equation_interline" => {
            (!content.is_empty()).then(|| format!("$$\n{content}\n$$"))
        }
        "code" | "algorithm" => (!content.is_empty()).then(|| format!("```\n{content}\n```")),
        "list" | "index" => {
            let items = item
                .get("list_items")
                .or_else(|| item.pointer("/content/list_items"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(preferred_text)
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| format!("- {}", text.trim()))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or(content);
            (!items.trim().is_empty()).then_some(items)
        }
        _ => (!content.is_empty()).then_some(content),
    }
}

fn preferred_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .map(preferred_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
    }
    let Some(object) = value.as_object() else {
        return String::new();
    };
    for key in [
        "text",
        "content",
        "paragraph_content",
        "title_content",
        "math_content",
        "code_body",
        "code_content",
        "table_body",
        "list_items",
    ] {
        if let Some(value) = object.get(key) {
            let text = preferred_text(value);
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    String::new()
}

async fn download_remote_resource(
    client: &Client,
    source: &str,
    url: &str,
    namespace: &str,
) -> Result<OcrResourceData, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("下载 OCR 图片失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载 OCR 图片失败：HTTP {}", response.status()));
    }
    let media_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value))
        .filter(|value| value.starts_with("image/"))
        .map(str::to_owned)
        .or_else(|| image_media_type(source).map(str::to_owned))
        .unwrap_or_else(|| "image/png".into());
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取 OCR 图片失败：{error}"))?
        .to_vec();
    Ok(namespaced_resource_data(
        source, namespace, bytes, media_type,
    ))
}

async fn paddle_resource(
    client: &Client,
    source: &str,
    data: &str,
    namespace: &str,
) -> Result<OcrResourceData, String> {
    if data.starts_with("https://") || data.starts_with("http://") {
        return download_remote_resource(client, source, data, namespace).await;
    }
    let (encoded, declared_media_type) = data
        .strip_prefix("data:")
        .and_then(|value| value.split_once(','))
        .map_or((data, None), |(header, encoded)| {
            (encoded, header.strip_suffix(";base64"))
        });
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|error| format!("解析 PaddleOCR 图片失败：{error}"))?;
    let media_type = declared_media_type
        .filter(|value| value.starts_with("image/"))
        .map(str::to_owned)
        .or_else(|| image_media_type(source).map(str::to_owned))
        .unwrap_or_else(|| "image/png".into());
    Ok(namespaced_resource_data(
        source, namespace, bytes, media_type,
    ))
}

async fn download_text(client: &Client, url: &str, label: &str) -> Result<String, String> {
    let bytes = download_bytes(client, url, label).await?;
    String::from_utf8(bytes).map_err(|error| format!("{label} 不是有效 UTF-8：{error}"))
}

async fn download_bytes(client: &Client, url: &str, label: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("下载 {label} 失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载 {label} 失败：HTTP {}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESULT_BYTES as u64)
    {
        return Err(format!("{label} 过大"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取 {label} 失败：{error}"))?;
    if bytes.len() > MAX_RESULT_BYTES {
        return Err(format!("{label} 过大"));
    }
    Ok(bytes.to_vec())
}

fn resource_from_bytes(source: &str, bytes: Vec<u8>) -> OcrResourceData {
    let normalized = source.replace('\\', "/");
    let source = normalized
        .rfind("/images/")
        .map_or(normalized.as_str(), |index| &normalized[index + 1..]);
    resource_data(
        source,
        bytes,
        image_media_type(source)
            .unwrap_or("application/octet-stream")
            .into(),
    )
}

fn resource_data(source: &str, bytes: Vec<u8>, media_type: String) -> OcrResourceData {
    resource_data_with_identity(source, source, bytes, media_type)
}

fn namespaced_resource_data(
    source: &str,
    namespace: &str,
    bytes: Vec<u8>,
    media_type: String,
) -> OcrResourceData {
    resource_data_with_identity(source, &format!("{namespace}/{source}"), bytes, media_type)
}

fn resource_data_with_identity(
    source: &str,
    identity: &str,
    bytes: Vec<u8>,
    media_type: String,
) -> OcrResourceData {
    let extension = extension_for_media(&media_type)
        .or_else(|| {
            Path::new(source)
                .extension()
                .and_then(|value| value.to_str())
        })
        .unwrap_or("bin");
    let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
    let file_name = format!("{digest}.{extension}");
    OcrResourceData {
        source: source.replace('\\', "/"),
        href: format!("OcrResources/{file_name}"),
        file_name,
        media_type,
        bytes,
    }
}

fn replace_resource_reference(markdown: &str, source: &str, href: &str) -> String {
    let normalized = source.replace('\\', "/");
    markdown
        .replace(source, &format!("../{href}"))
        .replace(&normalized, &format!("../{href}"))
}

fn normalize_page_count(pages: &mut Vec<StoredOcrPage>, page_count: usize) {
    if pages.len() > page_count && page_count > 0 {
        pages.truncate(page_count);
    }
    while pages.len() < page_count {
        pages.push(StoredOcrPage::default());
    }
}

fn normalize_page_range(pages: &mut Vec<StoredOcrPage>, start_page: usize, end_page: usize) {
    let page_count = end_page.saturating_sub(start_page) + 1;
    if pages.len() > page_count {
        pages.truncate(page_count);
    }
    while pages.len() < page_count {
        pages.push(StoredOcrPage::default());
    }
}

fn save_document(book_id: &str, parsed: ParsedOcrDocument) -> io::Result<()> {
    let directory = book_directory(book_id)?;
    let resource_directory = directory.join("resources");
    fs::create_dir_all(&resource_directory)?;
    let resources = parsed
        .resources
        .into_iter()
        .map(|resource| {
            fs::write(resource_directory.join(&resource.file_name), resource.bytes)?;
            Ok(StoredOcrResource {
                href: resource.href,
                file_name: resource.file_name,
                media_type: resource.media_type,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let document = StoredPdfOcrDocument {
        version: PDF_OCR_VERSION,
        book_id: book_id.to_owned(),
        provider: parsed.provider,
        model: parsed.model,
        view_mode: PdfOcrViewMode::Reflow,
        pages: parsed.pages,
        resources,
        page_roles: load_pdf_ocr_page_roles(book_id)?,
    };
    write_json_atomic(&directory.join(DOCUMENT_FILE), &document)?;
    write_json_atomic(&directory.join(VIEW_MODE_FILE), &PdfOcrViewMode::Reflow)?;
    crate::sync::mark_derived_dirty(book_id, crate::sync::DerivedDataKind::Ocr)
}

pub(crate) fn save_pdf_ocr_page_roles(
    book_id: &str,
    roles: &[PdfOcrPageRoleAssignment],
) -> io::Result<()> {
    let directory = book_directory(book_id)?;
    fs::create_dir_all(&directory)?;
    let mut roles = roles.to_vec();
    roles.retain(|assignment| assignment.physical_page > 0);
    roles.sort_unstable_by_key(|assignment| assignment.physical_page);
    roles.dedup_by_key(|assignment| assignment.physical_page);
    write_json_atomic(
        &directory.join(PAGE_ROLES_FILE),
        &StoredPdfOcrPageRoles {
            version: PAGE_ROLES_VERSION,
            book_id: book_id.to_owned(),
            roles: roles.clone(),
        },
    )?;
    if let Some(mut document) = load_document(book_id)? {
        document.page_roles = roles;
        write_json_atomic(&directory.join(DOCUMENT_FILE), &document)?;
        crate::sync::mark_derived_dirty(book_id, crate::sync::DerivedDataKind::Ocr)?;
    }
    Ok(())
}

fn load_pdf_ocr_page_roles(book_id: &str) -> io::Result<Vec<PdfOcrPageRoleAssignment>> {
    let bytes = match fs::read(book_directory(book_id)?.join(PAGE_ROLES_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let stored: StoredPdfOcrPageRoles = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored.version != PAGE_ROLES_VERSION || stored.book_id != book_id {
        return Ok(Vec::new());
    }
    Ok(stored.roles)
}

pub(crate) fn export_pdf_ocr_sync_data(book_id: &str) -> io::Result<Option<PdfOcrSyncData>> {
    let Some(mut document) = load_document(book_id)? else {
        return Ok(None);
    };
    // The selected PDF/OCR view is a per-device preference, not derived content.
    document.view_mode = PdfOcrViewMode::Original;
    let resource_directory = book_directory(book_id)?.join("resources");
    let mut resources = Vec::with_capacity(document.resources.len());
    for resource in &document.resources {
        validate_sync_resource_name(&resource.file_name)?;
        resources.push((
            resource.file_name.clone(),
            fs::read(resource_directory.join(&resource.file_name))?,
        ));
    }
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(Some(PdfOcrSyncData {
        document: serde_json::to_vec_pretty(&document).map_err(io::Error::other)?,
        resources,
    }))
}

pub(crate) fn import_pdf_ocr_sync_data(book_id: &str, data: PdfOcrSyncData) -> io::Result<()> {
    let mut document: StoredPdfOcrDocument = serde_json::from_slice(&data.document)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if document.version != PDF_OCR_VERSION || document.book_id != book_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "PDF OCR result does not match the synced book",
        ));
    }
    document.view_mode = PdfOcrViewMode::Original;
    let resources = data.resources.into_iter().collect::<BTreeMap<_, _>>();
    let expected_resources = document
        .resources
        .iter()
        .map(|resource| resource.file_name.clone())
        .collect::<BTreeSet<_>>();
    if expected_resources.len() != document.resources.len()
        || expected_resources.len() != resources.len()
        || !expected_resources
            .iter()
            .all(|file_name| resources.contains_key(file_name))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "PDF OCR resource list contains duplicates or missing files",
        ));
    }
    for resource in &document.resources {
        validate_sync_resource_name(&resource.file_name)?;
    }

    let directory = book_directory(book_id)?;
    let resource_directory = directory.join("resources");
    fs::create_dir_all(&resource_directory)?;
    for (file_name, bytes) in &resources {
        validate_sync_resource_name(file_name)?;
        write_bytes_atomic(&resource_directory.join(file_name), bytes)?;
    }
    write_json_atomic(&directory.join(DOCUMENT_FILE), &document)?;
    if let Ok(entries) = fs::read_dir(&resource_directory) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !resources.contains_key(&file_name) {
                fs::remove_file(entry.path()).ok();
            }
        }
    }
    Ok(())
}

fn validate_sync_resource_name(file_name: &str) -> io::Result<()> {
    if file_name.is_empty()
        || file_name.contains(['/', '\\'])
        || !file_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "PDF OCR resource name is unsafe",
        ));
    }
    Ok(())
}

fn load_document(book_id: &str) -> io::Result<Option<StoredPdfOcrDocument>> {
    let path = document_path(book_id)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let document: StoredPdfOcrDocument = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if document.version != PDF_OCR_VERSION || document.book_id != book_id {
        return Ok(None);
    }
    Ok(Some(document))
}

fn document_path(book_id: &str) -> io::Result<PathBuf> {
    Ok(book_directory(book_id)?.join(DOCUMENT_FILE))
}

fn view_mode_path(book_id: &str) -> io::Result<PathBuf> {
    Ok(book_directory(book_id)?.join(VIEW_MODE_FILE))
}

fn paddle_job_path(book_id: &str) -> io::Result<PathBuf> {
    Ok(book_directory(book_id)?.join(PADDLE_JOB_FILE))
}

fn load_pdf_ocr_view_mode(book_id: &str, fallback: PdfOcrViewMode) -> io::Result<PdfOcrViewMode> {
    let bytes = match fs::read(view_mode_path(book_id)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(fallback),
        Err(error) => return Err(error),
    };
    Ok(serde_json::from_slice(&bytes).unwrap_or(fallback))
}

fn book_directory(book_id: &str) -> io::Result<PathBuf> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "application data directory is unavailable",
        )
    })?;
    let safe_id = if book_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        book_id.to_owned()
    } else {
        format!("{:x}", Sha256::digest(book_id.as_bytes()))
    };
    Ok(project
        .data_local_dir()
        .join(PDF_OCR_DIRECTORY)
        .join(safe_id))
}

fn reflow_ocr_page_boundaries(
    pages: &mut [StoredOcrPage],
    page_roles: &HashMap<usize, PdfOcrPageRole>,
) {
    for index in 0..pages.len().saturating_sub(1) {
        if page_roles.contains_key(&index) || page_roles.contains_key(&(index + 1)) {
            continue;
        }
        let mut current = markdown_blocks(&pages[index].markdown);
        let mut next = markdown_blocks(&pages[index + 1].markdown);
        let Some((tail, head)) = current.last().zip(next.first()) else {
            continue;
        };
        if !should_join_page_paragraphs(tail, head) {
            continue;
        }
        let merged = join_page_paragraphs(tail, head);
        if let Some(tail) = current.last_mut() {
            *tail = merged;
        }
        next.remove(0);
        pages[index].markdown = current.join("\n\n");
        pages[index + 1].markdown = next.join("\n\n");
    }
}

fn remove_missing_ocr_page_placeholders(pages: &mut [StoredOcrPage]) {
    static PLACEHOLDER: OnceLock<Regex> = OnceLock::new();
    let regex = PLACEHOLDER.get_or_init(|| {
        Regex::new(
            r"(?s)^\s*#{1,6}\s*第\s*\d+\s*页\s*本页(?:未识别到|尚未生成\s*OCR\s*)正文[。.]?\s*$",
        )
        .expect("missing OCR page placeholder regex should compile")
    });
    for page in pages {
        if regex.is_match(&page.markdown) {
            page.markdown.clear();
        }
    }
}

fn markdown_blocks(markdown: &str) -> Vec<String> {
    markdown
        .replace("\r\n", "\n")
        .split("\n\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .map(str::to_owned)
        .collect()
}

fn should_join_page_paragraphs(tail: &str, head: &str) -> bool {
    if !is_plain_ocr_paragraph(tail) || !is_plain_ocr_paragraph(head) {
        return false;
    }
    let tail = tail.trim_end();
    let head = head.trim_start();
    if tail.is_empty() || head.is_empty() {
        return false;
    }
    if tail.ends_with(['-', '‐', '‑', '\u{ad}']) {
        return true;
    }
    let terminal = tail
        .trim_end_matches(['”', '’', '"', '\'', ')', '）', ']', '】'])
        .chars()
        .next_back();
    match terminal {
        Some('.') => head.chars().next().is_some_and(char::is_lowercase),
        Some(character) if rebook_reader::is_sentence_terminal(character) => false,
        Some(_) => true,
        None => false,
    }
}

fn is_plain_ocr_paragraph(block: &str) -> bool {
    let options = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_MATH;
    let mut events = Parser::new_ext(block.trim(), options);
    if !matches!(events.next(), Some(Event::Start(Tag::Paragraph))) {
        return false;
    }
    !events.any(|event| {
        matches!(
            event,
            Event::Start(Tag::Image { .. })
                | Event::Html(_)
                | Event::InlineHtml(_)
                | Event::DisplayMath(_)
        )
    })
}

fn join_page_paragraphs(tail: &str, head: &str) -> String {
    let mut tail = tail.trim_end().to_owned();
    let head = head.trim_start();
    if tail.ends_with(['-', '‐', '‑', '\u{ad}']) {
        tail.pop();
        tail.push_str(head);
        return tail;
    }
    let insert_space = tail
        .chars()
        .next_back()
        .zip(head.chars().next())
        .is_some_and(|(left, right)| left.is_ascii_alphanumeric() && right.is_ascii_alphanumeric());
    if insert_space {
        tail.push(' ');
    }
    tail.push_str(head);
    tail
}

impl OcrReflowBookSource {
    fn new(
        inner: Arc<dyn BookSource>,
        document: StoredPdfOcrDocument,
        reflow_content: bool,
    ) -> io::Result<Self> {
        let mut book = inner.book().clone();
        book.metadata.layout = RenditionLayout::Reflowable;
        let resource_directory = book_directory(&document.book_id)?.join("resources");
        let resources = document
            .resources
            .into_iter()
            .map(|resource| {
                (
                    resource.href,
                    StoredResourceLocation {
                        path: resource_directory.join(resource.file_name),
                        media_type: resource.media_type,
                    },
                )
            })
            .collect();
        let page_roles = document
            .page_roles
            .into_iter()
            .filter_map(|assignment| {
                assignment
                    .physical_page
                    .checked_sub(1)
                    .map(|index| (index, assignment.role))
            })
            .collect::<HashMap<_, _>>();
        let mut pages = document.pages;
        remove_missing_ocr_page_placeholders(&mut pages);
        if reflow_content {
            reflow_ocr_page_boundaries(&mut pages, &page_roles);
        }
        let reflow_sections = if reflow_content {
            build_continuous_reflow_sections(&mut book, &pages, inner.table_of_contents_origin())?
        } else {
            let page_ranges = (0..pages.len())
                .map(|index| index..index + 1)
                .collect::<Vec<_>>();
            let page_targets = book
                .sections
                .iter()
                .map(|section| section.href.clone())
                .collect();
            ContinuousReflowSections {
                page_ranges,
                page_targets,
                toc_anchors: vec![Vec::new(); pages.len()],
            }
        };
        Ok(Self {
            inner,
            book,
            pages,
            page_ranges: reflow_sections.page_ranges,
            page_targets: reflow_sections.page_targets,
            toc_anchors: reflow_sections.toc_anchors,
            page_roles,
            resources,
        })
    }
}

fn build_continuous_reflow_sections(
    book: &mut Book,
    pages: &[StoredOcrPage],
    toc_origin: TableOfContentsOrigin,
) -> io::Result<ContinuousReflowSections> {
    let page_count = pages.len();
    let original_sections = book.sections.clone();
    let page_indices = original_sections
        .iter()
        .enumerate()
        .map(|(index, section)| (section.href.path().to_owned(), index))
        .collect::<HashMap<_, _>>();
    let mut starts = vec![0];
    if toc_origin != TableOfContentsOrigin::Fallback {
        starts.extend(
            book.table_of_contents
                .iter()
                .filter_map(|entry| first_toc_page(entry, &page_indices)),
        );
    }
    starts.retain(|start| *start < page_count);
    starts.sort_unstable();
    starts.dedup();
    if starts.is_empty() {
        starts.push(0);
    }

    let mut page_ranges = Vec::with_capacity(starts.len());
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(page_count);
        if start < end || (page_count == 0 && page_ranges.is_empty()) {
            page_ranges.push(start..end);
        }
    }
    if page_ranges.is_empty() {
        page_ranges.push(0..page_count);
    }

    let mut page_targets = Vec::with_capacity(page_count);
    let mut sections = Vec::with_capacity(page_ranges.len());
    for (index, range) in page_ranges.iter().enumerate() {
        let href = PublicationUrl::parse(&format!("Text/ocr-reflow-{}.xhtml", index + 1))
            .map_err(publication_io_error)?;
        sections.push(SpineItem {
            id: SpineItemId::new(format!("pdf-ocr-reflow-{}", index + 1))
                .map_err(publication_io_error)?,
            href: href.clone(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        });
        for page_index in range.clone() {
            page_targets.push(
                href.resolve(&format!("#{PDF_PAGE_ANCHOR_PREFIX}{}", page_index + 1))
                    .map_err(publication_io_error)?,
            );
        }
    }
    let mut toc_anchors = vec![Vec::new(); page_count];
    let heading_keys = pages
        .iter()
        .map(|page| markdown_heading_keys(&page.markdown))
        .collect::<Vec<_>>();
    let mut next_anchor = 0;
    remap_reflow_toc(
        &mut book.table_of_contents,
        &page_indices,
        &page_targets,
        &heading_keys,
        &mut toc_anchors,
        &mut next_anchor,
    )?;
    book.sections = sections;
    Ok(ContinuousReflowSections {
        page_ranges,
        page_targets,
        toc_anchors,
    })
}

fn first_toc_page(entry: &TocEntry, page_indices: &HashMap<String, usize>) -> Option<usize> {
    entry
        .href
        .as_ref()
        .and_then(|href| page_indices.get(href.path()))
        .copied()
        .or_else(|| {
            entry
                .children
                .iter()
                .filter_map(|child| first_toc_page(child, page_indices))
                .min()
        })
}

fn remap_reflow_toc(
    entries: &mut [TocEntry],
    page_indices: &HashMap<String, usize>,
    page_targets: &[PublicationUrl],
    heading_keys: &[Vec<String>],
    toc_anchors: &mut [Vec<OcrTocAnchor>],
    next_anchor: &mut usize,
) -> io::Result<()> {
    for entry in entries {
        if let Some(page_index) = entry
            .href
            .as_ref()
            .and_then(|href| page_indices.get(href.path()))
            .copied()
            && let Some(page_target) = page_targets.get(page_index)
        {
            let descendant_labels = toc_descendant_heading_keys(&entry.children);
            let first_child_page = entry
                .children
                .first()
                .and_then(|child| child.href.as_ref())
                .and_then(|href| page_indices.get(href.path()))
                .copied();
            let heading_page = find_matching_heading_page(
                heading_keys,
                page_index,
                &entry.label,
                &descendant_labels,
                &[],
                first_child_page,
            );
            let target = if let Some(heading_page) = heading_page {
                let fragment = format!("ocr-toc-{}", *next_anchor);
                *next_anchor += 1;
                toc_anchors[heading_page].push(OcrTocAnchor {
                    fragment: fragment.clone(),
                    label: entry.label.clone(),
                });
                page_targets[heading_page]
                    .resource_url()
                    .resolve(&format!("#{fragment}"))
                    .map_err(publication_io_error)?
            } else {
                page_target.clone()
            };
            entry.href = Some(target);
        }
        remap_reflow_toc(
            &mut entry.children,
            page_indices,
            page_targets,
            heading_keys,
            toc_anchors,
            next_anchor,
        )?;
    }
    Ok(())
}

fn toc_descendant_heading_keys(entries: &[TocEntry]) -> Vec<String> {
    let mut labels = Vec::new();
    for entry in entries {
        let label = normalize_toc_heading(&entry.label);
        if !label.is_empty() {
            labels.push(label);
        }
        labels.extend(toc_descendant_heading_keys(&entry.children));
    }
    labels
}

fn find_matching_heading_page(
    heading_keys: &[Vec<String>],
    preferred_page: usize,
    label: &str,
    descendant_labels: &[String],
    excluded_pages: &[bool],
    maximum_page: Option<usize>,
) -> Option<usize> {
    const SEARCH_RADIUS: usize = 12;

    if heading_keys.is_empty() || preferred_page >= heading_keys.len() {
        return None;
    }
    let label = normalize_toc_heading(label);
    if label.is_empty() {
        return None;
    }
    let start = preferred_page.saturating_sub(SEARCH_RADIUS);
    let end = preferred_page
        .saturating_add(SEARCH_RADIUS)
        .min(heading_keys.len() - 1);
    (start..=end)
        .filter(|page| !excluded_pages.get(*page).copied().unwrap_or(false))
        .filter(|page| maximum_page.is_none_or(|maximum| *page <= maximum))
        .filter(|page| {
            heading_keys[*page]
                .iter()
                .any(|heading| normalized_toc_headings_match(heading, &label))
        })
        .min_by_key(|page| {
            let descendant_matches = descendant_labels
                .iter()
                .filter(|descendant| {
                    heading_keys[*page]
                        .iter()
                        .any(|heading| normalized_toc_headings_match(heading, descendant))
                })
                .count();
            (
                descendant_matches,
                page.abs_diff(preferred_page),
                usize::from(*page < preferred_page),
            )
        })
}

fn publication_io_error(error: PublicationError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

impl BookSource for OcrReflowBookSource {
    fn book(&self) -> &Book {
        &self.book
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.inner.table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let descriptor = self
            .book
            .sections
            .get(index)
            .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))?;
        let range = self
            .page_ranges
            .get(index)
            .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))?;
        let mut body = String::new();
        for page_index in range.clone() {
            let _ = write!(
                body,
                r#"<div id="{PDF_PAGE_ANCHOR_PREFIX}{}"></div>"#,
                page_index + 1
            );
            if self.page_roles.contains_key(&page_index) {
                if let Ok(page) = self.inner.parse_section(page_index)
                    && let Some(image) = page.blocks.iter().find_map(|block| match block {
                        rebook_publication::Block::Image(image) => Some(image),
                        rebook_publication::Block::Figure(figure) => figure.images.first(),
                        rebook_publication::Block::Text(_)
                        | rebook_publication::Block::Quote(_)
                        | rebook_publication::Block::Table(_)
                        | rebook_publication::Block::Separator
                        | rebook_publication::Block::LineBreak
                        | rebook_publication::Block::PageBreak => None,
                    })
                {
                    let _ = write!(
                        body,
                        r#"<img src="{}" alt="PDF page {}" style="display:block;max-width:100%;max-height:100%;margin:0 auto" />"#,
                        format_args!("../{}", image.href.path()),
                        page_index + 1
                    );
                }
                body.push_str("<br />");
                continue;
            }
            let markdown = self
                .pages
                .get(page_index)
                .map_or("", |page| page.markdown.as_str());
            if !markdown.trim().is_empty() {
                body.push_str(&markdown_to_html_with_toc_anchors(
                    markdown,
                    self.toc_anchors.get(page_index).map_or(&[], Vec::as_slice),
                ));
            }
        }
        let document = format!(
            "<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title></title><style>h1 {{ font-size: 1.75em; margin-top: 32px; margin-bottom: 12px; }} h2 {{ font-size: 1.5em; margin-top: 28px; margin-bottom: 10px; }} h3 {{ font-size: 1.28em; margin-top: 22px; margin-bottom: 8px; }} h4, h5, h6 {{ font-size: 1.12em; margin-top: 18px; margin-bottom: 6px; }}</style></head><body>{body}</body></html>"
        );
        rebook_html::parse_section(&document, descriptor, |_| None)
            .map_err(|error| PublicationError::InvalidPublication(error.to_string()))
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        let resource_url = href.resource_url();
        let path = resource_url.path();
        if let Some(resource) = self.resources.get(path) {
            let bytes = fs::read(&resource.path)
                .map_err(|_| PublicationError::ResourceNotFound(href.to_string()))?;
            let media_type = if resource.media_type.starts_with("image/") {
                resource.media_type.clone()
            } else {
                image_media_type(path).map_or_else(|| resource.media_type.clone(), str::to_owned)
            };
            return Ok(Resource {
                href: href.resource_url(),
                media_type,
                bytes: bytes.into(),
            });
        }
        self.inner.resource(href)
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        if self.resources.contains_key(href.resource_url().path()) {
            return Ok(None);
        }
        self.inner.raster_resource(href)
    }
}

#[cfg(test)]
fn markdown_to_html(markdown: &str) -> String {
    markdown_to_html_with_toc_anchors(markdown, &[])
}

fn markdown_to_html_with_toc_anchors(markdown: &str, anchors: &[OcrTocAnchor]) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    let normalized = normalize_ocr_math_delimiters(markdown);
    let events = Parser::new_ext(&normalized, options).collect::<Vec<_>>();
    let mut anchors_at_event = HashMap::<usize, String>::new();
    let mut matched = vec![false; anchors.len()];
    let mut cursor = 0;
    while cursor < events.len() {
        if matches!(events[cursor], Event::Start(Tag::Heading { .. })) {
            let start = cursor;
            let mut heading = String::new();
            cursor += 1;
            while cursor < events.len() && !matches!(events[cursor], Event::End(TagEnd::Heading(_)))
            {
                match &events[cursor] {
                    Event::Text(value)
                    | Event::Code(value)
                    | Event::InlineMath(value)
                    | Event::DisplayMath(value) => heading.push_str(value),
                    Event::SoftBreak | Event::HardBreak => heading.push(' '),
                    _ => {}
                }
                cursor += 1;
            }
            for (index, anchor) in anchors.iter().enumerate() {
                if !matched[index]
                    && toc_heading_matches(&heading, &anchor.label)
                    && let std::collections::hash_map::Entry::Vacant(entry) =
                        anchors_at_event.entry(start)
                {
                    entry.insert(anchor.fragment.clone());
                    matched[index] = true;
                }
            }
        }
        cursor += 1;
    }

    let parser = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| match event {
            Event::Start(Tag::Heading {
                level,
                id: _,
                classes,
                attrs,
            }) if anchors_at_event.contains_key(&index) => Event::Start(Tag::Heading {
                level,
                id: anchors_at_event
                    .get(&index)
                    .map(|fragment| CowStr::Boxed(fragment.clone().into_boxed_str())),
                classes,
                attrs,
            }),
            Event::Html(value) | Event::InlineHtml(value) => {
                Event::Html(CowStr::Boxed(sanitize_ocr_html(&value).into_boxed_str()))
            }
            Event::InlineMath(value) => Event::Html(CowStr::Boxed(
                format!(
                    r#"<span class="math math-inline">{}</span>"#,
                    escape_xml_text(&value)
                )
                .into_boxed_str(),
            )),
            Event::DisplayMath(value) => Event::Html(CowStr::Boxed(
                format!(
                    r#"<span class="math math-display">{}</span>"#,
                    escape_xml_text(&value)
                )
                .into_boxed_str(),
            )),
            other => other,
        });
    let mut output = String::new();
    html::push_html(&mut output, parser);
    output
}

fn markdown_heading_keys(markdown: &str) -> Vec<String> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    let normalized = normalize_ocr_math_delimiters(markdown);
    let events = Parser::new_ext(&normalized, options).collect::<Vec<_>>();
    let mut headings = Vec::new();
    let mut cursor = 0;
    while cursor < events.len() {
        if matches!(events[cursor], Event::Start(Tag::Heading { .. })) {
            let mut heading = String::new();
            cursor += 1;
            while cursor < events.len() && !matches!(events[cursor], Event::End(TagEnd::Heading(_)))
            {
                match &events[cursor] {
                    Event::Text(value) | Event::Code(value) => heading.push_str(value),
                    Event::SoftBreak | Event::HardBreak => heading.push(' '),
                    _ => {}
                }
                cursor += 1;
            }
            let heading = normalize_toc_heading(&heading);
            if !heading.is_empty() {
                headings.push(heading);
            }
        }
        cursor += 1;
    }
    headings
}

fn toc_heading_matches(heading: &str, label: &str) -> bool {
    let heading = normalize_toc_heading(heading);
    let label = normalize_toc_heading(label);
    normalized_toc_headings_match(&heading, &label)
}

fn normalized_toc_headings_match(heading: &str, label: &str) -> bool {
    heading == label
        || (heading.chars().count().min(label.chars().count()) >= 4
            && (heading.contains(&label) || label.contains(&heading)))
}

fn normalize_toc_heading(value: &str) -> String {
    let normalized = value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    let prefix_bytes = normalized
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_digit() || ('０'..='９').contains(character)
        })
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    let without_numeric_prefix = &normalized[prefix_bytes..];
    if prefix_bytes > 0 && !without_numeric_prefix.is_empty() {
        without_numeric_prefix.to_owned()
    } else {
        normalized
    }
}

pub(crate) fn correct_generated_toc_pages_from_ocr(
    book_id: &str,
    entries: &mut [GeneratedTocEntry],
) -> io::Result<bool> {
    let Some(document) = load_document(book_id)? else {
        return Ok(false);
    };
    Ok(correct_generated_toc_entries_from_pages(
        entries,
        &document.pages,
    ))
}

fn correct_generated_toc_entries_from_pages(
    entries: &mut [GeneratedTocEntry],
    pages: &[StoredOcrPage],
) -> bool {
    if pages.is_empty() {
        return false;
    }
    let headings = pages
        .iter()
        .map(|page| markdown_heading_keys(&page.markdown))
        .collect::<Vec<_>>();
    let toc_labels = entries
        .iter()
        .map(|entry| normalize_toc_heading(&entry.title))
        .filter(|label| !label.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let toc_like_pages = headings
        .iter()
        .map(|page_headings| {
            toc_labels
                .iter()
                .filter(|label| {
                    page_headings
                        .iter()
                        .any(|heading| normalized_toc_headings_match(heading, label))
                })
                .count()
                >= 3
        })
        .collect::<Vec<_>>();

    let descendant_labels = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            entries[index + 1..]
                .iter()
                .take_while(|candidate| candidate.depth > entry.depth)
                .map(|candidate| normalize_toc_heading(&candidate.title))
                .filter(|label| !label.is_empty())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let direct_matches = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let first_child_page = entries
                .get(index + 1)
                .filter(|candidate| candidate.depth > entry.depth)
                .and_then(|candidate| candidate.physical_page.checked_sub(1));
            entry.physical_page.checked_sub(1).and_then(|current_page| {
                find_matching_heading_page(
                    &headings,
                    current_page,
                    &entry.title,
                    &descendant_labels[index],
                    &toc_like_pages,
                    first_child_page,
                )
            })
        })
        .collect::<Vec<_>>();

    let mut matched = 0;
    let mut delta_counts = HashMap::<isize, usize>::new();
    for (entry, candidate) in entries.iter().zip(&direct_matches) {
        let Some(current_page) = entry.physical_page.checked_sub(1) else {
            continue;
        };
        let Some(candidate) = *candidate else {
            continue;
        };
        let (Ok(candidate), Ok(current_page)) =
            (isize::try_from(candidate), isize::try_from(current_page))
        else {
            continue;
        };
        matched += 1;
        *delta_counts.entry(candidate - current_page).or_default() += 1;
    }
    let common_delta = delta_counts
        .into_iter()
        .max_by_key(|(delta, support)| (*support, std::cmp::Reverse(delta.abs())))
        .filter(|(_, support)| *support >= 2 && *support * 2 >= matched)
        .map(|(delta, _)| delta);

    let mut changed = false;
    let mut previous_page = 1;
    for (entry, direct_match) in entries.iter_mut().zip(direct_matches) {
        let fallback_page = common_delta.and_then(|delta| {
            isize::try_from(entry.physical_page)
                .ok()
                .and_then(|page| page.checked_add(delta))
                .and_then(|page| usize::try_from(page).ok())
                .filter(|page| (1..=pages.len()).contains(page))
        });
        let corrected_page = direct_match
            .and_then(|page| page.checked_add(1))
            .or(fallback_page)
            .unwrap_or(entry.physical_page)
            .clamp(previous_page, pages.len());
        changed |= corrected_page != entry.physical_page;
        entry.physical_page = corrected_page;
        previous_page = corrected_page;
    }
    changed
}

fn normalize_ocr_math_delimiters(markdown: &str) -> String {
    let bytes = markdown.as_bytes();
    let mut output = String::with_capacity(markdown.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'$' || is_escaped_at(bytes, cursor) {
            let next = markdown[cursor..]
                .chars()
                .next()
                .map_or(bytes.len(), |character| cursor + character.len_utf8());
            output.push_str(&markdown[cursor..next]);
            cursor = next;
            continue;
        }
        let delimiter_len = if bytes.get(cursor + 1) == Some(&b'$') {
            2
        } else {
            1
        };
        let content_start = cursor + delimiter_len;
        let Some(close) = find_math_close(bytes, content_start, delimiter_len) else {
            output.push_str(&markdown[cursor..content_start]);
            cursor = content_start;
            continue;
        };
        let content = &markdown[content_start..close];
        let trimmed = content.trim();
        if trimmed.is_empty() {
            output.push_str(&markdown[cursor..close + delimiter_len]);
        } else {
            let delimiter = if delimiter_len == 2 { "$$" } else { "$" };
            output.push_str(delimiter);
            output.push_str(trimmed);
            output.push_str(delimiter);
        }
        cursor = close + delimiter_len;
    }
    output
}

fn find_math_close(bytes: &[u8], mut cursor: usize, delimiter_len: usize) -> Option<usize> {
    while cursor < bytes.len() {
        if bytes[cursor] == b'$' && !is_escaped_at(bytes, cursor) {
            if delimiter_len == 2 {
                if bytes.get(cursor + 1) == Some(&b'$') {
                    return Some(cursor);
                }
            } else if bytes.get(cursor + 1) != Some(&b'$')
                && (cursor == 0 || bytes.get(cursor - 1) != Some(&b'$'))
            {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

fn is_escaped_at(bytes: &[u8], index: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = index;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn escape_xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn decode_html_entities_once(value: &str) -> String {
    static ENTITY: OnceLock<Regex> = OnceLock::new();
    let regex = ENTITY.get_or_init(|| {
        Regex::new(r"&(?:#[xX][0-9A-Fa-f]+|#[0-9]+|[A-Za-z][A-Za-z0-9]+);")
            .expect("HTML entity regex should compile")
    });
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    for entity in regex.find_iter(value) {
        output.push_str(&value[cursor..entity.start()]);
        match unescape(entity.as_str()) {
            Ok(decoded) => output.push_str(&decoded),
            Err(_) => output.push_str(entity.as_str()),
        }
        cursor = entity.end();
    }
    output.push_str(&value[cursor..]);
    output
}

fn sanitize_ocr_html(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(open_offset) = value[cursor..].find('<') {
        let open = cursor + open_offset;
        output.push_str(&sanitize_ocr_html_text(&value[cursor..open]));
        let Some(close_offset) = value[open..].find('>') else {
            output.push_str(&sanitize_ocr_html_text(&value[open..]));
            return output;
        };
        let close = open + close_offset + 1;
        let tag = &value[open..close];
        if is_html_image_tag(tag) {
            if let Some(image) = sanitize_ocr_image_tag(tag) {
                output.push_str(&image);
            }
        } else if let Some(structure) = sanitize_ocr_structure_tag(tag) {
            output.push_str(&structure);
        } else {
            output.push(' ');
        }
        cursor = close;
    }
    output.push_str(&sanitize_ocr_html_text(&value[cursor..]));
    output
}

fn sanitize_ocr_html_text(value: &str) -> String {
    let decoded = decode_html_entities_once(value);
    let value = decoded.as_str();
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(relative) = value[cursor..].find('$') else {
            output.push_str(&escape_xml_text(&value[cursor..]));
            break;
        };
        let open = cursor + relative;
        output.push_str(&escape_xml_text(&value[cursor..open]));
        if is_escaped_at(bytes, open) {
            output.push('$');
            cursor = open + 1;
            continue;
        }
        let delimiter_len = usize::from(bytes.get(open + 1) == Some(&b'$')) + 1;
        let content_start = open + delimiter_len;
        let Some(close) = find_math_close(bytes, content_start, delimiter_len) else {
            output.push('$');
            cursor = open + 1;
            continue;
        };
        let latex = value[content_start..close].trim();
        if latex.is_empty() {
            output.push_str(&escape_xml_text(&value[open..close + delimiter_len]));
        } else {
            let class = if delimiter_len == 2 {
                "math math-display"
            } else {
                "math math-inline"
            };
            write!(
                output,
                r#"<span class="{class}">{}</span>"#,
                escape_xml_text(latex)
            )
            .expect("writing to a String should not fail");
        }
        cursor = close + delimiter_len;
    }
    output
}

fn is_html_image_tag(tag: &str) -> bool {
    let name = tag.strip_prefix('<').unwrap_or(tag).trim_start().as_bytes();
    name.get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"img"))
        && name
            .get(3)
            .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'))
}

fn sanitize_ocr_image_tag(tag: &str) -> Option<String> {
    let source = html_attribute(tag, "src")?;
    let source = safe_ocr_image_source(&source)?;
    let mut output = format!(r#"<img src="{}""#, escape_xml_attribute(&source));
    if let Some(alt) = html_attribute(tag, "alt") {
        write!(output, r#" alt="{}""#, escape_xml_attribute(&alt)).ok()?;
    }
    for name in ["width", "height"] {
        if let Some(value) = html_attribute(tag, name)
            .as_deref()
            .and_then(safe_image_dimension)
        {
            write!(output, r#" {name}="{}""#, escape_xml_attribute(&value)).ok()?;
        }
    }
    output.push_str(" />");
    Some(output)
}

fn sanitize_ocr_structure_tag(tag: &str) -> Option<String> {
    let inner = tag.strip_prefix('<')?.strip_suffix('>')?.trim();
    let (closing, inner) = inner
        .strip_prefix('/')
        .map_or((false, inner), |value| (true, value.trim_start()));
    let name_end = inner
        .find(|character: char| character.is_ascii_whitespace() || character == '/')
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    if !matches!(
        name.as_str(),
        "div" | "table" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" | "caption" | "br"
    ) {
        return None;
    }
    if closing {
        return (name != "br").then(|| format!("</{name}>"));
    }
    if name == "br" {
        return Some("<br />".into());
    }
    let mut output = format!("<{name}");
    if matches!(name.as_str(), "td" | "th") {
        for attribute in ["rowspan", "colspan"] {
            if let Some(span) = html_attribute(tag, attribute).and_then(|value| {
                value
                    .trim()
                    .parse::<u16>()
                    .ok()
                    .filter(|value| (1..=64).contains(value))
            }) {
                write!(output, r#" {attribute}="{span}""#)
                    .expect("writing to a String should not fail");
            }
        }
    }
    if matches!(name.as_str(), "div" | "td" | "th")
        && let Some(align) = safe_ocr_text_align(tag)
    {
        write!(output, r#" style="text-align:{align}""#)
            .expect("writing to a String should not fail");
    }
    output.push('>');
    Some(output)
}

fn safe_ocr_text_align(tag: &str) -> Option<String> {
    html_attribute(tag, "style")
        .and_then(|style| {
            style.split(';').find_map(|declaration| {
                let (property, value) = declaration.split_once(':')?;
                property
                    .trim()
                    .eq_ignore_ascii_case("text-align")
                    .then(|| value.trim().to_ascii_lowercase())
            })
        })
        .or_else(|| html_attribute(tag, "align").map(|value| value.trim().to_ascii_lowercase()))
        .filter(|value| {
            matches!(
                value.as_str(),
                "left" | "start" | "center" | "right" | "end"
            )
        })
}

fn html_attribute(tag: &str, expected: &str) -> Option<String> {
    static ATTRIBUTE: OnceLock<Regex> = OnceLock::new();
    let regex = ATTRIBUTE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(src|alt|width|height|rowspan|colspan|style|align)\s*=\s*(?:"([^"]*)"|'([^']*)')"#,
        )
        .expect("OCR HTML attribute regex should compile")
    });
    regex.captures_iter(tag).find_map(|capture| {
        capture[1].eq_ignore_ascii_case(expected).then(|| {
            capture
                .get(2)
                .or_else(|| capture.get(3))
                .map_or(String::new(), |value| {
                    decode_html_entities_once(value.as_str())
                })
        })
    })
}

fn safe_ocr_image_source(source: &str) -> Option<String> {
    let normalized = source.trim().replace('\\', "/");
    let resource = normalized
        .strip_prefix("../")
        .or_else(|| normalized.strip_prefix("./"))
        .unwrap_or(&normalized);
    let file_name = resource.strip_prefix("OcrResources/")?;
    if file_name.is_empty()
        || file_name.contains('/')
        || !safe_archive_name(resource)
        || !file_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return None;
    }
    Some(format!("../OcrResources/{file_name}"))
}

fn safe_image_dimension(value: &str) -> Option<String> {
    let value = value.trim();
    let (number, maximum) = value
        .strip_suffix('%')
        .map_or((value, 100_000.0), |number| (number, 100.0));
    number
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|number| number.is_finite() && *number > 0.0 && *number <= maximum)
        .map(|_| value.to_owned())
}

fn escape_xml_attribute(value: &str) -> String {
    escape_xml_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn safe_archive_name(name: &str) -> bool {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains(':')
        || normalized.contains('\0')
    {
        return false;
    }

    normalized
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn image_media_type(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("webp") => Some("image/webp"),
        Some("gif") => Some("image/gif"),
        Some("bmp") => Some("image/bmp"),
        _ => None,
    }
}

fn extension_for_media(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/bmp" => Some("bmp"),
        _ => None,
    }
}

fn api_code(value: &Value) -> Option<i64> {
    value
        .get("code")
        .or_else(|| value.get("errorCode"))
        .and_then(Value::as_i64)
}

fn api_error(provider: &str, status: u16, value: &Value) -> String {
    let message = value
        .get("msg")
        .or_else(|| value.get("errorMsg"))
        .or_else(|| value.pointer("/data/errorMsg"))
        .and_then(Value::as_str)
        .unwrap_or("请求失败");
    format!("{provider} 请求失败（HTTP {status}）：{message}")
}

fn value_as_usize(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| value.as_str()?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ocr_documents_default_to_no_special_page_roles() {
        let document: StoredPdfOcrDocument = serde_json::from_value(json!({
            "version": PDF_OCR_VERSION,
            "book_id": "legacy",
            "provider": "paddle-ocr",
            "model": "legacy-model",
            "view_mode": "reflow",
            "pages": [{"markdown": "legacy OCR text"}],
            "resources": []
        }))
        .unwrap();

        assert!(document.page_roles.is_empty());
        assert_eq!(document.pages[0].markdown, "legacy OCR text");
    }

    #[test]
    fn generated_toc_uses_repeated_ocr_heading_offset_instead_of_toc_mentions() {
        let mut pages = vec![StoredOcrPage::default(); 40];
        pages[6].markdown =
            "## 1 书籍的过去、现在和未来\n\n## 2 一本书的诞生\n\n## 3 设计方法".into();
        pages[9].markdown = "## 书籍的过去、现在和未来".into();
        pages[16].markdown = "## 一本书的诞生".into();
        pages[26].markdown = "## 设计方法".into();
        let entry = |title: &str, printed_page: usize| GeneratedTocEntry {
            depth: 0,
            title: title.into(),
            printed_page: printed_page.to_string(),
            physical_page: printed_page,
            confidence: 0.9,
        };
        let mut entries = vec![
            entry("1 书籍的过去、现在和未来", 6),
            entry("2 一本书的诞生", 13),
            entry("3 设计方法", 23),
        ];

        assert!(correct_generated_toc_entries_from_pages(
            &mut entries,
            &pages
        ));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>(),
            vec![10, 17, 27]
        );
    }

    #[test]
    fn generated_toc_prefers_a_parent_heading_before_its_child_heading_page() {
        let mut pages = vec![StoredOcrPage::default(); 48];
        pages[32].markdown = "## Ⅱ The book designer's palette 书籍设计师的画板".into();
        pages[33].markdown = "## 书籍设计师的画板\n\n## 版式\n\n正文".into();
        let entry = |depth, title: &str| GeneratedTocEntry {
            depth,
            title: title.into(),
            printed_page: "30".into(),
            physical_page: 34,
            confidence: 0.9,
        };
        let mut entries = vec![entry(0, "书籍设计师的画板"), entry(1, "4 版式")];

        assert!(correct_generated_toc_entries_from_pages(
            &mut entries,
            &pages
        ));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>(),
            vec![33, 34]
        );
    }

    #[test]
    fn generated_toc_does_not_move_a_parent_after_its_first_child() {
        let mut pages = vec![StoredOcrPage::default(); 260];
        pages[243].markdown = "## 文稿格式".into();
        pages[252].markdown = "## 延伸阅读".into();
        pages[253].markdown = "## 附录".into();
        let entry = |depth, title: &str, physical_page: usize| GeneratedTocEntry {
            depth,
            title: title.into(),
            printed_page: physical_page.to_string(),
            physical_page,
            confidence: 0.9,
        };
        let mut entries = vec![
            entry(0, "附录", 244),
            entry(1, "文稿格式", 244),
            entry(1, "延伸阅读", 253),
        ];

        assert!(!correct_generated_toc_entries_from_pages(
            &mut entries,
            &pages
        ));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>(),
            vec![244, 244, 253]
        );
    }

    #[test]
    #[ignore = "diagnoses the latest local PDF OCR/generated TOC cache"]
    fn diagnose_latest_cached_generated_toc_correction() {
        let project = ProjectDirs::from("com", "Rebook", "Rebook").unwrap();
        let root = project.data_local_dir().join(PDF_OCR_DIRECTORY);
        let latest = std::env::var("TORTO_DIAG_BOOK_ID").ok().unwrap_or_else(|| {
            fs::read_dir(root)
                .unwrap()
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let path = entry.path().join(DOCUMENT_FILE);
                    let modified = fs::metadata(&path).ok()?.modified().ok()?;
                    Some((modified, entry.file_name().to_string_lossy().into_owned()))
                })
                .max_by_key(|(modified, _)| *modified)
                .unwrap()
                .1
        });
        let mut draft = crate::generated_toc::load(&latest).unwrap().unwrap();
        let changed = correct_generated_toc_pages_from_ocr(&latest, &mut draft.entries).unwrap();
        println!(
            "book={latest} changed={changed} pages={:?}",
            draft
                .entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>()
        );
    }

    struct StubBookSource {
        book: Book,
    }

    impl BookSource for StubBookSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            Err(PublicationError::ResourceNotFound("stub".into()))
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    #[test]
    fn pdf_ocr_source_switches_layout_without_reopening_the_publication() {
        let book = |layout| Book {
            id: rebook_publication::PublicationId::new("switch-test").unwrap(),
            metadata: rebook_publication::Metadata {
                layout,
                ..rebook_publication::Metadata::default()
            },
            cover: None,
            sections: Vec::new(),
            table_of_contents: Vec::new(),
        };
        let original: Arc<dyn BookSource> = Arc::new(StubBookSource {
            book: book(RenditionLayout::PrePaginated),
        });
        let reflow: Arc<dyn BookSource> = Arc::new(StubBookSource {
            book: book(RenditionLayout::Reflowable),
        });
        let source =
            PdfOcrSourceController::new(original, reflow, Vec::new(), PdfOcrViewMode::Original);

        assert_eq!(source.book().metadata.layout, RenditionLayout::PrePaginated);
        assert!(!source.is_reflow_enabled());
        source.set_mode(PdfOcrViewMode::Reflow);
        assert_eq!(source.book().metadata.layout, RenditionLayout::Reflowable);
        assert!(source.is_reflow_enabled());
        assert_eq!(
            source.original_source().book().metadata.layout,
            RenditionLayout::PrePaginated
        );
        source.set_mode(PdfOcrViewMode::Original);
        assert_eq!(source.book().metadata.layout, RenditionLayout::PrePaginated);
        assert!(!source.is_reflow_enabled());
    }

    #[test]
    fn special_pdf_pages_render_the_original_page_image_instead_of_ocr_text() {
        struct SpecialPageSource {
            book: Book,
        }

        impl BookSource for SpecialPageSource {
            fn book(&self) -> &Book {
                &self.book
            }

            fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
                let descriptor = self.book.sections.get(index).ok_or_else(|| {
                    PublicationError::ResourceNotFound(format!("section {index}"))
                })?;
                Ok(Section {
                    id: descriptor.id.clone(),
                    href: descriptor.href.clone(),
                    blocks: vec![rebook_publication::Block::Image(
                        rebook_publication::ImageBlock {
                            href: PublicationUrl::parse("Pages/page-00001.png")?,
                            alt: "PDF cover".into(),
                            style: rebook_publication::ImageStyle::default(),
                            source: None,
                            text_layer: None,
                        },
                    )],
                    anchors: Vec::new(),
                })
            }

            fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
                Err(PublicationError::ResourceNotFound(href.to_string()))
            }
        }

        let section = SpineItem {
            id: SpineItemId::new("page-1").unwrap(),
            href: PublicationUrl::parse("Text/page-1.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let source: Arc<dyn BookSource> = Arc::new(SpecialPageSource {
            book: Book {
                id: rebook_publication::PublicationId::new("special-page-test").unwrap(),
                metadata: rebook_publication::Metadata {
                    layout: RenditionLayout::PrePaginated,
                    ..rebook_publication::Metadata::default()
                },
                cover: None,
                sections: vec![section],
                table_of_contents: Vec::new(),
            },
        });
        let reflow = OcrReflowBookSource::new(
            source,
            StoredPdfOcrDocument {
                version: PDF_OCR_VERSION,
                book_id: "special-page-test".into(),
                provider: PdfOcrProviderKind::PaddleOcr,
                model: "test".into(),
                view_mode: PdfOcrViewMode::Reflow,
                pages: vec![StoredOcrPage {
                    markdown: "OCR text must remain cached but hidden in reflow.".into(),
                }],
                resources: Vec::new(),
                page_roles: vec![PdfOcrPageRoleAssignment {
                    physical_page: 1,
                    role: PdfOcrPageRole::Cover,
                }],
            },
            false,
        )
        .unwrap();

        assert_eq!(
            reflow.pages[0].markdown,
            "OCR text must remain cached but hidden in reflow."
        );
        let parsed = reflow.parse_section(0).unwrap();
        assert!(parsed.blocks.iter().any(|block| {
            matches!(block, rebook_publication::Block::Image(image) if image.href.path() == "Pages/page-00001.png" && image.text_layer.is_none())
        }));
        assert!(!parsed.blocks.iter().any(|block| {
            matches!(block, rebook_publication::Block::Text(text) if crate::plugins::text_block_text(text).contains("OCR text must remain cached"))
        }));
    }

    #[test]
    fn content_reflow_joins_only_safe_cross_page_paragraphs() {
        let mut pages = vec![
            StoredOcrPage {
                markdown: "作为一个物理学研究生，我担任".into(),
            },
            StoredOcrPage {
                markdown: "课程助教。\n\n下一段。".into(),
            },
            StoredOcrPage {
                markdown: "完整句子。".into(),
            },
            StoredOcrPage {
                markdown: "# 新标题".into(),
            },
        ];
        reflow_ocr_page_boundaries(&mut pages, &HashMap::new());
        assert_eq!(pages[0].markdown, "作为一个物理学研究生，我担任课程助教。");
        assert_eq!(pages[1].markdown, "下一段。");
        assert_eq!(pages[2].markdown, "完整句子。");
        assert_eq!(pages[3].markdown, "# 新标题");
    }

    #[test]
    fn missing_ocr_page_placeholders_are_hidden_but_real_content_is_preserved() {
        let mut pages = vec![
            StoredOcrPage {
                markdown: "## 第 7 页\n\n本页未识别到正文。".into(),
            },
            StoredOcrPage {
                markdown: "## 第 9 页\n\n本页尚未生成 OCR 正文。".into(),
            },
            StoredOcrPage {
                markdown: "正文中提到本页未识别到正文。".into(),
            },
        ];

        remove_missing_ocr_page_placeholders(&mut pages);

        assert!(pages[0].markdown.is_empty());
        assert!(pages[1].markdown.is_empty());
        assert_eq!(pages[2].markdown, "正文中提到本页未识别到正文。");
    }

    #[test]
    fn html_entities_are_decoded_once_without_dropping_unknown_entities() {
        assert_eq!(decode_html_entities_once("Newton&#x27;s"), "Newton's");
        assert_eq!(decode_html_entities_once("A &amp; B"), "A & B");
        assert_eq!(
            decode_html_entities_once("&#39; &#x27; &nbsp;"),
            "' ' \u{a0}"
        );
        assert_eq!(decode_html_entities_once("&unknown;"), "&unknown;");
        assert_eq!(decode_html_entities_once("&amp;#x27;"), "&#x27;");
    }

    #[test]
    fn ocr_html_table_text_decodes_character_entities() {
        let html = markdown_to_html(
            "<table><tr><th>Quantity</th></tr><tr><td>Newton&#x27;s constant &amp; value</td></tr></table>",
        );

        assert!(html.contains("Newton's constant &amp; value"), "{html}");
        assert!(!html.contains("&amp;#x27;"), "{html}");
    }

    #[test]
    fn ocr_markdown_text_and_tables_decode_character_entities() {
        let html = markdown_to_html(
            "# Newton&#x27;s laws\n\nA &amp; B\n\n| Name | Value |\n| --- | --- |\n| Newton&#39;s constant | 1&nbsp;unit |",
        );

        assert!(html.contains("Newton's laws"), "{html}");
        assert!(html.contains("A &amp; B"), "{html}");
        assert!(html.contains("Newton's constant"), "{html}");
        assert!(html.contains("1\u{a0}unit"), "{html}");
        assert!(!html.contains("&amp;#"), "{html}");
    }

    #[test]
    fn content_reflow_restores_english_page_break_hyphenation() {
        let mut pages = vec![
            StoredOcrPage {
                markdown: "An inter-".into(),
            },
            StoredOcrPage {
                markdown: "national example.\n\nAnother paragraph.".into(),
            },
        ];
        reflow_ocr_page_boundaries(&mut pages, &HashMap::new());
        assert_eq!(pages[0].markdown, "An international example.");
        assert_eq!(pages[1].markdown, "Another paragraph.");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn content_reflow_groups_physical_pages_by_top_level_toc() {
        let id = rebook_publication::PublicationId::new("continuous-ocr-test").unwrap();
        let page = |number: usize| SpineItem {
            id: SpineItemId::new(format!("page-{number}")).unwrap(),
            href: PublicationUrl::parse(&format!("Text/section-{number}.xhtml")).unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let source: Arc<dyn BookSource> = Arc::new(StubBookSource {
            book: Book {
                id,
                metadata: rebook_publication::Metadata {
                    layout: RenditionLayout::PrePaginated,
                    ..rebook_publication::Metadata::default()
                },
                cover: None,
                sections: vec![page(1), page(2), page(3)],
                table_of_contents: vec![
                    TocEntry {
                        label: "Chapter 1".into(),
                        href: Some(PublicationUrl::parse("Text/section-1.xhtml").unwrap()),
                        children: vec![TocEntry {
                            label: "Section 1.1".into(),
                            href: Some(PublicationUrl::parse("Text/section-2.xhtml").unwrap()),
                            children: Vec::new(),
                        }],
                    },
                    TocEntry {
                        label: "Chapter 2".into(),
                        href: Some(PublicationUrl::parse("Text/section-3.xhtml").unwrap()),
                        children: Vec::new(),
                    },
                ],
            },
        });
        let reflow = OcrReflowBookSource::new(
            source,
            StoredPdfOcrDocument {
                version: PDF_OCR_VERSION,
                book_id: "continuous-ocr-test".into(),
                provider: PdfOcrProviderKind::PaddleOcr,
                model: "test".into(),
                view_mode: PdfOcrViewMode::Reflow,
                pages: [
                    "First page.",
                    "## Section 1.1\n\nSecond page.",
                    "Third page.",
                ]
                .into_iter()
                .map(|markdown| StoredOcrPage {
                    markdown: markdown.into(),
                })
                .collect(),
                resources: Vec::new(),
                page_roles: Vec::new(),
            },
            true,
        )
        .unwrap();

        assert_eq!(reflow.book().sections.len(), 2);
        assert_eq!(reflow.page_ranges, vec![0..2, 2..3]);
        let first = reflow.parse_section(0).unwrap();
        let text = first
            .blocks
            .iter()
            .filter_map(|block| match block {
                rebook_publication::Block::Text(block) => Some(
                    block
                        .content
                        .iter()
                        .filter_map(|inline| match inline {
                            rebook_publication::Inline::Text(run) => Some(run.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains("First page."));
        assert!(text.contains("Second page."));
        assert!(
            first
                .blocks
                .iter()
                .all(|block| !matches!(block, rebook_publication::Block::PageBreak))
        );
        assert_eq!(
            first
                .anchors
                .iter()
                .map(|anchor| anchor.fragment.as_str())
                .collect::<Vec<_>>(),
            vec!["pdf-page-1", "pdf-page-2", "ocr-toc-0"]
        );
        assert_eq!(
            reflow.book().table_of_contents[0].children[0]
                .href
                .as_ref()
                .and_then(PublicationUrl::fragment),
            Some("ocr-toc-0")
        );
        assert_eq!(
            reflow.book().table_of_contents[1]
                .href
                .as_ref()
                .map(PublicationUrl::path),
            Some("Text/ocr-reflow-2.xhtml")
        );
    }

    #[test]
    fn content_reflow_targets_parent_and_child_headings_instead_of_empty_section_starts() {
        let id = rebook_publication::PublicationId::new("continuous-ocr-anchor-test").unwrap();
        let page = |number: usize| SpineItem {
            id: SpineItemId::new(format!("page-{number}")).unwrap(),
            href: PublicationUrl::parse(&format!("Text/section-{number}.xhtml")).unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let source: Arc<dyn BookSource> = Arc::new(StubBookSource {
            book: Book {
                id,
                metadata: rebook_publication::Metadata {
                    layout: RenditionLayout::PrePaginated,
                    ..rebook_publication::Metadata::default()
                },
                cover: None,
                sections: vec![page(1), page(2), page(3)],
                table_of_contents: vec![TocEntry {
                    label: "Part".into(),
                    href: Some(PublicationUrl::parse("Text/section-2.xhtml").unwrap()),
                    children: vec![TocEntry {
                        label: "1 Chapter".into(),
                        href: Some(PublicationUrl::parse("Text/section-2.xhtml").unwrap()),
                        children: Vec::new(),
                    }],
                }],
            },
        });
        let reflow = OcrReflowBookSource::new(
            source,
            StoredPdfOcrDocument {
                version: PDF_OCR_VERSION,
                book_id: "continuous-ocr-anchor-test".into(),
                provider: PdfOcrProviderKind::PaddleOcr,
                model: "test".into(),
                view_mode: PdfOcrViewMode::Reflow,
                pages: ["## Part", "## Part\n\n## Chapter\n\n正文", "后续正文"]
                    .into_iter()
                    .map(|markdown| StoredOcrPage {
                        markdown: markdown.into(),
                    })
                    .collect(),
                resources: Vec::new(),
                page_roles: Vec::new(),
            },
            true,
        )
        .unwrap();

        let parent = &reflow.book().table_of_contents[0];
        let child = &parent.children[0];
        assert_eq!(
            parent.href.as_ref().map(PublicationUrl::path),
            Some("Text/ocr-reflow-1.xhtml")
        );
        assert_eq!(
            parent.href.as_ref().and_then(PublicationUrl::fragment),
            Some("ocr-toc-0")
        );
        assert_eq!(
            child.href.as_ref().map(PublicationUrl::path),
            Some("Text/ocr-reflow-2.xhtml")
        );
        assert_eq!(
            child.href.as_ref().and_then(PublicationUrl::fragment),
            Some("ocr-toc-1")
        );
    }

    #[test]
    fn mineru_content_list_is_grouped_by_physical_page() {
        let value = json!([
            {"type":"title","text":"Chapter","text_level":1,"page_idx":0},
            {"type":"text","text":"First page","page_idx":0},
            {"type":"text","text":"Second page","page_idx":1}
        ]);
        let pages = mineru_pages_from_value(&value);
        assert_eq!(pages.len(), 2);
        assert!(pages[0].markdown.contains("# Chapter"));
        assert!(pages[1].markdown.contains("Second page"));
    }

    #[test]
    fn raw_html_tables_are_safely_reconstructed() {
        let html = markdown_to_html(
            r#"before<table onclick="alert(1)"><tr><td rowspan="2" colspan="3" style="color:red;text-align:center">$x^2$</td></tr></table>after"#,
        );
        assert!(html.contains("<table>"));
        assert!(html.contains(
            r#"<td rowspan="2" colspan="3" style="text-align:center"><span class="math math-inline">x^2</span></td>"#
        ));
        assert!(html.contains("before"));
        assert!(html.contains("after"));
        assert!(!html.contains("onclick"));
        assert!(!html.contains("color:red"));
    }

    #[test]
    fn sanitized_ocr_table_becomes_structured_reader_content() {
        let body = markdown_to_html(
            r#"<table><thead><tr><th colspan="2">Header</th></tr></thead><tbody><tr><td rowspan="2">A</td><td>$E=mc^2$</td></tr><tr><td>B</td></tr></tbody></table>"#,
        );
        let descriptor = rebook_publication::SpineItem {
            id: rebook_publication::SpineItemId::new("table-page").unwrap(),
            href: PublicationUrl::parse("Text/table-page.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let section = rebook_html::parse_section(
            &format!("<html><body>{body}</body></html>"),
            &descriptor,
            |_| None,
        )
        .unwrap();
        let table = section
            .blocks
            .iter()
            .find_map(|block| match block {
                rebook_publication::Block::Table(table) => Some(table),
                _ => None,
            })
            .expect("sanitized table should remain structured");

        assert_eq!(table.rows.len(), 3);
        assert!(table.rows[0].cells[0].header);
        assert_eq!(table.rows[0].cells[0].column_span, 2);
        assert_eq!(table.rows[1].cells[0].row_span, 2);
        assert!(table.rows[1].cells[1]
            .text
            .content
            .iter()
            .any(|inline| matches!(inline, rebook_publication::Inline::Math(math) if math.latex == "E=mc^2")));
    }

    #[test]
    fn centered_ocr_div_keeps_only_safe_alignment_and_reader_semantics() {
        let body = markdown_to_html(
            r#"<div id="caption" onclick="alert(1)" style="color:red; text-align: center; font-size:99px">Average path $\lambda$</div>"#,
        );
        assert!(body.contains(r#"<div style="text-align:center">"#));
        assert!(!body.contains("onclick"));
        assert!(!body.contains("color:red"));
        assert!(!body.contains("font-size"));

        let descriptor = rebook_publication::SpineItem {
            id: rebook_publication::SpineItemId::new("centered-caption").unwrap(),
            href: PublicationUrl::parse("Text/centered-caption.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let section = rebook_html::parse_section(
            &format!("<html><body>{body}</body></html>"),
            &descriptor,
            |_| None,
        )
        .unwrap();
        let caption = section
            .blocks
            .iter()
            .find_map(|block| match block {
                rebook_publication::Block::Text(text) => Some(text),
                _ => None,
            })
            .expect("centered OCR caption should become reader text");
        assert_eq!(
            caption.style.align,
            rebook_publication::TextAlignment::Center
        );
        assert!(caption.content.iter().any(
            |inline| matches!(inline, rebook_publication::Inline::Text(run) if run.text.contains("Average path"))
        ));
        assert!(caption
            .content
            .iter()
            .any(|inline| matches!(inline, rebook_publication::Inline::Math(math) if math.latex == r"\lambda")));
    }

    #[test]
    fn local_ocr_images_survive_html_sanitization() {
        let html = markdown_to_html(
            r#"<div style="text-align:center"><img src="../OcrResources/figure.jpg" alt="Figure" width="37%" /></div>
<img src="https://example.com/tracker.png" />"#,
        );

        assert!(
            html.contains(r#"<img src="../OcrResources/figure.jpg" alt="Figure" width="37%" />"#)
        );
        assert!(html.contains(r#"<div style="text-align:center">"#));
        assert!(!html.contains("example.com"));
    }

    #[test]
    fn archive_paths_cannot_escape_the_ocr_directory() {
        assert!(safe_archive_name("images/figure.png"));
        assert!(!safe_archive_name("../document.json"));
        assert!(!safe_archive_name(r"..\document.json"));
        assert!(!safe_archive_name("C:/document.json"));
        assert!(!safe_archive_name(r"C:\document.json"));
        assert!(!safe_archive_name("C:document.json"));
        assert!(!safe_archive_name("/document.json"));
        assert!(!safe_archive_name(r"\\server\share\document.json"));
    }

    #[test]
    fn paddle_vl_uses_vl_specific_document_options() {
        let payload = paddle_optional_payload("PaddleOCR-VL-1.6");
        assert_eq!(payload["useLayoutDetection"], true);
        assert_eq!(payload["prettifyMarkdown"], true);
        assert!(payload.get("useTableRecognition").is_none());
    }

    #[test]
    fn paddle_jobs_split_long_pdfs_into_hundred_page_ranges() {
        let chunks = paddle_page_chunks(250);
        assert_eq!(chunks.len(), 3);
        assert_eq!((chunks[0].start_page, chunks[0].end_page), (1, 100));
        assert_eq!((chunks[1].start_page, chunks[1].end_page), (101, 200));
        assert_eq!((chunks[2].start_page, chunks[2].end_page), (201, 250));
    }

    #[test]
    fn paddle_chunk_padding_keeps_physical_page_numbers() {
        let mut pages = vec![StoredOcrPage {
            markdown: "page 101".into(),
        }];
        normalize_page_range(&mut pages, 101, 103);
        assert_eq!(pages.len(), 3);
        assert!(pages[1].markdown.is_empty());
        assert!(pages[2].markdown.is_empty());
    }

    #[test]
    fn paddle_chunk_resources_do_not_overwrite_each_other() {
        let first = namespaced_resource_data(
            "images/figure.png",
            "pages-1-100",
            vec![1],
            "image/png".into(),
        );
        let second = namespaced_resource_data(
            "images/figure.png",
            "pages-101-200",
            vec![2],
            "image/png".into(),
        );
        assert_ne!(first.file_name, second.file_name);
        assert_ne!(first.href, second.href);
    }

    #[test]
    fn mineru_archive_image_uses_markdown_relative_path() {
        let resource = resource_from_bytes("result/book/images/figure.jpg", vec![1, 2, 3]);
        assert_eq!(resource.source, "images/figure.jpg");
        assert_eq!(resource.media_type, "image/jpeg");
    }

    #[test]
    fn cached_ocr_pages_become_reflowable_reader_sections() {
        let id = rebook_publication::PublicationId::new("ocr-test").unwrap();
        let section_id = rebook_publication::SpineItemId::new("page-1").unwrap();
        let href = PublicationUrl::parse("Text/section-1.xhtml").unwrap();
        let source: Arc<dyn BookSource> = Arc::new(StubBookSource {
            book: Book {
                id,
                metadata: rebook_publication::Metadata {
                    title: "Scanned book".into(),
                    layout: RenditionLayout::PrePaginated,
                    ..rebook_publication::Metadata::default()
                },
                cover: None,
                sections: vec![rebook_publication::SpineItem {
                    id: section_id.clone(),
                    href,
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
        });
        let reflow = OcrReflowBookSource::new(
            source,
            StoredPdfOcrDocument {
                version: PDF_OCR_VERSION,
                book_id: "ocr-test".into(),
                provider: PdfOcrProviderKind::PaddleOcr,
                model: "PaddleOCR-VL-1.6".into(),
                view_mode: PdfOcrViewMode::Reflow,
                pages: vec![StoredOcrPage {
                    markdown: "# 科学与工程中的洞察力\n\nReadable body with $ x_{1}=2x_{2} $.\n\n$$\\frac{a}{b}$$\n\n<div style=\"text-align:center\"><img src=\"../OcrResources/figure.jpg\" alt=\"Figure\" width=\"42%\" /></div>".into(),
                }],
                resources: vec![StoredOcrResource {
                    href: "OcrResources/figure.jpg".into(),
                    file_name: "figure.jpg".into(),
                    media_type: "application/octet-stream".into(),
                }],
                page_roles: Vec::new(),
            },
            false,
        )
        .unwrap();
        let section = reflow.parse_section(0).unwrap();
        assert_eq!(reflow.book().metadata.layout, RenditionLayout::Reflowable);
        assert_eq!(section.id, section_id);
        assert!(!section.blocks.is_empty());
        let rebook_publication::Block::Text(heading) = &section.blocks[0] else {
            panic!("OCR Markdown heading should remain a semantic text block");
        };
        assert_eq!(heading.kind, rebook_publication::TextBlockKind::Heading(1));
        let Some(rebook_publication::Inline::Text(title)) = heading.content.first() else {
            panic!("heading should contain styled text");
        };
        assert!((title.style.size_scale - 1.75).abs() < f32::EPSILON);
        let formulas = section
            .blocks
            .iter()
            .filter_map(|block| match block {
                rebook_publication::Block::Text(block) => Some(&block.content),
                _ => None,
            })
            .flatten()
            .filter_map(|inline| match inline {
                rebook_publication::Inline::Math(formula) => Some(formula),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(formulas.len(), 2);
        assert!(!formulas[0].display);
        assert!(formulas[1].display);
        let image = section
            .blocks
            .iter()
            .find_map(|block| match block {
                rebook_publication::Block::Image(image) => Some(image),
                _ => None,
            })
            .expect("cached OCR image should become a reader image block");
        assert_eq!(image.href.path(), "OcrResources/figure.jpg");
        assert_eq!(
            image.style.width,
            Some(rebook_publication::ImageLength::Fraction(0.42))
        );
    }

    #[test]
    #[ignore = "diagnoses images in the latest local PDF OCR cache"]
    fn diagnose_latest_cached_ocr_images() {
        let project = ProjectDirs::from("com", "Rebook", "Rebook").unwrap();
        let root = project.data_local_dir().join(PDF_OCR_DIRECTORY);
        let latest = fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path().join(DOCUMENT_FILE);
                let modified = fs::metadata(&path).ok()?.modified().ok()?;
                Some((modified, path))
            })
            .max_by_key(|(modified, _)| *modified)
            .unwrap()
            .1;
        let document: StoredPdfOcrDocument =
            serde_json::from_slice(&fs::read(&latest).unwrap()).unwrap();
        let resources = document
            .resources
            .iter()
            .map(|resource| (resource.href.as_str(), resource))
            .collect::<HashMap<_, _>>();
        let resource_directory = latest.parent().unwrap().join("resources");
        let mut total = 0;
        let mut failures = Vec::new();
        for (page_index, page) in document.pages.iter().enumerate() {
            let html = markdown_to_html(&page.markdown);
            let descriptor = rebook_publication::SpineItem {
                id: rebook_publication::SpineItemId::new(format!("page-{page_index}")).unwrap(),
                href: PublicationUrl::parse(&format!("Text/section-{}.xhtml", page_index + 1))
                    .unwrap(),
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: Vec::new(),
            };
            let section = rebook_html::parse_section(
                &format!("<html><body>{html}</body></html>"),
                &descriptor,
                |_| None,
            )
            .unwrap();
            for image in section.blocks.iter().filter_map(|block| match block {
                rebook_publication::Block::Image(image) => Some(image),
                _ => None,
            }) {
                total += 1;
                let Some(resource) = resources.get(image.href.path()) else {
                    failures.push(format!(
                        "page {} references missing {}",
                        page_index + 1,
                        image.href.path()
                    ));
                    continue;
                };
                let path = resource_directory.join(&resource.file_name);
                match fs::read(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| {
                        image::load_from_memory(&bytes)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    }) {
                    Ok(()) => {}
                    Err(error) => failures.push(format!(
                        "page {} failed to decode {}: {error}",
                        page_index + 1,
                        path.display()
                    )),
                }
            }
        }
        println!("IMAGES={total} FAILURES={}", failures.len());
        assert!(total > 0, "latest OCR cache did not produce image blocks");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "diagnoses tables in the latest local PDF OCR cache"]
    fn diagnose_latest_cached_ocr_tables() {
        let project = ProjectDirs::from("com", "Rebook", "Rebook").unwrap();
        let root = project.data_local_dir().join(PDF_OCR_DIRECTORY);
        let latest = fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path().join(DOCUMENT_FILE);
                let modified = fs::metadata(&path).ok()?.modified().ok()?;
                Some((modified, path))
            })
            .max_by_key(|(modified, _)| *modified)
            .unwrap()
            .1;
        let document: StoredPdfOcrDocument =
            serde_json::from_slice(&fs::read(&latest).unwrap()).unwrap();
        let mut tables = 0;
        let mut cells = 0;
        let mut merged_cells = 0;
        let mut formulas = 0;
        for (page_index, page) in document.pages.iter().enumerate() {
            let html = markdown_to_html(&page.markdown);
            let descriptor = rebook_publication::SpineItem {
                id: rebook_publication::SpineItemId::new(format!("page-{page_index}")).unwrap(),
                href: PublicationUrl::parse(&format!("Text/page-{page_index}.xhtml")).unwrap(),
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: Vec::new(),
            };
            let section = rebook_html::parse_section(
                &format!("<html><body>{html}</body></html>"),
                &descriptor,
                |_| None,
            )
            .unwrap();
            for table in section.blocks.iter().filter_map(|block| match block {
                rebook_publication::Block::Table(table) => Some(table),
                _ => None,
            }) {
                tables += 1;
                for cell in table.rows.iter().flat_map(|row| &row.cells) {
                    cells += 1;
                    merged_cells += usize::from(cell.column_span > 1 || cell.row_span > 1);
                    formulas += cell
                        .text
                        .content
                        .iter()
                        .filter(|inline| matches!(inline, rebook_publication::Inline::Math(_)))
                        .count();
                }
            }
        }
        println!("TABLES={tables} CELLS={cells} MERGED_CELLS={merged_cells} FORMULAS={formulas}");
        assert!(tables > 0, "latest OCR cache did not produce table blocks");
        assert!(cells > 0, "latest OCR cache tables did not contain cells");
    }

    #[test]
    #[ignore = "diagnoses formulas in the latest local PDF OCR cache"]
    #[allow(
        clippy::too_many_lines,
        reason = "full-cache diagnostic keeps formula context beside each validation step"
    )]
    fn diagnose_latest_cached_ocr_formulas() {
        let project = ProjectDirs::from("com", "Rebook", "Rebook").unwrap();
        let root = project.data_local_dir().join(PDF_OCR_DIRECTORY);
        let latest = fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path().join(DOCUMENT_FILE);
                let modified = fs::metadata(&path).ok()?.modified().ok()?;
                Some((modified, path))
            })
            .max_by_key(|(modified, _)| *modified)
            .unwrap()
            .1;
        let document: StoredPdfOcrDocument =
            serde_json::from_slice(&fs::read(latest).unwrap()).unwrap();
        let mut svg_options = resvg::usvg::Options::default();
        svg_options.fontdb_mut().load_system_fonts();
        let mut total = 0;
        let mut failures = Vec::new();
        for (page_index, page) in document.pages.iter().enumerate() {
            let html = markdown_to_html(&page.markdown);
            let descriptor = rebook_publication::SpineItem {
                id: rebook_publication::SpineItemId::new(format!("page-{page_index}")).unwrap(),
                href: PublicationUrl::parse(&format!("Text/page-{page_index}.xhtml")).unwrap(),
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: Vec::new(),
            };
            let section = rebook_html::parse_section(
                &format!("<html><body>{html}</body></html>"),
                &descriptor,
                |_| None,
            )
            .unwrap();
            for formula in section
                .blocks
                .iter()
                .filter_map(|block| match block {
                    rebook_publication::Block::Text(block) => Some(&block.content),
                    _ => None,
                })
                .flatten()
                .filter_map(|inline| match inline {
                    rebook_publication::Inline::Math(formula) => Some(formula),
                    _ => None,
                })
            {
                total += 1;
                match rebook_math::math::render_math(
                    &formula.latex,
                    16.0,
                    "#262624",
                    formula.display,
                ) {
                    Ok(rendered) => {
                        if rendered.svg_fragment.contains("PARSE ERROR") {
                            let parse_errors = rendered
                                .svg_fragment
                                .split("[PARSE ERROR: ")
                                .skip(1)
                                .filter_map(|tail| tail.split(']').next())
                                .collect::<Vec<_>>()
                                .join(" | ");
                            failures.push((
                                page_index + 1,
                                formula.latex.clone(),
                                format!("partial parser error: {parse_errors}"),
                            ));
                            continue;
                        }
                        let width = rendered.width.max(1.0);
                        let height = (rendered.ascent + rendered.descent).max(1.0);
                        let svg = format!(
                            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 {} {} {}">{}</svg>"#,
                            -rendered.ascent, width, height, rendered.svg_fragment
                        );
                        if let Err(error) =
                            resvg::usvg::Tree::from_data(svg.as_bytes(), &svg_options)
                        {
                            failures.push((
                                page_index + 1,
                                formula.latex.clone(),
                                format!("SVG parse error: {error}"),
                            ));
                        }
                    }
                    Err(error) => {
                        failures.push((page_index + 1, formula.latex.clone(), error));
                    }
                }
            }
        }
        println!("FORMULAS={total} FAILURES={}", failures.len());
        let mut categories = BTreeMap::new();
        for (_, _, error) in &failures {
            *categories.entry(error.as_str()).or_insert(0_usize) += 1;
        }
        for (error, count) in categories {
            println!("CATEGORY={count} ERROR={error}");
        }
        for (page, latex, error) in failures.iter().take(100) {
            println!("PAGE={page} LATEX={latex:?} ERROR={error}");
        }
    }

    #[test]
    #[ignore = "diagnoses generated TOC positions in the latest local PDF OCR cache"]
    fn diagnose_latest_cached_ocr_toc_positions() {
        let project = ProjectDirs::from("com", "Rebook", "Rebook").unwrap();
        let root = project.data_local_dir().join(PDF_OCR_DIRECTORY);
        let latest = fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path().join(DOCUMENT_FILE);
                let modified = fs::metadata(&path).ok()?.modified().ok()?;
                Some((modified, path))
            })
            .max_by_key(|(modified, _)| *modified)
            .unwrap()
            .1;
        let document: StoredPdfOcrDocument =
            serde_json::from_slice(&fs::read(&latest).unwrap()).unwrap();
        let page = |index: usize| SpineItem {
            id: SpineItemId::new(format!("page-{}", index + 1)).unwrap(),
            href: PublicationUrl::parse(&format!("Text/page-{}.xhtml", index + 1)).unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let source: Arc<dyn BookSource> = Arc::new(StubBookSource {
            book: Book {
                id: rebook_publication::PublicationId::new(document.book_id.clone()).unwrap(),
                metadata: rebook_publication::Metadata {
                    layout: RenditionLayout::PrePaginated,
                    ..rebook_publication::Metadata::default()
                },
                cover: None,
                sections: (0..document.pages.len()).map(page).collect(),
                table_of_contents: Vec::new(),
            },
        });
        let source = crate::generated_toc::load_source(source).unwrap();
        let reflow = OcrReflowBookSource::new(source, document, true).unwrap();
        let source: Arc<dyn BookSource> = Arc::new(reflow);
        let rewrite = Arc::new(crate::plugins::rewrite::RewriteBookSource::new(source));
        let source: Arc<dyn BookSource> =
            Arc::new(crate::plugins::translation::TranslationBookSource::new(
                rewrite,
                crate::plugins::TranslationMode::default(),
            ));
        let viewport = rebook_layout::LayoutViewport::new(940, 780).unwrap();
        let style = rebook_layout::ReaderStyle {
            spread: rebook_layout::SpreadMode::Double,
            ..rebook_layout::ReaderStyle::default()
        };
        let mut reader = rebook_reader::ReaderSession::open(source, viewport, style).unwrap();
        let items = reader
            .toc_items()
            .iter()
            .filter(|item| item.label.starts_with('9') || item.label.starts_with("第9章"))
            .map(|item| (item.label.clone(), item.target.clone().unwrap()))
            .collect::<Vec<_>>();
        for (label, target) in items {
            let before = reader.position_for_href(&target);
            let result = reader.go_to_href(&target).unwrap();
            let active = result
                .snapshot
                .active_toc_id
                .as_deref()
                .and_then(|id| reader.toc_items().iter().find(|item| item.id == id))
                .map(|item| item.label.as_str());
            println!("TOC={label:?} TARGET={before:?} ACTIVE={active:?}");
        }
    }
}
