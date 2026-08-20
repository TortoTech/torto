use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use image::imageops::FilterType;
use image::{DynamicImage, Rgb, RgbImage};
use rebook_publication::BookSource;
use reqwest::Client;
use serde::{Deserialize, Deserializer};
use serde_json::json;
use tokio::task::JoinSet;

#[cfg(test)]
use super::pdf_vision::is_retryable_vision_response_error;
use super::pdf_vision::{
    PAGE_IMAGE_MAX_DIMENSION, encode_jpeg_data_url, parse_json_value, render_page_data_url,
    render_page_image, request_vision_json,
};
use super::{AiProvider, PluginSettings};
use super::{PdfOcrPageRole, PdfOcrPageRoleAssignment};
use crate::generated_metadata::GeneratedPdfMetadata;
use crate::generated_toc::{GeneratedTocDraft, GeneratedTocEntry};

const SCAN_BATCH_SIZE: usize = 8;
const SCAN_PAGE_LIMIT: usize = 20;
const SCAN_IMAGE_MAX_DIMENSION: u32 = 560;
const EXTRACTION_BATCH_SIZE: usize = 1;
const VISION_REQUEST_CONCURRENCY: usize = 4;
const METADATA_PAGE_LIMIT: usize = 6;
const PAGE_VERIFICATION_BATCH_SIZE: usize = 2;
const PAGE_VERIFICATION_RADIUS: usize = 2;

#[derive(Debug, Deserialize)]
struct ScanResponse {
    #[serde(default)]
    p: Vec<ScannedPage>,
    #[serde(default)]
    m: Option<MetadataResponse>,
}

#[derive(Debug, Deserialize)]
struct ScannedPage {
    i: usize,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    k: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    n: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    h: String,
}

fn scanned_page_role(kind: &str) -> Option<PdfOcrPageRole> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "cover" => Some(PdfOcrPageRole::Cover),
        "title_page" | "title-page" => Some(PdfOcrPageRole::TitlePage),
        "back_cover" | "back-cover" => Some(PdfOcrPageRole::BackCover),
        _ => None,
    }
}

fn page_role_is_plausible(role: PdfOcrPageRole, physical_page: usize, page_count: usize) -> bool {
    match role {
        PdfOcrPageRole::Cover => physical_page == 1,
        PdfOcrPageRole::TitlePage => physical_page <= SCAN_PAGE_LIMIT.min(page_count),
        PdfOcrPageRole::BackCover => physical_page > page_count.saturating_sub(SCAN_BATCH_SIZE),
    }
}

#[derive(Debug, Deserialize)]
struct ExtractionResponse {
    #[serde(default)]
    e: Vec<ExtractedEntry>,
}

#[derive(Debug, Deserialize)]
struct PageVerificationResponse {
    #[serde(default)]
    r: Vec<PageVerificationChoice>,
}

#[derive(Debug, Deserialize)]
struct PageVerificationChoice {
    id: usize,
    #[serde(default)]
    i: Option<usize>,
}

#[derive(Clone, Debug)]
struct PageVerificationTarget {
    entry_index: usize,
    title: String,
    candidates: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct MetadataResponse {
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    t: String,
    #[serde(default)]
    a: MetadataAuthors,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum MetadataAuthors {
    Many(Vec<String>),
    One(String),
    #[default]
    Missing,
}

impl MetadataAuthors {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::Many(authors) => authors,
            Self::One(author) => vec![author],
            Self::Missing => Vec::new(),
        }
    }
}

