use super::*;
use base64::Engine as _;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use rebook_publication::{ImageBlock, ImageFormula, ImageText};
use std::io::Cursor;

pub(super) const CHAPTER_PROMPT: &str = include_str!("formulas/chapter-v5-prompt.md");
pub(super) const PROMPT: &str = include_str!("formulas/prompt.md");
pub(super) const TRANSCRIBE_PROMPT: &str = include_str!("formulas/transcribe.md");
const SYMBOL_PROMPT: &str = include_str!("formulas/symbols.md");
const REPLACEMENT_PROMPT: &str = include_str!("formulas/replacement.md");
pub(super) const TEXT_REVISION: u32 = 7;
mod batch;

// Keep the chapter/group identity stable. Image policy has its own revision and
// fingerprint, so updating it must not invalidate unrelated semantic work.
pub(super) fn chapter_cache_options() -> Value {
    let item = json!({"type":"object","additionalProperties":false,"properties":{
        "image_id":{"type":"integer","description":"Copy an ID from requested_ids. IDs need not be consecutive."},
        "status":{"type":"string","enum":["recognized","recognized_text","not_formula","unreadable","unreadable_text"],"description":"recognized: the whole image is mathematics or a formal rule. recognized_text: the whole meaningful image is faithful Unicode text, including letters or script examples. not_formula: diagrams, plots, tables, photos, decorations or mixed visual/text images. unreadable: formula details are unclear; unreadable_text: text details are unclear or unsupported. Never infer characters from the surrounding words."},
        "text":{"type":["string","null"],"description":"Exact visible Unicode text for recognized_text only; otherwise null. Preserve case, accents, combining characters, punctuation and meaningful spaces. No Markdown, XML/HTML, LaTeX, explanation or guessed characters."},
        "literal":{"type":"boolean","description":"For recognized_text, true for character/script/pronunciation examples and literal symbols which must remain unchanged in translation; false for ordinary prose/words. Otherwise false."},
        "latex":{"type":["string","null"],"description":"Transcribe faithful supported LaTeX without dollar delimiters or Markdown. For a negative status, return null. Use standard math commands, text operators and matrix/aligned environments. Exclude custom macros, packages, images, URLs and executable commands. Encode each command backslash as two JSON backslashes; encode a row separator as four. Do not drop backslashes or turn command prefixes into control characters."},
        "equation_number":{"type":["string","null"],"description":"Copy only a separate equation number visible in this image. Exclude it from latex. Do not infer it from context. If absent or the status is negative, return null."}
    },"required":["image_id","status","latex","equation_number","text","literal"]});
    json!({"temperature":0.0,"output_schema":{
            "type":"object","additionalProperties":false,
            "properties":{"results":{"type":"array","description":"Return one result per requested ID, including negative results. Use an array for a single image too. IDs must match exactly; order is unrestricted.","items":item}},
            "required":["results"]
        }
    })
}

