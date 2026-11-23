use super::{FontSet, catalog::{self, catalog}, store};
pub(super) use rebook_app_core::fonts::service::Update;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CustomImportTarget {
    Cjk,
    CjkLatin,
    Latin,
    LatinCjk,
    Monospace,
}

#[derive(Clone, Debug)]
pub(crate) struct CustomFont {
    pub(crate) family: String,
    pub(crate) display_name: String,
    pub(crate) is_cjk: bool,
    pub(crate) data: peniko::Blob<u8>,
}

pub(crate) struct CustomImportMessage {
    pub(crate) target: CustomImportTarget,
    pub(crate) result: Result<Vec<CustomFont>, String>,
}

pub(crate) struct Message {
    generation: u64,
    pub(super) update: Update,
}

pub(crate) struct FontService {
    pub(crate) fonts: Arc<FontSet>,
    pub(crate) custom_fonts: Vec<CustomFont>,
    pub(crate) reader_fonts: Arc<[peniko::Blob<u8>]>,
    pub(crate) statuses: Vec<Status>,
    generation: u64,
    requested: bool,
    pending_indices: Vec<usize>,
    running: bool,
    worker: Option<JoinHandle<()>>,
    pub(crate) revision: u64,
    pub(crate) initialized: bool,
}
impl Default for FontService {
    fn default() -> Self {
        let custom_fonts = load_custom_fonts();
        let fonts = Arc::new(FontSet::default());
        let reader_fonts = merge_reader_fonts(&fonts, &custom_fonts);
        Self {
            fonts,
            custom_fonts,
            reader_fonts,
            statuses: vec![Status::Pending; catalog().fonts.len()],
            generation: 0,
            requested: true,
            pending_indices: Vec::new(),
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

    pub(crate) fn is_family_ready(&self, family: &str) -> bool {
        let indices = catalog::indices_for_family(family);
        indices.is_empty()
            || indices
                .iter()
                .all(|index| self.statuses.get(*index) == Some(&Status::Ready))
    }

    pub(crate) fn request_family_download(&mut self, family: &str) {
        let indices = catalog::indices_for_family(family);
        let mut added = false;
        for index in indices {
            if self.statuses.get(index) != Some(&Status::Ready)
                && !self.pending_indices.contains(&index)
            {
                if let Some(slot) = self.statuses.get_mut(index) {
                    *slot = Status::Downloading(0);
                }
                self.pending_indices.push(index);
                added = true;
            }
        }
        if added {
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
        let mut indices = std::mem::take(&mut self.pending_indices);
        if indices.is_empty() {
            indices = catalog::preload_indices();
        }
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
                    for index in &indices {
                        send(Update::Finished(*index, Err(error.clone())));
                    }
                    send(Update::Stopped);
                    return;
                }
            };
            rebook_app_core::fonts::service::run_indices(
                root,
                previous,
                !crate::smoke::enabled(),
                indices,
                send,
            )
            .await;
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
                    if let Some(slot) = self.statuses.get_mut(index) {
                        *slot = if face.is_some() {
                            Status::Ready
                        } else if !matches!(slot, Status::Failed(_) | Status::Downloading(_)) {
                            Status::Pending
                        } else {
                            slot.clone()
                        };
                    }
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
            Update::Progress(index, percent) => {
                if let Some(slot) = self.statuses.get_mut(index) {
                    *slot = Status::Downloading(percent);
                }
            }
            Update::Finished(index, result) => match result {
                Ok(face) => {
                    if let Some(slot) = Arc::make_mut(&mut self.fonts).faces.get_mut(index) {
                        *slot = Some(face);
                    }
                    if let Some(slot) = self.statuses.get_mut(index) {
                        *slot = Status::Ready;
                    }
                    changed = true;
                }
                Err(error) => {
                    if let Some(slot) = self.statuses.get_mut(index) {
                        *slot = Status::Failed(error);
                    }
                }
            },
            Update::Stopped => {
                self.initialized = true;
                self.running = false;
                self.worker = None;
            }
        }
        if changed {
            self.revision = self.revision.wrapping_add(1);
            self.reader_fonts = merge_reader_fonts(&self.fonts, &self.custom_fonts);
        }
        changed
    }

