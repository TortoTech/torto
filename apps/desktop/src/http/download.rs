//! Bounded binary transport shared by fonts and application updates.
use sha2::{Digest, Sha256};

pub(crate) fn download_builder() -> reqwest::ClientBuilder {
    super::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .read_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(180))
}

pub(crate) async fn fetch_sized_body(
    request: reqwest::RequestBuilder,
    expected: u64,
    limit: u64,
    progress: impl FnMut(u8),
) -> Result<Vec<u8>, String> {
    let response = request
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    read_sized_body(response, expected, limit, progress).await
}

pub(crate) async fn read_sized_body(
    mut response: reqwest::Response,
    expected: u64,
    limit: u64,
    mut progress: impl FnMut(u8),
) -> Result<Vec<u8>, String> {
    if expected > limit || expected > usize::MAX as u64 {
        return Err("Download exceeds size limit".into());
    }
    if response
        .content_length()
        .is_some_and(|size| size != expected)
    {
        return Err("Unexpected download size".into());
    }
    let mut bytes = Vec::with_capacity((expected as usize).min(1024 * 1024));
    let mut previous = 0;
    progress(0);
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if chunk.len() as u64 > expected.saturating_sub(bytes.len() as u64) {
            return Err("Download exceeds its declared size".into());
        }
        bytes.extend_from_slice(&chunk);
        let percent = ((bytes.len() as u128 * 100) / u128::from(expected.max(1))).min(99) as u8;
        if percent >= previous + 5 {
            previous = percent;
            progress(percent);
        }
    }
    if bytes.len() as u64 != expected {
        return Err("Incomplete download".into());
    }
    Ok(bytes)
}

pub(crate) fn verify_sized_sha256(
    bytes: &[u8],
    expected: u64,
    limit: u64,
    sha256: &str,
) -> Result<(), String> {
    let actual = bytes.len() as u64;
    if actual != expected || actual > limit {
        return Err(format!(
            "Downloaded file size mismatch: expected {expected}, received {actual}"
        ));
    }
    if format!("{:x}", Sha256::digest(bytes)) != sha256 {
        return Err("Downloaded file failed SHA-256 verification".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    #[test]
    fn bodies_without_content_length_are_still_bounded_and_complete() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = crate::http::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        for (body, expected, valid) in [
            (b"1234".as_slice(), 4, true),
            (b"12", 4, false),
            (b"12345", 4, false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0; 2048];
                assert!(stream.read(&mut request).unwrap() > 0);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                    .unwrap();
                let _ = stream.write_all(body);
            });
            let result = runtime.block_on(async {
                let response = client
                    .get(format!("http://{address}/file"))
                    .send()
                    .await
                    .unwrap();
                read_sized_body(response, expected, 4, |_| {}).await
            });
            assert_eq!(result.is_ok(), valid);
            if valid {
                assert_eq!(result.unwrap(), body);
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn matching_size_does_not_allow_corrupt_or_over_budget_content() {
        let bytes = b"font";
        let hash = format!("{:x}", Sha256::digest(bytes));
        assert!(verify_sized_sha256(bytes, 4, 4, &hash).is_ok());
        assert!(verify_sized_sha256(b"fake", 4, 4, &hash).is_err());
        assert!(verify_sized_sha256(bytes, 4, 3, &hash).is_err());
    }
}