fn current_image_options() -> Value {
    let item = json!({"type":"object","additionalProperties":false,"properties":{
        "image_id":{"type":"integer","description":"Copy an ID from requested_ids. IDs need not be consecutive."},
        "status":{"type":"string","enum":["recognized","recognized_text","not_formula","unreadable","unreadable_text","recognized_table"],"description":"recognized: the whole image is mathematics or a formal rule. recognized_text: the whole meaningful image is faithful Unicode text, including letters or script examples. recognized_table: a complete legible data table, including merged headers and continued tables. not_formula: diagrams, plots, photos, decorations or mixed visual/text images. unreadable: formula details are unclear; unreadable_text: text details are unclear or unsupported. Never infer characters from the surrounding words."},
        "table_html":{"type":["string","null"],"description":"For recognized_table only, a complete faithful XHTML table using table/thead/tbody/tfoot/tr/th/td/caption/br with rowspan and colspan. Preserve all visible cells, merged/multilevel headers, numbers, units, notes and continuation labels. Escape XML characters; close br as <br/>. No style, images, links, scripts or Markdown. For other statuses null."},
        "text":{"type":["string","null"],"description":"Exact visible Unicode text for recognized_text only; otherwise null. Preserve case, accents, combining characters, punctuation and meaningful spaces. No Markdown, XML/HTML, LaTeX, explanation or guessed characters."},
        "literal":{"type":"boolean","description":"For recognized_text, true for character/script/pronunciation examples and literal symbols which must remain unchanged in translation; false for ordinary prose/words. Otherwise false."},
        "latex":{"type":["string","null"],"description":"Transcribe faithful supported LaTeX without dollar delimiters or Markdown. For a negative status, return null. Use standard math commands, text operators and matrix/aligned environments. Exclude custom macros, packages, images, URLs and executable commands. Encode each command backslash as two JSON backslashes; encode a row separator as four. Do not drop backslashes or turn command prefixes into control characters."},
        "equation_number":{"type":["string","null"],"description":"Copy only a separate equation number visible in this image. Exclude it from latex. Do not infer it from context. If absent or the status is negative, return null."}
    },"required":["image_id","status","latex","equation_number","text","literal","table_html"]});
    json!({"temperature":0.0,"output_schema":{
            "type":"object","additionalProperties":false,
            "properties":{"results":{"type":"array","description":"Return one result per requested ID, including negative results. Use an array for a single image too. IDs must match exactly; order is unrestricted.","items":item}},
            "required":["results"]
        }
    })
}

pub(super) fn options() -> Value {
    let mut value = current_image_options();
    let item = &mut value["output_schema"]["properties"]["results"]["items"];
    item["properties"]["replacement_safe"] = json!({"type":"boolean", "description":"True only if the proposed text or LaTeX preserves the complete meaning under ordinary reader reflow and typography. Otherwise retain the original image; use not_formula and null content. When uncertain, false."});
    item["required"].as_array_mut().unwrap().push(json!("replacement_safe"));
    value
}

pub(super) fn request_prompt() -> String {
    format!("{PROMPT}\n{TRANSCRIBE_PROMPT}\n{SYMBOL_PROMPT}\n{REPLACEMENT_PROMPT}")
}

#[derive(Clone)]
pub(super) struct Candidate {
    pub image: ImageBlock,
    pub inline: bool,
    pub context: String,
}

