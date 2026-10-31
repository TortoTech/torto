TLS loopback fixture

The DER certificate and PKCS#8 private key in this directory are a public,
test-only self-signed identity for the local HTTPS regression server. They
are not trusted certificates, user secrets, or production signing keys.
The test must reject this identity through Android system certificate
verification; it does not install a CA or contact external services.

reader-interactions.epub is an authored synthetic test book containing short
paragraphs, an internal link, a footnote, a formula and the Torto test image.
It contains no imported user book or personal library data.