impl MetadataResponse {
    fn into_generated(self, provider_name: &str, model: &str) -> Option<GeneratedPdfMetadata> {
        let title = self.t.trim().to_owned();
        let mut authors = self
            .a
            .into_vec()
            .into_iter()
            .map(|author| author.trim().to_owned())
            .filter(|author| !author.is_empty())
            .collect::<Vec<_>>();
        authors.dedup();
        (!title.is_empty() || !authors.is_empty()).then(|| GeneratedPdfMetadata {
            title,
            authors,
            provider_name: provider_name.to_owned(),
            model: model.to_owned(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct ExtractedEntry {
    #[serde(default)]
    d: usize,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    t: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    n: String,
    #[serde(default)]
    c: Option<f32>,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone)]
struct PageNumberAnchor {
    physical_page: usize,
    printed_page: String,
}

#[derive(Clone, Debug)]
struct PageHeadingAnchor {
    physical_page: usize,
    title: String,
}

pub(crate) async fn generate_pdf_metadata(
    source: Arc<dyn BookSource>,
    settings: PluginSettings,
) -> Result<GeneratedPdfMetadata, String> {
    let (provider, model) = settings.ocr_endpoint()?;
    let provider = provider.clone();
    let model = model.to_owned();
    let client = Client::builder()
        .timeout(Duration::from_secs(150))
        .build()
        .map_err(|error| format!("无法创建 AI 请求客户端：{error}"))?;
    let page_count = source.book().sections.len();
    if page_count == 0 {
        return Err("PDF 没有可识别的页面".into());
    }
    let page_indices = (0..page_count.min(METADATA_PAGE_LIMIT)).collect::<Vec<_>>();
    let content = vec![
        json!({
            "type": "text",
            "text": "Identify the book's bibliographic metadata from these opening PDF pages. Prefer the title page over covers, running headers, advertisements, series names, subtitles presented as endorsements, and filenames. Preserve the original language and spelling. Return compact JSON only: {\"t\":\"full book title\",\"a\":[\"author name\"]}. Use an empty string or array only when the value is not visible."
        }),
        json!({
            "type": "image_url",
            "image_url": {
                "url": render_contact_sheet(source.as_ref(), &page_indices, 900)?
            }
        }),
    ];
    let value = request_vision_json(&client, &provider, &model, content).await?;
    let response: MetadataResponse = parse_json_value(&value)?;
    response
        .into_generated(&provider.name, &model)
        .ok_or_else(|| "未能从 PDF 前置页面识别出标题或作者".into())
}

pub(crate) struct PdfMetadataExtraction {
    pub(crate) toc: Option<GeneratedTocDraft>,
    pub(crate) toc_error: Option<String>,
    pub(crate) metadata: Option<GeneratedPdfMetadata>,
    pub(crate) page_roles: Vec<PdfOcrPageRoleAssignment>,
}

#[cfg(test)]
pub(crate) async fn generate_pdf_toc<F>(
    source: Arc<dyn BookSource>,
    settings: PluginSettings,
    on_progress: F,
) -> Result<GeneratedTocDraft, String>
where
    F: FnMut(String) + Send,
{
    let result = extract_pdf_metadata(source, settings, true, true, false, on_progress).await?;
    result
        .toc
        .ok_or_else(|| result.toc_error.unwrap_or_else(|| "目录识别失败".into()))
}

#[allow(
    clippy::too_many_lines,
    reason = "PDF metadata extraction keeps the shared scan result and TOC fallback together"
)]
pub(crate) async fn extract_pdf_metadata<F>(
    source: Arc<dyn BookSource>,
    settings: PluginSettings,
    need_toc: bool,
    need_page_roles: bool,
    need_book_metadata: bool,
    mut on_progress: F,
) -> Result<PdfMetadataExtraction, String>
where
    F: FnMut(String) + Send,
{
    if !need_toc && !need_page_roles {
        let metadata = if need_book_metadata {
            Some(generate_pdf_metadata(source, settings).await?)
        } else {
            None
        };
        return Ok(PdfMetadataExtraction {
            toc: None,
            toc_error: None,
            metadata,
            page_roles: Vec::new(),
        });
    }
    let (provider, model) = settings.ocr_endpoint()?;
    let provider = provider.clone();
    let model = model.to_owned();
    let client = Client::builder()
        .timeout(Duration::from_secs(150))
        .build()
        .map_err(|error| format!("无法创建 AI 请求客户端：{error}"))?;
    let page_count = source.book().sections.len();
    if page_count == 0 {
        return Err("PDF 没有可识别的页面".into());
    }

    let (toc_pages, anchors, heading_anchors, metadata, page_roles) = locate_toc_pages(
        &client,
        &provider,
        &model,
        Arc::clone(&source),
        page_count,
        need_book_metadata,
        &mut on_progress,
    )
    .await?;
    if !need_toc {
        return Ok(PdfMetadataExtraction {
            toc: None,
            toc_error: None,
            metadata,
            page_roles,
        });
    }
    if toc_pages.is_empty() {
        return Ok(PdfMetadataExtraction {
            toc: None,
            toc_error: Some("未在 PDF 前部识别到印刷目录页".into()),
            metadata,
            page_roles,
        });
    }

    on_progress(format!("正在提取 {} 页目录…", toc_pages.len()));
    let extracted = match extract_entries(
        &client,
        &provider,
        &model,
        Arc::clone(&source),
        &toc_pages,
        &mut on_progress,
    )
    .await
    {
        Ok(extracted) => extracted,
        Err(error) => {
            return Ok(PdfMetadataExtraction {
                toc: None,
                toc_error: Some(error),
                metadata,
                page_roles,
            });
        }
    };
    let last_toc_page = toc_pages.iter().copied().max().unwrap_or(0);
    let Some((offset, offset_support)) = infer_page_offset(&anchors, last_toc_page) else {
        return Ok(PdfMetadataExtraction {
            toc: None,
            toc_error: Some("已识别目录，但无法建立印刷页码与 PDF 页码的映射".into()),
            metadata,
            page_roles,
        });
    };
    let confidence_factor = if offset_support >= 3 { 1.0 } else { 0.88 };
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for entry in extracted {
        let Some(printed_page) = parse_arabic_page_number(&entry.n) else {
            continue;
        };
        let physical_page = isize::try_from(printed_page)
            .ok()
            .and_then(|page| page.checked_add(offset))
            .and_then(|page| usize::try_from(page).ok())
            .filter(|page| (1..=page_count).contains(page));
        let Some(physical_page) = physical_page else {
            continue;
        };
        let title = entry.t.trim().to_owned();
        if title.is_empty() || !seen.insert((title.clone(), physical_page)) {
            continue;
        }
        entries.push(GeneratedTocEntry {
            depth: entry.d,
            title,
            printed_page: entry.n.trim().to_owned(),
            physical_page,
            confidence: entry.c.unwrap_or(0.9).clamp(0.0, 1.0) * confidence_factor,
        });
    }
    if entries.len() < 2 {
        return Ok(PdfMetadataExtraction {
            toc: None,
            toc_error: Some("目录条目过少，无法生成可靠的导航目录".into()),
            metadata,
            page_roles,
        });
    }
    let offset_mapped_entries = entries.clone();
    let mut heading_verified = apply_restarted_page_sequences(&mut entries, &anchors);
    heading_verified.extend(apply_scanned_heading_anchors(
        &mut entries,
        &heading_anchors,
    ));
    if let Err(error) = verify_top_level_toc_pages(
        &client,
        &provider,
        &model,
        Arc::clone(&source),
        page_count,
        &mut entries,
        &heading_verified,
        &mut on_progress,
    )
    .await
    {
        // Page verification only refines the offset-derived positions. The
        // inferred mapping remains usable if a model request or page render
        // fails, so do not discard an otherwise valid TOC.
        tracing::warn!(%error, "failed to refine generated PDF TOC pages");
    }
    if !toc_page_mapping_is_plausible(&entries) {
        tracing::warn!(
            entries = entries.len(),
            "discarding a degenerate visual PDF TOC page correction"
        );
        entries = offset_mapped_entries;
    }
    on_progress(format!("已生成 {} 个目录条目", entries.len()));
    Ok(PdfMetadataExtraction {
        toc: Some(GeneratedTocDraft {
            provider_name: provider.name,
            model,
            source_pages: toc_pages,
            entries,
        }),
        toc_error: None,
        metadata,
        page_roles,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the shared opening-page scan collects TOC pages, page numbers, headings, and metadata from one request"
)]
async fn locate_toc_pages<F>(
    client: &Client,
    provider: &AiProvider,
    model: &str,
    source: Arc<dyn BookSource>,
    page_count: usize,
    extract_metadata: bool,
    on_progress: &mut F,
) -> Result<
    (
        Vec<usize>,
        Vec<PageNumberAnchor>,
        Vec<PageHeadingAnchor>,
        Option<GeneratedPdfMetadata>,
        Vec<PdfOcrPageRoleAssignment>,
    ),
    String,
>
where
    F: FnMut(String),
{
    let scan_limit = page_count.min(SCAN_PAGE_LIMIT);
    let mut jobs = VecDeque::new();
    let mut scan_ranges = (0..scan_limit)
        .step_by(SCAN_BATCH_SIZE)
        .map(|batch_start| (batch_start, (batch_start + SCAN_BATCH_SIZE).min(scan_limit)))
        .collect::<Vec<_>>();
    let back_start = page_count.saturating_sub(SCAN_BATCH_SIZE);
    if page_count > scan_limit
        && !scan_ranges
            .iter()
            .any(|(start, end)| back_start >= *start && back_start < *end)
    {
        scan_ranges.push((back_start, page_count));
    }
    for (batch_start, batch_end) in scan_ranges {
        let tail_only = page_count > scan_limit && batch_start >= scan_limit;
        let page_indices = (batch_start..batch_end).collect::<Vec<_>>();
        let page_mapping = page_indices
            .iter()
            .enumerate()
            .map(|(slot, page)| format!("{slot}={}", page + 1))
            .collect::<Vec<_>>()
            .join(",");
        let metadata_instruction = if extract_metadata && batch_start == 0 {
            " Also identify the book title and authors from these opening pages, preferring the title page over covers, running headers, advertisements, series names, endorsements, and filenames. Preserve original language and spelling. Include m as {\"t\":\"full book title\",\"a\":[\"author name\"]}; use empty values only when not visible."
        } else {
            ""
        };
        let response_shape = if extract_metadata && batch_start == 0 {
            "{\"p\":[{\"i\":0,\"k\":\"toc|cover|title_page|back_cover|other\",\"n\":\"printed page number or empty\",\"h\":\"section heading or empty\"}],\"m\":{\"t\":\"title\",\"a\":[\"author\"]}}"
        } else {
            "{\"p\":[{\"i\":0,\"k\":\"toc|cover|title_page|back_cover|other\",\"n\":\"printed page number or empty\",\"h\":\"section heading or empty\"}]}"
        };
        let classification_instruction = if tail_only {
            "These slots are from the end of the PDF. Classify k as back_cover only for the exterior rear cover; otherwise classify it as other. Return empty n and h."
        } else {
            "Classify k as toc for a printed table-of-contents page listing multiple headings with page numbers; cover only for the exterior front cover on PDF page 1; title_page for a formal interior title page or half-title page; otherwise other. Do not classify an opening page as back_cover. A copyright page is other. Use each special role only when visually supported. For h, return the full visible heading only when that page starts a chapter, preface, acknowledgements, introduction, appendix, or other navigable section; otherwise return an empty string. Do not use running headers or incidental mentions as h."
        };
        let content = vec![
            json!({
                "type": "text",
                "text": format!("The image is a 2-column contact sheet in row-major slot order. Slot-to-PDF-page mapping: {page_mapping}. Inspect every slot. {classification_instruction}{metadata_instruction} Return compact JSON only: {response_shape}. Include exactly one p item for every slot. Do not infer n when it is not visibly printed."),
            }),
            json!({
                "type": "image_url",
                "image_url": {
                    "url": render_contact_sheet(source.as_ref(), &page_indices, SCAN_IMAGE_MAX_DIMENSION)?
                }
            }),
        ];
        jobs.push_back((batch_start, batch_end, content));
    }

    let total_batches = jobs.len();
    let mut tasks = JoinSet::new();
    while tasks.len() < VISION_REQUEST_CONCURRENCY
        && let Some((batch_start, batch_end, content)) = jobs.pop_front()
    {
        let client = client.clone();
        let provider = provider.clone();
        let model = model.to_owned();
        tasks.spawn(async move {
            let value = request_vision_json(&client, &provider, &model, content).await?;
            let response: ScanResponse = parse_json_value(&value)?;
            Ok::<_, String>((batch_start, batch_end, response))
        });
    }

    let mut toc_pages = Vec::new();
    let mut anchors = Vec::new();
    let mut heading_anchors = Vec::new();
    let mut page_roles = Vec::new();
    let mut metadata = None;
    let mut completed_batches = 0;
    while let Some(result) = tasks.join_next().await {
        let (batch_start, batch_end, response) =
            result.map_err(|error| format!("目录页识别任务异常结束：{error}"))??;
        completed_batches += 1;
        on_progress(format!(
            "正在查找目录页：{completed_batches}/{total_batches} 批"
        ));
        if batch_start == 0 {
            metadata = response
                .m
                .and_then(|metadata| metadata.into_generated(&provider.name, model));
        }
        for page in response.p {
            if page.i >= batch_end - batch_start {
                continue;
            }
            let physical_page = batch_start + page.i + 1;
            let is_toc = page.k.eq_ignore_ascii_case("toc");
            if is_toc {
                toc_pages.push(physical_page);
            }
            let role = scanned_page_role(&page.k);
            if let Some(role) = role {
                if page_role_is_plausible(role, physical_page, page_count) {
                    page_roles.push(PdfOcrPageRoleAssignment {
                        physical_page,
                        role,
                    });
                }
            }
            // A TOC page contains many printed page numbers and headings. It is
            // not a valid anchor for either value even when the vision model
            // redundantly fills n/h beside k=toc. The same applies to covers
            // and title pages.
            let can_anchor_content = !is_toc && role.is_none();
            if can_anchor_content && !page.n.trim().is_empty() {
                anchors.push(PageNumberAnchor {
                    physical_page,
                    printed_page: page.n,
                });
            }
            let title = page.h.trim();
            if can_anchor_content && !title.is_empty() {
                heading_anchors.push(PageHeadingAnchor {
                    physical_page,
                    title: title.to_owned(),
                });
            }
        }

        if let Some((batch_start, batch_end, content)) = jobs.pop_front() {
            let client = client.clone();
            let provider = provider.clone();
            let model = model.to_owned();
            tasks.spawn(async move {
                let value = request_vision_json(&client, &provider, &model, content).await?;
                let response: ScanResponse = parse_json_value(&value)?;
                Ok::<_, String>((batch_start, batch_end, response))
            });
        }
    }
    toc_pages.sort_unstable();
    toc_pages.dedup();
    heading_anchors.sort_unstable_by_key(|anchor| anchor.physical_page);
    heading_anchors.dedup_by(|left, right| {
        left.physical_page == right.physical_page && left.title == right.title
    });
    page_roles.sort_unstable_by_key(|assignment| assignment.physical_page);
    page_roles.dedup_by_key(|assignment| assignment.physical_page);
    Ok((toc_pages, anchors, heading_anchors, metadata, page_roles))
}

async fn extract_entries<F>(
    client: &Client,
    provider: &AiProvider,
    model: &str,
    source: Arc<dyn BookSource>,
    toc_pages: &[usize],
    on_progress: &mut F,
) -> Result<Vec<ExtractedEntry>, String>
where
    F: FnMut(String),
{
    let mut jobs = VecDeque::new();
    for (batch_index, pages) in toc_pages.chunks(EXTRACTION_BATCH_SIZE).enumerate() {
        let mut content = vec![json!({
            "type": "text",
            "text": "Extract every navigable entry from this printed table-of-contents page in visual reading order. Preserve the original title text. d is zero-based hierarchy depth, t is title without leaders or page number, n is the printed target page label, c is confidence 0..1. Ignore running headers and the page's own footer. Return compact JSON only: {\"e\":[{\"d\":0,\"t\":\"title\",\"n\":\"12\",\"c\":0.98}]} ."
        })];
        for physical_page in pages {
            content.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": render_page_data_url(source.as_ref(), physical_page - 1, PAGE_IMAGE_MAX_DIMENSION)?
                }
            }));
        }
        jobs.push_back((batch_index, content));
    }

    let total_batches = jobs.len();
    let mut tasks = JoinSet::new();
    while tasks.len() < VISION_REQUEST_CONCURRENCY
        && let Some((batch_index, content)) = jobs.pop_front()
    {
        let client = client.clone();
        let provider = provider.clone();
        let model = model.to_owned();
        tasks.spawn(async move {
            let value = request_vision_json(&client, &provider, &model, content).await?;
            let response: ExtractionResponse = parse_json_value(&value)?;
            Ok::<_, String>((batch_index, response.e))
        });
    }

    let mut completed_batches = 0;
    let mut batches = Vec::with_capacity(total_batches);
    while let Some(result) = tasks.join_next().await {
        let batch = result.map_err(|error| format!("目录文字提取任务异常结束：{error}"))??;
        completed_batches += 1;
        on_progress(format!(
            "正在读取目录文字：{completed_batches}/{total_batches} 页"
        ));
        batches.push(batch);

        if let Some((batch_index, content)) = jobs.pop_front() {
            let client = client.clone();
            let provider = provider.clone();
            let model = model.to_owned();
            tasks.spawn(async move {
                let value = request_vision_json(&client, &provider, &model, content).await?;
                let response: ExtractionResponse = parse_json_value(&value)?;
                Ok::<_, String>((batch_index, response.e))
            });
        }
    }

    batches.sort_unstable_by_key(|(batch_index, _)| *batch_index);
    Ok(batches
        .into_iter()
        .flat_map(|(_, entries)| entries)
        .collect())
}

