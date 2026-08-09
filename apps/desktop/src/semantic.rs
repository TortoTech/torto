//! Local, rebuildable semantic index backed by sqlite-vec.
//!
//! This module deliberately owns a database separate from sync-v1.sqlite3. The
//! chunks and vectors are derived caches and never participate in `WebDAV` sync.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use bytemuck::cast_slice;
use directories::ProjectDirs;
use rebook_publication::{Block, BookSource, SourceAnchor, SourceRange};
use reqwest::Client;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::plugins::{AiProvider, PluginSettings, section_title, text_block_kind, text_block_text};

const DATABASE_FILE: &str = "semantic-v1.sqlite3";
const MAX_BATCH_ITEMS: usize = 32;
const MAX_BATCH_CHARS: usize = 30_000;
const DEFAULT_SEARCH_LIMIT: usize = 30;
static INDEX_LOCKS: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SemanticSearchScope {
    #[default]
    CurrentBook,
    IndexedBooks,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticSearchResult {
    pub(crate) book_id: String,
    pub(crate) book_title: String,
    pub(crate) section_index: usize,
    pub(crate) section_title: String,
    pub(crate) text: String,
    pub(crate) block_kind: String,
    pub(crate) range: SourceRange,
    pub(crate) similarity: f32,
    pub(crate) modality: SemanticModality,
    pub(crate) image_href: Option<String>,
    pub(crate) image_preview: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SemanticModality {
    #[default]
    Text,
    Image,
}

impl SemanticModality {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }

    fn parse(value: &str) -> Self {
        if value == "image" {
            Self::Image
        } else {
            Self::Text
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SemanticIndexSummary {
    pub(crate) already_indexed: bool,
    pub(crate) total_chunks: usize,
}

#[derive(Clone, Debug)]
struct SemanticChunk {
    key: String,
    section_index: usize,
    section_title: String,
    block_kind: String,
    text: String,
    range: SourceRange,
    modality: SemanticModality,
    image_href: Option<String>,
    image_preview: Option<Vec<u8>>,
    input: EmbeddingInput,
    content_hash: String,
}

#[derive(Clone, Debug)]
struct StoredChunk {
    id: i64,
    input: EmbeddingInput,
}

#[derive(Clone, Debug)]
enum EmbeddingInput {
    Text(String),
}

impl EmbeddingInput {
    fn payload_chars(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
        }
    }
}

#[derive(Clone, Debug)]
struct EmbeddingIdentity {
    fingerprint: String,
    provider_name: String,
    model: String,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

#[allow(
    clippy::too_many_lines,
    reason = "indexing keeps its resumable parse, batch, persistence, and progress flow explicit"
)]
pub(crate) async fn index_book<F>(
    source: Arc<dyn BookSource>,
    settings: PluginSettings,
    mut on_progress: F,
) -> Result<SemanticIndexSummary, String>
where
    F: FnMut(usize, usize) + Send + 'static,
{
    let (provider, model) = settings.embedding_endpoint()?;
    let provider = provider.clone();
    let model = model.clone();
    let identity = embedding_identity(&provider, &model.id);
    let book_id = source.book().id.to_string();
    let book_title = display_book_title(source.as_ref());
    let database_path = semantic_database_path()?;
    let lock_key = format!("{}:{}", identity.fingerprint, book_id);
    let book_lock = {
        let locks = INDEX_LOCKS.get_or_init(Default::default);
        let mut locks = locks.lock().await;
        Arc::clone(
            locks
                .entry(lock_key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let _book_guard = book_lock.lock().await;

    let already_indexed = {
        let path = database_path.clone();
        let identity = identity.clone();
        let book_id = book_id.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = SemanticStore::open(&path)?;
            store.activate_identity(&identity)?;
            store.is_book_complete(&book_id, &identity.fingerprint)
        })
        .await
        .map_err(|error| format!("语义索引任务异常结束：{error}"))??
    };
    if let Some(total_chunks) = already_indexed {
        return Ok(SemanticIndexSummary {
            already_indexed: true,
            total_chunks,
        });
    }

    let extracted = tokio::task::spawn_blocking(move || extract_chunks(source.as_ref()))
        .await
        .map_err(|error| format!("语义内容提取任务异常结束：{error}"))??;
    let total_chunks = extracted.len();

    let mut pending = {
        let path = database_path.clone();
        let identity = identity.clone();
        let book_id = book_id.clone();
        let book_title = book_title.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = SemanticStore::open(&path)?;
            store.activate_identity(&identity)?;
            store.prepare_book(&book_id, &book_title, &identity.fingerprint, &extracted)
        })
        .await
        .map_err(|error| format!("保存语义分段任务异常结束：{error}"))??
    };

    let completed = total_chunks.saturating_sub(pending.len());
    on_progress(completed, total_chunks);
    if pending.is_empty() {
        let path = database_path.clone();
        let identity = identity.clone();
        let book_id = book_id.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = SemanticStore::open(&path)?;
            store.finalize_book(&book_id, &identity.fingerprint)
        })
        .await
        .map_err(|error| format!("完成语义索引任务异常结束：{error}"))??;
        return Ok(SemanticIndexSummary {
            already_indexed: false,
            total_chunks,
        });
    }

    let client = Client::new();
    let mut completed = completed;
    while !pending.is_empty() {
        let batch_len = semantic_batch_len(&pending);
        let batch = pending.drain(..batch_len).collect::<Vec<_>>();
        let inputs = batch.iter().map(|chunk| &chunk.input).collect::<Vec<_>>();
        let vectors = match request_embeddings(&client, &provider, &model.id, &inputs).await {
            Ok(vectors) => vectors,
            Err(error) => {
                record_index_failure(
                    database_path.clone(),
                    book_id.clone(),
                    identity.fingerprint.clone(),
                    error.clone(),
                )
                .await;
                return Err(error);
            }
        };
        let dimensions = vectors.first().map_or(0, Vec::len);
        if dimensions == 0 {
            return Err("Embedding 服务返回了空向量".into());
        }

        let path = database_path.clone();
        let identity = identity.clone();
        let book_id = book_id.clone();
        let book_title = book_title.clone();
        let batch_ids = batch.iter().map(|chunk| chunk.id).collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            let mut store = SemanticStore::open(&path)?;
            store.require_active_identity(&identity.fingerprint)?;
            let reset = store.ensure_dimensions(dimensions)?;
            if reset {
                return Err(
                    "Embedding 向量维度发生变化，旧索引已失效；请重新打开书籍建立索引".into(),
                );
            }
            store.store_embeddings(
                &book_id,
                &book_title,
                &identity.fingerprint,
                &batch_ids,
                &vectors,
            )
        })
        .await
        .map_err(|error| format!("保存 Embedding 任务异常结束：{error}"))??;

        completed += batch.len();
        on_progress(completed, total_chunks);
    }

    let path = database_path;
    let identity = identity.clone();
    let final_book_id = book_id.clone();
    tokio::task::spawn_blocking(move || {
        let mut store = SemanticStore::open(&path)?;
        store.finalize_book(&final_book_id, &identity.fingerprint)
    })
    .await
    .map_err(|error| format!("完成语义索引任务异常结束：{error}"))??;

    Ok(SemanticIndexSummary {
        already_indexed: false,
        total_chunks,
    })
}