pub(super) fn candidates(section: &Section) -> Vec<Candidate> {
    fn visit(blocks: &[Block], result: &mut Vec<Candidate>) {
        for (i, block) in blocks.iter().enumerate() {
            match block {
                Block::Image(image) if image.text_layer.is_none() => {
                    let context = blocks[i.saturating_sub(1)..(i + 2).min(blocks.len())]
                        .iter()
                        .filter_map(|b| match b {
                            Block::Text(t) => Some(text_block_text(t)),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    result.push(Candidate {
                        image: image.clone(),
                        inline: false,
                        context: context.chars().take(900).collect(),
                    });
                }
                Block::Text(t) => text(t, result),
                Block::Quote(q) => {
                    for t in q.body.iter().chain(q.attribution.iter()) {
                        text(t, result);
                    }
                }
                Block::Table(table) => {
                    for c in table.text_blocks() {
                        text(c, result);
                    }
                }
                Block::Note(n) => visit(&n.blocks, result),
                Block::Figure(figure) => {
                    for image in &figure.images {
                        if image.text_layer.is_none() {
                            result.push(Candidate { image: image.clone(), inline: false,
                                context: figure.caption_blocks().map(text_block_text).collect::<Vec<_>>().join("\n").chars().take(900).collect() });
                        }
                    }
                    visit(&figure.body, result);
                    for caption in &figure.captions { text(caption, result); }
                }
                // The recognizer judges captioned images along with other images.
                _ => {}
            }
        }
    }
    fn text(t: &TextBlock, out: &mut Vec<Candidate>) {
        let marker = t.style.authored_list_marker.and_then(|_| t.content.iter().position(|i|
            !matches!(i, Inline::Text(r) if r.text.trim().is_empty())));
        for (index,i) in t.content.iter().enumerate() {
            if marker == Some(index) { continue }
            if let Inline::Image(run) = i {
                out.push(Candidate {
                    image: run.image.clone(),
                    inline: true,
                    context: text_block_text(t).chars().take(900).collect(),
                });
            }
        }
    }
    let mut result = Vec::new();
    visit(&section.blocks, &mut result);
    result
}

pub(super) fn validate_formula(formula: &ImageFormula) -> Result<(), String> {
    let latex = formula.latex.trim();
    if latex.is_empty()
        || latex.len() > 4096
        || latex.contains('$')
        || [
            "\\include",
            "\\input",
            "\\href",
            "\\url",
            "\\def",
            "\\newcommand",
        ]
        .iter()
        .any(|s| latex.contains(s))
    {
        return Err("Unsupported or empty formula source".into());
    }
    let mut depth = 0_u32;
    for c in latex.chars() {
        if c == '{' {
            depth += 1;
            if depth > 32 {
                return Err("Formula nesting limit".into());
            }
        } else if c == '}' {
            depth = depth.checked_sub(1).ok_or("Unbalanced formula braces")?;
        }
    }
    if depth != 0 {
        return Err("Unbalanced formula braces".into());
    }
    if formula
        .equation_number
        .as_ref()
        .is_some_and(|n| n.trim().is_empty() || n.len() > 32 || n.contains(['\\', '{', '}', '\n']))
    {
        return Err("Invalid equation number".into());
    }
    let rendered = rebook_math::math::render_math(latex, 20.0, "#000000", true)?;
    if rendered.width <= 0.0
        || rendered.ascent + rendered.descent <= 0.0
        || rendered.width > 20000.0
        || rendered.ascent + rendered.descent > 4000.0
    {
        return Err("Invalid formula dimensions".into());
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    #[serde(default)]
    table_html: Option<String>,
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    replacement_safe: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    literal: bool,
    #[serde(skip)]
    transient: bool,
    status: String,
    latex: Option<String>,
    equation_number: Option<String>,
}

impl Response {
    fn formula(&self) -> Result<Option<ImageFormula>, String> {
        if self.status == "recognized_table" {
            if !self.replacement_safe { return Ok(None); }
            if self.text.is_some() || self.latex.is_some() || self.equation_number.is_some() || self.literal { return Err("Invalid table result".into()); }
            rebook_html::parse_recognized_table(self.table_html.as_deref().ok_or("Missing table HTML")?)?;
            return Ok(None);
        }
        if self.table_html.is_some() { return Err("Unexpected table HTML".into()); }
        if !self.replacement_safe && matches!(self.status.as_str(), "recognized" | "recognized_text") {
            return Ok(None);
        }
        if self.status == "recognized_text" {
            if self.latex.is_some() || self.equation_number.is_some() { return Err("Text result contains formula data".into()) }
            validate_text(self.text.as_deref().ok_or("Missing image text")?)?;
            return Ok(None);
        }
        if self.text.is_some() || self.literal { return Err("Unexpected text data in non-text result".into()) }
        match self.status.as_str() {
            "not_formula" | "unreadable" | "unreadable_text"
                if self.latex.is_none() && self.equation_number.is_none() =>
            {
                Ok(None)
            }
            "recognized" => {
                let formula = ImageFormula {
                    latex: self.latex.clone().ok_or("Missing LaTeX")?,
                    equation_number: self.equation_number.clone(),
                };
                validate_formula(&formula)?;
                Ok(Some(formula))
            }
            _ => Err("Invalid formula result".into()),
        }
    }
}

pub(super) fn validate_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > 4096
        || text.contains('\u{fffd}')
        || text.chars().any(|c| c.is_control() && c != '\n')
    { return Err("Empty, damaged or unsupported image text".into()) }
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    if let Ok(reader) = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()
        && let Ok((width, height)) = reader.into_dimensions()
        && u64::from(width) * u64::from(height) > 16_000_000
    {
        return Err("Image exceeds formula decoding limits".into());
    }
    if let Ok(image) = image::load_from_memory(bytes) {
        return Ok(image);
    }
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    let scale = (1600.0 / tree.size().width().max(tree.size().height())).min(1.0);
    let w = (tree.size().width() * scale).ceil().max(1.0) as u32;
    let h = (tree.size().height() * scale).ceil().max(1.0) as u32;
    let mut pixels = resvg::tiny_skia::Pixmap::new(w, h).ok_or("Invalid SVG dimensions")?;
    pixels.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    Ok(DynamicImage::ImageRgba8(
        RgbaImage::from_raw(w, h, pixels.take()).ok_or("Invalid raster")?,
    ))
}

fn image_url(image: DynamicImage) -> Result<String, String> {
    let scale = (1600.0 / image.width().max(image.height()) as f32).min(3.0);
    let size = [
        (image.width() as f32 * scale).round().max(1.0) as u32,
        (image.height() as f32 * scale).round().max(1.0) as u32,
    ];
    let enlarged = rebook_layout::image_processing::resize_exact(
        image,
        size,
        image::imageops::FilterType::Lanczos3,
    );
    let mut canvas = RgbaImage::from_pixel(
        enlarged.width() + 16,
        enlarged.height() + 16,
        Rgba([255, 255, 255, 255]),
    );
    image::imageops::overlay(&mut canvas, &enlarged, 8, 8);
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(canvas)
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    ))
}

