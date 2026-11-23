//! Structured decisions through the selected System One model.

use super::*;
use citations::WindowCandidate;
use std::ops::Range;

const MAX_QUESTIONS: usize = 16;
const YES: f64 = 0.80;
const NO: f64 = 0.20;
const TRUE: &str =
    "只含外部文献来源信息，如作者年份、书目页码或参考文献编号；书目后所附定位信息可保留。";
const FALSE: &str =
    "正文说明、生卒日期、年份附在叙述性作者名之后、图表交叉引用、数组下标，或含独立正文论述。";
const QUESTION: &str = "在 line 中，target 作为整体是否可独立标注文献出处？";

pub(super) struct Endpoint<'a> {
    provider: &'a super::super::AiProvider,
    model: &'a str,
    url: reqwest::Url,
}

fn provider_url(provider: &super::super::AiProvider) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(provider.base_url.trim()).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url)
}

fn endpoint<'a>(provider: &'a super::super::AiProvider, model: &'a str) -> Option<Endpoint<'a>> {
    let mut url = provider_url(provider)?;
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix("/chat/completions").unwrap_or(path);
    let path = if path.ends_with("/systemone") {
        path.to_owned()
    } else if path.ends_with("/compatible-mode/v1") || path.ends_with("/typesafe/v1") {
        format!("{path}/systemone")
    } else {
        let prefix = path.strip_suffix("/v1").unwrap_or(path);
        format!("{prefix}/typesafe/v1/systemone")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Some(Endpoint {
        provider,
        model,
        url,
    })
}

pub(super) fn selected_endpoint<'a>(
    provider: &'a super::super::AiProvider,
    model: &'a str,
) -> Option<Endpoint<'a>> {
    (model_protocol(provider, model) == super::LayoutModelProtocol::Decision)
        .then(|| endpoint(provider, model))
        .flatten()
}

pub(super) fn supports_images(endpoint: &Endpoint<'_>) -> bool {
    !matches!(endpoint.model, "aliyun/decision" | "decision-model-preview")
}

type ProbeCell = std::sync::Arc<tokio::sync::OnceCell<super::LayoutModelProtocol>>;

static PROBES: LazyLock<std::sync::Mutex<HashMap<String, ProbeCell>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn probe_key(endpoint: &Endpoint<'_>) -> String {
    digest(format!("{}\n{}", endpoint.url, endpoint.model).as_bytes())
}

fn cached_protocol(endpoint: &Endpoint<'_>) -> Option<super::LayoutModelProtocol> {
    PROBES
        .try_lock()
        .ok()?
        .get(&probe_key(endpoint))?
        .get()
        .copied()
}

pub(super) fn model_protocol(
    provider: &super::super::AiProvider,
    model: &str,
) -> super::LayoutModelProtocol {
    use super::LayoutModelProtocol::*;
    let Some(endpoint) = endpoint(provider, model) else {
        return Unconfirmed;
    };
    cached_protocol(&endpoint).unwrap_or_else(|| match model {
        "aliyun/decision" | FIGURE_MODEL => Decision,
        "decision-model-preview"
            if endpoint
                .url
                .path()
                .ends_with("/compatible-mode/v1/systemone") =>
        {
            Decision
        }
        _ => Unconfirmed,
    })
}

fn probe_cache_path(endpoint: &Endpoint<'_>) -> Option<PathBuf> {
    Some(
        crate::environment::paths()?
            .cache_dir()
            .join("layout-model-protocol-v1")
            .join(probe_key(endpoint)),
    )
}

pub(super) fn restore_protocol(provider: &super::super::AiProvider, model: &str) {
    let Some(endpoint) = endpoint(provider, model) else {
        return;
    };
    if cached_protocol(&endpoint).is_some() {
        return;
    }
    let Some(protocol) = probe_cache_path(&endpoint)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<super::LayoutModelProtocol>(&bytes).ok())
        .and_then(|protocol| protocol.as_cache_value().copied())
    else {
        return;
    };
    if let Ok(mut probes) = PROBES.lock() {
        probes.entry(probe_key(&endpoint)).or_insert_with(|| {
            std::sync::Arc::new(tokio::sync::OnceCell::new_with(Some(protocol)))
        });
    }
}

