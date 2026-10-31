//! Optional device indexes. Older clients remain readable through the scan fallback.
//! The index is published only after its referenced payloads have been accepted.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{SyncResult, rate_limit, webdav::WebDavClient};

const LOCAL: &str = "change-index:local-v1";
const PENDING: &str = "change-index:pending-v1";
const REPAIR: &str = "change-index:validated-v1";
const RECHECK_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Revision {
    reading: Option<String>,
    derived: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{CloudProviderKind, FakeWebDav, SyncSettings, SyncStore};

    fn client(server: &FakeWebDav) -> WebDavClient {
        let mut settings = SyncSettings::new_device();
        settings.enabled = true;
        settings.provider = CloudProviderKind::Custom;
        settings.base_url = server.base_url();
        settings.username = "reader".into();
        let path =
            std::env::temp_dir().join(format!("torto-index-{}.sqlite3", uuid::Uuid::new_v4()));
        let store = SyncStore::open_at(path, settings.device_id.clone()).unwrap();
        WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store, crate::sync::account_key(&settings))
    }

    #[test]
    fn indexes_select_changes_and_missing_or_legacy_indexes_fall_back_safely() {
        let server = FakeWebDav::start();
        let local = client(&server);
        let peer = client(&server);
        let books = (1..=87)
            .map(|id| format!("{id:064x}"))
            .collect::<BTreeSet<_>>();
        let devices = BTreeSet::from([local.device_id.clone(), peer.device_id.clone()]);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            publish(&local, true).await.unwrap();
            publish(&peer, true).await.unwrap();
            let initial = Plan::prepare(&local, devices.clone(), &books)
                .await
                .unwrap();
            assert_eq!(initial.reading.len(), 87);
            assert_eq!(initial.derived.len(), 87);
            initial.acknowledge(&local).unwrap();
            server.requests.lock().unwrap().clear();
            let warm = Plan::prepare(&local, devices.clone(), &books)
                .await
                .unwrap();
            assert!(warm.reading.is_empty() && warm.derived.is_empty());
            assert_eq!(server.requests.lock().unwrap().len(), 2);
            warm.acknowledge(&local).unwrap();
            let reading = books.first().unwrap();
            let derived = books.last().unwrap();
            record(&peer, reading, false).unwrap();
            record(&peer, derived, true).unwrap();
            publish(&peer, false).await.unwrap();
            let changed = Plan::prepare(&local, devices.clone(), &books)
                .await
                .unwrap();
            assert_eq!(changed.reading, BTreeSet::from([reading.clone()]));
            assert_eq!(changed.derived, BTreeSet::from([derived.clone()]));
            // A failed apply must leave the index unapplied for the next job.
            let retry = Plan::prepare(&local, devices.clone(), &books)
                .await
                .unwrap();
            assert_eq!(retry.reading, changed.reading);
            assert_eq!(retry.derived, changed.derived);
            changed.acknowledge(&local).unwrap();
            assert!(
                Plan::prepare(&local, devices.clone(), &books)
                    .await
                    .unwrap()
                    .reading
                    .is_empty()
            );
            let path = format!("/dav/Rebook/v1/library/changes/{}.json", peer.device_id);
            server
                .objects
                .lock()
                .unwrap()
                .insert(path.clone(), (b"invalid".to_vec(), "\"invalid\"".into()));
            let malformed = Plan::prepare(&local, devices.clone(), &books)
                .await
                .unwrap();
            assert_eq!(malformed.reading.len(), 87);
            assert_eq!(malformed.derived.len(), 87);
            server.objects.lock().unwrap().remove(&path);
            let legacy = Plan::prepare(&local, devices.clone(), &books)
                .await
                .unwrap();
            assert_eq!(legacy.reading.len(), 87);
            assert_eq!(legacy.derived.len(), 87);
            // A periodic audit still checks payloads even with unchanged indexes.
            peer.invalidate_object(&format!("library/changes/{}.json", peer.device_id))
                .unwrap();
            publish(&peer, true).await.unwrap();
            let unchanged = Plan::prepare(&local, devices.clone(), &books)
                .await
                .unwrap();
            assert!(unchanged.reading.is_empty());
            local.cache_set(REPAIR, &0_u64.to_le_bytes()).unwrap();
            assert_eq!(
                Plan::prepare(&local, devices, &books)
                    .await
                    .unwrap()
                    .reading
                    .len(),
                87
            );
        });
        server.stop();
    }
}

#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    device: String,
    books: BTreeMap<String, Revision>,
}

fn decode(bytes: &[u8], device: &str) -> Option<Index> {
    if bytes.len() > 16 * 1024 * 1024 {
        return None;
    }
    let index: Index = serde_json::from_slice(bytes).ok()?;
    (index.version == 1
        && index.device == device
        && index.books.len() <= 100_000
        && index.books.iter().all(|(id, revision)| {
            super::engine::validate_book_id(id).is_ok()
                && [&revision.reading, &revision.derived]
                    .iter()
                    .all(|value| value.as_ref().is_none_or(|value| value.len() <= 128))
        }))
    .then_some(index)
}

