//! A server that sends its leaf without the intermediate. The intermediate is
//! only reachable through the leaf's AIA `caIssuers` URL, served over local
//! plain HTTP. Every test sets SSL_CERT_FILE, so these rely on nextest running
//! one test per process (as the rest of the workspace does).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use obscura_net::{CookieJar, ObscuraHttpClient};
use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, IsCa, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if content.len() < 0x80 {
        out.push(content.len() as u8);
    } else {
        out.extend([0x82, (content.len() >> 8) as u8, content.len() as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn aia_extension(url: &str) -> CustomExtension {
    // AccessDescription { caIssuers (1.3.6.1.5.5.7.48.2), uniformResourceIdentifier }
    let mut desc = der(0x06, &[0x2B, 6, 1, 5, 5, 7, 48, 2]);
    desc.extend(der(0x86, url.as_bytes()));
    CustomExtension::from_oid_content(&[1, 3, 6, 1, 5, 5, 7, 1, 1], der(0x30, &der(0x30, &desc)))
}

fn ca(
    name: &str,
    signer: Option<(&rcgen::Certificate, &KeyPair)>,
) -> (rcgen::Certificate, KeyPair) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(DnType::CommonName, name);
    let cert = match signer {
        Some((issuer, issuer_key)) => params.signed_by(&key, issuer, issuer_key).unwrap(),
        None => params.self_signed(&key).unwrap(),
    };
    (cert, key)
}

struct Fixture {
    https_port: u16,
    aia_hits: Arc<AtomicUsize>,
}

/// Serve `body` at an AIA URL, counting requests. Returns (URL, hit counter).
async fn serve_aia(body: Vec<u8>, status: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}/inter.cer"), hits)
}

/// HTTPS server presenting only `leaf` (no intermediate).
async fn serve_https(leaf: Vec<u8>, leaf_key: Vec<u8>) -> u16 {
    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(leaf)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key)),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else { return };
                let mut buf = [0u8; 4096];
                let _ = tls.read(&mut buf).await;
                let body = "incomplete chain ok";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tls.write_all(resp.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    port
}

/// How the AIA URL answers.
enum Aia {
    /// The real intermediate as DER.
    Intermediate,
    /// An intermediate with the real one's name and key, re-issued by a CA
    /// nobody trusts: it matches the leaf's issuer but cannot chain.
    ImpostorIntermediate,
    NotFound,
}

/// Root (trusted through SSL_CERT_FILE) -> intermediate -> leaf for 127.0.0.1.
async fn fixture(aia: Aia) -> Fixture {
    let (root, root_key) = ca("AIA Test Root", None);
    let ca_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ca_file.path(), root.pem()).unwrap();
    let (_, path) = ca_file.keep().unwrap();
    std::env::set_var("SSL_CERT_FILE", path);

    let (inter, inter_key) = ca("AIA Test Intermediate", Some((&root, &root_key)));
    let (aia_url, aia_hits) = match aia {
        Aia::Intermediate => serve_aia(inter.der().to_vec(), "200 OK").await,
        Aia::NotFound => serve_aia(b"missing".to_vec(), "404 Not Found").await,
        Aia::ImpostorIntermediate => {
            let (untrusted, untrusted_key) = ca("Untrusted Root", None);
            let mut params = CertificateParams::new(Vec::new()).unwrap();
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params.distinguished_name.push(DnType::CommonName, "AIA Test Intermediate");
            let fake = params.signed_by(&inter_key, &untrusted, &untrusted_key).unwrap();
            serve_aia(fake.der().to_vec(), "200 OK").await
        }
    };

    let leaf_key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
    params.custom_extensions.push(aia_extension(&aia_url));
    let leaf = params.signed_by(&leaf_key, &inter, &inter_key).unwrap();
    let https_port = serve_https(leaf.der().to_vec(), leaf_key.serialize_der()).await;
    Fixture { https_port, aia_hits }
}

fn url(f: &Fixture) -> Url {
    Url::parse(&format!("https://127.0.0.1:{}/", f.https_port)).unwrap()
}

fn default_client() -> ObscuraHttpClient {
    ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true)
}

#[tokio::test]
async fn default_client_completes_an_incomplete_chain_via_aia() {
    let f = fixture(Aia::Intermediate).await;
    let client = default_client();
    let resp = client.fetch(&url(&f)).await.expect("AIA intermediate must complete the chain");
    assert_eq!((resp.status, resp.text().as_str()), (200, "incomplete chain ok"));
    assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1);
    // Cached: later connections reuse the intermediate without another fetch.
    client.fetch(&url(&f)).await.unwrap();
    assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn default_client_fails_when_the_aia_url_has_no_certificate() {
    let f = fixture(Aia::NotFound).await;
    assert!(default_client().fetch(&url(&f)).await.is_err());
    assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn default_client_ignores_an_intermediate_that_does_not_chain() {
    let f = fixture(Aia::ImpostorIntermediate).await;
    let client = default_client();
    assert!(client.fetch(&url(&f)).await.is_err(), "an untrusted chain must stay rejected");
    assert!(client.fetch(&url(&f)).await.is_err());
    assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1, "the failed URL is not refetched");
}

#[cfg(feature = "stealth")]
mod stealth {
    use super::*;
    use obscura_net::StealthHttpClient;

    fn client() -> StealthHttpClient {
        StealthHttpClient::with_proxy(Arc::new(CookieJar::new()), None, true)
    }

    #[tokio::test]
    async fn stealth_client_completes_an_incomplete_chain_via_aia() {
        let f = fixture(Aia::Intermediate).await;
        let client = client();
        let resp = client.fetch(&url(&f)).await.expect("AIA intermediate must complete the chain");
        assert_eq!((resp.status, resp.text().as_str()), (200, "incomplete chain ok"));
        assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1);
        client.fetch(&url(&f)).await.unwrap();
        assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stealth_client_fails_when_the_aia_url_has_no_certificate() {
        let f = fixture(Aia::NotFound).await;
        assert!(client().fetch(&url(&f)).await.is_err());
        assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stealth_client_ignores_an_intermediate_that_does_not_chain() {
        let f = fixture(Aia::ImpostorIntermediate).await;
        let client = client();
        assert!(client.fetch(&url(&f)).await.is_err(), "an untrusted chain must stay rejected");
        assert!(client.fetch(&url(&f)).await.is_err());
        assert_eq!(f.aia_hits.load(Ordering::SeqCst), 1);
    }
}