#[allow(
    clippy::too_many_arguments,
    reason = "page verification needs the shared AI endpoint plus the headings already confirmed by the opening-page scan"
)]
async fn verify_top_level_toc_pages<F>(
    client: &Client,
    provider: &AiProvider,
    model: &str,
    source: Arc<dyn BookSource>,
    page_count: usize,
    entries: &mut [GeneratedTocEntry],
    heading_verified: &HashSet<usize>,
    on_progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(String),
{
    let targets = entries
        .iter()
        .enumerate()
        .filter(|(entry_index, entry)| entry.depth == 0 && !heading_verified.contains(entry_index))
        .map(|(entry_index, entry)| PageVerificationTarget {
            entry_index,
            title: entry.title.clone(),
            candidates: verification_candidate_pages(entry.physical_page, page_count),
        })
        .filter(|target| !target.candidates.is_empty())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Ok(());
    }

    let mut jobs = VecDeque::new();
    for batch in targets.chunks(PAGE_VERIFICATION_BATCH_SIZE) {
        let batch = batch.to_vec();
        let content = page_verification_content(source.as_ref(), &batch)?;
        jobs.push_back((batch, content));
    }

    let total_batches = jobs.len();
    let mut tasks = JoinSet::new();
    while tasks.len() < VISION_REQUEST_CONCURRENCY
        && let Some((batch, content)) = jobs.pop_front()
    {
        let client = client.clone();
        let provider = provider.clone();
        let model = model.to_owned();
        tasks.spawn(async move {
            let value = request_vision_json(&client, &provider, &model, content).await?;
            let response: PageVerificationResponse = parse_json_value(&value)?;
            Ok::<_, String>((batch, response))
        });
    }

    let mut completed_batches = 0;
    let mut verified = HashMap::new();
    while let Some(result) = tasks.join_next().await {
        let (batch, response) =
            result.map_err(|error| format!("目录页校准任务异常结束：{error}"))??;
        completed_batches += 1;
        on_progress(format!(
            "正在校准目录页：{completed_batches}/{total_batches} 批"
        ));
        for choice in response.r {
            let Some(target) = batch.iter().find(|target| target.entry_index == choice.id) else {
                continue;
            };
            let Some(page) = choice
                .i
                .and_then(|slot| target.candidates.get(slot))
                .copied()
            else {
                continue;
            };
            verified.insert(target.entry_index, page);
        }

        if let Some((batch, content)) = jobs.pop_front() {
            let client = client.clone();
            let provider = provider.clone();
            let model = model.to_owned();
            tasks.spawn(async move {
                let value = request_vision_json(&client, &provider, &model, content).await?;
                let response: PageVerificationResponse = parse_json_value(&value)?;
                Ok::<_, String>((batch, response))
            });
        }
    }
    apply_consistent_top_level_page_correction(entries, &verified, heading_verified, page_count);
    if verified.len() != targets.len() {
        tracing::warn!(
            verified = verified.len(),
            total = targets.len(),
            "some top-level PDF TOC pages kept their offset-derived positions"
        );
    }
    Ok(())
}