pub(crate) async fn search(
    query: &str,
    current_book_id: &str,
    scope: SemanticSearchScope,
    settings: &PluginSettings,
    limit: Option<usize>,
    include_images: bool,
) -> Result<Vec<SemanticSearchResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let (provider, model) = settings.embedding_endpoint()?;
    let identity = embedding_identity(provider, &model.id);
    let database_path = semantic_database_path()?;
    let searchable = {
        let path = database_path.clone();
        let identity = identity.clone();
        let current_book_id = current_book_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut store = SemanticStore::open(&path)?;
            store.activate_identity(&identity)?;
            match scope {
                SemanticSearchScope::CurrentBook => store
                    .is_book_complete(&current_book_id, &identity.fingerprint)
                    .map(|count| count.is_some()),
                SemanticSearchScope::IndexedBooks => {
                    store.has_complete_books(&identity.fingerprint)
                }
            }
        })
        .await
        .map_err(|error| format!("检查语义索引任务异常结束：{error}"))??
    };
    if !searchable {
        return Err(match scope {
            SemanticSearchScope::CurrentBook => "当前书籍的语义索引尚未完成，请稍后再试".into(),
            SemanticSearchScope::IndexedBooks => "尚无已完成索引的书籍".into(),
        });
    }
    let client = Client::new();
    let query_input = EmbeddingInput::Text(query.to_owned());
    let mut query_vectors =
        request_embeddings(&client, provider, &model.id, &[&query_input]).await?;
    let query_vector = query_vectors
        .pop()
        .ok_or_else(|| "Embedding 服务没有返回查询向量".to_owned())?;
    let current_book_id = current_book_id.to_owned();
    let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT).clamp(1, 100);

    tokio::task::spawn_blocking(move || {
        let mut store = SemanticStore::open(&database_path)?;
        store.activate_identity(&identity)?;
        let expected_dimensions = store
            .dimensions()?
            .ok_or_else(|| "尚未建立可搜索的语义索引".to_owned())?;
        if query_vector.len() != expected_dimensions {
            store.invalidate_for_dimension_change(query_vector.len())?;
            return Err("Embedding 向量维度发生变化，旧索引已失效；请重新打开书籍建立索引".into());
        }
        store.search(
            &query_vector,
            scope,
            (scope == SemanticSearchScope::CurrentBook).then_some(current_book_id.as_str()),
            limit,
            include_images,
        )
    })
    .await
    .map_err(|error| format!("语义搜索任务异常结束：{error}"))?
}

fn extract_chunks(source: &dyn BookSource) -> Result<Vec<SemanticChunk>, String> {
    let mut chunks = Vec::new();
    for section_index in 0..source.book().sections.len() {
        let section = source
            .parse_section(section_index)
            .map_err(|error| format!("解析第 {} 节失败：{error}", section_index + 1))?;
        let title = section_title(source, section_index, &section.blocks);
        for (block_index, block) in section.blocks.iter().enumerate() {
            match block {
                Block::Text(block) => {
                    if let Some(range) = &block.source {
                        push_chunk(
                            &mut chunks,
                            format!("s{section_index}:b{block_index}"),
                            section_index,
                            &title,
                            text_block_kind(block),
                            &text_block_text(block),
                            range,
                        );
                    }
                }
                Block::Table(table) => {
                    for (row_index, row) in table.rows.iter().enumerate() {
                        for (cell_index, cell) in row.cells.iter().enumerate() {
                            if let Some(range) = &cell.text.source {
                                push_chunk(
                                    &mut chunks,
                                    format!(
                                        "s{section_index}:b{block_index}:r{row_index}:c{cell_index}"
                                    ),
                                    section_index,
                                    &title,
                                    "table-cell",
                                    &text_block_text(&cell.text),
                                    range,
                                );
                            }
                        }
                    }
                }
                Block::Image(image) => {
                    if let (Some(layer), Some(range)) = (&image.text_layer, &image.source) {
                        push_chunk(
                            &mut chunks,
                            format!("s{section_index}:b{block_index}:image-text"),
                            section_index,
                            &title,
                            "image-text",
                            &layer.text,
                            range,
                        );
                    }
                }
                Block::Separator | Block::PageBreak => {}
            }
        }
    }
    Ok(chunks)
}

#[allow(clippy::too_many_arguments)]
fn push_chunk(
    output: &mut Vec<SemanticChunk>,
    key: String,
    section_index: usize,
    section_title: &str,
    block_kind: &str,
    text: &str,
    range: &SourceRange,
) {
    let leading_chars = text.chars().take_while(|ch| ch.is_whitespace()).count();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let trimmed_chars = trimmed.chars().count();
    let range = trimmed_source_range(range, leading_chars, trimmed_chars);
    output.push(SemanticChunk {
        key,
        section_index,
        section_title: section_title.to_owned(),
        block_kind: block_kind.to_owned(),
        text: trimmed.to_owned(),
        range,
        modality: SemanticModality::Text,
        image_href: None,
        image_preview: None,
        input: EmbeddingInput::Text(trimmed.to_owned()),
        content_hash: hash_text(trimmed),
    });
}

