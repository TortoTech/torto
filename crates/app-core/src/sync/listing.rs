//! Validators from a fresh directory listing, scoped to one sync client.
use super::SyncResult;
use quick_xml::{Reader, events::Event};
use reqwest::Url;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const FRESHNESS: Duration = Duration::from_secs(30);
const MAX_ENTRIES: usize = 4096;

#[derive(Default)]
pub(super) struct ListingCache(HashMap<String, (String, Instant)>);

impl ListingCache {
    pub(super) fn replace(&mut self, directory: &str, entries: Vec<(String, Option<String>)>) {
        let directory = format!("{}/", directory.trim_matches('/'));
        let now = Instant::now();
        self.0.retain(|path, (_, at)| {
            at.elapsed() <= FRESHNESS
                && !path
                    .strip_prefix(&directory)
                    .is_some_and(|tail| !tail.contains('/'))
        });
        for (name, etag) in entries {
            let Some(etag) = etag.filter(|t| strong_etag(t)) else {
                continue;
            };
            if self.0.len() >= MAX_ENTRIES {
                break;
            }
            self.0.insert(format!("{directory}{name}"), (etag, now));
        }
    }

    pub(super) fn matches(&self, path: &str, etag: Option<&str>) -> bool {
        self.0
            .get(path.trim_start_matches('/'))
            .is_some_and(|(tag, at)| at.elapsed() <= FRESHNESS && etag == Some(tag.as_str()))
    }

    pub(super) fn invalidate(&mut self, path: &str) {
        self.0.remove(path.trim_start_matches('/'));
    }
}

fn strong_etag(etag: &str) -> bool {
    !etag.is_empty()
        && etag.len() <= 1024
        && !etag.starts_with("W/")
        && !etag.chars().any(char::is_control)
}

#[derive(Default)]
struct Response {
    href: String,
    etag: Option<String>,
    status: String,
}

/// Keep successful propstat validators associated with their own response.
/// Missing, weak or failed validators merely disable the optimization.
pub(super) fn parse(xml: &str, base: &Url) -> SyncResult<Vec<(Url, Option<String>)>> {
    if xml.len() > 8 * 1024 * 1024 {
        return Err(std::io::Error::other("WebDAV directory listing exceeds size limit").into());
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut response = None::<Response>;
    let mut propstat = false;
    let mut property_etag = String::new();
    let mut property_status = String::new();
    let mut field = Vec::new();
    let mut result = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(node) => {
                let name = node.local_name();
                match name.as_ref() {
                    b"response" => response = Some(Response::default()),
                    b"propstat" => {
                        propstat = true;
                        property_etag.clear();
                        property_status.clear();
                    }
                    b"href" | b"getetag" | b"status" => field = name.as_ref().to_vec(),
                    _ => {}
                }
            }
            Event::Text(text) => {
                let value = text.decode()?;
                append(
                    &mut response,
                    propstat,
                    &field,
                    &mut property_etag,
                    &mut property_status,
                    &value,
                );
            }
            Event::GeneralRef(reference) => {
                let escaped = format!("&{};", reference.decode()?);
                let value = quick_xml::escape::unescape(&escaped)?;
                append(
                    &mut response,
                    propstat,
                    &field,
                    &mut property_etag,
                    &mut property_status,
                    &value,
                );
            }
            Event::CData(text) => append(
                &mut response,
                propstat,
                &field,
                &mut property_etag,
                &mut property_status,
                &text.decode()?,
            ),
            Event::End(node) => match node.local_name().as_ref() {
                b"href" | b"getetag" | b"status" => field.clear(),
                b"propstat" => {
                    if successful(&property_status)
                        && strong_etag(&property_etag)
                        && let Some(response) = &mut response
                    {
                        response.etag = Some(property_etag.clone());
                    }
                    propstat = false;
                }
                b"response" => {
                    if let Some(response) = response.take()
                        && (response.status.is_empty() || successful(&response.status))
                        && let Ok(url) =
                            Url::parse(&response.href).or_else(|_| base.join(&response.href))
                    {
                        result.push((url, response.etag));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(result)
}

fn successful(status: &str) -> bool {
    status.split_whitespace().nth(1) == Some("200")
}

fn append(
    response: &mut Option<Response>,
    propstat: bool,
    field: &[u8],
    etag: &mut String,
    status: &mut String,
    value: &str,
) {
    let Some(response) = response else { return };
    match field {
        b"href" if !propstat => response.href.push_str(value),
        b"getetag" if propstat => etag.push_str(value),
        b"status" if propstat => status.push_str(value),
        b"status" => response.status.push_str(value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_and_propstat_failures_never_supply_validators() {
        let base = Url::parse("https://dav.example.test/state/").unwrap();
        let xml = r#"<d:multistatus xmlns:d="DAV:">
        <d:response><d:href>a.json</d:href><d:propstat><d:prop><d:getetag>&quot;a&quot;</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:propstat><d:prop><d:getetag>bad</d:getetag></d:prop><d:status>HTTP/1.1 403 Forbidden</d:status></d:propstat></d:response>
        <d:response><d:href>b.json</d:href><d:propstat><d:prop><d:getetag>bad</d:getetag></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
        <d:response><d:href>c.json</d:href><d:propstat><d:prop><d:getetag><![CDATA[W/"c"]]></d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
        <d:response><d:href>missing.json</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response></d:multistatus>"#;
        let entries = parse(xml, &base).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0].0.as_str(),
            "https://dav.example.test/state/a.json"
        );
        assert_eq!(entries[0].1.as_deref(), Some("\"a\""));
        assert!(entries[1].1.is_none() && entries[2].1.is_none());
    }

    #[test]
    fn escaped_paths_and_numeric_entities_preserve_validator_bytes() {
        let base = Url::parse("https://dav.example.test/state/").unwrap();
        let xml = r#"<multistatus><response><href>a&amp;b.json</href><propstat><prop><getetag>&#34;a&#x26;b&#34;</getetag></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        let entries = parse(xml, &base).unwrap();
        assert_eq!(
            entries[0].0.as_str(),
            "https://dav.example.test/state/a&b.json"
        );
        assert_eq!(entries[0].1.as_deref(), Some("\"a&b\""));
    }

    #[test]
    fn fresh_listings_replace_old_proofs_and_expire_without_persisting() {
        let mut cache = ListingCache::default();
        cache.replace(
            "state/book/devices/",
            vec![("peer.json".into(), Some("v1".into()))],
        );
        assert!(cache.matches("state/book/devices/peer.json", Some("v1")));
        assert!(!cache.matches("state/other/devices/peer.json", Some("v1")));
        cache.replace(
            "state/book/devices/",
            vec![("peer.json".into(), Some("v2".into()))],
        );
        assert!(!cache.matches("state/book/devices/peer.json", Some("v1")));
        cache.0.get_mut("state/book/devices/peer.json").unwrap().1 -=
            FRESHNESS + Duration::from_secs(1);
        assert!(!cache.matches("state/book/devices/peer.json", Some("v2")));
        cache.replace("state/book/devices/", vec![]);
        assert!(cache.0.is_empty());
    }
}
