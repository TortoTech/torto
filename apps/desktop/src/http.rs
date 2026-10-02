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
    use chrono::Datelike;
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn https_validates_certificates_with_the_shared_ring_backend() {
        // macOS enforces short-lived server certificates. Generate fresh test-only
        // credentials so the fixture neither violates that policy nor expires.
        let today = chrono::Utc::now().date_naive();
        let start = today - chrono::Duration::days(1);
        let end = today + chrono::Duration::days(30);
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.not_before = rcgen::date_time_ymd(start.year(), start.month() as u8, start.day() as u8);
        ca.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
        ca.distinguished_name
            .push(DnType::CommonName, "Torto test CA");
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = KeyPair::generate().unwrap();
        let root = ca.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca, ca_key);
        let mut leaf = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        leaf.not_before =
            rcgen::date_time_ymd(start.year(), start.month() as u8, start.day() as u8);
        leaf.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
        leaf.distinguished_name
            .push(DnType::CommonName, "localhost");
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = leaf.signed_by(&leaf_key, &issuer).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for trusted in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let certificate = leaf.der().clone();
            let key = rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der());
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
                builder = builder
                    .add_root_certificate(reqwest::Certificate::from_der(root.der()).unwrap());
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