#[allow(clippy::too_many_arguments)]
fn trimmed_source_range(
    range: &SourceRange,
    leading_chars: usize,
    trimmed_chars: usize,
) -> SourceRange {
    if range.start.spine != range.end.spine || range.start.node != range.end.node {
        return range.clone();
    }
    let leading_chars = u64::try_from(leading_chars).unwrap_or(u64::MAX);
    let trimmed_chars = u64::try_from(trimmed_chars).unwrap_or(u64::MAX);
    let start = range.start.text_offset.saturating_add(leading_chars);
    let end = start.saturating_add(trimmed_chars);
    if end > range.end.text_offset || start >= end {
        return range.clone();
    }
    SourceRange {
        start: SourceAnchor {
            spine: range.start.spine.clone(),
            node: range.start.node.clone(),
            text_offset: start,
        },
        end: SourceAnchor {
            spine: range.end.spine.clone(),
            node: range.end.node.clone(),
            text_offset: end,
        },
    }
}

fn semantic_batch_len(chunks: &[StoredChunk]) -> usize {
    let mut payload_chars = 0;
    for (index, chunk) in chunks.iter().take(MAX_BATCH_ITEMS).enumerate() {
        let next = chunk.input.payload_chars();
        if index > 0 && payload_chars + next > MAX_BATCH_CHARS {
            return index;
        }
        payload_chars += next;
    }
    chunks.len().clamp(1, MAX_BATCH_ITEMS)
}

async fn request_embeddings(
    client: &Client,
    provider: &AiProvider,
    model: &str,
    input: &[&EmbeddingInput],
) -> Result<Vec<Vec<f32>>, String> {
    let input = embedding_request_input(input);
    let response = client
        .post(embeddings_url(&provider.base_url))
        .bearer_auth(provider.api_key.trim())
        .json(&json!({ "model": model, "input": input }))
        .send()
        .await
        .map_err(|error| format!("Embedding 请求失败：{error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("读取 Embedding 响应失败：{error}"))?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|payload| {
                payload
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or(body);
        return Err(format!("Embedding 服务返回 {status}：{message}"));
    }
    let payload: EmbeddingResponse =
        serde_json::from_str(&body).map_err(|error| format!("Embedding 响应格式无效：{error}"))?;
    reorder_embeddings(payload.data, input.len())
}

fn embedding_request_input(input: &[&EmbeddingInput]) -> Vec<serde_json::Value> {
    input
        .iter()
        .map(|input| match input {
            EmbeddingInput::Text(text) => json!(text),
        })
        .collect()
}

fn reorder_embeddings(items: Vec<EmbeddingItem>, expected: usize) -> Result<Vec<Vec<f32>>, String> {
    let mut ordered = BTreeMap::new();
    for item in items {
        if item.index >= expected || item.embedding.is_empty() {
            return Err("Embedding 响应包含无效的 index 或空向量".into());
        }
        if item.embedding.iter().any(|value| !value.is_finite()) {
            return Err("Embedding 响应包含非有限数值".into());
        }
        if ordered.insert(item.index, item.embedding).is_some() {
            return Err("Embedding 响应包含重复的 index".into());
        }
    }
    let mut vectors = Vec::with_capacity(expected);
    for index in 0..expected {
        vectors.push(
            ordered
                .remove(&index)
                .ok_or_else(|| format!("Embedding 响应缺少第 {index} 项"))?,
        );
    }
    let dimensions = vectors.first().map_or(0, Vec::len);
    if dimensions == 0 || vectors.iter().any(|vector| vector.len() != dimensions) {
        return Err("Embedding 响应的向量维度不一致".into());
    }
    Ok(vectors)
}

fn embeddings_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/embeddings") {
        return base.to_owned();
    }
    if let Some(prefix) = base.strip_suffix("/chat/completions") {
        return format!("{prefix}/embeddings");
    }
    format!("{base}/embeddings")
}

fn embedding_identity(provider: &AiProvider, model: &str) -> EmbeddingIdentity {
    let normalized_base = provider.base_url.trim().trim_end_matches('/');
    let fingerprint = hash_text(&format!(
        "semantic-v3\n{}\n{}\n{}",
        provider.id,
        normalized_base.to_ascii_lowercase(),
        model.trim(),
    ));
    EmbeddingIdentity {
        fingerprint,
        provider_name: provider.name.clone(),
        model: model.trim().to_owned(),
    }
}

fn display_book_title(source: &dyn BookSource) -> String {
    let title = source.book().metadata.title.trim();
    if title.is_empty() {
        source.book().id.to_string()
    } else {
        title.to_owned()
    }
}

