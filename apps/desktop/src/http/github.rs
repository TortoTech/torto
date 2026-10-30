//! GitHub API addresses and request headers shared by fonts and the updater.
use reqwest::{
    Client, RequestBuilder, Url,
    header::{ACCEPT, USER_AGENT},
};

const API_VERSION: &str = "2026-03-10";

#[derive(Clone, Copy)]
pub(crate) struct Repository(&'static str);
pub(crate) const TORTO: Repository = Repository("torto");
pub(crate) const LAB: Repository = Repository("lab");

impl Repository {
    fn endpoint(self, path: &str) -> String {
        format!("https://api.github.com/repos/TortoTech/{}/{path}", self.0)
    }
    pub(crate) fn releases_url(self) -> String {
        self.endpoint("releases")
    }
    pub(crate) fn latest_release_url(self) -> String {
        self.endpoint("releases/latest")
    }
    pub(crate) fn asset_url(self, id: u64) -> String {
        self.endpoint(&format!("releases/assets/{id}"))
    }

    pub(crate) fn is_asset_url(self, url: &Url) -> bool {
        is_api_url(url)
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .path()
                .strip_prefix(&format!("/repos/TortoTech/{}/releases/assets/", self.0))
                .is_some_and(|id| {
                    id.bytes().all(|byte| byte.is_ascii_digit())
                        && id.parse::<u64>().is_ok_and(|id| id > 0)
                })
    }
}

pub(crate) fn is_api_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("api.github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
}

fn request(client: &Client, url: &str, accept: &'static str) -> RequestBuilder {
    client
        .get(url)
        .header(ACCEPT, accept)
        .header("X-GitHub-Api-Version", API_VERSION)
        .header(USER_AGENT, concat!("Torto/", env!("CARGO_PKG_VERSION")))
}

pub(crate) fn json_request(client: &Client, url: &str) -> RequestBuilder {
    request(client, url, "application/vnd.github+json")
}

pub(crate) fn asset_request(client: &Client, url: &str) -> RequestBuilder {
    // Both a direct binary 200 and a redirect 302 are valid asset responses.
    request(client, url, "application/octet-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_addresses_and_trust_are_scoped_to_the_requested_repository() {
        let lab = Url::parse(&LAB.asset_url(42)).unwrap();
        assert!(LAB.is_asset_url(&lab));
        assert!(!TORTO.is_asset_url(&lab));
        assert_eq!(
            TORTO.latest_release_url(),
            "https://api.github.com/repos/TortoTech/torto/releases/latest"
        );
        assert_eq!(
            TORTO.releases_url(),
            "https://api.github.com/repos/TortoTech/torto/releases"
        );
        for url in [
            "https://api.github.com/repos/TortoTech/lab/releases/assets/0",
            "https://api.github.com/repos/TortoTech/lab/releases/assets/+42",
            "https://api.github.com/repos/TortoTech/lab/releases/assets/42?redirect=evil",
            "https://api.github.com/repos/TortoTech/lab/releases/assets/42#fragment",
            "https://user@api.github.com/repos/TortoTech/lab/releases/assets/42",
            "https://api.github.com:444/repos/TortoTech/lab/releases/assets/42",
        ] {
            assert!(!LAB.is_asset_url(&Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn metadata_and_binary_requests_share_api_version_without_credentials() {
        let client = crate::http::client();
        for (request, accept) in [
            (
                json_request(&client, &TORTO.latest_release_url()),
                "application/vnd.github+json",
            ),
            (
                asset_request(&client, &LAB.asset_url(42)),
                "application/octet-stream",
            ),
        ] {
            let request = request.build().unwrap();
            assert_eq!(request.headers()[ACCEPT], accept);
            assert_eq!(request.headers()["X-GitHub-Api-Version"], API_VERSION);
            assert!(
                request.headers()[USER_AGENT]
                    .to_str()
                    .unwrap()
                    .starts_with("Torto/")
            );
            assert!(!request.headers().contains_key("authorization"));
        }
    }
}
