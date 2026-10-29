use super::{FontSet, catalog::catalog, download, store};
use crate::{platform::UserEvent, preferences::AppLanguage};
use std::sync::Arc;
use tokio::task::JoinHandle;

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    #[default]
    Pending,
    Downloading(u8),
    Ready,
    Failed(String),
}
impl Status {
    pub(crate) fn label(&self, language: AppLanguage) -> String {
        match self {
            Self::Pending => language.text("未下载", "Not downloaded").into(),
            Self::Downloading(percent) => {
                format!("{} {percent}%", language.text("下载中", "Downloading"))
            }
            Self::Ready => language.text("已就绪", "Ready").into(),
            Self::Failed(_) => language
                .text(
                    "下载失败，使用系统字体",
                    "Download failed; using system fonts",
                )
                .into(),
        }
    }
}

pub(crate) struct Message {
    generation: u64,
    pub(super) update: Update,
}
pub(super) enum Update {
    Loaded(Arc<FontSet>),
    Progress(usize, u8),
    Finished(usize, Result<super::Face, String>),
    Stopped,
}

pub(crate) struct FontService {
    pub(crate) fonts: Arc<FontSet>,
    pub(crate) reader_fonts: Arc<[peniko::Blob<u8>]>,
    pub(crate) statuses: [Status; 3],
    generation: u64,
    requested: bool,
    running: bool,
    worker: Option<JoinHandle<()>>,
    pub(crate) revision: u64,
    pub(crate) initialized: bool,
}
impl Default for FontService {
    fn default() -> Self {
        Self {
            fonts: Arc::new(FontSet::default()),
            reader_fonts: Arc::default(),
            statuses: Default::default(),
            generation: 0,
            requested: true,
            running: false,
            worker: None,
            revision: 0,
            initialized: false,
        }
    }
}
impl FontService {
    pub(crate) fn settled(&self) -> bool {
        self.initialized && !self.running && !self.requested
    }
    pub(crate) fn retry(&mut self) {
        if !self.running
            || self
                .statuses
                .iter()
                .any(|status| matches!(status, Status::Failed(_)))
        {
            // Coalesce manual retries behind the active batch. A failed font
            // must not lose a retry click while another font is downloading.
            self.requested = true;
        }
    }

    pub(crate) fn spawn(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if !self.requested || self.running {
            return;
        }
        self.requested = false;
        self.running = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let previous = self.fonts.as_ref().clone();
        let proxy = proxy.clone();
        self.worker = Some(runtime.spawn(async move {
            let send = |update| {
                use crate::diagnostics::{Field, log};
                match &update {
                    Update::Loaded(fonts) => log(
                        "font.local_loaded",
                        &[Field::Usize("ready", fonts.faces.iter().flatten().count())],
                    ),
                    Update::Finished(index, result) => log(
                        "font.download_finished",
                        &[
                            Field::Text("font", &catalog().fonts[*index].id),
                            Field::Bool("success", result.is_ok()),
                        ],
                    ),
                    _ => {}
                }
                let _ = proxy.send_event(UserEvent::Fonts(Message { generation, update }));
            };
            // Worker starts only after the first displayed frame. Neither disk
            // reads nor system discovery can block startup's UI/event thread.
            let root = match store::directory() {
                Ok(root) => root,
                Err(error) => {
                    for index in 0..3 {
                        send(Update::Finished(index, Err(error.clone())));
                    }
                    send(Update::Stopped);
                    return;
                }
            };
            let load_root = root.clone();
            let set = match tokio::task::spawn_blocking(move || store::load(&load_root, previous))
                .await
            {
                Ok(set) => Arc::new(set),
                Err(error) => {
                    for index in 0..3 {
                        send(Update::Finished(index, Err(error.to_string())));
                    }
                    send(Update::Stopped);
                    return;
                }
            };
            send(Update::Loaded(set.clone()));
            // Smoke runs exercise isolated offline startup and must not depend
            // on public endpoints or write the real user font directory.
            if crate::smoke::enabled() {
                send(Update::Stopped);
                return;
            }
            let client = download::client();
            // Small Latin faces become available before the larger CJK file.
            for index in [1, 2, 0] {
                if set.faces[index].is_some() {
                    continue;
                }
                let result = match &client {
                    Ok(client) => {
                        download::fetch(
                            client,
                            root.clone(),
                            catalog().fonts[index].clone(),
                            |percent| send(Update::Progress(index, percent)),
                        )
                        .await
                    }
                    Err(error) => Err(error.clone()),
                };
                send(Update::Finished(index, result));
            }
            send(Update::Stopped);
        }));
    }