    pub(crate) fn add_custom_fonts(&mut self, imported: Vec<CustomFont>) {
        for font in imported {
            if let Some(existing) = self
                .custom_fonts
                .iter_mut()
                .find(|item| item.family.eq_ignore_ascii_case(&font.family))
            {
                *existing = font;
            } else {
                self.custom_fonts.push(font);
            }
        }
        self.custom_fonts
            .sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
        self.revision = self.revision.wrapping_add(1);
        self.reader_fonts = merge_reader_fonts(&self.fonts, &self.custom_fonts);
    }

    pub(crate) fn spawn_custom_import(
        &self,
        target: CustomImportTarget,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        let proxy = proxy.clone();
        runtime.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter("Font", &["ttf", "otf", "ttc"])
                .pick_files()
                .await;
            let Some(handles) = picked else {
                return;
            };
            let paths: Vec<std::path::PathBuf> =
                handles.into_iter().map(|h| h.path().to_path_buf()).collect();
            let result = tokio::task::spawn_blocking(move || import_custom_font_files(&paths))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let _ = proxy.send_event(UserEvent::CustomFontsImported(CustomImportMessage {
                target,
                result,
            }));
        });
    }
}

fn custom_directory() -> Result<std::path::PathBuf, String> {
    store::directory().map(|dir| dir.join("custom"))
}

fn parse_custom_font_bytes(bytes: Vec<u8>) -> Option<CustomFont> {
    let (family, display_name, is_cjk) = rebook_layout::inspect_font_bytes(&bytes)?;
    let data = peniko::Blob::new(Arc::new(bytes));
    rebook_layout::register_font_blob_family_override(data.id(), family.clone());
    Some(CustomFont {
        family,
        display_name,
        is_cjk,
        data,
    })
}

fn load_custom_fonts() -> Vec<CustomFont> {
    let Ok(dir) = custom_directory() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut list: Vec<CustomFont> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Some(font) = parse_custom_font_bytes(bytes)
            && !list
                .iter()
                .any(|item| item.family.eq_ignore_ascii_case(&font.family))
        {
            list.push(font);
        }
    }
    list.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    list
}

fn import_custom_font_files(paths: &[std::path::PathBuf]) -> Result<Vec<CustomFont>, String> {
    let dir = custom_directory()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create custom font dir: {e}"))?;
    let mut imported = Vec::new();
    for path in paths {
        let bytes = std::fs::read(path).map_err(|e| format!("Failed to read font file: {e}"))?;
        let Some(font) = parse_custom_font_bytes(bytes.clone()) else {
            return Err("Invalid or unsupported font file".to_owned());
        };
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("custom-font.ttf");
        let dest = dir.join(file_name);
        std::fs::write(&dest, &bytes).map_err(|e| format!("Failed to save font file: {e}"))?;
        imported.push(font);
    }
    Ok(imported)
}

fn merge_reader_fonts(fonts: &FontSet, custom_fonts: &[CustomFont]) -> Arc<[peniko::Blob<u8>]> {
    let mut blobs: Vec<peniko::Blob<u8>> = fonts.reader_fonts().iter().cloned().collect();
    for font in custom_fonts {
        rebook_layout::register_font_blob_family_override(font.data.id(), font.family.clone());
        if !blobs.iter().any(|blob| blob.id() == font.data.id()) {
            blobs.push(font.data.clone());
        }
    }
    blobs.into()
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
    statuses: Vec<Status>,
    language: AppLanguage,
    custom_cjk: Vec<(String, String)>,
    custom_latin: Vec<(String, String)>,
}
pub(crate) fn publish_labels(
    ctx: &egui::Context,
    statuses: &[Status],
    custom_fonts: &[CustomFont],
    language: AppLanguage,
) {
    let mut custom_cjk = Vec::new();
    let mut custom_latin = Vec::new();
    for font in custom_fonts {
        let label = if language.text("zh", "en") == "zh" {
            font.display_name.clone()
        } else {
            font.family.clone()
        };
        if font.is_cjk {
            custom_cjk.push((font.family.clone(), label));
        } else {
            custom_latin.push((font.family.clone(), label));
        }
    }
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::unique("downloaded-font-labels"),
            Labels {
                statuses: statuses.to_vec(),
                language,
                custom_cjk,
                custom_latin,
            },
        )
    });
}

