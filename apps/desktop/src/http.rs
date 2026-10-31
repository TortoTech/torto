pub(crate) use rebook_app_core::http::*;

#[cfg(test)]
pub(crate) fn client() -> reqwest::Client { builder().build().expect("HTTP client") }