    /// Reject stale progress and completion, including after a retry/profile switch.
    /// Returns true only when the immutable font collection actually changes.
    pub(crate) fn complete(&mut self, message: Message) -> bool {
        if message.generation != self.generation || !self.running {
            return false;
        }
        let mut changed = false;
        match message.update {
            Update::Loaded(fonts) => {
                self.initialized = true;
                for (index, face) in fonts.faces.iter().enumerate() {
                    self.statuses[index] = if face.is_some() {
                        Status::Ready
                    } else {
                        Status::Pending
                    };
                }
                changed = fonts
                    .faces
                    .iter()
                    .zip(&self.fonts.faces)
                    .any(|(next, previous)| {
                        next.as_ref().map(|face| face.data.id())
                            != previous.as_ref().map(|face| face.data.id())
                    });
                self.fonts = fonts;
            }
            Update::Progress(index, percent) => self.statuses[index] = Status::Downloading(percent),
            Update::Finished(index, result) => match result {
                Ok(face) => {
                    Arc::make_mut(&mut self.fonts).faces[index] = Some(face);
                    self.statuses[index] = Status::Ready;
                    changed = true;
                }
                Err(error) => self.statuses[index] = Status::Failed(error),
            },
            Update::Stopped => {
                self.initialized = true;
                self.running = false;
                self.worker = None;
            }
        }
        if changed {
            self.revision = self.revision.wrapping_add(1);
            self.reader_fonts = self.fonts.reader_fonts();
        }
        changed
    }
}
impl Drop for FontService {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

// Labels are read by the existing selectors without changing stored family names.
#[derive(Clone)]
struct Labels {
    statuses: [Status; 3],
    language: AppLanguage,
}
pub(crate) fn publish_labels(ctx: &egui::Context, statuses: &[Status; 3], language: AppLanguage) {
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::unique("downloaded-font-labels"),
            Labels {
                statuses: statuses.clone(),
                language,
            },
        )
    });
}
pub(crate) fn family_label(ctx: &egui::Context, family: &str) -> String {
    let Some(labels) =
        ctx.data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))
    else {
        return family.into();
    };
    let indices: &[usize] = match family {
        "Literata" => &[1, 2],
        "LXGW WenKai GB Screen" => &[0],
        _ => return family.into(),
    };
    let pending = indices
        .iter()
        .map(|index| &labels.statuses[*index])
        .find(|status| **status != Status::Ready);
    pending.map_or_else(
        || family.into(),
        |status| format!("{family} · {}", status.label(labels.language)),
    )
}

pub(crate) fn request_family(ctx: &egui::Context, family: &str) {
    let indices: &[usize] = match family {
        "Literata" => &[1, 2],
        "LXGW WenKai GB Screen" => &[0],
        _ => return,
    };
    let ready = ctx
        .data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))
        .is_some_and(|labels| {
            indices
                .iter()
                .all(|index| labels.statuses[*index] == Status::Ready)
        });
    if !ready {
        ctx.data_mut(|data| data.insert_temp(egui::Id::unique("font-download-requested"), true));
    }
}
pub(crate) fn take_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|data| data.remove_temp::<bool>(egui::Id::unique("font-download-requested")))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_font_retry_waits_for_the_active_batch_without_losing_the_request() {
        let mut service = FontService::default();
        service.requested = false;
        service.running = true;
        service.generation = 2;
        service.statuses = [
            Status::Downloading(50),
            Status::Failed("offline".into()),
            Status::Ready,
        ];
        service.retry();
        service.retry();
        assert!(service.requested);
        assert!(service.running);
        assert_eq!(service.generation, 2);
        service.complete(Message {
            generation: 2,
            update: Update::Stopped,
        });
        assert!(!service.running);
        assert!(service.requested);
        assert!(!service.settled());
    }
    #[test]
    fn retries_coalesce_and_old_completions_cannot_replace_current_fonts() {
        let mut service = FontService::default();
        service.requested = false;
        service.running = true;
        service.generation = 2;
        service.retry();
        assert!(!service.requested);
        assert!(!service.complete(Message {
            generation: 1,
            update: Update::Stopped
        }));
        assert!(service.running);
        assert!(!service.complete(Message {
            generation: 1,
            update: Update::Progress(0, 90)
        }));
        assert_eq!(service.statuses[0], Status::Pending);
        assert!(!service.complete(Message {
            generation: 2,
            update: Update::Loaded(Arc::new(FontSet::default()))
        }));
        assert!(service.initialized);
        service.complete(Message {
            generation: 2,
            update: Update::Finished(0, Err("offline".into())),
        });
        service.complete(Message {
            generation: 2,
            update: Update::Stopped,
        });
        assert!(service.settled());
        assert!(matches!(service.statuses[0], Status::Failed(_)));
        service.retry();
        service.retry();
        assert!(service.requested);
        assert!(!service.settled());
    }
}
