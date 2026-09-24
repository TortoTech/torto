//! Optional source-backed semantics. Recognition uses original text; composition
//! runs after translation, so translation's block/segment keys never change.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use rebook_publication::{
    Block, Book, BookSource, CaptionPosition, FigureBlock, Inline, PublicationError,
    PublicationUrl, QuoteBlock, RasterResource, Resource, Section, SourceRange, TextBlock,
    TextBlockKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{PluginSettings, ReasoningEffort, ai, llm_json, text_block_text};
mod visible;
pub(crate) use visible::{
    block_ranges, empty_recognition, merge_recognition, needs_recognition, recognition_groups,
};

mod citations;
mod formulas;
mod headings;
mod log;
pub(crate) use log::translation_event;
mod quote_sources;

// This is the on-disk data format, not an application release or prompt revision.
const CACHE_FORMAT_VERSION: u32 = 1;
const WINDOW_CHARS: usize = 16_000;
const WINDOW_BLOCKS: usize = 48;
const OVERLAP: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SemanticLayoutSettings {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
}

impl Default for SemanticLayoutSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: String::new(),
            model: String::new(),
        }
    }
}

// Internal request roles, not user settings. All recognizers are always enabled.
#[derive(Clone)]
struct RecognitionRoles {
    quotes: bool,
    captions: bool,
    headings: bool,
}
impl Default for RecognitionRoles {
    fn default() -> Self {
        Self {
            quotes: true,
            captions: true,
            headings: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Proposal {
    QuoteBefore {
        body: Vec<usize>,
        attribution: usize,
        alignment: Option<QuoteAlignment>,
    },
    QuoteInline {
        body: Vec<usize>,
        credit: String,
        alignment: Option<QuoteAlignment>,
    },
    SectionHeading {
        block: usize,
    },
    Quote {
        body: Vec<usize>,
        attribution: Option<usize>,
        #[serde(default)]
        alignment: Option<QuoteAlignment>,
    },
    Figure {
        images: Vec<usize>,
        captions: Vec<usize>,
    },
    QuoteAttribution {
        quote: usize,
        attribution: Option<usize>,
        body_index: Option<usize>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    groups: Vec<Proposal>,
    #[serde(default)]
    citations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Annotation {
    UnreadableFormula {
        href: PublicationUrl,
    },
    ImageFormula {
        href: PublicationUrl,
        formula: rebook_publication::ImageFormula,
    },
    InlineCitations {
        source: SourceRange,
        spans: Vec<citations::Citation>,
    },
    QuoteBefore {
        body: Vec<SourceRange>,
        attribution: SourceRange,
        alignment: Option<QuoteAlignment>,
    },
    QuoteInline {
        body: Vec<SourceRange>,
        credit: String,
        alignment: Option<QuoteAlignment>,
    },
    SectionHeading {
        source: SourceRange,
    },
    Quote {
        body: Vec<SourceRange>,
        attribution: Option<SourceRange>,
        #[serde(default)]
        alignment: Option<QuoteAlignment>,
    },
    Figure {
        images: Vec<SourceRange>,
        captions: Vec<SourceRange>,
        before: bool,
    },
    QuoteAttribution {
        quote: SourceRange,
        attribution: SourceRange,
        inside: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QuoteAlignment {
    Start,
    Center,
    End,
    Justify,
}

impl QuoteAlignment {
    fn text_alignment(self) -> rebook_publication::TextAlignment {
        use rebook_publication::TextAlignment;
        match self {
            Self::Start => TextAlignment::Start,
            Self::Center => TextAlignment::Center,
            Self::End => TextAlignment::End,
            Self::Justify => TextAlignment::Justify,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Recognition {
    #[serde(default)]
    formulas_checked: bool,
    fingerprint: String,
    annotations: Vec<Annotation>,
    #[serde(default)]
    skipped_groups: usize,
}

#[derive(Default)]
struct WindowResult {
    groups: Vec<Proposal>,
    citations: Vec<String>,
    skipped_groups: usize,
}

#[derive(Serialize, Deserialize)]
struct WindowCache {
    key: String,
    groups: Vec<Proposal>,
    citations: Vec<String>,
}

pub(crate) struct SemanticLayoutSource {
    transaction: RwLock<()>,
    enabled: std::sync::atomic::AtomicBool,
    inner: Arc<dyn BookSource>,
    original: Arc<dyn BookSource>,
    state: RwLock<HashMap<usize, Recognition>>,
    partial: RwLock<HashMap<usize, BTreeMap<(usize, usize), Recognition>>>,
    cache_identity: RwLock<Option<Value>>,
}

impl SemanticLayoutSource {
    pub(crate) fn new(inner: Arc<dyn BookSource>, original: Arc<dyn BookSource>) -> Self {
        Self {
            transaction: RwLock::new(()),
            enabled: std::sync::atomic::AtomicBool::new(true),
            inner,
            original,
            state: RwLock::new(HashMap::new()),
            partial: RwLock::new(HashMap::new()),
            cache_identity: RwLock::new(None),
        }
    }

    pub(crate) fn commit_together<T>(&self, commit: impl FnOnce() -> T) -> T {
        let _guard = self.transaction.write().expect("content commit lock");
        commit()
    }

    pub(crate) fn original(&self) -> Arc<dyn BookSource> {
        self.original.clone()
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.write() {
            state.clear();
        }
        if let Ok(mut partial) = self.partial.write() {
            partial.clear();
        }
    }

    /// A subsection is cached independently, but composed using durable original
    /// source ranges. Never mark the whole chapter complete after a partial scan.
    #[cfg(test)]
    pub(crate) fn install_scope(
        &self,
        index: usize,
        full_hash: &str,
        range: std::ops::Range<usize>,
        result: Recognition,
    ) -> bool {
        let Ok(original) = self.original.parse_section(index) else {
            return false;
        };
        if fingerprint(&original) != full_hash {
            return false;
        }
        self.install_prepared_scope(index, &original, full_hash, range, result)
    }

    pub(crate) fn install_prepared_scope(
        &self,
        index: usize,
        original: &Section,
        full_hash: &str,
        range: std::ops::Range<usize>,
        mut result: Recognition,
    ) -> bool {
        if result.annotations.is_empty() {
            return false;
        }
        if range.end > original.blocks.len() || range.start >= range.end {
            return false;
        }
        if fingerprint(&scope_section(&original, range.clone())) != result.fingerprint {
            return false;
        }
        result.fingerprint = full_hash.to_owned();
        let changed = !result.annotations.is_empty();
        if let Ok(mut partial) = self.partial.write() {
            let groups = partial.entry(index).or_default();
            groups.retain(|(start, end), _| *end <= range.start || range.end <= *start);
            groups.insert((range.start, range.end), result);
        }
        changed
    }

    pub(crate) fn configure(&self, book_id: &str, settings: &PluginSettings) {
        self.enabled.store(
            settings.semantic_layout.enabled,
            std::sync::atomic::Ordering::Relaxed,
        );
        if let Ok(mut identity) = self.cache_identity.write() {
            // Same lock order as cache installation below. A concurrent old
            // prefetch must not repopulate annotations after a model switch.
            self.clear();
            *identity = recognition_identity(book_id, settings);
        }
    }

    /// Apply only inline citation structure to original translation input.
    /// Keeping all block indices intact is essential for translation storage.
    pub(crate) fn prepare_citation_input(&self, index: usize, hash: &str, section: &mut Section) {
        if let Ok(state) = self.state.read()
            && let Some(result) = state
                .get(&index)
                .filter(|result| result.fingerprint == hash)
        {
            apply_translation_citations(section, result);
        }
        if let Ok(state) = self.partial.read()
            && let Some(results) = state.get(&index)
        {
            for result in results.values().filter(|result| result.fingerprint == hash) {
                apply_translation_citations(section, result);
            }
        }
    }

    pub(crate) fn has_recognition(&self, index: usize, hash: &str) -> bool {
        self.state
            .read()
            .ok()
            .is_some_and(|state| state.get(&index).is_some_and(|r| r.fingerprint == hash))
    }

    #[cfg(test)]
    pub(crate) fn install(&self, index: usize, result: Recognition) -> bool {
        let Ok(original) = self.original.parse_section(index) else {
            return false;
        };
        if fingerprint(&original) != result.fingerprint {
            return false;
        }
        let changed = !result.annotations.is_empty();
        if let Ok(mut state) = self.state.write() {
            state.insert(index, result);
        }
        changed
    }
}

impl BookSource for SemanticLayoutSource {
    fn book(&self) -> &Book {
        self.inner.book()
    }
    fn table_of_contents_origin(&self) -> rebook_publication::TableOfContentsOrigin {
        self.inner.table_of_contents_origin()
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let _transaction = self.transaction.read().map_err(|_| {
            PublicationError::InvalidPublication("content commit lock poisoned".into())
        })?;
        let mut section = self.inner.parse_section(index)?;
        if !self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            citations::clear_markers(&mut section.blocks);
            return Ok(section);
        }
        let mut recognition = self
            .state
            .read()
            .ok()
            .and_then(|state| state.get(&index).cloned());
        if recognition.is_none() {
            let identity = self
                .cache_identity
                .read()
                .ok()
                .and_then(|value| value.clone());
            if let Some(identity) = identity {
                let original = self.original.parse_section(index)?;
                recognition = load_recognition(&original, &identity);
                if let Ok(current) = self.cache_identity.read() {
                    if current.as_ref() == Some(&identity) {
                        if let Some(result) = &recognition
                            && let Ok(mut state) = self.state.write()
                        {
                            state.insert(index, result.clone());
                        }
                    } else {
                        recognition = None;
                    }
                }
            }
        }
        let partial = self
            .partial
            .read()
            .ok()
            .and_then(|state| state.get(&index).cloned());
        if recognition.is_some() || partial.is_some() {
            let hash = fingerprint(&self.original.parse_section(index)?);
            if let Some(recognition) = recognition.filter(|result| result.fingerprint == hash) {
                for annotation in &recognition.annotations {
                    compose(&mut section.blocks, annotation);
                }
            } else if let Some(results) = partial {
                for recognition in results.values().filter(|result| result.fingerprint == hash) {
                    for annotation in &recognition.annotations {
                        compose(&mut section.blocks, annotation);
                    }
                }
            }
        }
        Ok(section)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.inner.resource(href)
    }
    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.inner.raster_resource(href)
    }
    fn fixed_page_dimensions(
        &self,
        index: usize,
    ) -> Result<Option<rebook_publication::FixedPageDimensions>, PublicationError> {
        self.inner.fixed_page_dimensions(index)
    }
}

pub(crate) fn fingerprint(section: &Section) -> String {
    digest(&serde_json::to_vec(section).expect("section serializes"))
}

pub(crate) fn scope_section(section: &Section, range: std::ops::Range<usize>) -> Section {
    Section {
        id: section.id.clone(),
        href: section.href.clone(),
        blocks: section.blocks[range].to_vec(),
        anchors: section.anchors.clone(),
    }
}

pub(crate) fn log_scope(
    settings: &PluginSettings,
    index: usize,
    start: usize,
    end: usize,
    prefetch: bool,
) {
    if let Ok((provider, model)) = settings.semantic_layout_endpoint() {
        log::event(
            provider,
            model,
            "scope.scheduled",
            json!({"section_index":index,"start":start,"end":end,"prefetch":prefetch}),
        );
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn paragraph(block: &Block) -> Option<&TextBlock> {
    match block {
        Block::Text(text)
            if text.kind == TextBlockKind::Paragraph
                && text.source.is_some()
                && !text_block_text(text).trim().is_empty() =>
        {
            Some(text)
        }
        _ => None,
    }
}

fn quote_anchor(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Quote(quote) => quote
            .source
            .as_ref()
            .or_else(|| quote.body.iter().find_map(|text| text.source.as_ref())),
        Block::Text(text) if text.kind == TextBlockKind::Blockquote => text.source.as_ref(),
        _ => None,
    }
}

fn unattributed_quote_body(block: &Block) -> Option<&[TextBlock]> {
    quote_anchor(block)?;
    match block {
        Block::Quote(quote) if quote.attribution.is_none() && !quote.body.is_empty() => {
            Some(&quote.body)
        }
        Block::Text(text) if text.kind == TextBlockKind::Blockquote => {
            Some(std::slice::from_ref(text))
        }
        _ => None,
    }
}

fn attribution_text(block: &Block) -> Option<&TextBlock> {
    match block {
        Block::Text(text)
            if matches!(
                text.kind,
                TextBlockKind::Paragraph | TextBlockKind::QuoteAttribution
            ) && text.source.is_some()
                && !text_block_text(text).trim().is_empty() =>
        {
            Some(text)
        }
        _ => None,
    }
}

fn source(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(text) => text.source.as_ref(),
        Block::Image(image) => image.source.as_ref(),
        _ => None,
    }
}

fn input_block(index: usize, block: &Block) -> Value {
    if let Some(text) = paragraph(block) {
        return json!({"id": index, "type": "paragraph", "text": text_block_text(text),
            "align": format!("{:?}", text.style.align)});
    }
    match block {
        Block::Image(image) if image.source.is_some() => {
            json!({"id": index, "type": "image", "alt": image.alt})
        }
        Block::Text(text) if matches!(text.kind, TextBlockKind::Heading(_)) => {
            json!({"id": index, "type": "boundary", "heading": text_block_text(text)})
        }
        // Already recognized semantics are never sent as classification targets.
        _ => json!({"id": index, "type": "protected_boundary"}),
    }
}

fn image_needs_caption(section: &Section, index: usize) -> bool {
    if !matches!(section.blocks.get(index), Some(Block::Image(image)) if image.source.is_some() && image.text_layer.is_none() && image.formula.is_none() && !image.formula_image)
    {
        return false;
    }
    // The parser can leave a recognized caption adjacent to standalone images;
    // the existing renderer already groups those. Protect that entire image run.
    let mut start = index;
    let mut end = index + 1;
    while start > 0 && matches!(section.blocks[start - 1], Block::Image(_)) {
        start -= 1;
    }
    while end < section.blocks.len() && matches!(section.blocks[end], Block::Image(_)) {
        end += 1;
    }
    let caption = |block: Option<&Block>| matches!(block, Some(Block::Text(text)) if text.kind == TextBlockKind::Caption);
    !caption(start.checked_sub(1).and_then(|i| section.blocks.get(i)))
        && !caption(section.blocks.get(end))
}

fn section_input_block(section: &Section, index: usize) -> Value {
    if let Some(body) = unattributed_quote_body(&section.blocks[index]) {
        json!({"id":index,"type":"quote_missing_attribution","body":body.iter().enumerate().map(|(i,text)|
            json!({"index":i,"text":text_block_text(text),"attribution_eligible":i>0 && i+1==body.len() && text.source.is_some()})).collect::<Vec<_>>()})
    } else if matches!(section.blocks[index],Block::Text(ref text) if text.kind==TextBlockKind::QuoteAttribution)
        && index > 0
        && unattributed_quote_body(&section.blocks[index - 1]).is_some()
        && attribution_text(&section.blocks[index]).is_some()
    {
        json!({"id":index,"type":"attribution_candidate","text":text_block_text(attribution_text(&section.blocks[index]).expect("source-backed attribution"))})
    } else if matches!(section.blocks[index], Block::Image(_))
        && !image_needs_caption(section, index)
    {
        json!({"id":index,"type":"protected_boundary"})
    } else {
        input_block(index, &section.blocks[index])
    }
}

const PROMPT: &str = include_str!("semantic_layout/prompt.md");

fn window_prompt(
    section: &Section,
    roles: &RecognitionRoles,
    target: std::ops::Range<usize>,
    context: std::ops::Range<usize>,
) -> String {
    let mut out = String::new();
    for part in PROMPT.split("\n## ") {
        let enabled = if part.starts_with("Headings") {
            roles.headings
                && target
                    .clone()
                    .any(|i| headings::candidate(&section.blocks[i]).is_some())
        } else if part.starts_with("Quotations") {
            roles.quotes
                && target.clone().any(|i| {
                    paragraph(&section.blocks[i]).is_some()
                        || unattributed_quote_body(&section.blocks[i]).is_some()
                })
        } else if part.starts_with("Captions") {
            roles.captions && context.clone().any(|i| image_needs_caption(section, i))
        } else if part.starts_with("Inline citations") {
            !citations::window_candidates(section, target.clone()).is_empty()
        } else {
            true
        };
        if enabled {
            if !out.is_empty() {
                out.push_str("\n## ");
            }
            out.push_str(part);
        }
    }
    out
}

fn request_contract_fingerprint() -> &'static str {
    static FINGERPRINT: LazyLock<String> = LazyLock::new(|| {
        digest(
            &serde_json::to_vec(&json!([
                PROMPT,
                citations::PROMPT,
                formulas::PROMPT,
                formulas::options(),
                completion_options(&RecognitionRoles {
                    quotes: true,
                    captions: false,
                    headings: false
                }),
                completion_options(&RecognitionRoles {
                    quotes: false,
                    captions: true,
                    headings: false
                }),
                completion_options(&RecognitionRoles {
                    quotes: false,
                    captions: false,
                    headings: true
                }),
                WINDOW_CHARS,
                WINDOW_BLOCKS,
                headings::MAX_CHARS,
                OVERLAP
            ]))
            .expect("request contract serializes"),
        )
    });
    FINGERPRINT.as_str()
}

fn completion_options(roles: &RecognitionRoles) -> Value {
    let ids = json!({"type":"array","items":{"type":"integer"}});
    let quote = json!({
        "type":"object", "additionalProperties":false,
        "properties":{
            "kind":{"type":"string","enum":["quote"]},
            "body":ids,
            "attribution":{"type":"integer","description":"Immediately following paragraph containing an explicit author or work credit. Required for every new quote."},
            "alignment":{"type":["string","null"],"enum":["start","center","end","justify",null],"description":"Recommended quote-body alignment for unified typesetting; null retains normal reader behavior."}
        },
        "required":["kind","body","attribution","alignment"]
    });
    let figure = json!({
        "type":"object", "additionalProperties":false,
        "properties":{
            "kind":{"type":"string","enum":["figure"]},
            "images":ids, "captions":ids
        },
        "required":["kind","images","captions"]
    });
    let attribution = json!({
        "type":"object","additionalProperties":false,
        "properties":{
            "kind":{"type":"string","enum":["quote_attribution"]},
            "quote":{"type":"integer"},
            "attribution":{"type":["integer","null"]},
            "body_index":{"type":["integer","null"]}
        },
        "required":["kind","quote","attribution","body_index"]
    });
    let heading = json!({
        "type":"object", "additionalProperties":false,
        "properties":{
            "kind":{"type":"string","enum":["section_heading"]},
            "block":{"type":"integer"}
        },
        "required":["kind","block"]
    });
    let mut items = Vec::new();
    if roles.quotes {
        let mut before = quote.clone();
        before["properties"]["kind"]["enum"] = json!(["quote_before"]);
        before["properties"]["attribution"]["description"] = json!(
            "Immediately preceding paragraph naming the source and explicitly introducing the excerpt with a colon or terminal reporting cue such as 'writes' or 'as follows'."
        );
        let mut inline = quote.clone();
        inline["properties"]["kind"]["enum"] = json!(["quote_inline"]);
        inline["properties"]
            .as_object_mut()
            .unwrap()
            .remove("attribution");
        inline["properties"]["credit"] = json!({"type":"string","description":"Exact nonempty suffix of the last body paragraph, including its credit delimiter (dash, opening parenthesis or line break). Copy the explicitly written author/work credit; never invent it."});
        inline["required"] = json!(["kind", "body", "credit", "alignment"]);
        items.extend([quote, before, inline, attribution]);
    }
    if roles.captions {
        items.push(figure);
    }
    if roles.headings {
        items.push(heading);
    }
    let item = if items.len() == 1 {
        items.remove(0)
    } else {
        json!({"anyOf":items})
    };
    json!({"temperature":0.0,"response_format":{
        "type":"json_schema",
        "json_schema":{
            "name":"ebook_semantic_groups", "strict":true,
            "schema":{
                "type":"object", "additionalProperties":false,
                "properties":{"groups":{"type":"array","items":item}, "citations":{"type":"array","items":{"type":"string"}}},
                "required":["groups","citations"]
            }
        }
    }})
}

fn cache_path(key: &str) -> Option<PathBuf> {
    crate::smoke::project_dirs().map(|dirs| {
        dirs.cache_dir()
            .join("semantic-layout-v1")
            .join(format!("{key}.json"))
    })
}

fn recognition_identity(book_id: &str, settings: &PluginSettings) -> Option<Value> {
    let config = &settings.semantic_layout;
    if !config.enabled {
        return None;
    }
    let (provider, model) = settings.semantic_layout_endpoint().ok()?;
    Some(json!([
        CACHE_FORMAT_VERSION,
        request_contract_fingerprint(),
        book_id,
        provider.id,
        provider.base_url,
        model,
        true,
        true
    ]))
}

fn recognition_path(identity: &Value, hash: &str) -> Option<PathBuf> {
    cache_path(&digest(format!("chapter:{identity}:{hash}").as_bytes()))
}

fn load_recognition(section: &Section, identity: &Value) -> Option<Recognition> {
    let hash = fingerprint(section);
    let bytes = std::fs::read(recognition_path(identity, &hash)?).ok()?;
    let result: Recognition = serde_json::from_slice(&bytes).ok()?;
    if !result.formulas_checked || !formulas::validate_annotations(section, &result.annotations) {
        return None;
    }
    if result.fingerprint != hash {
        return None;
    }
    let locate = |range: &SourceRange| {
        section
            .blocks
            .iter()
            .position(|block| source(block).or_else(|| quote_anchor(block)) == Some(range))
    };
    let groups: Option<Vec<_>> = result
        .annotations
        .iter()
        .filter(|a| {
            !matches!(
                a,
                Annotation::InlineCitations { .. }
                    | Annotation::ImageFormula { .. }
                    | Annotation::UnreadableFormula { .. }
            )
        })
        .map(|a| {
            Some(match a {
                Annotation::InlineCitations { .. }
                | Annotation::ImageFormula { .. }
                | Annotation::UnreadableFormula { .. } => {
                    unreachable!()
                }
                Annotation::QuoteBefore {
                    body,
                    attribution,
                    alignment,
                } => Proposal::QuoteBefore {
                    body: body.iter().map(locate).collect::<Option<Vec<_>>>()?,
                    attribution: locate(attribution)?,
                    alignment: *alignment,
                },
                Annotation::QuoteInline {
                    body,
                    credit,
                    alignment,
                } => Proposal::QuoteInline {
                    body: body.iter().map(locate).collect::<Option<Vec<_>>>()?,
                    credit: credit.clone(),
                    alignment: *alignment,
                },
                Annotation::SectionHeading { source } => Proposal::SectionHeading {
                    block: locate(source)?,
                },
                Annotation::Quote {
                    body,
                    attribution,
                    alignment,
                } => Proposal::Quote {
                    body: body.iter().map(locate).collect::<Option<Vec<_>>>()?,
                    alignment: *alignment,
                    attribution: match attribution {
                        Some(range) => Some(locate(range)?),
                        None => None,
                    },
                },
                Annotation::Figure {
                    images,
                    captions,
                    before,
                } => {
                    let images = images.iter().map(locate).collect::<Option<Vec<_>>>()?;
                    let captions = captions.iter().map(locate).collect::<Option<Vec<_>>>()?;
                    if (captions.first()? < images.first()?) != *before {
                        return None;
                    }
                    Proposal::Figure { images, captions }
                }
                Annotation::QuoteAttribution {
                    quote,
                    attribution,
                    inside,
                } => {
                    let index = section
                        .blocks
                        .iter()
                        .position(|b| quote_anchor(b) == Some(quote))?;
                    if *inside {
                        let body = unattributed_quote_body(&section.blocks[index])?;
                        let child = body
                            .iter()
                            .position(|text| text.source.as_ref() == Some(attribution))?;
                        Proposal::QuoteAttribution {
                            quote: index,
                            attribution: None,
                            body_index: Some(child),
                        }
                    } else {
                        Proposal::QuoteAttribution {
                            quote: index,
                            attribution: Some(locate(attribution)?),
                            body_index: None,
                        }
                    }
                }
            })
        })
        .collect();
    validate_window(
        &groups?,
        section,
        &RecognitionRoles::default(),
        0..section.blocks.len(),
        0..section.blocks.len(),
    )
    .ok()?;
    if !citations::validate_annotations(section, &result.annotations) {
        return None;
    }
    Some(result)
}

#[cfg(test)]
pub(crate) async fn recognize(
    section: &Section,
    book_id: &str,
    settings: &PluginSettings,
) -> Result<Recognition, String> {
    recognize_impl(section, book_id, settings, None, None).await
}

#[cfg(test)]
pub(crate) async fn recognize_with_source(
    section: &Section,
    book_id: &str,
    settings: &PluginSettings,
    source: &dyn BookSource,
) -> Result<Recognition, String> {
    recognize_impl(section, book_id, settings, Some(source), None).await
}

pub(crate) async fn recognize_visible(
    section: &Section,
    target: std::ops::Range<usize>,
    book_id: &str,
    settings: &PluginSettings,
    source: &dyn BookSource,
) -> Result<Recognition, String> {
    recognize_impl(section, book_id, settings, Some(source), Some(target)).await
}

async fn recognize_impl(
    section: &Section,
    book_id: &str,
    settings: &PluginSettings,
    source: Option<&dyn BookSource>,
    target: Option<std::ops::Range<usize>>,
) -> Result<Recognition, String> {
    let (provider, model) = settings.semantic_layout_endpoint()?;
    let started = std::time::Instant::now();
    log::event(
        provider,
        model,
        "chapter.start",
        json!({"book":book_id,"section":section.id,"href":section.href,"blocks":section.blocks.len()}),
    );
    let result = recognize_inner(section, book_id, settings, source, target).await;
    let mut details = json!({"book":book_id,"section":section.id,"href":section.href,"elapsed_ms":started.elapsed().as_millis()});
    match &result {
        Ok(recognition) => {
            details["groups"] = json!(recognition.annotations.len());
            details["skipped_groups"] = json!(recognition.skipped_groups);
        }
        Err(error) => {
            details["error"] = json!(error);
        }
    }
    log::event(
        provider,
        model,
        if result.is_ok() {
            "chapter.complete"
        } else {
            "chapter.failed"
        },
        details,
    );
    result
}

async fn recognize_inner(
    section: &Section,
    book_id: &str,
    settings: &PluginSettings,
    book_source: Option<&dyn BookSource>,
    target: Option<std::ops::Range<usize>>,
) -> Result<Recognition, String> {
    let (provider, model) = settings.semantic_layout_endpoint()?;
    let config = &RecognitionRoles::default();
    let fingerprint = fingerprint(section);
    let mut identity = recognition_identity(book_id, settings);
    if let Some(target) = &target {
        identity = identity
            .map(|identity| json!([identity, "visible-target-v1", target.start, target.end]));
    }
    let target = target.unwrap_or(0..section.blocks.len());
    let target_section = scope_section(section, target.clone());
    if let Some(identity) = &identity
        && let Some(cached) = load_recognition(section, identity)
    {
        return Ok(cached);
    }
    let mut annotations = Vec::new();
    let mut skipped_groups = 0;
    let mut used = HashSet::new();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;
    let formulas_checked =
        book_source.is_some() || formulas::candidates(&target_section).is_empty();
    if let Some(source) = book_source {
        let (recognized, failed) =
            formulas::recognize(&client, provider, model, source, &target_section).await;
        annotations.extend(recognized);
        skipped_groups += failed;
    }
    let mut working = section.clone();
    for annotation in &annotations {
        compose(&mut working.blocks, annotation);
    }
    let section = &working;
    let mut start = target.start;
    while start < target.end {
        let end = window_end(section, start).min(target.end);
        let lo = start.saturating_sub(OVERLAP);
        let hi = (end + OVERLAP).min(section.blocks.len());
        let citation_candidates = citations::window_candidates(section, start..end);
        if !citation_candidates.is_empty()
            || section.blocks[start..end]
                .iter()
                .any(|block| paragraph(block).is_some() || unattributed_quote_body(block).is_some())
        {
            let input =
                unified_window_input(section, config, start..end, lo..hi, &citation_candidates);
            let key = digest(
                &serde_json::to_vec(&json!([
                    CACHE_FORMAT_VERSION,
                    request_contract_fingerprint(),
                    book_id,
                    fingerprint,
                    provider.id,
                    provider.base_url,
                    model,
                    config.quotes,
                    config.captions,
                    input
                ]))
                .map_err(|e| e.to_string())?,
            );
            let path = cache_path(&key);
            let cached = path
                .as_ref()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<WindowCache>(&bytes).ok())
                .filter(|cache| {
                    cache.key == key
                        && validate_citation_ids(&cache.citations, &citation_candidates).is_ok()
                        && validate_window(&cache.groups, section, config, start..end, lo..hi)
                            .is_ok()
                });
            let window = if let Some(cache) = cached {
                WindowResult {
                    groups: cache.groups,
                    citations: cache.citations,
                    skipped_groups: 0,
                }
            } else {
                let groups = request_window_groups(
                    &client,
                    (provider, model),
                    &input,
                    section,
                    config,
                    start..end,
                    lo..hi,
                )
                .await
                .map_err(|error| format!("window {start}..{end}: {error}"))?;
                if let Some(path) = &path
                    && groups.skipped_groups == 0
                    && let Err(error) = crate::persistence::write_json_atomic(
                        path,
                        &WindowCache {
                            key,
                            groups: groups.groups.clone(),
                            citations: groups.citations.clone(),
                        },
                    )
                {
                    tracing::warn!(%error, "failed to cache semantic layout");
                }
                groups
            };
            skipped_groups += window.skipped_groups;
            validate_window(&window.groups, section, config, start..end, lo..hi)?;
            annotations.extend(citations::window_annotations(
                &citation_candidates,
                &window.citations,
            ));
            for group in &window.groups {
                let ids = proposal_ids(group);
                if ids.iter().any(|id| used.contains(id)) {
                    continue;
                }
                used.extend(ids);
                annotations.push(annotation(group, section));
            }
        }
        start = end;
    }
    let result = Recognition {
        formulas_checked,
        fingerprint,
        annotations,
        skipped_groups,
    };
    if let Some(path) = identity
        .as_ref()
        .and_then(|identity| recognition_path(identity, &result.fingerprint))
        && result.skipped_groups == 0
        && result.formulas_checked
        && let Err(error) = crate::persistence::write_json_atomic(&path, &result)
    {
        tracing::warn!(%error, "failed to cache semantic chapter");
    }
    Ok(result)
}

async fn request_window_groups(
    client: &reqwest::Client,
    endpoint: (&super::AiProvider, &str),
    input: &Value,
    section: &Section,
    config: &RecognitionRoles,
    target: std::ops::Range<usize>,
    context: std::ops::Range<usize>,
) -> Result<WindowResult, String> {
    request_groups(client, endpoint, input, section, config, target, context).await
}

fn unified_window_input(
    section: &Section,
    config: &RecognitionRoles,
    target: std::ops::Range<usize>,
    context: std::ops::Range<usize>,
    candidates: &[citations::WindowCandidate],
) -> Value {
    let blocks: Vec<_> = context
        .map(|index| {
            let mut block = section_input_block(section, index);
            if headings::candidate(&section.blocks[index]).is_some()
                && let Some(text) = paragraph(&section.blocks[index])
            {
                block["style"] = headings::style(text);
            }
            let available: Vec<_> = candidates.iter().filter(|c| c.block == index).collect();
            block["citation_candidates"] = json!(
                available
                    .iter()
                    .map(|c| json!({"id":c.id,"paragraph":c.paragraph,"text":c.span.text}))
                    .collect::<Vec<_>>()
            );
            if !available.is_empty() && block.get("text").is_none() && block.get("body").is_none() {
                let mut seen = HashSet::new();
                block["citation_paragraphs"] = json!(
                    available
                        .iter()
                        .filter(|c| seen.insert(c.paragraph))
                        .map(|c| json!({"index":c.paragraph,"text":c.paragraph_text}))
                        .collect::<Vec<_>>()
                );
            }
            block
        })
        .collect();
    let mut input = json!({"target_start":target.start,"target_end_exclusive":target.end,
        "quotes_enabled":config.quotes,"captions_enabled":config.captions,"headings_enabled":config.headings,
        "blocks":blocks,
        "targets":{"classify_headings":target.clone().filter(|i|headings::candidate(&section.blocks[*i]).is_some()).collect::<Vec<_>>(),"classify_blocks":target.clone().filter(|i|paragraph(&section.blocks[*i]).is_some()).collect::<Vec<_>>(),
            "complete_quote_sources":target.clone().filter(|i|unattributed_quote_body(&section.blocks[*i]).is_some()).collect::<Vec<_>>(),
            "classify_citations":candidates.iter().map(|c|&c.id).collect::<Vec<_>>()}});
    if config.headings
        && target
            .clone()
            .any(|i| headings::number(&section.blocks[i]).is_some())
    {
        input["numbered_candidates"] = headings::context(section, target.start);
    }
    input
}

fn validate_citation_ids(
    ids: &[String],
    candidates: &[citations::WindowCandidate],
) -> Result<(), String> {
    let mut seen = HashSet::new();
    if ids
        .iter()
        .any(|id| !seen.insert(id) || !candidates.iter().any(|c| &c.id == id))
    {
        return Err("Use each eligible citation candidate ID at most once; never select context-only or invented IDs".into());
    }
    Ok(())
}

fn window_end(section: &Section, start: usize) -> usize {
    let mut end = start;
    let mut chars = 0;
    while end < section.blocks.len() && end - start < WINDOW_BLOCKS {
        chars += paragraph(&section.blocks[end]).map_or_else(
            || {
                unattributed_quote_body(&section.blocks[end]).map_or(0, |body| {
                    body.iter()
                        .map(|text| text_block_text(text).chars().count())
                        .sum()
                })
            },
            |text| text_block_text(text).chars().count(),
        );
        end += 1;
        if chars >= WINDOW_CHARS {
            break;
        }
    }
    end
}

async fn request_groups(
    client: &reqwest::Client,
    endpoint: (&super::AiProvider, &str),
    input: &Value,
    section: &Section,
    config: &RecognitionRoles,
    target: std::ops::Range<usize>,
    context: std::ops::Range<usize>,
) -> Result<WindowResult, String> {
    let (provider, model) = endpoint;
    let mut messages = vec![
        json!({"role":"system","content":window_prompt(section, config, target.clone(), context.clone())}),
        json!({"role":"user","content":input.to_string()}),
    ];
    let candidates = citations::window_candidates(section, target.clone());
    let mut fallback: Option<WindowResult> = None;
    let mut last_error = String::new();
    for attempt in 0..2 {
        let message = ai::request_completion(
            client,
            provider,
            model,
            &messages,
            None,
            Some(4096),
            ReasoningEffort::Default,
            Some(&completion_options(config)),
        )
        .await?;
        let content = ai::message_content(&message).ok_or("AI排版返回了空内容")?;
        let parsed = llm_json::parse::<Response>(&content)
            .map_err(|e| format!("AI排版格式无效：{e}"))
            .and_then(|response| {
                log::event(provider, model, "window.proposals", json!({"section":section.id,"start":target.start,"end":target.end,"attempt":attempt+1,"groups":response.groups,"citations":response.citations}));
                let validation = validate_window(
                    &response.groups,
                    section,
                    config,
                    target.clone(),
                    context.clone(),
                );
                let validation = validation.and_then(|()| validate_citation_ids(&response.citations, &candidates));
                if validation.is_err() {
                    let mut safe = retain_valid_groups(
                        &response.groups,
                        section,
                        config,
                        target.clone(),
                        context.clone(),
                    );
                    let mut seen = HashSet::new();
                    safe.citations = response.citations.iter().filter(|id| candidates.iter().any(|c| &c.id==*id) && seen.insert((*id).clone())).cloned().collect();
                    safe.skipped_groups += response.citations.len() - safe.citations.len();
                    if fallback
                        .as_ref()
                        .is_none_or(|previous| safe.groups.len()+safe.citations.len() > previous.groups.len()+previous.citations.len())
                    {
                        fallback = Some(safe);
                    }
                }
                validation.map(|()| response)
            });
        // Validate before persisting; malformed output remains retryable.
        match parsed {
            Ok(response) => {
                return Ok(WindowResult {
                    groups: response.groups,
                    citations: response.citations,
                    skipped_groups: 0,
                });
            }
            Err(error) => {
                log::event(
                    provider,
                    model,
                    "window.invalid_output",
                    json!({"section":section.id,"href":section.href,"start":target.start,"end":target.end,"attempt":attempt + 1,"quotes":config.quotes,"captions":config.captions,"error":error}),
                );
                last_error = error;
                messages.push(json!({"role":"assistant","content":content}));
                let feedback = format!(
                    "# Correct the response\n\n## Structural error\n\n{last_error}\n\n## Requirements\n\n\
                     - Use the previous book input and the same response schema.\n\
                     - Use existing eligible IDs, preserve order and adjacency, and avoid overlaps or protected blocks.\n\
                     - Omit groups whose IDs cannot satisfy these structural requirements."
                );
                messages.push(json!({"role":"user","content":feedback}));
            }
        }
    }
    if let Some(safe) = fallback {
        log::event(
            provider,
            model,
            "window.partial",
            json!({"section":section.id,"href":section.href,"start":target.start,"end":target.end,"accepted":safe.groups.len(),"skipped":safe.skipped_groups}),
        );
        return Ok(safe);
    }
    Err(last_error)
}

fn retain_valid_groups(
    groups: &[Proposal],
    section: &Section,
    config: &RecognitionRoles,
    target: std::ops::Range<usize>,
    context: std::ops::Range<usize>,
) -> WindowResult {
    let mut safe = WindowResult::default();
    for group in groups {
        safe.groups.push(group.clone());
        if validate_window(
            &safe.groups,
            section,
            config,
            target.clone(),
            context.clone(),
        )
        .is_err()
        {
            safe.groups.pop();
            safe.skipped_groups += 1;
        }
    }
    safe
}

fn proposal_ids(group: &Proposal) -> Vec<usize> {
    let mut ids = match group {
        Proposal::QuoteBefore {
            body, attribution, ..
        } => body.iter().copied().chain([*attribution]).collect(),
        Proposal::QuoteInline { body, .. } => body.clone(),
        Proposal::SectionHeading { block } => vec![*block],
        Proposal::Quote {
            body, attribution, ..
        } => body
            .iter()
            .copied()
            .chain(attribution.iter().copied())
            .collect::<Vec<_>>(),
        Proposal::Figure { images, captions } => images.iter().chain(captions).copied().collect(),
        Proposal::QuoteAttribution {
            quote, attribution, ..
        } => std::iter::once(*quote)
            .chain(attribution.iter().copied())
            .collect(),
    };
    ids.sort_unstable();
    ids
}

fn consecutive(ids: &[usize]) -> bool {
    !ids.is_empty()
        && ids
            .windows(2)
            .all(|pair| pair[0].checked_add(1) == Some(pair[1]))
}

fn validate_window(
    groups: &[Proposal],
    section: &Section,
    config: &RecognitionRoles,
    target: std::ops::Range<usize>,
    context: std::ops::Range<usize>,
) -> Result<(), String> {
    let mut used = HashSet::new();
    for group in groups {
        let ids = proposal_ids(group);
        let valid_ids = consecutive(&ids)
            && ids.iter().any(|id| target.contains(id))
            && ids
                .iter()
                .all(|id| context.contains(id) && *id < section.blocks.len() && used.insert(*id));
        if !valid_ids {
            return Err(format!("AI排版包含越界、重叠或不连续的段落：{ids:?}"));
        }
        let text_ids = |ids: &[usize]| {
            ids.iter()
                .all(|id| paragraph(&section.blocks[*id]).is_some())
        };
        let valid = match group {
            Proposal::QuoteBefore {
                body, attribution, ..
            } => {
                config.quotes
                    && consecutive(body)
                    && text_ids(body)
                    && attribution.checked_add(1) == body.first().copied()
                    && text_ids(&[*attribution])
                    && quote_sources::introduces_quote(&text_block_text(
                        paragraph(&section.blocks[*attribution]).unwrap(),
                    ))
            }
            Proposal::QuoteInline { body, credit, .. } => {
                config.quotes
                    && consecutive(body)
                    && if body.len() == 1
                        && unattributed_quote_body(&section.blocks[body[0]]).is_some()
                    {
                        unattributed_quote_body(&section.blocks[body[0]])
                            .unwrap()
                            .last()
                            .is_some_and(|text| quote_sources::split_credit(text, credit).is_some())
                    } else {
                        text_ids(body)
                            && body.last().is_some_and(|id| {
                                quote_sources::split_credit(
                                    paragraph(&section.blocks[*id]).unwrap(),
                                    credit,
                                )
                                .is_some()
                            })
                    }
            }
            Proposal::SectionHeading { block } => {
                config.headings && headings::candidate(&section.blocks[*block]).is_some()
            }
            Proposal::Quote {
                body, attribution, ..
            } => {
                config.quotes
                    && consecutive(body)
                    && text_ids(body)
                    && attribution.is_some_and(|id| {
                        body.last().and_then(|last| last.checked_add(1)) == Some(id)
                            && text_ids(&[id])
                            && quote_sources::credit_like(&text_block_text(
                                paragraph(&section.blocks[id]).unwrap(),
                            ))
                    })
            }
            Proposal::Figure { images, captions } => {
                config.captions
                    && consecutive(images)
                    && consecutive(captions)
                    && text_ids(captions)
                    && images.iter().all(|id| image_needs_caption(section, *id))
                    && (images.last().and_then(|id| id.checked_add(1)) == captions.first().copied()
                        || captions.last().and_then(|id| id.checked_add(1))
                            == images.first().copied())
            }
            Proposal::QuoteAttribution {
                quote,
                attribution,
                body_index,
            } => {
                config.quotes
                    && unattributed_quote_body(&section.blocks[*quote]).is_some_and(|body| {
                        match (*attribution, *body_index) {
                            (Some(id), None) => {
                                quote.checked_add(1) == Some(id)
                                    && attribution_text(&section.blocks[id]).is_some_and(|text| {
                                        quote_sources::credit_like(&text_block_text(text))
                                    })
                            }
                            (None, Some(index)) => {
                                index > 0
                                    && index.checked_add(1) == Some(body.len())
                                    && body[index].source.is_some()
                                    && quote_sources::credit_like(&text_block_text(&body[index]))
                            }
                            _ => false,
                        }
                    })
            }
        };
        if !valid {
            return Err(format!(
                "Invalid semantic group {ids:?}: use existing eligible IDs, preserve protected blocks and order, and avoid overlaps. Caption and external attribution IDs must be adjacent to their target. Quote completion requires a quote without attribution and exactly one eligible external credit or last-body index."
            ));
        }
    }
    Ok(())
}

fn annotation(group: &Proposal, section: &Section) -> Annotation {
    let range = |id: &usize| {
        source(&section.blocks[*id])
            .expect("validated source")
            .clone()
    };
    match group {
        Proposal::QuoteBefore {
            body,
            attribution,
            alignment,
        } => Annotation::QuoteBefore {
            body: body.iter().map(range).collect(),
            attribution: range(attribution),
            alignment: *alignment,
        },
        Proposal::QuoteInline {
            body,
            credit,
            alignment,
        } => Annotation::QuoteInline {
            body: body
                .iter()
                .map(|id| {
                    source(&section.blocks[*id])
                        .or_else(|| quote_anchor(&section.blocks[*id]))
                        .unwrap()
                        .clone()
                })
                .collect(),
            credit: credit.clone(),
            alignment: *alignment,
        },
        Proposal::SectionHeading { block } => Annotation::SectionHeading {
            source: range(block),
        },
        Proposal::Quote {
            body,
            attribution,
            alignment,
        } => Annotation::Quote {
            body: body.iter().map(range).collect(),
            attribution: attribution.as_ref().map(range),
            alignment: *alignment,
        },
        Proposal::Figure { images, captions } => Annotation::Figure {
            images: images.iter().map(range).collect(),
            captions: captions.iter().map(range).collect(),
            before: captions[0] < images[0],
        },
        Proposal::QuoteAttribution {
            quote,
            attribution,
            body_index,
        } => Annotation::QuoteAttribution {
            quote: quote_anchor(&section.blocks[*quote])
                .expect("validated quote")
                .clone(),
            attribution: attribution.map_or_else(
                || {
                    unattributed_quote_body(&section.blocks[*quote]).unwrap()[body_index.unwrap()]
                        .source
                        .clone()
                        .unwrap()
                },
                |id| source(&section.blocks[id]).unwrap().clone(),
            ),
            inside: body_index.is_some(),
        },
    }
}

// Resolve against source ranges, never translated block indices. Bilingual
// companion paragraphs have no source and stay attached to their original.
fn compose(blocks: &mut Vec<Block>, annotation: &Annotation) {
    if let Annotation::UnreadableFormula { href } = annotation {
        formulas::compose(blocks, href, None);
        return;
    }
    if let Annotation::ImageFormula { href, formula } = annotation {
        formulas::compose(blocks, href, Some(formula));
        return;
    }
    if let Annotation::InlineCitations { source, spans } = annotation {
        citations::compose(blocks, source, spans);
        return;
    }
    if quote_sources::compose(blocks, annotation) {
        return;
    }
    if let Annotation::SectionHeading { source } = annotation {
        headings::compose(blocks, source);
        return;
    }
    if let Annotation::QuoteAttribution {
        quote,
        attribution,
        inside,
    } = annotation
    {
        complete_quote_attribution(blocks, quote, attribution, *inside);
        return;
    }
    let ranges: Vec<_> = match annotation {
        Annotation::Quote {
            body, attribution, ..
        } => body.iter().chain(attribution).collect(),
        Annotation::Figure {
            images,
            captions,
            before,
        } => {
            if *before {
                captions.iter().chain(images).collect()
            } else {
                images.iter().chain(captions).collect()
            }
        }
        Annotation::QuoteAttribution { .. }
        | Annotation::SectionHeading { .. }
        | Annotation::QuoteBefore { .. }
        | Annotation::QuoteInline { .. }
        | Annotation::InlineCitations { .. }
        | Annotation::ImageFormula { .. }
        | Annotation::UnreadableFormula { .. } => unreachable!(),
    };
    let positions: Option<Vec<_>> = ranges
        .iter()
        .map(|range| {
            blocks
                .iter()
                .position(|block| source(block) == Some(*range))
        })
        .collect();
    let Some(positions) = positions else {
        return;
    };
    let Some(&start) = positions.first() else {
        return;
    };
    let mut end = positions.last().copied().unwrap() + 1;
    if end <= start {
        return;
    }
    if matches!(blocks.get(end), Some(Block::Text(text)) if text.source.is_none()) {
        end += 1;
    }
    let present: Vec<_> = blocks[start..end].iter().filter_map(source).collect();
    if present != ranges
        || blocks[start..end]
            .iter()
            .any(|b| !matches!(b, Block::Text(_) | Block::Image(_)))
    {
        return;
    }
    let spanning = Some(SourceRange {
        start: ranges[0].start.clone(),
        end: ranges.last().unwrap().end.clone(),
    });
    let selected = blocks[start..end].to_vec();
    let replacement = match annotation {
        Annotation::Quote {
            attribution,
            alignment,
            ..
        } => {
            let mut credit: Option<TextBlock> = None;
            let mut body = Vec::new();
            let mut in_credit = false;
            for block in selected {
                let Block::Text(mut text) = block else {
                    return;
                };
                if attribution
                    .as_ref()
                    .is_some_and(|range| text.source.as_ref() == Some(range))
                {
                    in_credit = true;
                }
                text.kind = if in_credit {
                    TextBlockKind::QuoteAttribution
                } else {
                    TextBlockKind::Blockquote
                };
                if in_credit {
                    if let Some(credit) = &mut credit {
                        credit.content.push(Inline::Break);
                        credit.content.extend(text.content);
                    } else {
                        credit = Some(text);
                    }
                } else {
                    text.style.semantic_alignment = alignment.map(QuoteAlignment::text_alignment);
                    body.push(text);
                }
            }
            Block::Quote(QuoteBlock {
                body,
                attribution: credit,
                source: spanning,
            })
        }
        Annotation::Figure { before, .. } => compose_figure(selected, *before, spanning),
        Annotation::QuoteAttribution { .. }
        | Annotation::SectionHeading { .. }
        | Annotation::QuoteBefore { .. }
        | Annotation::QuoteInline { .. }
        | Annotation::InlineCitations { .. }
        | Annotation::ImageFormula { .. }
        | Annotation::UnreadableFormula { .. } => unreachable!(),
    };
    blocks.splice(start..end, [replacement]);
}

fn compose_figure(selected: Vec<Block>, before: bool, source: Option<SourceRange>) -> Block {
    let mut images = Vec::new();
    let mut captions = Vec::new();
    for block in selected {
        match block {
            Block::Image(image) => images.push(image),
            Block::Text(mut text) => {
                text.kind = TextBlockKind::Caption;
                captions.push(text);
            }
            _ => unreachable!("composition only accepts text and image blocks"),
        }
    }
    Block::Figure(FigureBlock {
        images,
        captions,
        caption_position: if before {
            CaptionPosition::Before
        } else {
            CaptionPosition::After
        },
        style: rebook_publication::BlockStyle::default(),
        source,
    })
}

fn complete_quote_attribution(
    blocks: &mut Vec<Block>,
    anchor: &SourceRange,
    credit_source: &SourceRange,
    inside: bool,
) {
    let Some(index) = blocks.iter().position(|block| {
        quote_anchor(block) == Some(anchor) && unattributed_quote_body(block).is_some()
    }) else {
        return;
    };
    let standalone = matches!(blocks[index], Block::Text(_));
    let mut quote = match &blocks[index] {
        Block::Quote(quote) => quote.clone(),
        Block::Text(text) => QuoteBlock {
            body: vec![text.clone()],
            attribution: None,
            source: text.source.clone(),
        },
        _ => return,
    };
    let mut end = index + 1;
    let parts = if inside {
        let Some(child) = quote
            .body
            .iter()
            .position(|text| text.source.as_ref() == Some(credit_source))
        else {
            return;
        };
        if child == 0
            || quote.body[child + 1..]
                .iter()
                .any(|text| text.source.is_some())
        {
            return;
        }
        quote.body.split_off(child)
    } else {
        if standalone
            && let Some(Block::Text(companion)) = blocks.get(end)
            && companion.source.is_none()
        {
            quote.body.push(companion.clone());
            end += 1;
        }
        let Some(Block::Text(credit)) = blocks.get(end) else {
            return;
        };
        if credit.source.as_ref() != Some(credit_source) {
            return;
        }
        let mut parts = vec![credit.clone()];
        end += 1;
        if let Some(Block::Text(companion)) = blocks.get(end)
            && companion.source.is_none()
        {
            parts.push(companion.clone());
            end += 1;
        }
        parts
    };
    let mut parts = parts.into_iter();
    let Some(mut credit) = parts.next() else {
        return;
    };
    credit.kind = TextBlockKind::QuoteAttribution;
    for companion in parts {
        credit.content.push(Inline::Break);
        credit.content.extend(companion.content);
    }
    quote.attribution = Some(credit);
    quote.source = Some(SourceRange {
        start: anchor.start.clone(),
        end: credit_source.end.clone(),
    });
    blocks.splice(index..end, [Block::Quote(quote)]);
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod attribution_tests;

pub(crate) fn apply_translation_citations(section: &mut Section, result: &Recognition) {
    for annotation in &result.annotations {
        if let Annotation::InlineCitations { source, spans } = annotation {
            citations::compose(&mut section.blocks, source, spans);
        }
    }
}
