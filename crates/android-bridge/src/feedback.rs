//! Typed feedback boundary. The form owns no HTTP, credentials or issue IDs.
use crate::*;
use rebook_app_core::feedback::{
    images, model as data, store,
    transport::{Failure, Outcome},
};
use std::io::Cursor;

#[derive(Clone, uniffi::Record)]
pub struct FeedbackEnvironment {
    pub os_version: String,
    pub device: String,
    pub arch: String,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}
#[derive(Clone, uniffi::Record)]
pub struct FeedbackImage {
    pub index: u32,
    pub thumbnail: Vec<u8>,
}
#[derive(uniffi::Record)]
pub struct FeedbackState {
    pub kind: String,
    pub description: String,
    pub count: u32,
    pub valid: bool,
    pub locked: bool,
    pub images: Vec<FeedbackImage>,
    pub context: String,
    pub status: String,
    pub error: Option<String>,
    pub retry_until: u64,
    pub issue_url: Option<String>,
    pub issue_number: Option<u64>,
}
struct State {
    draft: data::Draft,
    thumbnails: Vec<Vec<u8>>,
    locked: bool,
    busy: bool,
    status: String,
    error: Option<String>,
    issue: Option<rebook_app_core::feedback::transport::Issue>,
}
#[derive(uniffi::Object)]
pub struct MobileFeedback {
    root: PathBuf,
    state: Mutex<State>,
}
fn code(f: Failure) -> String {
    match f {
        Failure::Network => "network",
        Failure::RateLimited => "rate_limited",
        Failure::Invalid => "invalid",
        Failure::Conflict => "conflict",
        Failure::Protocol => "protocol",
        Failure::Storage => "storage",
        Failure::Configuration => "configuration",
        Failure::Service => "service",
    }
    .into()
}
fn thumbnail(image: &data::Screenshot) -> Result<Vec<u8>> {
    let image = images::decode(image).map_err(error)?.thumbnail(160, 112);
    let mut bytes = Cursor::new(vec![]);
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(error)?;
    Ok(bytes.into_inner())
}
fn record(s: &State) -> FeedbackState {
    FeedbackState {
        kind: if s.draft.payload.kind == data::Kind::Bug {
            "bug"
        } else {
            "feature"
        }
        .into(),
        description: s.draft.payload.description.clone(),
        count: data::character_count(&s.draft.payload.description) as u32,
        valid: data::valid_description(&s.draft.payload.description),
        locked: s.locked,
        images: s
            .thumbnails
            .iter()
            .enumerate()
            .map(|(i, b)| FeedbackImage {
                index: i as u32,
                thumbnail: b.clone(),
            })
            .collect(),
        context: serde_json::to_string(&s.draft.payload.context).unwrap_or_default(),
        status: if s.busy {
            "sending".into()
        } else {
            s.status.clone()
        },
        error: s.error.clone(),
        retry_until: s.draft.retry_until.unwrap_or(0) * 1000,
        issue_url: s.issue.as_ref().map(|i| i.url.clone()),
        issue_number: s.issue.as_ref().map(|i| i.number),
    }
}
fn context(reader: Option<&MobileReader>, env: FeedbackEnvironment) -> Result<data::Context> {
    if !env.scale.is_finite()
        || !(0.1..=20.).contains(&env.scale)
        || env.width == 0
        || env.height == 0
        || env.width > 100000
        || env.height > 100000
    {
        return Err(error("configuration"));
    }
    let mut context = data::Context {
        source: "shelf".into(),
        app: data::App {
            version: env!("CARGO_PKG_VERSION").into(),
        },
        os: data::Os {
            name: "Android".into(),
            version: data::clipped(&format!("{} · {}", env.os_version, env.device), 100),
            arch: data::clipped(&env.arch, 40),
        },
        display: data::Display {
            width: env.width,
            height: env.height,
            scale: env.scale,
        },
        book: None,
        location: None,
        reader: None,
    };
    if let Some(reader) = reader {
        reader.with(|s| {
            let book = s.pipeline.rewrite.book();
            let snapshot = s.session.snapshot();
            let locator = s.session.current_locator();
            context.source = "reader".into();
            context.book = Some(data::Book {
                title: if book.metadata.title.trim().is_empty() {
                    "(missing original title)".into()
                } else {
                    data::clipped(&book.metadata.title, 500)
                },
                authors: book
                    .metadata
                    .authors
                    .iter()
                    .take(30)
                    .map(|a| data::clipped(a, 200))
                    .filter(|s| !s.is_empty())
                    .collect(),
                languages: book
                    .metadata
                    .languages
                    .iter()
                    .take(12)
                    .map(|l| data::clipped(l, 40))
                    .filter(|s| !s.is_empty())
                    .collect(),
                format: s.format.label().into(),
                layout:
                    if book.metadata.layout == rebook_publication::RenditionLayout::PrePaginated {
                        "fixed"
                    } else {
                        "reflowable"
                    }
                    .into(),
            });
            let anchor = s.focus_anchor.as_ref().or(locator.source.as_ref());
            let focus = s
                .focus
                .then(|| {
                    anchor.and_then(|range| {
                        let ranges = s
                            .focus_units
                            .iter()
                            .find(|u| {
                                u.ranges.iter().any(|r| {
                                    r.start.spine == range.start.spine
                                        && r.start.node == range.start.node
                                })
                            })
                            .map(|u| u.ranges.as_slice())
                            .unwrap_or(&[]);
                        data::FocusLocation::from_source(range, ranges)
                    })
                })
                .flatten();
            let section = anchor
                .and_then(|r| book.sections.iter().find(|p| p.id == r.start.spine))
                .or_else(|| book.sections.get(snapshot.location.section_index));
            let document = section.and_then(|p| data::book_document(p.href.path()));
            let item = snapshot
                .active_toc_id
                .as_ref()
                .and_then(|id| s.session.toc_items().iter().find(|t| &t.id == id));
            context.location = Some(data::Location {
                chapter: item
                    .map(|t| data::clipped(&t.label, 500))
                    .filter(|s| !s.is_empty()),
                document: document.clone(),
                anchor: item
                    .and_then(|t| t.target.as_ref())
                    .and_then(|p| p.fragment())
                    .map(|v| data::clipped(v, 500))
                    .filter(|s| !s.is_empty()),
                page: (s.format == rebook_formats::BookFormat::Pdf)
                    .then_some(snapshot.location.section_index + 1),
                focus: document.and(focus),
            });
            let settings =
                rebook_app_core::plugins::PluginSettings::load_default().map_err(error)?;
            context.reader = Some(data::Reader {
                mode: if s.focus { "focus" } else { "classic" }.into(),
                typesetting: if s.focus
                    || s.session.style().typesetting.mode == TypesettingMode::Unified
                {
                    "unified"
                } else {
                    "original"
                }
                .into(),
                pdf_reflow: (s.format == rebook_formats::BookFormat::Pdf).then(|| "off".into()),
                translation: data::Translation {
                    enabled: s.options.translated,
                    target_language: data::clipped(
                        &settings.resolved_target_language(
                            rebook_app_core::preferences::AppLanguage::system_translation_target(),
                        ),
                        40,
                    ),
                },
            });
            Ok(())
        })?;
    }
    Ok(context)
}
#[uniffi::export]
impl MobileCore {
    pub async fn open_feedback(
        self: Arc<Self>,
        reader: Option<Arc<MobileReader>>,
        environment: FeedbackEnvironment,
    ) -> Result<Arc<MobileFeedback>> {
        work(move || {
            let context = context(reader.as_deref(), environment)?;
            let root = self.root.join("feedback");
            let pending = store::load(&root.join("draft.json"), true).map_err(error)?;
            let locked = pending.is_some();
            let mut draft = if let Some(pending) = pending {
                pending
            } else {
                store::load(&root.join("editor.json"), false)
                    .map_err(error)?
                    .unwrap_or(store::new_draft(context.clone()).map_err(error)?)
            };
            if !locked {
                draft.payload.context = context;
            }
            let thumbnails = draft
                .payload
                .screenshots
                .iter()
                .map(thumbnail)
                .collect::<Result<Vec<_>>>()?;
            Ok(Arc::new(MobileFeedback {
                root,
                state: Mutex::new(State {
                    draft,
                    thumbnails,
                    locked,
                    busy: false,
                    status: if locked { "pending" } else { "editing" }.into(),
                    error: None,
                    issue: None,
                }),
            }))
        })
        .await
    }
}
#[uniffi::export]
impl MobileFeedback {
    pub async fn snapshot(self: Arc<Self>) -> Result<FeedbackState> {
        work(move || Ok(record(&*self.state.lock().map_err(error)?))).await
    }
    pub async fn edit(self: Arc<Self>, kind: String, description: String) -> Result<FeedbackState> {
        work(move || {
            if description.encode_utf16().count() > 10000 {
                return Err(error("invalid"));
            }
            let mut state = self.state.lock().map_err(error)?;
            if !state.locked && !state.busy && state.issue.is_none() {
                state.draft.payload.kind = match kind.as_str() {
                    "bug" => data::Kind::Bug,
                    "feature" => data::Kind::Feature,
                    _ => return Err(error("invalid")),
                };
                state.draft.payload.description = description;
                state.error = None;
                store::save(&self.root.join("editor.json"), &state.draft).map_err(error)?;
            }
            Ok(record(&state))
        })
        .await
    }
    pub async fn add_image(self: Arc<Self>, path: String) -> Result<FeedbackState> {
        work(move || {
            {
                let state = self.state.lock().map_err(error)?;
                if state.locked
                    || state.busy
                    || state.draft.payload.screenshots.len() >= images::MAX_IMAGES
                {
                    return Err(error("limit"));
                }
            }
            if std::fs::metadata(&path).map_err(error)?.len() > 20 * 1024 * 1024 {
                return Err(error("image"));
            }
            let screenshot = images::prepare(std::fs::read(path).map_err(error)?).map_err(error)?;
            let thumb = thumbnail(&screenshot)?;
            let mut state = self.state.lock().map_err(error)?;
            if state.locked
                || state.busy
                || state.draft.payload.screenshots.len() >= images::MAX_IMAGES
            {
                return Err(error("limit"));
            }
            state.draft.payload.screenshots.push(screenshot);
            state.thumbnails.push(thumb);
            store::save(&self.root.join("editor.json"), &state.draft).map_err(error)?;
            Ok(record(&state))
        })
        .await
    }
    pub async fn remove_image(self: Arc<Self>, index: u32) -> Result<FeedbackState> {
        work(move || {
            let mut state = self.state.lock().map_err(error)?;
            if !state.locked && !state.busy && (index as usize) < state.thumbnails.len() {
                state.thumbnails.remove(index as usize);
                state.draft.payload.screenshots.remove(index as usize);
                store::save(&self.root.join("editor.json"), &state.draft).map_err(error)?;
            }
            Ok(record(&state))
        })
        .await
    }
    pub async fn image(self: Arc<Self>, index: u32) -> Result<Vec<u8>> {
        work(move || {
            let state = self.state.lock().map_err(error)?;
            images::encoded_bytes(
                state
                    .draft
                    .payload
                    .screenshots
                    .get(index as usize)
                    .ok_or_else(|| error("image"))?,
            )
            .map_err(error)
        })
        .await
    }
    pub async fn submit(self: Arc<Self>) -> Result<FeedbackState> {
        runtime()
            .spawn(async move {
                let this = self.clone();
                let draft = work(move || {
                    let mut state = this.state.lock().map_err(error)?;
                    if state.busy
                        || state.issue.is_some()
                        || store::remaining_retry(&state.draft).is_some()
                    {
                        return Ok(None);
                    }
                    if !data::valid_description(&state.draft.payload.description) {
                        state.error = Some("invalid".into());
                        return Ok(None);
                    }
                    if !state.locked {
                        state.draft.payload.request_id =
                            store::new_draft(state.draft.payload.context.clone())
                                .map_err(error)?
                                .payload
                                .request_id;
                        state.draft.payload.description =
                            state.draft.payload.description.trim().into();
                    }
                    state.locked = true;
                    state.busy = true;
                    state.error = None;
                    Ok(Some(state.draft.clone()))
                })
                .await?;
                if let Some(draft) = draft {
                    let result = store::send_saved(self.root.join("draft.json"), draft).await;
                    let this = self.clone();
                    work(move || {
                        let mut state = this.state.lock().map_err(error)?;
                        state.busy = false;
                        match result {
                            Ok((draft, outcome)) => {
                                state.draft = draft;
                                match outcome {
                                    Outcome::Submitted(issue) => {
                                        state.issue = Some(issue);
                                        state.status = "submitted".into();
                                        let _ = std::fs::remove_file(this.root.join("editor.json"));
                                    }
                                    Outcome::Pending(_) => {
                                        state.status = "pending".into();
                                    }
                                    Outcome::Failed(f, _) => {
                                        state.status = "failed".into();
                                        state.error = Some(code(f));
                                        if f == Failure::Invalid {
                                            state.locked = false;
                                            store::save(
                                                &this.root.join("editor.json"),
                                                &state.draft,
                                            )
                                            .map_err(error)?;
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                state.status = "failed".into();
                                state.error = Some("storage".into());
                            }
                        }
                        Ok(())
                    })
                    .await?;
                }
                self.snapshot().await
            })
            .await
            .map_err(error)?
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    pub async fn verify(core: &Arc<MobileCore>, book_id: &str, root: &std::path::Path) {
        let env = FeedbackEnvironment {
            os_version: "Android 16 (API 36)".into(),
            device: "Test phone".into(),
            arch: "arm64-v8a".into(),
            width: 1080,
            height: 2400,
            scale: 2.625,
        };
        let reader = core
            .clone()
            .open_book(
                book_id.into(),
                Viewport {
                    width: 360,
                    height: 640,
                    scale: 1.,
                },
            )
            .await
            .unwrap();
        reader.clone().page().await.unwrap();
        let position = reader.with(|s| Ok(s.session.current_locator())).unwrap();
        let form = core
            .clone()
            .open_feedback(Some(reader.clone()), env.clone())
            .await
            .unwrap();
        let initial = form.clone().snapshot().await.unwrap();
        let context: serde_json::Value = serde_json::from_str(&initial.context).unwrap();
        assert_eq!(context["os"]["name"], "Android");
        assert_eq!(context["source"], "reader");
        assert!(context["book"]["title"].as_str().is_some());
        assert!(!initial.valid);
        assert_eq!(initial.count, 0);
        let short = form
            .clone()
            .edit("bug".into(), "  a b  ".into())
            .await
            .unwrap();
        assert_eq!(short.count, 2);
        assert!(!short.valid);
        let oversized = form
            .clone()
            .edit("bug".into(), "x".repeat(5001))
            .await
            .unwrap();
        assert_eq!(oversized.count, 5001);
        assert!(!oversized.valid);
        let recovered = core.clone().open_feedback(None, env.clone()).await.unwrap();
        assert_eq!(
            recovered
                .clone()
                .snapshot()
                .await
                .unwrap()
                .description
                .len(),
            5001
        );
        drop(recovered);
        let edited = form
            .clone()
            .edit(
                "feature".into(),
                "  希望增加一个用于检查反馈草稿的测试入口  ".into(),
            )
            .await
            .unwrap();
        assert!(edited.valid);
        let image = root.join("feedback-source.png");
        image::RgbaImage::from_pixel(40, 30, image::Rgba([7, 193, 96, 255]))
            .save(&image)
            .unwrap();
        for _ in 0..3 {
            form.clone()
                .add_image(image.to_string_lossy().into())
                .await
                .unwrap();
        }
        assert!(
            form.clone()
                .add_image(image.to_string_lossy().into())
                .await
                .is_err()
        );
        assert_eq!(form.clone().snapshot().await.unwrap().images.len(), 3);
        form.clone().remove_image(1).await.unwrap();
        let bytes = form.clone().image(0).await.unwrap();
        assert!(bytes.len() <= images::MAX_BYTES);
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        drop(form);
        let restored = core.clone().open_feedback(None, env.clone()).await.unwrap();
        let draft = restored.clone().snapshot().await.unwrap();
        assert_eq!(draft.description, edited.description);
        assert_eq!(draft.images.len(), 2);
        let context: serde_json::Value = serde_json::from_str(&draft.context).unwrap();
        assert_eq!(context["source"], "shelf");
        assert!(context.get("book").is_none());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        restored.state.lock().unwrap().draft.endpoint = format!("http://{address}/api/feedback");
        let records = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let copies = records.clone();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            for index in 0..2 {
                let mut socket = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline);
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut socket);
                let mut size = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        size = value.trim().parse::<usize>().unwrap();
                    }
                }
                assert!(size <= 9 * 1024 * 1024);
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                copies.lock().unwrap().push(body);
                let result=if index==0 {serde_json::json!({"requestId":payload["requestId"],"status":"pending"})}else{serde_json::json!({"requestId":payload["requestId"],"status":"submitted","issue":{"number":12345,"url":"https://github.com/TortoTech/torto/issues/12345"}})}.to_string();
                let status = if index == 0 { "202 Accepted" } else { "200 OK" };
                write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 60\r\nConnection: close\r\n\r\n{result}",result.len()).unwrap();
            }
        });
        let pending = restored.clone().submit().await.unwrap();
        assert!(pending.locked);
        assert_eq!(pending.status, "pending");
        let unchanged = restored
            .clone()
            .edit(
                "bug".into(),
                "Changing a pending report is forbidden".into(),
            )
            .await
            .unwrap();
        assert_eq!(unchanged.description.trim(), draft.description.trim());
        assert_eq!(unchanged.kind, "feature");
        assert!(
            restored
                .clone()
                .add_image(image.to_string_lossy().into())
                .await
                .is_err()
        );
        restored.clone().submit().await.unwrap();
        assert_eq!(
            records.lock().unwrap().len(),
            1,
            "retry-after prevents an early repeat"
        );
        drop(restored);
        let resumed = core
            .clone()
            .open_feedback(Some(reader.clone()), env)
            .await
            .unwrap();
        let state = resumed.clone().snapshot().await.unwrap();
        assert!(state.locked);
        assert_eq!(state.status, "pending");
        assert_eq!(
            state.context, pending.context,
            "pending reports keep their original context after app restart or book changes"
        );
        resumed.state.lock().unwrap().draft.retry_until = None;
        let success = resumed.clone().submit().await.unwrap();
        assert_eq!(success.issue_number, Some(12345));
        assert_eq!(success.status, "submitted");
        server.join().unwrap();
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0], records[1],
            "retries reuse the exact payload and issue ID"
        );
        assert!(!root.join("feedback/draft.json").exists());
        assert!(!root.join("feedback/editor.json").exists());
        assert_eq!(
            reader.with(|s| Ok(s.session.current_locator())).unwrap(),
            position,
            "feedback never navigates the underlying book"
        );
        reader.shutdown().await.unwrap();
    }
}