fn page_verification_content(
    source: &dyn BookSource,
    batch: &[PageVerificationTarget],
) -> Result<Vec<serde_json::Value>, String> {
    let mut content = vec![json!({
        "type": "text",
        "text": "Verify the physical PDF page where each requested top-level chapter or introduction begins. Each following image is an independent 2-column contact sheet in row-major slot order. Select the slot where the requested heading visibly starts the chapter; ignore running headers and incidental mentions. The heading may appear anywhere on the page and may use different Chinese scripts. Return compact JSON only: {\"r\":[{\"id\":0,\"i\":1}]}. id must equal the supplied entry id. i is the zero-based candidate slot, or null when none matches. Never invent a page outside the supplied candidates."
    })];
    for target in batch {
        let mapping = target
            .candidates
            .iter()
            .enumerate()
            .map(|(slot, page)| format!("{slot}=PDF page {page}"))
            .collect::<Vec<_>>()
            .join(", ");
        let title = serde_json::to_string(&target.title)
            .map_err(|error| format!("无法编码目录标题：{error}"))?;
        content.push(json!({
            "type": "text",
            "text": format!("Entry id {}. Requested heading: {title}. Slot mapping: {mapping}.", target.entry_index)
        }));
        let page_indices = target
            .candidates
            .iter()
            .map(|page| page - 1)
            .collect::<Vec<_>>();
        content.push(json!({
            "type": "image_url",
            "image_url": {
                "url": render_contact_sheet(source, &page_indices, 1_000)?
            }
        }));
    }
    Ok(content)
}