fn hash_text(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn hash_bytes(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn content_fingerprint(chunks: &[SemanticChunk]) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for chunk in chunks {
        hasher.update(chunk.key.as_bytes());
        hasher.update([0]);
        hasher.update(chunk.content_hash.as_bytes());
        hasher.update([0]);
        let range = serde_json::to_vec(&chunk.range)
            .map_err(|error| format!("序列化正文位置失败：{error}"))?;
        hasher.update(range);
    }
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

fn semantic_database_path() -> Result<PathBuf, String> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or_else(|| "无法确定语义索引目录".to_owned())?;
    std::fs::create_dir_all(project.data_local_dir())
        .map_err(|error| format!("创建语义索引目录失败：{error}"))?;
    Ok(project.data_local_dir().join(DATABASE_FILE))
}

async fn record_index_failure(path: PathBuf, book_id: String, fingerprint: String, error: String) {
    let result = tokio::task::spawn_blocking(move || {
        let store = SemanticStore::open(&path)?;
        store.mark_book_failed(&book_id, &fingerprint, &error)
    })
    .await;
    match result {
        Err(task_error) => {
            tracing::warn!(%task_error, "failed to record semantic index failure");
        }
        Ok(Err(store_error)) => {
            tracing::warn!(%store_error, "failed to record semantic index failure");
        }
        Ok(Ok(())) => {}
    }
}

struct SemanticStore {
    connection: Connection,
}

impl SemanticStore {
    fn open(path: &Path) -> Result<Self, String> {
        rebook_sqlite_vec_extension::register();
        let connection =
            Connection::open(path).map_err(|error| format!("打开语义索引数据库失败：{error}"))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("设置语义索引数据库超时失败：{error}"))?;
        let schema_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(|error| format!("读取语义索引版本失败：{error}"))?;
        if schema_version != 2 {
            connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                     DROP TABLE IF EXISTS semantic_embeddings;
                     DROP TABLE IF EXISTS semantic_pending_embeddings;
                     DROP TABLE IF EXISTS semantic_chunks;
                     DROP TABLE IF EXISTS semantic_books;
                     DROP TABLE IF EXISTS semantic_meta;
                     PRAGMA user_version = 2;
                     PRAGMA foreign_keys = ON;",
                )
                .map_err(|error| format!("重建语义索引结构失败：{error}"))?;
        }
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS semantic_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS semantic_books (
                    id INTEGER PRIMARY KEY,
                    book_id TEXT NOT NULL UNIQUE,
                    title TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    content_fingerprint TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'pending',
                    total_chunks INTEGER NOT NULL DEFAULT 0,
                    indexed_chunks INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS semantic_chunks (
                    id INTEGER PRIMARY KEY,
                    book_key INTEGER NOT NULL REFERENCES semantic_books(id) ON DELETE CASCADE,
                    chunk_key TEXT NOT NULL,
                    section_index INTEGER NOT NULL,
                    section_title TEXT NOT NULL,
                    block_kind TEXT NOT NULL,
                    text TEXT NOT NULL,
                    source_range_json TEXT NOT NULL,
                    modality TEXT NOT NULL DEFAULT 'text',
                    image_href TEXT,
                    image_preview BLOB,
                    content_hash TEXT NOT NULL DEFAULT '',
                    embedded INTEGER NOT NULL DEFAULT 0,
                    UNIQUE(book_key, chunk_key)
                 );
                 CREATE TABLE IF NOT EXISTS semantic_pending_embeddings (
                    chunk_id INTEGER PRIMARY KEY
                        REFERENCES semantic_chunks(id) ON DELETE CASCADE,
                    book_key INTEGER NOT NULL,
                    embedding BLOB NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS semantic_chunks_book_pending
                    ON semantic_chunks(book_key, embedded);
                 CREATE INDEX IF NOT EXISTS semantic_pending_embeddings_book
                    ON semantic_pending_embeddings(book_key);",
            )
            .map_err(|error| format!("初始化语义索引数据库失败：{error}"))?;
        Ok(Self { connection })
    }

    fn activate_identity(&mut self, identity: &EmbeddingIdentity) -> Result<(), String> {
        let active = self.meta("embedding_fingerprint")?;
        if active
            .as_deref()
            .is_some_and(|value| value != identity.fingerprint)
        {
            self.clear_index()?;
        }
        self.set_meta("embedding_fingerprint", &identity.fingerprint)?;
        self.set_meta("embedding_provider", &identity.provider_name)?;
        self.set_meta("embedding_model", &identity.model)
    }

    fn require_active_identity(&self, fingerprint: &str) -> Result<(), String> {
        if self.meta("embedding_fingerprint")?.as_deref() != Some(fingerprint) {
            return Err("Embedding 配置已变化，已停止旧的索引任务".into());
        }
        Ok(())
    }

    fn dimensions(&self) -> Result<Option<usize>, String> {
        self.meta("embedding_dimensions")?
            .map(|value| {
                value
                    .parse::<usize>()
                    .map_err(|error| format!("语义索引维度记录无效：{error}"))
            })
            .transpose()
    }

    fn ensure_dimensions(&mut self, dimensions: usize) -> Result<bool, String> {
        if dimensions == 0 {
            return Err("Embedding 向量维度不能为 0".into());
        }
        if let Some(existing) = self.dimensions()? {
            if existing != dimensions {
                self.invalidate_for_dimension_change(dimensions)?;
                return Ok(true);
            }
        } else {
            self.set_meta("embedding_dimensions", &dimensions.to_string())?;
            self.create_vector_table(dimensions)?;
        }
        Ok(false)
    }

    fn invalidate_for_dimension_change(&mut self, dimensions: usize) -> Result<(), String> {
        self.connection
            .execute_batch(
                "DROP TABLE IF EXISTS semantic_embeddings;
                 DELETE FROM semantic_chunks;
                 DELETE FROM semantic_books;",
            )
            .map_err(|error| format!("清理旧维度语义索引失败：{error}"))?;
        self.set_meta("embedding_dimensions", &dimensions.to_string())?;
        self.create_vector_table(dimensions)
    }

    fn clear_index(&mut self) -> Result<(), String> {
        self.connection
            .execute_batch(
                "DROP TABLE IF EXISTS semantic_embeddings;
                 DELETE FROM semantic_chunks;
                 DELETE FROM semantic_books;
                 DELETE FROM semantic_meta;",
            )
            .map_err(|error| format!("清理失效语义索引失败：{error}"))
    }

    fn create_vector_table(&self, dimensions: usize) -> Result<(), String> {
        self.connection
            .execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS semantic_embeddings USING vec0(
                    chunk_id INTEGER PRIMARY KEY,
                    book_key INTEGER PARTITION KEY,
                    embedding FLOAT[{dimensions}] DISTANCE_METRIC=cosine
                );"
            ))
            .map_err(|error| format!("创建 sqlite-vec 向量表失败：{error}"))
    }

    fn is_book_complete(&self, book_id: &str, fingerprint: &str) -> Result<Option<usize>, String> {
        self.connection
            .query_row(
                "SELECT total_chunks FROM semantic_books
                 WHERE book_id = ?1 AND model_fingerprint = ?2 AND status = 'complete'",
                params![book_id, fingerprint],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.and_then(|value| usize::try_from(value).ok()))
            .map_err(|error| format!("检查书籍语义索引失败：{error}"))
    }

    fn has_complete_books(&self, fingerprint: &str) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM semantic_books
                    WHERE model_fingerprint = ?1 AND status = 'complete'
                 )",
                [fingerprint],
                |row| row.get(0),
            )
            .map_err(|error| format!("检查语义索引状态失败：{error}"))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "book preparation atomically reconciles metadata, chunks, and resumable state"
    )]
    fn prepare_book(
        &mut self,
        book_id: &str,
        title: &str,
        fingerprint: &str,
        chunks: &[SemanticChunk],
    ) -> Result<Vec<StoredChunk>, String> {
        self.require_active_identity(fingerprint)?;
        let content_fingerprint = content_fingerprint(chunks)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("开始语义索引事务失败：{error}"))?;
        let existing = transaction
            .query_row(
                "SELECT id, content_fingerprint, model_fingerprint FROM semantic_books
                 WHERE book_id = ?1",
                [book_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("读取书籍语义索引失败：{error}"))?;
        if let Some((book_key, stored_content, stored_model)) = existing
            && (stored_content != content_fingerprint || stored_model != fingerprint)
        {
            let has_vector_table = transaction
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM sqlite_master
                        WHERE type = 'table' AND name = 'semantic_embeddings'
                     )",
                    [],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|error| format!("检查语义向量表失败：{error}"))?;
            if has_vector_table {
                transaction
                    .execute(
                        "DELETE FROM semantic_embeddings WHERE book_key = ?1",
                        [book_key],
                    )
                    .map_err(|error| format!("删除书籍旧向量失败：{error}"))?;
            }
            transaction
                .execute("DELETE FROM semantic_books WHERE id = ?1", [book_key])
                .map_err(|error| format!("删除书籍旧分段失败：{error}"))?;
        }

        transaction
            .execute(
                "INSERT INTO semantic_books (
                    book_id, title, model_fingerprint, content_fingerprint, status,
                    total_chunks, indexed_chunks, last_error, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, 0, NULL, ?6)
                 ON CONFLICT(book_id) DO UPDATE SET
                    title = excluded.title,
                    model_fingerprint = excluded.model_fingerprint,
                    content_fingerprint = excluded.content_fingerprint,
                    total_chunks = excluded.total_chunks,
                    status = 'pending',
                    last_error = NULL,
                    updated_at = excluded.updated_at",
                params![
                    book_id,
                    title,
                    fingerprint,
                    content_fingerprint,
                    i64::try_from(chunks.len()).unwrap_or(i64::MAX),
                    unix_timestamp(),
                ],
            )
            .map_err(|error| format!("保存书籍语义索引状态失败：{error}"))?;
        let book_key = transaction
            .query_row(
                "SELECT id FROM semantic_books WHERE book_id = ?1",
                [book_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("读取书籍语义索引标识失败：{error}"))?;
        for chunk in chunks {
            let range_json = serde_json::to_string(&chunk.range)
                .map_err(|error| format!("序列化正文位置失败：{error}"))?;
            transaction
                .execute(
                    "INSERT INTO semantic_chunks (
                        book_key, chunk_key, section_index, section_title,
                        block_kind, text, source_range_json, modality,
                        image_href, image_preview, content_hash, embedded
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)
                     ON CONFLICT(book_key, chunk_key) DO UPDATE SET
                        section_index = excluded.section_index,
                        section_title = excluded.section_title,
                        block_kind = excluded.block_kind,
                        text = excluded.text,
                        source_range_json = excluded.source_range_json,
                        modality = excluded.modality,
                        image_href = excluded.image_href,
                        image_preview = excluded.image_preview,
                        content_hash = excluded.content_hash",
                    params![
                        book_key,
                        chunk.key,
                        i64::try_from(chunk.section_index).unwrap_or(i64::MAX),
                        chunk.section_title,
                        chunk.block_kind,
                        chunk.text,
                        range_json,
                        chunk.modality.as_str(),
                        chunk.image_href,
                        chunk.image_preview,
                        chunk.content_hash,
                    ],
                )
                .map_err(|error| format!("保存正文分段失败：{error}"))?;
        }
        transaction
            .execute(
                "UPDATE semantic_books SET indexed_chunks = (
                    SELECT COUNT(*) FROM semantic_chunks
                    WHERE book_key = semantic_books.id AND embedded = 1
                 ) WHERE id = ?1",
                [book_key],
            )
            .map_err(|error| format!("更新语义索引进度失败：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("提交正文分段失败：{error}"))?;

        let inputs = chunks
            .iter()
            .map(|chunk| (chunk.key.as_str(), chunk.input.clone()))
            .collect::<HashMap<_, _>>();
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, chunk_key FROM semantic_chunks
                 WHERE book_key = ?1 AND embedded = 0 ORDER BY id",
            )
            .map_err(|error| format!("读取待索引正文失败：{error}"))?;
        let rows = statement
            .query_map([book_key], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("读取待索引正文失败：{error}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("读取待索引正文失败：{error}"))?
            .into_iter()
            .map(|(id, key)| {
                inputs
                    .get(key.as_str())
                    .cloned()
                    .map(|input| StoredChunk { id, input })
                    .ok_or_else(|| format!("找不到待索引内容：{key}"))
            })
            .collect()
    }

    fn store_embeddings(
        &mut self,
        book_id: &str,
        title: &str,
        fingerprint: &str,
        chunk_ids: &[i64],
        vectors: &[Vec<f32>],
    ) -> Result<(), String> {
        if chunk_ids.len() != vectors.len() {
            return Err("正文分段与 Embedding 数量不一致".into());
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("开始保存 Embedding 事务失败：{error}"))?;
        let book_key = transaction
            .query_row(
                "SELECT id FROM semantic_books
                 WHERE book_id = ?1 AND model_fingerprint = ?2",
                params![book_id, fingerprint],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("语义索引状态已变化：{error}"))?;
        for (chunk_id, vector) in chunk_ids.iter().zip(vectors) {
            transaction
                .execute(
                    "INSERT INTO semantic_pending_embeddings (chunk_id, book_key, embedding)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(chunk_id) DO UPDATE SET
                        book_key = excluded.book_key,
                        embedding = excluded.embedding",
                    params![chunk_id, book_key, cast_slice::<f32, u8>(vector)],
                )
                .map_err(|error| format!("暂存 Embedding 失败：{error}"))?;
            transaction
                .execute(
                    "UPDATE semantic_chunks SET embedded = 1
                     WHERE id = ?1 AND book_key = ?2",
                    params![chunk_id, book_key],
                )
                .map_err(|error| format!("更新正文分段索引状态失败：{error}"))?;
        }
        transaction
            .execute(
                "UPDATE semantic_books SET
                    title = ?2,
                    indexed_chunks = (
                        SELECT COUNT(*) FROM semantic_chunks
                        WHERE book_key = semantic_books.id AND embedded = 1
                    ),
                    status = 'pending',
                    last_error = NULL,
                    updated_at = ?3
                 WHERE id = ?1",
                params![book_key, title, unix_timestamp()],
            )
            .map_err(|error| format!("更新语义索引进度失败：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("提交 Embedding 失败：{error}"))
    }

    fn finalize_book(&mut self, book_id: &str, fingerprint: &str) -> Result<(), String> {
        self.require_active_identity(fingerprint)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("开始完成语义索引事务失败：{error}"))?;
        let book_key = transaction
            .query_row(
                "SELECT id FROM semantic_books
                 WHERE book_id = ?1 AND model_fingerprint = ?2",
                params![book_id, fingerprint],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("读取书籍语义索引状态失败：{error}"))?;
        let (total, indexed) = transaction
            .query_row(
                "SELECT total_chunks, indexed_chunks FROM semantic_books WHERE id = ?1",
                [book_key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|error| format!("读取语义索引进度失败：{error}"))?;
        if total != indexed {
            return Err(format!("语义索引尚未完成：{indexed}/{total}"));
        }
        if total > 0 {
            transaction
                .execute(
                    "DELETE FROM semantic_embeddings WHERE book_key = ?1",
                    [book_key],
                )
                .map_err(|error| format!("替换书籍旧向量失败：{error}"))?;
            let pending = {
                let mut statement = transaction
                    .prepare(
                        "SELECT chunk_id, embedding FROM semantic_pending_embeddings
                         WHERE book_key = ?1 ORDER BY chunk_id",
                    )
                    .map_err(|error| format!("读取待发布 Embedding 失败：{error}"))?;
                let rows = statement
                    .query_map([book_key], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })
                    .map_err(|error| format!("读取待发布 Embedding 失败：{error}"))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|error| format!("读取待发布 Embedding 失败：{error}"))?
            };
            if i64::try_from(pending.len()).unwrap_or(i64::MAX) != total {
                return Err(format!(
                    "待发布 Embedding 数量不完整：{}/{}",
                    pending.len(),
                    total
                ));
            }
            for (chunk_id, embedding) in pending {
                transaction
                    .execute(
                        "INSERT INTO semantic_embeddings (chunk_id, book_key, embedding)
                         VALUES (?1, ?2, ?3)",
                        params![chunk_id, book_key, embedding],
                    )
                    .map_err(|error| format!("发布书籍语义向量失败：{error}"))?;
            }
            transaction
                .execute(
                    "DELETE FROM semantic_pending_embeddings WHERE book_key = ?1",
                    [book_key],
                )
                .map_err(|error| format!("清理已发布 Embedding 失败：{error}"))?;
        }
        transaction
            .execute(
                "UPDATE semantic_books SET status = 'complete', updated_at = ?2
                 WHERE id = ?1",
                params![book_key, unix_timestamp()],
            )
            .map_err(|error| format!("完成书籍语义索引失败：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("提交书籍语义索引完成状态失败：{error}"))
    }

    fn mark_book_failed(
        &self,
        book_id: &str,
        fingerprint: &str,
        error: &str,
    ) -> Result<(), String> {
        self.connection
            .execute(
                "UPDATE semantic_books SET status = 'failed', last_error = ?3, updated_at = ?4
                 WHERE book_id = ?1 AND model_fingerprint = ?2",
                params![book_id, fingerprint, error, unix_timestamp()],
            )
            .map(|_| ())
            .map_err(|store_error| format!("保存语义索引失败状态失败：{store_error}"))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "exact KNN builds separate current-book and indexed-library queries"
    )]
    fn search(
        &self,
        query_vector: &[f32],
        scope: SemanticSearchScope,
        book_id: Option<&str>,
        limit: usize,
        include_images: bool,
    ) -> Result<Vec<SemanticSearchResult>, String> {
        let vector = rusqlite::types::Value::Blob(cast_slice::<f32, u8>(query_vector).to_vec());
        let requested_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let (sql, parameters): (&str, Vec<rusqlite::types::Value>) = match scope {
            SemanticSearchScope::CurrentBook => {
                let Some(book_key) = self
                    .connection
                    .query_row(
                        "SELECT id FROM semantic_books
                         WHERE book_id = ?1 AND status = 'complete'",
                        [book_id.unwrap_or_default()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(|error| format!("读取当前书籍语义索引失败：{error}"))?
                else {
                    return Ok(Vec::new());
                };
                let knn_k = if include_images {
                    requested_limit
                } else {
                    self.connection
                        .query_row(
                            "SELECT COUNT(*) FROM semantic_embeddings WHERE book_key = ?1",
                            [book_key],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(|error| format!("读取当前书籍向量数量失败：{error}"))?
                };
                if knn_k == 0 {
                    return Ok(Vec::new());
                }
                (
                    "SELECT b.book_id, b.title, c.section_index, c.section_title,
                        c.text, c.block_kind, c.source_range_json, c.modality,
                        c.image_href, c.image_preview, v.distance
                 FROM semantic_embeddings AS v
                 JOIN semantic_chunks AS c ON c.id = v.chunk_id
                 JOIN semantic_books AS b ON b.id = c.book_key
                 WHERE v.embedding MATCH ?1 AND k = ?2
                   AND v.book_key = ?3
                   AND (?4 = 1 OR c.modality = 'text')
                 ORDER BY v.distance
                 LIMIT ?5",
                    vec![
                        vector,
                        rusqlite::types::Value::Integer(knn_k),
                        rusqlite::types::Value::Integer(book_key),
                        rusqlite::types::Value::Integer(i64::from(include_images)),
                        rusqlite::types::Value::Integer(requested_limit),
                    ],
                )
            }
            SemanticSearchScope::IndexedBooks => {
                let knn_k = if include_images {
                    requested_limit
                } else {
                    self.connection
                        .query_row(
                            "SELECT COUNT(*) FROM semantic_embeddings AS v
                             JOIN semantic_books AS b ON b.id = v.book_key
                             WHERE b.status = 'complete'",
                            [],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(|error| format!("读取全书库向量数量失败：{error}"))?
                };
                if knn_k == 0 {
                    return Ok(Vec::new());
                }
                (
                    "SELECT b.book_id, b.title, c.section_index, c.section_title,
                        c.text, c.block_kind, c.source_range_json, c.modality,
                        c.image_href, c.image_preview, v.distance
                 FROM semantic_embeddings AS v
                 JOIN semantic_chunks AS c ON c.id = v.chunk_id
                 JOIN semantic_books AS b ON b.id = c.book_key
                 WHERE v.embedding MATCH ?1 AND k = ?2
                   AND v.book_key = b.id AND b.status = 'complete'
                   AND (?3 = 1 OR c.modality = 'text')
                 ORDER BY v.distance
                 LIMIT ?4",
                    vec![
                        vector,
                        rusqlite::types::Value::Integer(knn_k),
                        rusqlite::types::Value::Integer(i64::from(include_images)),
                        rusqlite::types::Value::Integer(requested_limit),
                    ],
                )
            }
        };
        let mut statement = self
            .connection
            .prepare(sql)
            .map_err(|error| format!("准备语义搜索失败：{error}"))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(parameters), |row| {
                let range_json = row.get::<_, String>(6)?;
                let range = serde_json::from_str::<SourceRange>(&range_json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                let distance = row.get::<_, f32>(10)?;
                Ok(SemanticSearchResult {
                    book_id: row.get(0)?,
                    book_title: row.get(1)?,
                    section_index: usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                    section_title: row.get(3)?,
                    text: row.get(4)?,
                    block_kind: row.get(5)?,
                    range,
                    similarity: (1.0 - distance).clamp(-1.0, 1.0),
                    modality: SemanticModality::parse(&row.get::<_, String>(7)?),
                    image_href: row.get(8)?,
                    image_preview: row.get(9)?,
                })
            })
            .map_err(|error| format!("执行语义搜索失败：{error}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| format!("读取语义搜索结果失败：{error}"))
    }

    fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.connection
            .query_row(
                "SELECT value FROM semantic_meta WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("读取语义索引元数据失败：{error}"))
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.connection
            .execute(
                "INSERT INTO semantic_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(|error| format!("保存语义索引元数据失败：{error}"))
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_chunk(key: &str, text: &str, range: SourceRange) -> SemanticChunk {
        SemanticChunk {
            key: key.into(),
            section_index: 0,
            section_title: "Chapter".into(),
            block_kind: "paragraph".into(),
            text: text.into(),
            range,
            modality: SemanticModality::Text,
            image_href: None,
            image_preview: None,
            input: EmbeddingInput::Text(text.into()),
            content_hash: hash_text(text),
        }
    }

    #[test]
    fn embeddings_endpoint_accepts_root_and_completion_urls() {
        assert_eq!(
            embeddings_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("https://example.test/v1/chat/completions"),
            "https://example.test/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("https://example.test/v1/embeddings/"),
            "https://example.test/v1/embeddings"
        );
    }

    #[test]
    fn embedding_response_is_reordered_and_validated() {
        let vectors = reorder_embeddings(
            vec![
                EmbeddingItem {
                    index: 1,
                    embedding: vec![0.0, 1.0],
                },
                EmbeddingItem {
                    index: 0,
                    embedding: vec![1.0, 0.0],
                },
            ],
            2,
        )
        .unwrap();
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn trimmed_chunks_keep_precise_source_offsets_for_navigation() {
        let range = SourceRange {
            start: SourceAnchor {
                spine: rebook_publication::SpineItemId::new("chapter").unwrap(),
                node: "paragraph".into(),
                text_offset: 10,
            },
            end: SourceAnchor {
                spine: rebook_publication::SpineItemId::new("chapter").unwrap(),
                node: "paragraph".into(),
                text_offset: 30,
            },
        };

        let trimmed = trimmed_source_range(&range, 2, 7);

        assert_eq!(trimmed.start.text_offset, 12);
        assert_eq!(trimmed.end.text_offset, 19);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the integration fixture creates two indexed books and verifies both scopes"
    )]
    fn sqlite_vec_returns_exact_cosine_neighbors() {
        let directory =
            std::env::temp_dir().join(format!("torto-semantic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(DATABASE_FILE);
        let identity = EmbeddingIdentity {
            fingerprint: "model-a".into(),
            provider_name: "Test".into(),
            model: "embedding-test".into(),
        };
        let mut store = SemanticStore::open(&path).unwrap();
        store.activate_identity(&identity).unwrap();
        assert!(!store.ensure_dimensions(2).unwrap());
        store
            .connection
            .execute(
                "INSERT INTO semantic_books (
                    book_id, title, model_fingerprint, content_fingerprint, status,
                    total_chunks, indexed_chunks, updated_at
                 ) VALUES ('book-a', 'Book A', 'model-a', 'content', 'complete', 2, 2, 0)",
                [],
            )
            .unwrap();
        let book_key = store.connection.last_insert_rowid();
        let range = SourceRange {
            start: SourceAnchor {
                spine: rebook_publication::SpineItemId::new("chapter").unwrap(),
                node: "p1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: rebook_publication::SpineItemId::new("chapter").unwrap(),
                node: "p1".into(),
                text_offset: 5,
            },
        };
        let range_json = serde_json::to_string(&range).unwrap();
        for (chunk_key, text, vector) in [
            ("one", "systems thinking", vec![0.8_f32, 0.2]),
            ("two", "cooking", vec![0.0_f32, 1.0]),
        ] {
            store
                .connection
                .execute(
                    "INSERT INTO semantic_chunks (
                        book_key, chunk_key, section_index, section_title,
                        block_kind, text, source_range_json, embedded
                     ) VALUES (?1, ?2, 0, 'Chapter', 'paragraph', ?3, ?4, 1)",
                    params![book_key, chunk_key, text, range_json],
                )
                .unwrap();
            let chunk_id = store.connection.last_insert_rowid();
            store
                .connection
                .execute(
                    "INSERT INTO semantic_embeddings (chunk_id, book_key, embedding)
                     VALUES (?1, ?2, ?3)",
                    params![chunk_id, book_key, cast_slice::<f32, u8>(&vector)],
                )
                .unwrap();
        }
        store
            .connection
            .execute(
                "INSERT INTO semantic_chunks (
                    book_key, chunk_key, section_index, section_title,
                    block_kind, text, source_range_json, modality,
                    image_href, image_preview, content_hash, embedded
                 ) VALUES (?1, 'figure', 0, 'Chapter', 'image', '系统结构图', ?2,
                    'image', 'Images/figure.jpg', ?3, 'image-hash', 1)",
                params![book_key, range_json, vec![1_u8, 2, 3]],
            )
            .unwrap();
        let image_chunk_id = store.connection.last_insert_rowid();
        store
            .connection
            .execute(
                "INSERT INTO semantic_embeddings (chunk_id, book_key, embedding)
                 VALUES (?1, ?2, ?3)",
                params![
                    image_chunk_id,
                    book_key,
                    cast_slice::<f32, u8>(&[1.0_f32, 0.0])
                ],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO semantic_books (
                    book_id, title, model_fingerprint, content_fingerprint, status,
                    total_chunks, indexed_chunks, updated_at
                 ) VALUES ('book-b', 'Book B', 'model-a', 'content', 'complete', 1, 1, 0)",
                [],
            )
            .unwrap();
        let second_book_key = store.connection.last_insert_rowid();
        store
            .connection
            .execute(
                "INSERT INTO semantic_chunks (
                    book_key, chunk_key, section_index, section_title,
                    block_kind, text, source_range_json, embedded
                 ) VALUES (?1, 'three', 0, 'Other', 'paragraph', 'closest global', ?2, 1)",
                params![second_book_key, range_json],
            )
            .unwrap();
        let second_chunk_id = store.connection.last_insert_rowid();
        store
            .connection
            .execute(
                "INSERT INTO semantic_embeddings (chunk_id, book_key, embedding)
                 VALUES (?1, ?2, ?3)",
                params![
                    second_chunk_id,
                    second_book_key,
                    cast_slice::<f32, u8>(&[1.0_f32, 0.0])
                ],
            )
            .unwrap();

        let results = store
            .search(
                &[0.99, 0.01],
                SemanticSearchScope::CurrentBook,
                Some("book-a"),
                2,
                false,
            )
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].text, "systems thinking");
        assert!(results[0].similarity > results[1].similarity);
        let image = store
            .search(
                &[1.0, 0.0],
                SemanticSearchScope::CurrentBook,
                Some("book-a"),
                1,
                true,
            )
            .unwrap();
        assert_eq!(image[0].modality, SemanticModality::Image);
        assert_eq!(image[0].image_href.as_deref(), Some("Images/figure.jpg"));
        assert_eq!(
            image[0].image_preview.as_deref(),
            Some([1_u8, 2, 3].as_slice())
        );
        let global = store
            .search(
                &[1.0, 0.0],
                SemanticSearchScope::IndexedBooks,
                None,
                1,
                false,
            )
            .unwrap();
        assert_eq!(global[0].book_id, "book-b");
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn interrupted_index_only_returns_unfinished_chunks() {
        let directory =
            std::env::temp_dir().join(format!("torto-semantic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(DATABASE_FILE);
        let identity = EmbeddingIdentity {
            fingerprint: "model-a".into(),
            provider_name: "Test".into(),
            model: "embedding-test".into(),
        };
        let range = SourceRange {
            start: SourceAnchor {
                spine: rebook_publication::SpineItemId::new("chapter").unwrap(),
                node: "p1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: rebook_publication::SpineItemId::new("chapter").unwrap(),
                node: "p1".into(),
                text_offset: 20,
            },
        };
        let chunks = vec![
            text_chunk("one", "systems thinking", range.clone()),
            text_chunk("two", "feedback loops", range),
        ];
        let mut store = SemanticStore::open(&path).unwrap();
        store.activate_identity(&identity).unwrap();
        store.ensure_dimensions(2).unwrap();
        let pending = store
            .prepare_book("book-a", "Book A", "model-a", &chunks)
            .unwrap();
        assert_eq!(pending.len(), 2);
        store
            .store_embeddings(
                "book-a",
                "Book A",
                "model-a",
                &[pending[0].id],
                &[vec![1.0, 0.0]],
            )
            .unwrap();
        assert!(
            store
                .search(
                    &[1.0, 0.0],
                    SemanticSearchScope::IndexedBooks,
                    None,
                    10,
                    false,
                )
                .unwrap()
                .is_empty()
        );
        drop(store);

        let mut resumed = SemanticStore::open(&path).unwrap();
        resumed.activate_identity(&identity).unwrap();
        let pending = resumed
            .prepare_book("book-a", "Book A", "model-a", &chunks)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            &pending[0].input,
            EmbeddingInput::Text(text) if text == "feedback loops"
        ));
        resumed
            .store_embeddings(
                "book-a",
                "Book A",
                "model-a",
                &[pending[0].id],
                &[vec![0.0, 1.0]],
            )
            .unwrap();
        resumed.finalize_book("book-a", "model-a").unwrap();
        assert_eq!(
            resumed.is_book_complete("book-a", "model-a").unwrap(),
            Some(2)
        );
        drop(resumed);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn changing_model_invalidates_old_vectors_and_dimensions() {
        let directory =
            std::env::temp_dir().join(format!("torto-semantic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(DATABASE_FILE);
        let first = EmbeddingIdentity {
            fingerprint: "model-a".into(),
            provider_name: "Test".into(),
            model: "embedding-a".into(),
        };
        let second = EmbeddingIdentity {
            fingerprint: "model-b".into(),
            provider_name: "Test".into(),
            model: "embedding-b".into(),
        };
        let mut store = SemanticStore::open(&path).unwrap();
        store.activate_identity(&first).unwrap();
        store.ensure_dimensions(2).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO semantic_books (
                    book_id, title, model_fingerprint, content_fingerprint, status,
                    total_chunks, indexed_chunks, updated_at
                 ) VALUES ('book-a', 'Book A', 'model-a', 'content', 'complete', 0, 0, 0)",
                [],
            )
            .unwrap();

        store.activate_identity(&second).unwrap();

        let book_count = store
            .connection
            .query_row("SELECT COUNT(*) FROM semantic_books", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(book_count, 0);
        assert_eq!(store.dimensions().unwrap(), None);
        assert_eq!(
            store.meta("embedding_fingerprint").unwrap().as_deref(),
            Some("model-b")
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
