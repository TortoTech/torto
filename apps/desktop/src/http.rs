//! One HTTP stack and TLS backend for both Rig and application requests.
use std::sync::Once;

pub(crate) fn builder() -> reqwest::ClientBuilder {
    static TLS: Once = Once::new();
    TLS.call_once(|| {
        // reqwest's rustls-no-provider feature needs this before constructing
        // any client. An already installed provider is also valid.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
}

pub(crate) fn client() -> reqwest::Client {
    builder().build().expect("HTTP client initialization")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn https_validates_certificates_with_the_shared_ring_backend() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for trusted in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let certificate = rustls::pki_types::CertificateDer::from(
                include_bytes!("http/testdata/localhost.der").to_vec(),
            );
            // Self-authored fixtures: this private key is only for local tests.
            let key = rustls::pki_types::PrivatePkcs8KeyDer::from(
                include_bytes!("http/testdata/localhost-key.der").to_vec(),
            );
            drop(super::builder());
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![certificate], key.into())
                .unwrap();
            let server = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
                let mut stream = rustls::StreamOwned::new(connection, socket);
                let mut request = [0; 1024];
                match stream.read(&mut request) {
                    Ok(length) => {
                        assert!(trusted && length > 0, "untrusted handshake must fail");
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                            .unwrap();
                        stream.flush().unwrap();
                    }
                    Err(_) => assert!(!trusted, "trusted handshake must succeed"),
                }
            });
            let mut builder = super::builder().no_proxy().timeout(Duration::from_secs(5));
            if trusted {
                builder = builder.add_root_certificate(
                    reqwest::Certificate::from_der(include_bytes!("http/testdata/root.der"))
                        .unwrap(),
                );
            }
            let client = builder.build().unwrap();
            runtime.block_on(async {
                let response = client
                    .get(format!("https://localhost:{port}/"))
                    .send()
                    .await;
                if trusted {
                    assert_eq!(response.unwrap().text().await.unwrap(), "OK");
                } else {
                    assert!(response.unwrap_err().is_connect());
                }
            });
            server.join().unwrap();
        }
    }
}