fn verification_candidate_pages(predicted_page: usize, page_count: usize) -> Vec<usize> {
    if page_count == 0 || predicted_page == 0 {
        return Vec::new();
    }
    let start = predicted_page
        .saturating_sub(PAGE_VERIFICATION_RADIUS)
        .max(1);
    let end = predicted_page
        .saturating_add(PAGE_VERIFICATION_RADIUS)
        .min(page_count);
    (start..=end).collect()
}

fn apply_scanned_heading_anchors(
    entries: &mut [GeneratedTocEntry],
    anchors: &[PageHeadingAnchor],
) -> HashSet<usize> {
    let top_level = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| (entry.depth == 0).then_some(index))
        .collect::<Vec<_>>();
    let mut used_pages = HashSet::new();
    let mut verified = HashSet::new();
    for (position, start) in top_level.iter().copied().enumerate() {
        let key = normalized_heading_key(&entries[start].title);
        if key.is_empty() {
            continue;
        }
        let matches = anchors
            .iter()
            .filter(|anchor| {
                !used_pages.contains(&anchor.physical_page)
                    && headings_match(&key, &normalized_heading_key(&anchor.title))
            })
            .collect::<Vec<_>>();
        let [anchor] = matches.as_slice() else {
            continue;
        };
        shift_top_level_group(
            entries,
            &top_level,
            position,
            anchor.physical_page,
            usize::MAX,
        );
        used_pages.insert(anchor.physical_page);
        verified.insert(start);
    }
    verified
}

fn apply_restarted_page_sequences(
    entries: &mut [GeneratedTocEntry],
    anchors: &[PageNumberAnchor],
) -> HashSet<usize> {
    let top_level = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| (entry.depth == 0).then_some(index))
        .collect::<Vec<_>>();
    let mut entries_by_printed_page = HashMap::<usize, Vec<usize>>::new();
    for &entry_index in &top_level {
        if let Some(printed_page) = parse_arabic_page_number(&entries[entry_index].printed_page) {
            entries_by_printed_page
                .entry(printed_page)
                .or_default()
                .push(entry_index);
        }
    }
    let mut verified = HashSet::new();
    for (printed_page, entry_indices) in entries_by_printed_page {
        if entry_indices.len() < 2 {
            continue;
        }
        let mut physical_pages = anchors
            .iter()
            .filter_map(|anchor| {
                (parse_arabic_page_number(&anchor.printed_page) == Some(printed_page))
                    .then_some(anchor.physical_page)
            })
            .collect::<Vec<_>>();
        physical_pages.sort_unstable();
        physical_pages.dedup();
        if physical_pages.len() != entry_indices.len() {
            continue;
        }
        for (entry_index, physical_page) in entry_indices.into_iter().zip(physical_pages) {
            let Some(position) = top_level.iter().position(|index| *index == entry_index) else {
                continue;
            };
            shift_top_level_group(entries, &top_level, position, physical_page, usize::MAX);
            verified.insert(entry_index);
        }
    }
    verified
}

