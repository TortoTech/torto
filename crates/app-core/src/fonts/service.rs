//! Host-independent default-font discovery, download order and completion events.
use super::{Face, FontSet, catalog::catalog, download, store};
use std::{future::Future, path::PathBuf, sync::Arc};

pub enum Update {
    Loaded(Arc<FontSet>),
    Progress(usize, u8),
    Finished(usize, Result<Face, String>),
    Stopped,
}
pub struct Report {
    pub fonts: Arc<FontSet>,
    pub downloaded: u32,
    pub error: Option<String>,
}
impl Report {
    pub fn ready(&self) -> bool {
        super::catalog::preload_indices()
            .into_iter()
            .all(|index| self.fonts.faces.get(index).is_some_and(Option::is_some))
    }
}

pub async fn run(root: PathBuf, previous: FontSet, network: bool, send: impl Fn(Update)) -> Report {
    run_indices(root, previous, network, super::catalog::preload_indices(), send).await
}

pub async fn run_indices(
    root: PathBuf,
    previous: FontSet,
    network: bool,
    indices: Vec<usize>,
    send: impl Fn(Update),
) -> Report {
    let load_root = root.clone();
    let fonts = match tokio::task::spawn_blocking(move || store::load(&load_root, previous)).await {
        Ok(fonts) => fonts,
        Err(error) => {
            let error = error.to_string();
            for index in &indices {
                send(Update::Finished(*index, Err(error.clone())));
            }
            send(Update::Stopped);
            return Report {
                fonts: Arc::default(),
                downloaded: 0,
                error: Some(error),
            };
        }
    };
    send(Update::Loaded(Arc::new(fonts.clone())));
    let report = if network {
        let client = download::client();
        download_missing(fonts, &indices, &send, |index| {
            let root = root.clone();
            let client = client.clone();
            let send = &send;
            async move {
                let client = client?;
                download::fetch(&client, root, catalog().fonts[index].clone(), |percent| {
                    send(Update::Progress(index, percent))
                })
                .await
            }
        })
        .await
    } else {
        Report {
            fonts: Arc::new(fonts),
            downloaded: 0,
            error: None,
        }
    };
    send(Update::Stopped);
    report
}

async fn download_missing<F, Fut>(
    mut fonts: FontSet,
    indices: &[usize],
    send: &impl Fn(Update),
    fetch: F,
) -> Report
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Result<Face, String>>,
{
    let mut downloaded = 0;
    let mut error = None;
    for &index in indices {
        if fonts.faces.get(index).is_some_and(Option::is_some) {
            continue;
        }
        let result = fetch(index).await;
        match &result {
            Ok(face) => {
                if let Some(slot) = fonts.faces.get_mut(index) {
                    *slot = Some(face.clone());
                }
                downloaded += 1;
            }
            Err(message) => error = Some(message.clone()),
        }
        send(Update::Finished(index, result));
    }
    Report {
        fonts: Arc::new(fonts),
        downloaded,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[test]
    fn offline_startup_publishes_local_fonts_and_stops_without_downloading() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let root =
            std::env::temp_dir().join(format!("torto-font-service-{}", uuid::Uuid::new_v4()));
        let face = Face {
            data: peniko::Blob::new(Arc::new(vec![1])),
            index: 0,
        };
        let fonts = FontSet {
            faces: vec![Some(face.clone()); catalog().fonts.len()],
        };
        let events = Mutex::new(Vec::new());
        let report = runtime.block_on(run(root.clone(), fonts, false, |event| {
            events.lock().unwrap().push(match event {
                Update::Loaded(fonts) => {
                    assert!(crate::fonts::catalog::preload_indices().into_iter().all(|i| fonts.faces[i].is_some()));
                    "loaded"
                }
                Update::Stopped => "stopped",
                _ => panic!("Offline startup must not attempt a download"),
            });
        }));
        assert_eq!(*events.lock().unwrap(), vec!["loaded", "stopped"]);
        assert!(report.ready());
        assert_eq!(report.downloaded, 0);
        assert!(report.error.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn skips_available_faces_keeps_partial_success_and_continues_after_failure() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let face = Face {
            data: peniko::Blob::new(Arc::new(vec![1])),
            index: 0,
        };
        let mut fonts = FontSet::default();
        fonts.faces[1] = Some(face.clone());
        let attempts = Mutex::new(Vec::new());
        let events = Mutex::new(Vec::new());
        let preload = crate::fonts::catalog::preload_indices();
        let expected_attempts: Vec<usize> = preload.iter().copied().filter(|&i| i != 1).collect();
        let expected_events: Vec<(usize, bool)> = expected_attempts
            .iter()
            .map(|&i| (i, i != 2))
            .collect();
        let report = runtime.block_on(download_missing(
            fonts,
            &preload,
            &|update| {
                if let Update::Finished(index, result) = update {
                    events.lock().unwrap().push((index, result.is_ok()));
                }
            },
            |index| {
                attempts.lock().unwrap().push(index);
                let face = face.clone();
                async move {
                    if index == 2 {
                        Err("offline".into())
                    } else {
                        Ok(face)
                    }
                }
            },
        ));
        assert_eq!(*attempts.lock().unwrap(), expected_attempts);
        assert_eq!(*events.lock().unwrap(), expected_events);
        assert_eq!(report.downloaded, (expected_attempts.len() - 1) as u32);
        assert!(report.fonts.faces[0].is_some());
        assert!(report.fonts.faces[1].is_some());
        assert!(report.fonts.faces[2].is_none());
        assert!(!report.ready());
        assert_eq!(report.error.as_deref(), Some("offline"));
        let retry = runtime.block_on(download_missing(
            report.fonts.as_ref().clone(),
            &preload,
            &|_| {},
            |index| {
                assert_eq!(index, 2);
                let face = face.clone();
                async move { Ok(face) }
            },
        ));
        assert_eq!(retry.downloaded, 1);
        assert!(retry.ready());
        assert!(retry.error.is_none());
    }
}
