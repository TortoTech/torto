use super::*;

pub(super) const MAX_IMAGES: usize = 5;
pub(super) const MAX_BYTES: usize = 6 * 1024 * 1024;

pub(super) struct Pending {
    pub key: String,
    pub path: Option<PathBuf>,
    pub candidate: Candidate,
    pub aliases: Vec<PublicationUrl>,
    pub url: String,
}

#[derive(Clone)]
struct Input {
    id: usize,
    context: Value,
    original: String,
}

/// Match by ID and retain independently valid results. Missing, conflicting or
/// malformed items remain pending; a broken sibling cannot erase a good result.
#[cfg(test)]
fn parse(content: &str, ids: &[usize]) -> HashMap<usize, Response> {
    parse_results(content, ids)
}

fn parse_results(content: &str, ids: &[usize]) -> HashMap<usize, Response> {
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
    let value = super::super::wire::decode(&value);
    if value.as_object().is_none_or(|o| o.len() != 1) {
        return HashMap::new();
    }
    let Some(items) = value["results"].as_array() else {
        return HashMap::new();
    };
    let mut seen = HashMap::new();
    let mut result = HashMap::new();
    for value in items {
        let Some(id) = value["image_id"]
            .as_u64()
            .and_then(|id| usize::try_from(id).ok())
            .filter(|id| ids.contains(id))
        else {
            continue;
        };
        if let Some(previous) = seen.get(&id) {
            if previous != &value {
                result.remove(&id);
            }
            continue;
        }
        seen.insert(id, value);
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
        let response = match response.formula() {
            Ok(_) => response,
            Err(_) if response.status == "recognized" => Response {
                status: "unreadable".into(),
                latex: None,
                equation_number: None,
                transient: true,
            },
            Err(_) => continue,
        };
        result.insert(item.image_id, response);
    }
    result
}

async fn stage(
    client: &reqwest::Client,
    provider: &super::super::super::AiProvider,
    model: &str,
    reasoning_effort: ReasoningEffort,
    items: &[Input],
) -> Result<HashMap<usize, Response>, String> {
    crate::plugins::llm::budgeted(async {
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
        let mut input_values = vec![json!({"requested_ids":ids,"retry":attempt>0})];
        let mut content = vec![
            json!({"type":"text","text":super::super::wire::encode(&input_values[0]).to_string()}),
        ];
        for item in pending {
            let input = json!({"image_id":item.id,"metadata":item.context});
            content.push(json!({"type":"text","text":super::super::wire::encode(&input).to_string()}));
            input_values.push(input);
            content.push(json!({"type":"image_url","image_url":{"url":item.original}}));
        }
        log::event(
            provider,
            model,
            "formula.batch.request",
            json!({"images":ids.len(),"attempt":attempt+1}),
        );
        let messages = vec![
            json!({"role":"system","content":super::super::wire::instructions(&request_prompt(), &input_values)}),
            json!({"role":"user","content":content}),
        ];
        let response = ai::request_completion(
            client,
            provider,
            model,
            &messages,
            None,
            Some(16384),
            reasoning_effort,
            Some(&super::super::wire::options(&options())),
        )
        .await;
        let message = match response {
            Ok(message) => message,
            Err(error) if !results.is_empty() => {
                log::event(
                    provider,
                    model,
                    "formula.batch.retry_failed",
                    json!({"error":error}),
                );
                break;
            }
            Err(error) => return Err(error),
        };
        let text = ai::message_content(&message).unwrap_or_default();
        results.extend(parse_results(&text, &ids));
    }
    Ok(results)
    }).await
}

pub(super) async fn request_batch(
    client: &reqwest::Client,
    provider: &super::super::super::AiProvider,
    model: &str,
    reasoning_effort: ReasoningEffort,
    batch: &[Pending],
) -> Result<Vec<Option<Response>>, String> {
    crate::plugins::llm::budgeted(async {
        let inputs: Vec<_> = batch
            .iter()
            .enumerate()
            .map(|(id, p)| Input {
                id,
                context: json!({"inline":p.candidate.inline,"context":p.candidate.context}),
                original: p.url.clone(),
            })
            .collect();
        let mut results = stage(client, provider, model, reasoning_effort, &inputs).await?;
        Ok((0..batch.len()).map(|id| results.remove(&id)).collect())
    })
    .await
}

#[cfg(test)]
mod tests;