fn normalized_heading_key(title: &str) -> String {
    title
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn headings_match(left: &str, right: &str) -> bool {
    left == right
        || (left.chars().count().min(right.chars().count()) >= 4
            && (left.contains(right) || right.contains(left)))
}

fn apply_consistent_top_level_page_correction(
    entries: &mut [GeneratedTocEntry],
    verified: &HashMap<usize, usize>,
    heading_verified: &HashSet<usize>,
    page_count: usize,
) {
    let top_level = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| (entry.depth == 0).then_some(index))
        .collect::<Vec<_>>();
    let mut delta_counts = HashMap::<isize, usize>::new();
    for (&entry_index, &verified_page) in verified {
        let Some(entry) = entries.get(entry_index) else {
            continue;
        };
        let (Ok(predicted), Ok(verified)) = (
            isize::try_from(entry.physical_page),
            isize::try_from(verified_page),
        ) else {
            continue;
        };
        *delta_counts.entry(verified - predicted).or_default() += 1;
    }
    let Some((delta, support)) = delta_counts
        .into_iter()
        .max_by_key(|(delta, support)| (*support, std::cmp::Reverse(delta.abs())))
    else {
        return;
    };
    if support < 2 || support * 2 <= verified.len() || delta == 0 {
        return;
    }
    for (position, start) in top_level.iter().copied().enumerate() {
        if heading_verified.contains(&start) {
            continue;
        }
        let Some(target_page) = isize::try_from(entries[start].physical_page)
            .ok()
            .and_then(|page| page.checked_add(delta))
            .and_then(|page| usize::try_from(page).ok())
            .filter(|page| (1..=page_count).contains(page))
        else {
            continue;
        };
        shift_top_level_group(entries, &top_level, position, target_page, page_count);
    }
}

fn toc_page_mapping_is_plausible(entries: &[GeneratedTocEntry]) -> bool {
    let top_level_pages = entries
        .iter()
        .filter_map(|entry| (entry.depth == 0).then_some(entry.physical_page))
        .collect::<Vec<_>>();
    if top_level_pages.len() < 4 {
        return true;
    }
    let distinct_pages = top_level_pages.iter().copied().collect::<HashSet<_>>();
    distinct_pages.len() >= 3 && top_level_pages.windows(2).all(|pages| pages[0] <= pages[1])
}

fn shift_top_level_group(
    entries: &mut [GeneratedTocEntry],
    top_level: &[usize],
    position: usize,
    target_page: usize,
    page_count: usize,
) {
    let start = top_level[position];
    let end = top_level
        .get(position + 1)
        .copied()
        .unwrap_or(entries.len());
    let (Ok(predicted_page), Ok(target_page_signed)) = (
        isize::try_from(entries[start].physical_page),
        isize::try_from(target_page),
    ) else {
        return;
    };
    let delta = target_page_signed - predicted_page;
    for entry in &mut entries[start..end] {
        let Some(page) = isize::try_from(entry.physical_page)
            .ok()
            .and_then(|page| page.checked_add(delta))
            .and_then(|page| usize::try_from(page).ok())
            .filter(|page| *page > 0 && (page_count == usize::MAX || *page <= page_count))
        else {
            continue;
        };
        entry.physical_page = page;
    }
    entries[start].physical_page = target_page;
}

fn render_contact_sheet(
    source: &dyn BookSource,
    page_indices: &[usize],
    max_dimension: u32,
) -> Result<String, String> {
    let images = page_indices
        .iter()
        .map(|page_index| render_page_image(source, *page_index, max_dimension))
        .collect::<Result<Vec<_>, _>>()?;
    let columns = 2_u32;
    let rows = u32::try_from(images.len())
        .unwrap_or(u32::MAX)
        .div_ceil(columns);
    let cell_width = images.iter().map(DynamicImage::width).max().unwrap_or(1) + 8;
    let cell_height = images.iter().map(DynamicImage::height).max().unwrap_or(1) + 8;
    let mut sheet = RgbImage::from_pixel(
        cell_width * columns,
        cell_height * rows,
        Rgb([238, 238, 238]),
    );
    for (slot, image) in images.iter().enumerate() {
        let slot = u32::try_from(slot).unwrap_or(0);
        let x = (slot % columns) * cell_width + (cell_width - image.width()) / 2;
        let y = (slot / columns) * cell_height + (cell_height - image.height()) / 2;
        image::imageops::overlay(&mut sheet, &image.to_rgb8(), i64::from(x), i64::from(y));
    }
    let sheet = DynamicImage::ImageRgb8(sheet).resize(2_000, 2_000, FilterType::Triangle);
    encode_jpeg_data_url(&sheet, page_indices[0])
}

fn infer_page_offset(anchors: &[PageNumberAnchor], last_toc_page: usize) -> Option<(isize, usize)> {
    let mut counts = HashMap::<isize, usize>::new();
    for anchor in anchors {
        if anchor.physical_page <= last_toc_page {
            continue;
        }
        let Some(printed) = parse_arabic_page_number(&anchor.printed_page) else {
            continue;
        };
        let (Ok(physical), Ok(printed)) = (
            isize::try_from(anchor.physical_page),
            isize::try_from(printed),
        ) else {
            continue;
        };
        if printed <= 0 || printed > physical {
            continue;
        }
        *counts.entry(physical - printed).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(offset, count)| (*count, std::cmp::Reverse(offset.abs())))
}