pub(super) async fn probe_model(
    provider: &super::super::AiProvider,
    model: &str,
) -> super::LayoutModelProtocol {
    use super::LayoutModelProtocol::*;
    let Some(endpoint) = endpoint(provider, model) else {
        return Unconfirmed;
    };
    restore_protocol(provider, model);
    let cell = match PROBES.lock() {
        Ok(mut probes) => probes.entry(probe_key(&endpoint)).or_default().clone(),
        Err(_) => return Unconfirmed,
    };
    let result = cell.get_or_try_init(|| async {
        let body = json!({
            "model": model,
            "state": {"number": 2},
            "questions": {"probe": {
                "type": "noul",
                "instructions": "Is the number equal to 2?",
                "criteria": {"true": "The number equals 2.", "false": "The number differs from 2."}
            }}
        });
        let protocol = match request_result(&endpoint, &body, Duration::from_secs(5)).await {
            Ok(answers) if answers.len() == 1 && answers.get("probe").is_some_and(|answer| {
                answer.kind == "noul" && (0.0..=1.0).contains(&answer.noul)
            }) => Decision,
            Err(DecisionError::Unsupported) if model_protocol(provider, model) != Decision => Chat,
            _ => return Err(()),
        };
        if let Some(path) = probe_cache_path(&endpoint) {
            let _ = crate::persistence::write_json_atomic(&path, &protocol);
        }
        Ok(protocol)
    }).await;
    result.copied().unwrap_or(Unconfirmed)
}

const FIGURE_MODEL: &str = "cloudflare/decision-base";
const FIGURE_MAX_IMAGES: usize = 4;
// Clef limits the decoded content of one image to 4 MiB.
const FIGURE_MAX_IMAGE_URL_BYTES: usize =
    (4_usize * 1024 * 1024).div_ceil(3) * 4 + request_images::JPEG_PREFIX.len();
const FIGURE_QUESTION: &str = "结合指定位置的图片和紧邻段落，target 是否确实是这张图的图注？只看绑定的图片，不要使用其他图片。";
const FIGURE_TRUE: &str =
    "文字直接给图命名，或描述图中可辨识的主体、面板、标注或图例，并且在版面上紧邻该图。";
const FIGURE_FALSE: &str =
    "普通正文、引用图号、与图内容不符、属于另一张图、仅是章节标题，或证据不充分。";

impl Endpoint<'_> {
    pub(super) fn figure_identity(&self) -> Value {
        json!([
            "figure-decisions-v2",
            self.provider.id,
            self.model,
            self.url.as_str(),
            FIGURE_QUESTION,
            FIGURE_TRUE,
            FIGURE_FALSE,
            YES,
            NO
        ])
    }

    pub(super) fn identity(&self) -> Value {
        json!([
            "citation-decisions-v2",
            self.provider.id,
            self.model,
            self.url.as_str(),
            QUESTION,
            TRUE,
            FALSE,
            YES,
            NO
        ])
    }
}

// Only use the alternate route where citation validation can complete this
// window. Preserve the normal combined request for any other plausible role.
pub(super) fn citation_only(
    section: &Section,
    target: Range<usize>,
    context: Range<usize>,
    citations: &[WindowCandidate],
) -> bool {
    if context.clone().any(|i| image_needs_caption(section, i)) {
        return false;
    }
    let blocks: Vec<_> = section.blocks.iter().collect();
    if rebook_layout::semantic_paragraph_groups(&blocks)
        .iter()
        .any(|group| group.start < target.end && group.end > target.start)
        || context.clone().any(|i| {
            let block = &section.blocks[i];
            unattributed_quote_body(block).is_some() || code_blocks::body(block).is_some()
        })
    {
        return false;
    }
    for i in context.clone() {
        if let Some(text) = paragraph(&section.blocks[i]) {
            let value = text_block_text(text);
            // Short neighbouring prose may be a standalone credit. Preserve
            // authored quotation boundaries and the full LLM context in that case.
            if i > context.start
                && paragraph(&section.blocks[i - 1]).is_some()
                && quote_sources::credit_like(&value)
            {
                return false;
            }
            if i + 1 < context.end
                && paragraph(&section.blocks[i + 1]).is_some()
                && quote_sources::introduces_quote(&value)
            {
                return false;
            }
            if !plain_prose(text, i, citations, true) {
                return false;
            }
        }
    }
    for i in target {
        let mut texts = Vec::new();
        text_formulas::texts(std::slice::from_ref(&section.blocks[i]), &mut texts);
        if texts
            .iter()
            .any(|text| !plain_prose(text, i, citations, false))
        {
            return false;
        }
    }
    true
}