fn record(
    response: Option<&Response>,
    href: &PublicationUrl,
    annotations: &mut Vec<Annotation>,
    failed: &mut usize,
) {
    let Some(response) = response else {
        *failed += 1;
        return;
    };
    if response.transient {
        *failed += 1;
    }
    if response.replacement_safe && response.status == "recognized_table" && response.formula().is_ok() {
        if let Ok(table) = rebook_html::parse_recognized_table(response.table_html.as_deref().unwrap_or_default()) {
            annotations.push(Annotation::ImageTable { href: href.clone(), table });
        }
    }
    if response.status == "unreadable" {
        annotations.push(Annotation::UnreadableFormula { href: href.clone() });
    }
    if let Ok(Some(formula)) = response.formula() {
        annotations.push(Annotation::ImageFormula {
            href: href.clone(),
            formula,
        });
    }
    if response.replacement_safe && response.status == "recognized_text" && response.formula().is_ok() {
        annotations.push(Annotation::ImageText {
            href: href.clone(),
            text: ImageText { text: response.text.clone().unwrap(), literal: response.literal, translated: None },
        });
    }
}

async fn flush(
    client: &reqwest::Client,
    provider: &super::super::AiProvider,
    model: &str,
    reasoning_effort: ReasoningEffort,
    pending: &mut Vec<batch::Pending>,
    resolved: &mut HashMap<String, Option<Response>>,
    annotations: &mut Vec<Annotation>,
    failed: &mut usize,
) -> bool {
    if pending.is_empty() {
        return false;
    }
    let batch = std::mem::take(pending);
    let mut unsupported = false;
    let results =
        match batch::request_batch(client, provider, model, reasoning_effort, &batch).await {
            Ok(results) => results,
            Err(error) => {
                log::event(
                    provider,
                    model,
                    "formula.batch.failed",
                    json!({"images":batch.len(),"error":error}),
                );
                let lower = error.to_lowercase();
                unsupported = (lower.contains("image")
                    || lower.contains("vision")
                    || lower.contains("text-only"))
                    && [
                        "not support",
                        "doesn't support",
                        "unsupported",
                        "only supported",
                        "not allowed",
                        "text-only",
                    ]
                    .iter()
                    .any(|s| lower.contains(s));
                vec![None; batch.len()]
            }
        };
    for (item, result) in batch.into_iter().zip(results) {
        if let Some(response) = &result
            && !response.transient
            && let Some(path) = item.path
        {
            let _ = crate::persistence::write_json_atomic(&path, response);
        }
        for href in &item.aliases {
            record(result.as_ref(), href, annotations, failed);
        }
        resolved.insert(item.key, result);
    }
    unsupported
}

