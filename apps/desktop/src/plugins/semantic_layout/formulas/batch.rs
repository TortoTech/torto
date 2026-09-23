use super::*;

pub(super) const MAX_IMAGES: usize = 8;
pub(super) const MAX_BYTES: usize = 6 * 1024 * 1024;

pub(super) struct Pending {
    pub key: String,
    pub path: Option<PathBuf>,
    pub candidate: Candidate,
    pub aliases: Vec<PublicationUrl>,
    pub url: String,
}

pub(super) fn options() -> Value {
    let mut item = image_options()["response_format"]["json_schema"]["schema"].clone();
    item["properties"]["image_id"] = json!({"type":"integer"});
    item["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("image_id"));
    json!({"temperature":0.0,"response_format":{"type":"json_schema","json_schema":{
        "name":"formula_image_batch","strict":true,"schema":{
            "type":"object","additionalProperties":false,
            "properties":{"results":{"type":"array","items":item}},"required":["results"]
        }
    }}})
}

#[derive(Clone)]
struct Input {
    id: usize,
    context: Value,
    original: String,
    rendered: Option<String>,
}
impl Input {
    fn bytes(&self) -> usize {
        self.original.len()
            + self.rendered.as_ref().map_or(0, String::len)
            + self.context.to_string().len()
            + 256
    }
}

/// Match by ID and retain independently valid results. Missing, duplicate or
/// malformed items remain pending; a broken sibling cannot erase a good result.
#[cfg(test)]
fn parse(content: &str, ids: &[usize]) -> HashMap<usize, Response> {
    parse_results(content, ids, false)
}

fn parse_results(
    content: &str,
    ids: &[usize],
    retain_invalid_proposals: bool,
) -> HashMap<usize, Response> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Item {
        image_id: usize,
        status: String,
        latex: Option<String>,
        equation_number: Option<String>,
    }
    let Ok(value) = llm_json::parse::<Value>(content) else {
        return HashMap::new();
    };
    if value.as_object().is_none_or(|o| o.len() != 1) {
        return HashMap::new();
    }
    let Some(items) = value["results"].as_array() else {
        return HashMap::new();
    };
    let mut seen = HashSet::new();
    let mut result = HashMap::new();
    for value in items {
        let Some(id) = value["image_id"]
            .as_u64()
            .and_then(|id| usize::try_from(id).ok())
            .filter(|id| ids.contains(id))
        else {
            continue;
        };
        if !seen.insert(id) {
            result.remove(&id);
            continue;
        }
        if value.get("latex").is_none() || value.get("equation_number").is_none() {
            continue;
        }
        let Ok(item) = serde_json::from_value::<Item>(value.clone()) else {
            continue;
        };
        let response = Response {
            status: item.status,
            latex: item.latex,
            equation_number: item.equation_number,
            transient: false,
        };
        if response.formula().is_ok()
            || (retain_invalid_proposals && response.status == "recognized")
        {
            result.insert(item.image_id, response);
        }
    }
    result
}

async fn stage(
    client: &reqwest::Client,
    provider: &super::super::super::AiProvider,
    model: &str,
    items: &[Input],
    mode: &str,
) -> Result<HashMap<usize, Response>, String> {
    let mut results = HashMap::new();
    for attempt in 0..2 {
        let pending: Vec<_> = items
            .iter()
            .filter(|i| !results.contains_key(&i.id))
            .collect();
        if pending.is_empty() {
            break;
        }
        let ids: Vec<_> = pending.iter().map(|i| i.id).collect();
        let mut content = vec![
            json!({"type":"text","text":json!({"mode":mode,"requested_ids":ids,"retry":attempt>0}).to_string()}),
        ];
        for item in pending {
            content.push(json!({"type":"text","text":json!({"image_id":item.id,"metadata":item.context,"next_image":"original"}).to_string()}));
            content.push(json!({"type":"image_url","image_url":{"url":item.original}}));
            if let Some(rendered) = &item.rendered {
                content.push(json!({"type":"text","text":format!("image_id {}: proposed rendering",item.id)}));
                content.push(json!({"type":"image_url","image_url":{"url":rendered}}));
            }
        }
        log::event(
            provider,
            model,
            "formula.batch.request",
            json!({"mode":mode,"images":ids.len(),"attempt":attempt+1}),
        );
        let messages = vec![
            json!({"role":"system","content":format!("{PROMPT}\n{BATCH_PROMPT}")}),
            json!({"role":"user","content":content}),
        ];
        let response = ai::request_completion(
            client,
            provider,
            model,
            &messages,
            None,
            Some(16384),
            ReasoningEffort::Default,
            Some(&options()),
        )
        .await;
        let message = match response {
            Ok(message) => message,
            Err(error) if !results.is_empty() => {
                log::event(
                    provider,
                    model,
                    "formula.batch.retry_failed",
                    json!({"mode":mode,"error":error}),
                );
                break;
            }
            Err(error) => return Err(error),
        };
        let text = ai::message_content(&message).unwrap_or_default();
        results.extend(parse_results(&text, &ids, mode == "transcribe"));
    }
    Ok(results)
}