pub(super) fn fresh(webdav: &WebDavClient, key: &str) -> SyncResult<bool> {
    let now = rate_limit::now_ms();
    Ok(webdav
        .cache_get(key)?
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map(u64::from_le_bytes)
        .is_some_and(|at| at <= now && now - at < RECHECK_MS))
}

pub(super) fn record(webdav: &WebDavClient, book: &str, derived: bool) -> SyncResult<()> {
    super::engine::validate_book_id(book)?;
    let mut index = webdav
        .cache_get(LOCAL)?
        .and_then(|bytes| decode(&bytes, &webdav.device_id))
        .unwrap_or_else(|| Index {
            version: 1,
            device: webdav.device_id.clone(),
            books: BTreeMap::new(),
        });
    let revision = index.books.entry(book.to_owned()).or_default();
    let token = Some(uuid::Uuid::new_v4().to_string());
    if derived {
        revision.derived = token;
    } else {
        revision.reading = token;
    }
    webdav.cache_set(LOCAL, &serde_json::to_vec(&index)?)?;
    webdav.cache_set(PENDING, b"1")
}

pub(super) async fn publish(webdav: &WebDavClient, initialize: bool) -> SyncResult<()> {
    let pending = webdav.cache_get(PENDING)?.as_deref() == Some(b"1");
    if !pending && !initialize {
        return Ok(());
    }
    let path = format!("library/changes/{}.json", webdav.device_id);
    let previous = match webdav.cached_object(&path)? {
        Some(object) => Some(object),
        None => webdav.get_optional(&path).await?,
    };
    let mut index = previous
        .as_ref()
        .and_then(|object| decode(&object.bytes, &webdav.device_id))
        .unwrap_or_else(|| Index {
            version: 1,
            device: webdav.device_id.clone(),
            books: BTreeMap::new(),
        });
    if let Some(local) = webdav
        .cache_get(LOCAL)?
        .and_then(|bytes| decode(&bytes, &webdav.device_id))
    {
        index.books.extend(local.books);
    }
    let bytes = serde_json::to_vec_pretty(&index)?;
    if pending || previous.as_ref().is_none_or(|object| object.bytes != bytes) {
        webdav.ensure_collection("library/changes/").await?;
        webdav.put_mutable_json(&path, &index).await?;
    }
    webdav.cache_set(LOCAL, &serde_json::to_vec(&index)?)?;
    webdav.cache_set(PENDING, b"0")
}

pub(super) struct Plan {
    pub reading: BTreeSet<String>,
    pub derived: BTreeSet<String>,
    snapshots: Vec<(String, Vec<u8>)>,
    books: BTreeSet<String>,
    repair: bool,
}

impl Plan {
    pub async fn prepare(
        webdav: &WebDavClient,
        devices: BTreeSet<String>,
        books: &BTreeSet<String>,
    ) -> SyncResult<Self> {
        let repair = !fresh(webdav, REPAIR)?;
        let mut plan = Self {
            reading: BTreeSet::new(),
            derived: BTreeSet::new(),
            snapshots: Vec::new(),
            books: books.clone(),
            repair,
        };
        let mut fallback = repair;
        for device in devices {
            if uuid::Uuid::parse_str(&device).is_err() {
                fallback = true;
                continue;
            }
            let path = format!("library/changes/{device}.json");
            let key = format!("change-index:applied:{device}");
            let Some(object) = webdav.get_optional(&path).await? else {
                fallback = true;
                continue;
            };
            let Some(current) = decode(&object.bytes, &device) else {
                fallback = true;
                continue;
            };
            let previous = webdav
                .cache_get(&key)?
                .and_then(|bytes| decode(&bytes, &device));
            if device != webdav.device_id {
                if let Some(previous) = previous {
                    for book in current.books.keys().chain(previous.books.keys()) {
                        let before = previous.books.get(book).cloned().unwrap_or_default();
                        let after = current.books.get(book).cloned().unwrap_or_default();
                        if before.reading != after.reading {
                            plan.reading.insert(book.clone());
                        }
                        if before.derived != after.derived {
                            plan.derived.insert(book.clone());
                        }
                    }
                } else {
                    fallback = true;
                }
            }
            plan.snapshots.push((key, object.bytes));
        }
        for book in books {
            if webdav
                .cache_get(&format!("change-index:book:{book}"))?
                .is_none()
            {
                plan.reading.insert(book.clone());
                plan.derived.insert(book.clone());
            }
        }
        if fallback {
            plan.reading.extend(books.iter().cloned());
            plan.derived.extend(books.iter().cloned());
        }
        plan.reading.retain(|book| books.contains(book));
        plan.derived.retain(|book| books.contains(book));
        Ok(plan)
    }

    // Call only after all selected reading/derived payloads have been applied.
    pub fn acknowledge(self, webdav: &WebDavClient) -> SyncResult<()> {
        for (key, bytes) in self.snapshots {
            webdav.cache_set(&key, &bytes)?;
        }
        for book in self.books {
            webdav.cache_set(&format!("change-index:book:{book}"), b"1")?;
        }
        if self.repair {
            webdav.cache_set(REPAIR, &rate_limit::now_ms().to_le_bytes())?;
        }
        Ok(())
    }
}