pub(super) async fn recognize(
    client: &reqwest::Client,
    provider: &super::super::AiProvider,
    model: &str,
    reasoning_effort: ReasoningEffort,
    source: &dyn BookSource,
    section: &Section,
) -> (Vec<Annotation>, usize) {
    let mut annotations = Vec::new();
    let mut failed = 0;
    let mut seen = HashSet::new();
    let mut resolved: HashMap<String, Option<Response>> = HashMap::new();
    let mut pending: Vec<batch::Pending> = Vec::new();
    let mut bytes = 0;
    for candidate in candidates(section) {
        if !seen.insert((candidate.image.href.clone(), candidate.inline)) {
            continue;
        }
        let resource = match source.resource(&candidate.image.href) {
            Ok(r) => r,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        if resource.bytes.len() > 12 * 1024 * 1024 {
            log::event(provider, model, "image.skipped", json!({"href":candidate.image.href,"reason":"resource_byte_budget","bytes":resource.bytes.len()}));
            continue;
        }
        // The wire contract participates in the fingerprint, not the cache format.
        let key = digest(
            &serde_json::to_vec(&json!([
                "replacement-fidelity-v1",
                PROMPT,
                TRANSCRIBE_PROMPT,
                SYMBOL_PROMPT,
                REPLACEMENT_PROMPT,
                options(),
                provider.id,
                provider.base_url,
                model,
                reasoning_effort,
                candidate.inline,
                digest(&resource.bytes)
            ]))
            .unwrap(),
        );
        if let Some(result) = resolved.get(&key) {
            record(
                result.as_ref(),
                &candidate.image.href,
                &mut annotations,
                &mut failed,
            );
            continue;
        }
        if let Some(item) = pending.iter_mut().find(|p| p.key == key) {
            item.aliases.push(candidate.image.href);
            continue;
        }
        let path = cache_path(&format!("formula-{key}"));
        let cached = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Response>(&b).ok())
            .filter(|r| r.revision == TEXT_REVISION && r.formula().is_ok());
        if let Some(cached) = cached {
            record(
                Some(&cached),
                &candidate.image.href,
                &mut annotations,
                &mut failed,
            );
            resolved.insert(key, Some(cached));
            continue;
        }
        let image = match decode(&resource.bytes) {
            Ok(i) => i,
            Err(error) => {
                log::event(provider, model, "image.skipped", json!({"href":candidate.image.href,"reason":"decode_or_pixel_budget","error":error}));
                continue;
            }
        };
        // Tables and other standalone content can be taller than a formula.
        // Decode/payload budgets bound the work; the model judges its content.
        let url = match image_url(image) {
            Ok(url) => url,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let size = url.len() + candidate.context.len() + 512;
        if size > batch::MAX_BYTES {
            log::event(provider, model, "image.skipped", json!({"href":candidate.image.href,"reason":"request_byte_budget","bytes":size}));
            failed += 1;
            continue;
        }
        if pending.len() == batch::MAX_IMAGES || bytes + size > batch::MAX_BYTES {
            if flush(
                client,
                provider,
                model,
                reasoning_effort,
                &mut pending,
                &mut resolved,
                &mut annotations,
                &mut failed,
            )
            .await
            {
                return (annotations, failed);
            }
            bytes = 0;
        }
        bytes += size;
        pending.push(batch::Pending {
            key,
            path,
            aliases: vec![candidate.image.href.clone()],
            candidate,
            url,
        });
    }
    flush(
        client,
        provider,
        model,
        reasoning_effort,
        &mut pending,
        &mut resolved,
        &mut annotations,
        &mut failed,
    )
    .await;
    (annotations, failed)
}

pub(super) fn compose(blocks: &mut [Block], href: &PublicationUrl, formula: Option<&ImageFormula>) {
    fn text(t: &mut TextBlock, href: &PublicationUrl, f: Option<&ImageFormula>) {
        for i in &mut t.content {
            if let Inline::Image(r) = i
                && &r.image.href == href
            {
                r.image.formula_image = true;
                r.image.formula = f.cloned();
            }
        }
    }
    for block in blocks {
        match block {
            Block::Image(i) if &i.href == href => {
                i.formula_image = true;
                i.formula = formula.cloned();
            }
            Block::Text(t) => text(t, href, formula),
            Block::Quote(q) => {
                for t in q.body.iter_mut().chain(q.attribution.iter_mut()) {
                    text(t, href, formula);
                }
            }
            Block::Table(table) => {
                for c in table.text_blocks_mut() {
                    text(c, href, formula);
                }
            }
            Block::Note(n) => compose(&mut n.blocks, href, formula),
            Block::Figure(figure) => {
                for image in &mut figure.images {
                    if &image.href == href {
                        image.formula_image = true;
                        image.formula = formula.cloned();
                    }
                }
                compose(&mut figure.body, href, formula);
                for caption in &mut figure.captions { text(caption, href, formula); }
            }
            _ => {}
        }
    }
}

pub(super) fn validate_annotations(section: &Section, annotations: &[Annotation]) -> bool {
    let candidates = candidates(section);
    annotations.iter().all(|a| match a {
        Annotation::ImageTable { href, table } => candidates.iter().any(|c| &c.image.href == href) && rebook_html::validate_recognized_table(table).is_ok(),
        Annotation::UnreadableFormula { href } => candidates.iter().any(|c| &c.image.href == href),
        Annotation::ImageFormula { href, formula } => {
            candidates.iter().any(|c| &c.image.href == href) && validate_formula(formula).is_ok()
        }
        Annotation::ImageText { href, text } => candidates.iter().any(|c| &c.image.href == href) && validate_text(&text.text).is_ok(),
        _ => true,
    })
}

pub(super) fn compose_text(blocks: &mut [Block], href: &PublicationUrl, text: &ImageText) {
    fn inline(block: &mut TextBlock, href: &PublicationUrl, text: &ImageText) {
        for item in &mut block.content {
            if let Inline::Image(image) = item && &image.image.href == href {
                image.image.ocr_text = Some(text.clone());
                image.image.formula = None;
                image.image.formula_image = false;
            }
        }
    }
    for block in blocks {
        match block {
            Block::Image(image) if &image.href == href => {
                let mut image = image.clone();
                image.ocr_text = Some(text.clone());
                image.formula = None;
                image.formula_image = false;
                let source = image.source.clone();
                *block = Block::Text(TextBlock { kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Image(Box::new(rebook_publication::InlineImageRun {
                        image, size_scale: 1.0, intrinsic_sizing: false,
                        vertical_align: rebook_publication::InlineImageAlignment::Baseline, presentation: false,
                    }))], style: Default::default(), source });
            }
            Block::Text(t) => inline(t, href, text),
            Block::Quote(q) => for t in q.body.iter_mut().chain(q.attribution.iter_mut()) { inline(t, href, text); },
            Block::Table(table) => for t in table.text_blocks_mut() { inline(t, href, text); },
            Block::Figure(figure) => { compose_text(&mut figure.body, href, text); for t in &mut figure.captions { inline(t, href, text); } },
            Block::Note(note) => compose_text(&mut note.blocks, href, text),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod image_table_regressions;