fn rendered_url(proposal: &Response) -> Result<String, String> {
    // A failed proposal may violate source-size/nesting restrictions. Never
    // bypass those limits merely to create an optional review illustration.
    validate_formula(&ImageFormula {
        latex: proposal.latex.clone().ok_or("Missing LaTeX")?,
        equation_number: None,
    })?;
    let math = rebook_math::math::render_math(
        proposal.latex.as_deref().ok_or("Missing LaTeX")?,
        24.0,
        "#000000",
        true,
    )?;
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="-4 {} {} {}">{}</svg>"##,
        math.width + 8.0,
        math.ascent + math.descent + 8.0,
        -math.ascent - 4.0,
        math.width + 8.0,
        math.ascent + math.descent + 8.0,
        math.svg_fragment
    );
    image_url(decode(svg.as_bytes())?)
}

pub(super) async fn request_batch(
    client: &reqwest::Client,
    provider: &super::super::super::AiProvider,
    model: &str,
    batch: &[Pending],
) -> Result<Vec<Option<Response>>, String> {
    let inputs: Vec<_> = batch
        .iter()
        .enumerate()
        .map(|(id, p)| Input {
            id,
            context: json!({"inline":p.candidate.inline,"context":p.candidate.context}),
            original: p.url.clone(),
            rendered: None,
        })
        .collect();
    let mut results = stage(client, provider, model, &inputs, "transcribe").await?;
    let mut verification = Vec::new();
    for input in &inputs {
        let Some(proposal) = results.get(&input.id) else {
            continue;
        };
        if proposal.status != "recognized" {
            continue;
        }
        // Formula validation includes supported syntax, bounded dimensions and
        // a real local render. Valid self-checked results need no second call.
        let Err(error) = proposal.formula() else {
            continue;
        };
        let mut item = input.clone();
        item.context = json!({"proposal":proposal,"local_validation_error":error,
            "inline":batch[input.id].candidate.inline,"context":batch[input.id].candidate.context});
        item.rendered = rendered_url(proposal).ok();
        // A render is optional for syntax errors; keep the original available.
        if item.bytes() > MAX_BYTES {
            item.rendered = None;
        }
        log::event(
            provider,
            model,
            "formula.batch.review_needed",
            json!({"image_id":input.id,"reason":error,"has_rendering":item.rendered.is_some()}),
        );
        if item.bytes() <= MAX_BYTES {
            verification.push(item);
        }
        // Only anomalous proposals fall back while awaiting review. A broken
        // sibling or network error cannot discard successfully validated items.
        results.insert(
            input.id,
            Response {
                status: "unreadable".into(),
                latex: None,
                equation_number: None,
                transient: true,
            },
        );
    }
    let mut start = 0;
    while start < verification.len() {
        let mut end = start;
        let mut bytes = 0;
        while end < verification.len()
            && end - start < MAX_IMAGES
            && bytes + verification[end].bytes() <= MAX_BYTES
        {
            bytes += verification[end].bytes();
            end += 1;
        }
        match stage(client, provider, model, &verification[start..end], "verify").await {
            Ok(verified) => results.extend(verified),
            Err(error) => log::event(
                provider,
                model,
                "formula.batch.verification_failed",
                json!({"images":end-start,"error":error}),
            ),
        }
        start = end;
    }
    Ok((0..batch.len()).map(|id| results.remove(&id)).collect())
}

#[cfg(test)]
mod tests;