fn plain_prose(
    text: &TextBlock,
    block: usize,
    citations: &[WindowCandidate],
    protect_quote: bool,
) -> bool {
    use rebook_publication::TextBaseline;

    if text.content.iter().any(|inline| match inline {
        Inline::Text(run) => run.style.baseline != TextBaseline::Normal,
        _ => true,
    }) {
        return false;
    }
    let plain = text_block_text(text);
    let chars: Vec<_> = plain.chars().collect();
    let ranges: Vec<_> = citations
        .iter()
        .filter_map(|candidate| {
            let span = &candidate.span;
            if candidate.block != block
                || text.source.as_ref() != Some(&candidate.source)
                || span.end > chars.len()
            {
                return None;
            }
            // Keep attached brackets such as a[12] and f(…) on the existing
            // math/code path. A terminal bracket group might be an inline credit.
            if span
                .start
                .checked_sub(1)
                .is_some_and(|i| chars[i].is_ascii_alphanumeric() || chars[i] == '_')
                || span
                    .text
                    .chars()
                    .any(|ch| math_script(ch) || "=+-*/\\^_|<>＝＋－×÷±−≤≥≠≈∑∫√".contains(ch))
                || (protect_quote && !chars[span.end..].iter().any(|ch| ch.is_alphabetic()))
            {
                return None;
            }
            Some(span.start..span.end)
        })
        .collect();
    chars.into_iter().enumerate().all(|(i, ch)| {
        ranges.iter().any(|span| span.contains(&i))
            || (ch.is_alphabetic() && !math_script(ch))
            || (ch.is_whitespace() && ch != '\n')
            || ".,:;!?'\"‘’“”，。；：！？、".contains(ch)
    })
}

fn math_script(ch: char) -> bool {
    matches!(ch, '\u{0370}'..='\u{03ff}' | '\u{1f00}'..='\u{1fff}' | '\u{1d400}'..='\u{1d7ff}')
}

fn request_body(model: &str, candidates: &[&WindowCandidate]) -> Value {
    let mut state = serde_json::Map::new();
    let mut questions = serde_json::Map::new();
    for candidate in candidates {
        let span = &candidate.span;
        let line: String = candidate
            .paragraph_text
            .chars()
            .skip(span.start.saturating_sub(76))
            .take(span.end - span.start.saturating_sub(76) + 76)
            .collect();
        state.insert(
            candidate.id.clone(),
            json!({"line":line,"target":span.text}),
        );
        questions.insert(
            candidate.id.clone(),
            json!({
                "type":"noul",
                "instructions":format!("只判断 {}。{QUESTION}", candidate.id),
                "criteria":{"true":TRUE,"false":FALSE}
            }),
        );
    }
    json!({"model":model,"state":state,"questions":questions})
}

#[derive(Deserialize, Serialize)]
struct Answer {
    #[serde(rename = "type")]
    kind: String,
    noul: f64,
}

fn valid(answers: &BTreeMap<String, Answer>, candidates: &[&WindowCandidate]) -> bool {
    answers.len() == candidates.len()
        && candidates.iter().all(|candidate| {
            answers
                .get(&candidate.id)
                .is_some_and(|answer| answer.kind == "noul" && (0.0..=1.0).contains(&answer.noul))
        })
}

enum DecisionError {
    Unsupported,
    Failed,
}

async fn request_result(
    endpoint: &Endpoint<'_>,
    body: &Value,
    timeout: Duration,
) -> Result<BTreeMap<String, Answer>, DecisionError> {
    #[derive(Deserialize)]
    struct Response {
        answers: BTreeMap<String, Answer>,
    }
    static CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
        crate::http::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok()
    });
    let mut request = CLIENT
        .as_ref()
        .ok_or(DecisionError::Failed)?
        .post(endpoint.url.clone())
        .timeout(timeout)
        .json(body);
    if !endpoint.provider.api_key.trim().is_empty() {
        request = request.bearer_auth(endpoint.provider.api_key.trim());
    }
    let mut failure = DecisionError::Failed;
    let result = crate::plugins::llm::budgeted(async {
        let mut response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status();
        if !status.is_success() {
            let unsupported = matches!(status.as_u16(), 404 | 405 | 501)
                || (matches!(status.as_u16(), 400 | 422)
                    && response.chunk().await.ok().flatten().is_some_and(|bytes| {
                        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
                        let text = text.to_ascii_lowercase();
                        [
                            "model_not_found",
                            "model_not_supported",
                            "unsupported_model",
                            "unsupported model",
                            "model is not supported",
                            "does not support systemone",
                        ]
                        .iter()
                        .any(|reason| text.contains(reason))
                    }));
            if unsupported {
                failure = DecisionError::Unsupported;
            }
            return Err(format!("Decision API returned HTTP {status}"));
        }
        response
            .json::<Response>()
            .await
            .map_err(|error| error.to_string())
    })
    .await;
    result.map(|response| response.answers).map_err(|_| failure)
}