fn parse_arabic_page_number(label: &str) -> Option<usize> {
    let digits = label
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;

    use reqwest::Client;
    use serde_json::json;

    use super::{
        MetadataResponse, PageHeadingAnchor, PageNumberAnchor, ScanResponse,
        apply_consistent_top_level_page_correction, apply_restarted_page_sequences,
        apply_scanned_heading_anchors, generate_pdf_toc, infer_page_offset,
        is_retryable_vision_response_error, page_role_is_plausible, parse_arabic_page_number,
        render_page_data_url, request_vision_json, scanned_page_role,
        toc_page_mapping_is_plausible, verification_candidate_pages,
    };
    use crate::plugins::PdfOcrPageRole;

    #[test]
    fn page_classification_accepts_cover_title_page_and_back_cover_roles() {
        assert_eq!(scanned_page_role("cover"), Some(PdfOcrPageRole::Cover));
        assert_eq!(
            scanned_page_role("title_page"),
            Some(PdfOcrPageRole::TitlePage)
        );
        assert_eq!(
            scanned_page_role("back-cover"),
            Some(PdfOcrPageRole::BackCover)
        );
        assert_eq!(scanned_page_role("toc"), None);
        assert_eq!(scanned_page_role("other"), None);
    }

    #[test]
    fn special_page_roles_are_restricted_to_plausible_pdf_regions() {
        assert!(page_role_is_plausible(PdfOcrPageRole::Cover, 1, 258));
        assert!(!page_role_is_plausible(PdfOcrPageRole::Cover, 2, 258));
        assert!(page_role_is_plausible(PdfOcrPageRole::TitlePage, 8, 258));
        assert!(!page_role_is_plausible(PdfOcrPageRole::TitlePage, 21, 258));
        assert!(!page_role_is_plausible(PdfOcrPageRole::BackCover, 8, 258));
        assert!(page_role_is_plausible(PdfOcrPageRole::BackCover, 258, 258));
    }

    #[test]
    fn degenerate_top_level_page_mapping_is_rejected() {
        let entry = |physical_page: usize| super::GeneratedTocEntry {
            depth: 0,
            title: format!("Chapter {physical_page}"),
            printed_page: physical_page.to_string(),
            physical_page,
            confidence: 0.9,
        };
        assert!(!toc_page_mapping_is_plausible(&[
            entry(2),
            entry(2),
            entry(2),
            entry(2),
        ]));
        assert!(toc_page_mapping_is_plausible(&[
            entry(10),
            entry(17),
            entry(27),
            entry(34),
        ]));
        assert!(!toc_page_mapping_is_plausible(&[
            entry(10),
            entry(27),
            entry(17),
            entry(34),
        ]));
    }

    #[test]
    fn combined_scan_response_keeps_book_metadata_beside_page_classification() {
        let response: ScanResponse = serde_json::from_value(json!({
            "p": [{"i": 0, "k": "other", "n": ""}],
            "m": {"t": "  Book title  ", "a": [" Author "]}
        }))
        .unwrap();
        let metadata = response
            .m
            .and_then(|value: MetadataResponse| value.into_generated("Provider", "model"))
            .unwrap();
        assert_eq!(metadata.title, "Book title");
        assert_eq!(metadata.authors, ["Author"]);
        assert_eq!(metadata.provider_name, "Provider");
        assert_eq!(metadata.model, "model");
    }

    #[test]
    fn nullable_toc_strings_are_treated_as_missing_values() {
        let response: super::ExtractionResponse = serde_json::from_value(json!({
            "e": [{"d": 0, "t": "Chapter", "n": null, "c": 0.5}]
        }))
        .unwrap();
        assert_eq!(response.e[0].t, "Chapter");
        assert!(response.e[0].n.is_empty());
    }

    #[test]
    fn page_offset_uses_the_most_supported_mapping() {
        let anchors = vec![
            PageNumberAnchor {
                physical_page: 14,
                printed_page: "1".into(),
            },
            PageNumberAnchor {
                physical_page: 15,
                printed_page: "2".into(),
            },
            PageNumberAnchor {
                physical_page: 17,
                printed_page: "4".into(),
            },
            PageNumberAnchor {
                physical_page: 18,
                printed_page: "9".into(),
            },
        ];
        assert_eq!(infer_page_offset(&anchors, 10), Some((13, 3)));
        assert_eq!(parse_arabic_page_number("第 128 页"), Some(128));
    }

    #[test]
    fn verification_candidates_stay_within_the_pdf() {
        assert_eq!(verification_candidate_pages(1, 10), vec![1, 2, 3]);
        assert_eq!(verification_candidate_pages(9, 10), vec![7, 8, 9, 10]);
        assert!(verification_candidate_pages(0, 10).is_empty());
        assert!(verification_candidate_pages(1, 0).is_empty());
    }

    #[test]
    fn scanned_headings_locate_independent_front_matter_page_sequences() {
        let entry = |title: &str, page: usize| crate::generated_toc::GeneratedTocEntry {
            depth: 0,
            title: title.into(),
            printed_page: "1".into(),
            physical_page: page,
            confidence: 0.9,
        };
        let mut entries = vec![
            entry("致谢", 19),
            entry("序言", 19),
            entry("第1章 手之初", 19),
            entry("第2章 手、思想和语言", 35),
        ];
        let anchors = vec![
            PageHeadingAnchor {
                physical_page: 7,
                title: "致谢".into(),
            },
            PageHeadingAnchor {
                physical_page: 10,
                title: "序言".into(),
            },
            PageHeadingAnchor {
                physical_page: 19,
                title: "第1章 手之初".into(),
            },
        ];

        let verified = apply_scanned_heading_anchors(&mut entries, &anchors);

        assert_eq!(verified, HashSet::from([0, 1, 2]));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>(),
            [7, 10, 19, 35]
        );
    }

    #[test]
    fn repeated_printed_pages_reuse_scan_anchors_in_toc_order() {
        let entry = |title: &str, page: usize| crate::generated_toc::GeneratedTocEntry {
            depth: 0,
            title: title.into(),
            printed_page: "1".into(),
            physical_page: page,
            confidence: 0.9,
        };
        let mut entries = vec![
            entry("致谢", 19),
            entry("序言", 19),
            entry("第1章 手之初", 19),
        ];
        let anchors = vec![
            PageNumberAnchor {
                physical_page: 7,
                printed_page: "1".into(),
            },
            PageNumberAnchor {
                physical_page: 10,
                printed_page: "1".into(),
            },
            PageNumberAnchor {
                physical_page: 19,
                printed_page: "1".into(),
            },
        ];

        let verified = apply_restarted_page_sequences(&mut entries, &anchors);

        assert_eq!(verified, HashSet::from([0, 1, 2]));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>(),
            [7, 10, 19]
        );
    }

    #[test]
    fn inconsistent_visual_page_offsets_do_not_override_inferred_mapping() {
        let entry = |page: usize| crate::generated_toc::GeneratedTocEntry {
            depth: 0,
            title: format!("entry-{page}"),
            printed_page: page.to_string(),
            physical_page: page,
            confidence: 0.9,
        };
        let mut entries = vec![entry(19), entry(35), entry(55), entry(69)];
        let verified = HashMap::from([(0, 20), (1, 35), (2, 53), (3, 70)]);

        apply_consistent_top_level_page_correction(&mut entries, &verified, &HashSet::new(), 100);

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>(),
            [19, 35, 55, 69]
        );
    }

    #[test]
    fn dominant_visual_page_offset_applies_to_unanchored_groups() {
        let entry = |depth: usize, page: usize| crate::generated_toc::GeneratedTocEntry {
            depth,
            title: format!("entry-{depth}-{page}"),
            printed_page: page.to_string(),
            physical_page: page,
            confidence: 0.9,
        };
        let mut entries = vec![
            entry(0, 10),
            entry(1, 12),
            entry(0, 30),
            entry(1, 32),
            entry(0, 50),
        ];
        apply_consistent_top_level_page_correction(
            &mut entries,
            &HashMap::from([(0, 11), (2, 31), (4, 51)]),
            &HashSet::new(),
            60,
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.physical_page)
                .collect::<Vec<_>>(),
            [11, 13, 31, 33, 51]
        );
    }

    #[test]
    fn retries_only_transient_vision_gateway_failures() {
        assert!(is_retryable_vision_response_error(
            "AI 响应缺少 choices[0].message"
        ));
        assert!(is_retryable_vision_response_error(
            "AI 服务返回 503 Service Unavailable"
        ));
        assert!(!is_retryable_vision_response_error(
            "AI 目录识别结果协议无效"
        ));
    }

    #[test]
    #[ignore = "uses the configured AI provider and a local PDF"]
    fn live_single_page_vision_probe() {
        let path = std::env::var_os("REBOOK_PDF_TOC_TEST_FILE")
            .expect("set REBOOK_PDF_TOC_TEST_FILE to a scanned PDF");
        let page_index = std::env::var("REBOOK_PDF_TOC_TEST_PAGE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let opened = rebook_formats::open_file(std::path::PathBuf::from(path))
            .expect("test PDF should open");
        let source = opened.source();
        let settings = super::PluginSettings::load_default().expect("AI settings should load");
        let (provider, model) = settings
            .ocr_endpoint()
            .expect("OCR endpoint should be valid");
        let client = Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
            .expect("HTTP client should build");
        let image_url =
            render_page_data_url(source.as_ref(), page_index, 1_200).expect("page should render");
        let content = vec![
            json!({
                "type": "text",
                "text": "Describe this scanned book page in one short sentence. Return JSON: {\"description\":\"...\"}."
            }),
            json!({
                "type": "image_url",
                "image_url": { "url": image_url }
            }),
        ];
        eprintln!("probing model {model} with PDF page {}", page_index + 1);
        let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime should start");
        let response = runtime
            .block_on(request_vision_json(&client, provider, model, content))
            .expect("vision request should succeed");
        eprintln!("vision response: {response}");
    }

    #[test]
    #[ignore = "uses the configured AI provider and a local PDF"]
    fn live_scanned_pdf_toc_generation() {
        let path = std::env::var_os("REBOOK_PDF_TOC_TEST_FILE")
            .expect("set REBOOK_PDF_TOC_TEST_FILE to a scanned PDF");
        let opened = rebook_formats::open_file(std::path::PathBuf::from(path))
            .expect("test PDF should open");
        let source = opened.source();
        let page_count = source.book().sections.len();
        let text_layer_pages = (0..page_count.min(24))
            .filter(|index| {
                source.parse_section(*index).ok().is_some_and(|section| {
                    section.blocks.iter().any(|block| {
                        matches!(block, rebook_publication::Block::Image(image) if image.text_layer.as_ref().is_some_and(|layer| !layer.text.trim().is_empty()))
                    })
                })
            })
            .count();
        eprintln!("non-empty text layers in first 24 pages: {text_layer_pages}");
        let settings = super::PluginSettings::load_default().expect("AI settings should load");
        let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime should start");
        let draft = runtime
            .block_on(generate_pdf_toc(source, settings, |message| {
                eprintln!("{message}");
            }))
            .expect("TOC generation should succeed");
        for entry in &draft.entries {
            eprintln!(
                "{}{} -> printed {}, PDF {} ({:.0}%)",
                "  ".repeat(entry.depth),
                entry.title,
                entry.printed_page,
                entry.physical_page,
                entry.confidence * 100.0
            );
        }
        assert!(draft.entries.len() >= 2);
        assert!(
            draft
                .entries
                .iter()
                .all(|entry| entry.physical_page <= page_count)
        );
    }

    #[test]
    #[ignore = "uses the configured AI provider and a local PDF"]
    fn live_pdf_metadata_extraction() {
        let path = std::env::var_os("REBOOK_PDF_TOC_TEST_FILE")
            .expect("set REBOOK_PDF_TOC_TEST_FILE to a scanned PDF");
        let opened = rebook_formats::open_file(std::path::PathBuf::from(path))
            .expect("test PDF should open");
        let source = opened.source();
        let settings = super::PluginSettings::load_default().expect("AI settings should load");
        let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime should start");
        let result = runtime
            .block_on(super::extract_pdf_metadata(
                source,
                settings,
                true,
                true,
                true,
                |message| eprintln!("{message}"),
            ))
            .expect("metadata extraction request should succeed");
        eprintln!("toc error: {:?}", result.toc_error);
        let metadata = result.metadata.expect("book metadata should be recognized");
        eprintln!("title: {}", metadata.title);
        eprintln!("authors: {:?}", metadata.authors);
        assert!(!metadata.title.is_empty() || !metadata.authors.is_empty());
    }
}
