use super::*;
use base64::Engine as _;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use rebook_publication::{ImageBlock, ImageFormula};
use std::io::Cursor;

pub(super) const PROMPT: &str = include_str!("formulas/prompt.md");
pub(super) const BATCH_PROMPT: &str = include_str!("formulas/batch.md");
mod batch;

pub(super) fn options() -> Value {
    image_options()
}

// Keep the per-image cache contract stable when only batching changes.
fn image_options() -> Value {
    json!({"temperature":0.0,"response_format":{"type":"json_schema","json_schema":{
        "name":"formula_image_transcription","strict":true,"schema":{
            "type":"object","additionalProperties":false,"properties":{
                "status":{"type":"string","enum":["recognized","not_formula","unreadable"]},
                "latex":{"type":["string","null"]},
                "equation_number":{"type":["string","null"]}
            },"required":["status","latex","equation_number"]
        }
    }}})
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
                    let mut first = i;
                    let mut last = i + 1;
                    while first > 0 && matches!(blocks[first - 1], Block::Image(_)) {
                        first -= 1;
                    }
                    while last < blocks.len() && matches!(blocks[last], Block::Image(_)) {
                        last += 1;
                    }
                    let is_caption = |b: Option<&Block>| matches!(b,Some(Block::Text(t)) if t.kind==TextBlockKind::Caption);
                    if is_caption(first.checked_sub(1).and_then(|p| blocks.get(p)))
                        || is_caption(blocks.get(last))
                    {
                        continue;
                    }
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
                    for c in table.rows.iter().flat_map(|r| &r.cells) {
                        text(&c.text, result);
                    }
                }
                Block::Note(n) => visit(&n.blocks, result),
                // Captioned figures and diagrams are not formula candidates.
                _ => {}
            }
        }
    }
    fn text(t: &TextBlock, out: &mut Vec<Candidate>) {
        for i in &t.content {
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
    #[serde(skip)]
    transient: bool,
    status: String,
    latex: Option<String>,
    equation_number: Option<String>,
}

impl Response {
    fn formula(&self) -> Result<Option<ImageFormula>, String> {
        match self.status.as_str() {
            "not_formula" | "unreadable"
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
    let enlarged = image.resize_exact(
        (image.width() as f32 * scale).round().max(1.0) as u32,
        (image.height() as f32 * scale).round().max(1.0) as u32,
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
    if response.status == "unreadable" {
        annotations.push(Annotation::UnreadableFormula { href: href.clone() });
    }
    if let Ok(Some(formula)) = response.formula() {
        annotations.push(Annotation::ImageFormula {
            href: href.clone(),
            formula,
        });
    }
}

async fn flush(
    client: &reqwest::Client,
    provider: &super::super::AiProvider,
    model: &str,
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
    let results = match batch::request_batch(client, provider, model, &batch).await {
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
        if !seen.insert(candidate.image.href.clone()) {
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
            continue;
        }
        // Same per-image contract as before batching: existing verified caches remain valid.
        let key = digest(
            &serde_json::to_vec(&json!([
                PROMPT,
                image_options(),
                provider.id,
                provider.base_url,
                model,
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
            .filter(|r| r.formula().is_ok());
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
            Err(_) => continue,
        };
        let hint = candidate.image.href.path().to_lowercase().contains("math")
            || candidate.image.alt.to_lowercase().contains("equation")
            || candidate.context.to_lowercase().contains("equation");
        if !candidate.inline && !hint && (image.height() > 240 || image.width() > 1600) {
            continue;
        }
        let url = match image_url(image) {
            Ok(url) => url,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let size = url.len() + candidate.context.len() + 512;
        if size > batch::MAX_BYTES {
            failed += 1;
            continue;
        }
        if pending.len() == batch::MAX_IMAGES || bytes + size > batch::MAX_BYTES {
            if flush(
                client,
                provider,
                model,
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
                for c in table.rows.iter_mut().flat_map(|r| &mut r.cells) {
                    text(&mut c.text, href, formula);
                }
            }
            Block::Note(n) => compose(&mut n.blocks, href, formula),
            _ => {}
        }
    }
}

pub(super) fn validate_annotations(section: &Section, annotations: &[Annotation]) -> bool {
    let candidates = candidates(section);
    annotations.iter().all(|a| match a {
        Annotation::UnreadableFormula { href } => candidates.iter().any(|c| &c.image.href == href),
        Annotation::ImageFormula { href, formula } => {
            candidates.iter().any(|c| &c.image.href == href) && validate_formula(formula).is_ok()
        }
        _ => true,
    })
}

#[cfg(test)]
mod tests;