async fn request(
    endpoint: &Endpoint<'_>,
    body: &Value,
    timeout: Duration,
) -> Option<BTreeMap<String, Answer>> {
    request_result(endpoint, body, timeout).await.ok()
}

pub(super) async fn resolve(
    endpoint: &Endpoint<'_>,
    book_id: &str,
    fingerprint: &str,
    section: &Section,
    candidates: &[WindowCandidate],
) -> Option<Vec<String>> {
    let (known, mut pending): (Vec<_>, Vec<_>) = candidates
        .iter()
        .partition(|candidate| citations::locally_recognized(section, candidate));
    let mut selected: Vec<_> = known
        .into_iter()
        .map(|candidate| candidate.id.clone())
        .collect();
    pending.retain(|candidate| candidate.span.end - candidate.span.start <= 160);
    pending.truncate(MAX_QUESTIONS);
    if pending.is_empty() {
        return Some(selected);
    }
    let body = request_body(endpoint.model, &pending);
    let key = digest(
        &serde_json::to_vec(&json!([book_id, fingerprint, endpoint.identity(), body])).ok()?,
    );
    let path = cache_path(&key);
    let cached = path
        .as_ref()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .filter(|answers| valid(answers, &pending));
    let answers = if let Some(answers) = cached {
        answers
    } else {
        let answers = request(endpoint, &body, Duration::from_secs(2))
            .await
            .filter(|answers| valid(answers, &pending));
        log::event(
            endpoint.provider,
            endpoint.model,
            "citation_decision.batch",
            json!({
                "candidates":pending.len(), "valid_response":answers.is_some()
            }),
        );
        let answers = answers?;
        if let Some(path) = &path {
            let _ = crate::persistence::write_json_atomic(path, &answers);
        }
        answers
    };
    for candidate in pending {
        let probability = answers.get(&candidate.id)?.noul;
        if probability >= YES {
            selected.push(candidate.id.clone());
        }
    }
    Some(selected)
}

pub(super) async fn resolve_figures(
    endpoint: &Endpoint<'_>,
    section: &Section,
    target: Range<usize>,
    context: Range<usize>,
    evidence: &[(usize, String)],
) -> Option<Vec<Proposal>> {
    if evidence.len() > FIGURE_MAX_IMAGES
        || evidence
            .iter()
            .any(|(_, url)| url.len() > FIGURE_MAX_IMAGE_URL_BYTES)
    {
        return Some(Vec::new());
    }
    let Some(pairs) = figure_captions::decision_pairs(section, target.clone(), context.clone(), evidence)
    else { return Some(Vec::new()) };
    let refs: Vec<_> = section.blocks.iter().collect();
    if rebook_layout::semantic_paragraph_groups(&refs)
        .iter()
        .any(|group| group.start < target.end && group.end > target.start)
        || context.clone().any(|i| {
            let block = &section.blocks[i];
            if unattributed_quote_body(block).is_some() || code_blocks::body(block).is_some() {
                return true;
            }
            let Some(text) = paragraph(block) else {
                return target.contains(&i) && !text_formulas::input(block).is_empty();
            };
            let value = text_block_text(text);
            if quote_sources::introduces_quote(&value)
                || value
                    .rsplit_once(['—', '–'])
                    .is_some_and(|(_, credit)| quote_sources::credit_like(credit))
            {
                return true;
            }
            if pairs.iter().any(|(_, neighbors)| neighbors.contains(&i)) {
                return value.chars().count() > 450
                    || value
                        .chars()
                        .any(|ch| math_script(ch) || "=^_|<>＝±×÷∑∫√\n".contains(ch))
                    || text
                        .content
                        .iter()
                        .any(|inline| !matches!(inline, Inline::Text(_)));
            }
            target.contains(&i) && !plain_prose(text, i, &[], true)
        })
    {
        return Some(Vec::new());
    }
    let mut state = serde_json::Map::new();
    let mut questions = serde_json::Map::new();
    for (index, (image, neighbors)) in pairs.iter().enumerate() {
        let Block::Image(block) = &section.blocks[*image] else {
            return Some(Vec::new());
        };
        for &neighbor in neighbors {
            let id = format!("fig{image}_p{neighbor}");
            let target = text_block_text(paragraph(&section.blocks[neighbor])?);
            state.insert(
                id.clone(),
                json!({
                    "image_position": index + 1, "image_block": image, "alt": block.alt,
                    "paragraph_block": neighbor, "target": target,
                    "side": if neighbor < *image { "above" } else { "below" }
                }),
            );
            questions.insert(id.clone(), json!({
                "type":"noul", "instructions":format!("只判断 {id} 的第 {} 张图片。{FIGURE_QUESTION}", index + 1),
                "criteria":{"true":FIGURE_TRUE,"false":FIGURE_FALSE}
            }));
        }
    }
    let images: Vec<_> = evidence.iter().map(|(_, image)| image).collect();
    let body =
        json!({"model":endpoint.model, "state":state, "questions":questions, "images":images});
    let answers = request(endpoint, &body, Duration::from_secs(8)).await;
    let groups = answers
        .and_then(|answers| figure_groups(&pairs, &questions, &answers))
        .filter(|groups| {
            validate_window(
                groups,
                section,
                &RecognitionRoles::default(),
                target,
                context,
            )
            .is_ok()
        });
    log::event(
        endpoint.provider,
        endpoint.model,
        "figure_decision.batch",
        json!({
            "images": images.len(), "candidates": questions.len(), "resolved": groups.is_some()
        }),
    );
    groups
}