pub(crate) fn custom_cjk_families(ctx: &egui::Context) -> Vec<(String, String)> {
    ctx.data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))
        .map(|l| l.custom_cjk)
        .unwrap_or_default()
}

pub(crate) fn custom_latin_families(ctx: &egui::Context) -> Vec<(String, String)> {
    ctx.data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))
        .map(|l| l.custom_latin)
        .unwrap_or_default()
}

pub(crate) fn request_custom_import(ctx: &egui::Context, target: CustomImportTarget) {
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::unique("font-custom-import-request"),
            Some(target),
        );
    });
}

pub(crate) fn take_custom_import_request(ctx: &egui::Context) -> Option<CustomImportTarget> {
    ctx.data_mut(|data| {
        data.remove_temp::<Option<CustomImportTarget>>(egui::Id::unique(
            "font-custom-import-request",
        ))
    })
    .flatten()
}

pub(crate) fn family_status(ctx: &egui::Context, family: &str) -> Option<Status> {
    let indices = catalog::indices_for_family(family);
    if indices.is_empty() {
        return None;
    }
    let labels = ctx.data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))?;
    let mut worst = Status::Ready;
    for index in indices {
        match labels.statuses.get(index) {
            Some(Status::Downloading(p)) => return Some(Status::Downloading(*p)),
            Some(Status::Failed(err)) => worst = Status::Failed(err.clone()),
            Some(Status::Pending) => {
                if worst == Status::Ready {
                    worst = Status::Pending;
                }
            }
            _ => {}
        }
    }
    Some(worst)
}

pub(crate) fn family_label(ctx: &egui::Context, family: &str) -> String {
    let display = catalog::display_name(family);
    let Some(labels) =
        ctx.data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))
    else {
        return display.into();
    };
    let indices = catalog::indices_for_family(family);
    if indices.is_empty() {
        return display.into();
    }
    let pending = indices
        .iter()
        .filter_map(|index| labels.statuses.get(*index))
        .find(|status| **status != Status::Ready);
    pending.map_or_else(
        || display.into(),
        |status| format!("{display} · {}", status.label(labels.language)),
    )
}

pub(crate) fn request_family(ctx: &egui::Context, family: &str) {
    let indices = catalog::indices_for_family(family);
    if indices.is_empty() {
        return;
    }
    let ready = ctx
        .data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))
        .is_some_and(|labels| {
            indices
                .iter()
                .all(|index| labels.statuses.get(*index) == Some(&Status::Ready))
        });
    if !ready {
        ctx.data_mut(|data| {
            let mut requests = data
                .get_temp::<Vec<String>>(egui::Id::unique("font-download-requested-families"))
                .unwrap_or_default();
            if !requests.iter().any(|item| item == family) {
                requests.push(family.to_owned());
            }
            data.insert_temp(
                egui::Id::unique("font-download-requested-families"),
                requests,
            );
        });
    }
}

pub(crate) fn any_downloading(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<Labels>(egui::Id::unique("downloaded-font-labels")))
        .is_some_and(|labels| {
            labels
                .statuses
                .iter()
                .any(|status| matches!(status, Status::Downloading(_)))
        })
}

pub(crate) fn is_family_ready(ctx: &egui::Context, family: &str) -> bool {
    matches!(family_status(ctx, family), None | Some(Status::Ready))
}

pub(crate) fn request_family_with_apply(
    ctx: &egui::Context,
    family: &str,
    change: crate::settings::ReaderSettingsChange,
) {
    request_family(ctx, family);
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::unique("font-download-pending-apply"),
            Some((family.to_owned(), change)),
        );
    });
}

pub(crate) fn take_apply_request(
    ctx: &egui::Context,
) -> Option<(String, crate::settings::ReaderSettingsChange)> {
    ctx.data_mut(|data| {
        data.remove_temp::<Option<(String, crate::settings::ReaderSettingsChange)>>(
            egui::Id::unique("font-download-pending-apply"),
        )
    })
    .flatten()
}

pub(crate) fn take_requests(ctx: &egui::Context) -> Vec<String> {
    ctx.data_mut(|data| {
        data.remove_temp::<Vec<String>>(egui::Id::unique("font-download-requested-families"))
    })
    .unwrap_or_default()
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
        service.statuses[0] = Status::Downloading(50);
        service.statuses[1] = Status::Failed("offline".into());
        service.statuses[2] = Status::Ready;
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