fn figure_groups(
    pairs: &[(usize, Vec<usize>)],
    questions: &serde_json::Map<String, Value>,
    answers: &BTreeMap<String, Answer>,
) -> Option<Vec<Proposal>> {
    if answers.len() != questions.len() {
        return None;
    }
    let mut groups = Vec::new();
    for (image, neighbors) in pairs {
        let mut caption = None;
        let mut ambiguous = false;
        for neighbor in neighbors {
            let answer = answers.get(&format!("fig{image}_p{neighbor}"))?;
            if answer.kind != "noul" || !(0.0..=1.0).contains(&answer.noul) {
                return None;
            }
            if answer.noul >= YES {
                ambiguous |= caption.replace(*neighbor).is_some();
            } else if answer.noul > NO {
                ambiguous = true;
            }
        }
        if let Some(caption) = caption.filter(|_| !ambiguous) {
            groups.push(Proposal::Figure {
                images: vec![*image],
                captions: vec![caption],
            });
        }
    }
    Some(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::semantic_layout::tests::{image, section, text};
    use crate::plugins::{AiModelConfig, AiProvider};

    #[test]
    fn decision_endpoint_preserves_the_layout_providers_origin() {
        let mut provider = AiProvider {
            base_url: "https://example.com/prefix/v1/chat/completions".into(),
            models: vec![AiModelConfig::language("aliyun/decision")],
            ..Default::default()
        };
        assert_eq!(
            selected_endpoint(&provider, "aliyun/decision").unwrap().url.as_str(),
            "https://example.com/prefix/typesafe/v1/systemone"
        );
        provider.base_url =
            "https://workspace.cn-beijing.maas.aliyuncs.com/compatible-mode/v1".into();
        provider.models = vec![AiModelConfig::language("decision-model-preview")];
        assert_eq!(
            selected_endpoint(&provider, "decision-model-preview").unwrap().url.path(),
            "/compatible-mode/v1/systemone"
        );
        provider.models.clear();
        assert!(selected_endpoint(&provider, "normal-llm").is_none());
    }

    #[test]
    fn other_roles_keep_the_combined_request() {
        let plain = section(vec![text("plain", "The finding has been replicated.")]);
        assert!(citation_only(&plain, 0..1, 0..1, &[]));
        let citation = "The findings (see Bell, 2019) provide further evidence.";
        let only = section(vec![text("n1", citation)]);
        assert!(citation_only(
            &only,
            0..1,
            0..1,
            &citations::window_candidates(&only, 0..1)
        ));
        for body in [
            section(vec![text("n1", &format!("{citation} Consider f(x)=x+1."))]),
            section(vec![text(
                "n1",
                &format!("{citation} Assign a[12] to the index."),
            )]),
            section(vec![text("n1", "The relation (x + 2 = y) follows.")]),
            section(vec![text("n1", citation), text("n2", "—Ada Bell")]),
            section(vec![text("n1", citation), image("img")]),
            section(vec![text("n1", "Thus ends the passage (Bell, 2019).")]),
        ] {
            let range = 0..body.blocks.len();
            let spans = citations::window_candidates(&body, range.clone());
            assert!(!spans.is_empty());
            assert!(!citation_only(&body, range.clone(), range, &spans));
        }
    }

    #[test]
    fn typed_questions_validate_each_candidate_id() {
        let body = section(vec![text(
            "n1",
            "The report (see Bell, 2019) shares data with the next study (see Xu, 2012) directly.",
        )]);
        let spans = citations::window_candidates(&body, 0..1);
        let candidates: Vec<_> = spans.iter().collect();
        let input = request_body("aliyun/decision", &candidates);
        assert_eq!(spans.len(), 2);
        assert_eq!(input["questions"].as_object().unwrap().len(), 2);
        assert_eq!(input["questions"]["c0_0_0"]["type"], "noul");
        assert_eq!(input["state"]["c0_0_1"]["target"], "(see Xu, 2012)");
        let mut answers = BTreeMap::from([(
            spans[0].id.clone(),
            Answer {
                kind: "noul".into(),
                noul: 0.96,
            },
        )]);
        assert!(!valid(&answers, &candidates));
        answers.insert(
            spans[1].id.clone(),
            Answer {
                kind: "noul".into(),
                noul: 0.08,
            },
        );
        assert!(valid(&answers, &candidates));
        answers.get_mut(&spans[1].id).unwrap().noul = 1.01;
        assert!(!valid(&answers, &candidates));
    }

    fn decision_server(
        respond: impl FnOnce(Value) -> Value + Send + 'static,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        decision_server_status("200 OK", respond)
    }

    fn decision_server_status(
        status: &'static str,
        respond: impl FnOnce(Value) -> Value + Send + 'static,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                if let Ok((socket, _)) = listener.accept() {
                    break socket;
                }
                assert!(std::time::Instant::now() < deadline, "no decision request");
                std::thread::sleep(Duration::from_millis(5));
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /typesafe/v1/systemone "));
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let body = respond(request).to_string();
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        (address, server)
    }

    #[test]
    fn probe_detects_a_custom_alias_and_reuses_the_response() {
        let (address, server) = decision_server(|request| {
            assert_eq!(request["model"], "my-custom-alias");
            assert_eq!(request["state"], json!({"number": 2}));
            json!({"answers":{"probe":{"type":"noul","noul":0.99}}})
        });
        let provider = AiProvider {
            base_url: format!("http://{address}/v1"),
            ..Default::default()
        };
        use super::super::LayoutModelProtocol::{Decision, Unconfirmed};
        assert_eq!(model_protocol(&provider, "my-custom-alias"), Unconfirmed);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            runtime.block_on(probe_model(&provider, "my-custom-alias")),
            Decision
        );
        server.join().unwrap();
        assert_eq!(
            runtime.block_on(probe_model(&provider, "my-custom-alias")),
            Decision
        );
        assert!(selected_endpoint(&provider, "my-custom-alias").is_some());
        assert!(selected_endpoint(&provider, "another-model").is_none());
    }

    #[test]
    fn chat_layout_model_fills_only_an_unconfigured_image_model() {
        let (address, server) = decision_server_status("400 Bad Request", |request| {
            assert_eq!(request["model"], "layout-llm");
            json!({"error":{"code":"model_not_supported"}})
        });
        let mut settings = PluginSettings::default().with_test_model();
        let provider = &mut settings.providers[0];
        provider.base_url = format!("http://{address}/v1");
        provider.models.push(AiModelConfig::language("layout-llm"));
        settings.semantic_layout.provider = provider.id.clone();
        settings.semantic_layout.model = "layout-llm".into();
        settings.semantic_layout.reasoning_effort = ReasoningEffort::Low;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            runtime.block_on(probe_model(provider, "layout-llm")),
            super::super::LayoutModelProtocol::Chat
        );
        server.join().unwrap();
        settings.fill_layout_image_model();
        assert_eq!(settings.semantic_layout.image_model, "layout-llm");
        assert_eq!(
            settings.semantic_layout.image_provider,
            settings.semantic_layout.provider
        );
        assert_eq!(
            settings.semantic_layout.image_reasoning_effort,
            ReasoningEffort::Low
        );

        settings.semantic_layout.image_model = "separately-configured-model".into();
        settings.fill_layout_image_model();
        assert_eq!(
            settings.semantic_layout.image_model,
            "separately-configured-model"
        );

        settings.semantic_layout.image_model.clear();
        settings.semantic_layout.model = "aliyun/decision".into();
        settings.fill_layout_image_model();
        assert!(settings.semantic_layout.image_model.is_empty());
    }

    #[test]
    fn figure_decision_receives_jpeg_and_selects_the_adjacent_caption() {
        let (address, server) = decision_server(|request| {
            assert_eq!(request["model"], FIGURE_MODEL);
            let images = request["images"].as_array().unwrap();
            assert_eq!(images.len(), 1);
            let image = request_images::decode_url(images[0].as_str().unwrap())
                .unwrap()
                .into_rgb8();
            let [red, green, blue] = image.get_pixel(image.width() / 2, image.height() / 2).0;
            assert!(red > 200 && green < 40 && blue < 40);
            assert_eq!(request["state"]["fig1_p0"]["image_position"], 1);
            assert_eq!(request["state"]["fig1_p2"]["target"], "A red square");
            json!({"answers":{
                "fig1_p0":{"type":"noul","noul":0.20},
                "fig1_p2":{"type":"noul","noul":0.80}
            }})
        });
        let mut settings = PluginSettings::default().with_test_model();
        let provider = &mut settings.providers[0];
        provider.base_url = format!("http://{address}/v1");
        provider.api_key = "fixture".into();
        provider.models.push(AiModelConfig::language(FIGURE_MODEL));
        settings.semantic_layout.enabled = true;
        settings.semantic_layout.provider = provider.id.clone();
        settings.semantic_layout.model = FIGURE_MODEL.into();

        let section = section(vec![
            text("prose", "Shapes can vary."),
            image("square"),
            text("caption", "A red square"),
        ]);
        let source = super::super::tests::picture_source(section.clone());
        let client = crate::http::builder().build().unwrap();
        let (groups, skipped) = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(recognize_windows(
                &client,
                &settings,
                &section,
                "visual-decision-test",
                &fingerprint(&section),
                0..3,
                true,
                None,
                Some(source.as_ref()),
            ))
            .unwrap();
        server.join().unwrap();
        assert_eq!(skipped, 0);
        assert!(
            matches!(groups.as_slice(), [Annotation::Figure { images, captions, before: false }]
            if images[0].start.node == "square" && captions[0].start.node == "caption")
        );
    }

    #[test]
    fn complete_decision_batch_replaces_the_chat_request() {
        let (address, server) = decision_server(|request| {
            let questions = request["questions"].as_object().unwrap();
            assert_eq!(questions.len(), 2);
            let mut ids = questions.keys();
            json!({"answers":{
                (ids.next().unwrap()):{"type":"noul","noul":0.80},
                (ids.next().unwrap()):{"type":"noul","noul":0.20}
            }})
        });

        let mut settings = PluginSettings::default().with_test_model();
        let provider = &mut settings.providers[0];
        provider.base_url = format!("http://{address}/v1");
        provider.api_key = "fixture".into();
        provider
            .models
            .push(AiModelConfig::language("aliyun/decision"));
        settings.semantic_layout.enabled = true;
        settings.semantic_layout.provider = provider.id.clone();
        settings.semantic_layout.model = "aliyun/decision".into();
        let raw = section(vec![text(
            "n1",
            "The report (as documented by Bell, 2019) shares data (adjusted in 2012) with other sources.",
        )]);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime
            .block_on(recognize(
                &raw,
                &format!("citation-decision-fixture-{address}"),
                &settings,
            ))
            .unwrap();
        assert!(matches!(
            result.annotations.as_slice(),
            [Annotation::InlineCitations { source, spans }]
                if source.start.node == "n1" && spans.len() == 1 && spans[0].text == "(as documented by Bell, 2019)"
        ));
        server.join().unwrap();
        let plain = section(vec![text("plain", "The finding has been replicated.")]);
        let result = runtime
            .block_on(recognize(
                &plain,
                &format!("citation-decision-empty-fixture-{address}"),
                &settings,
            ))
            .unwrap();
        assert!(result.annotations.is_empty());
    }
}
