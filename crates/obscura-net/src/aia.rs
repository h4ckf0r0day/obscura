//! AIA `caIssuers` fetching for servers that send an incomplete certificate
//! chain (a leaf without its intermediate). Chrome and Safari fetch the missing
//! intermediate from the leaf's Authority Information Access extension.
//!
//! Fetched certificates are only ever offered to the verifier as untrusted
//! *intermediates*. Trust anchors, hostname, expiry and signature checks are
//! unchanged, so a fetched certificate that does not lead to an already trusted
//! root is simply ignored.
//!
//! This module holds the pieces both transports share: a minimal DER reader
//! (just enough to read subject, issuer and the AIA extension, and to unwrap
//! PEM and PKCS#7 bundles), the bounded intermediate cache, and the guarded
//! fetch. The reqwest/rustls verifier hook lives at the bottom; the stealth
//! hook is in `wreq_client.rs`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::redirect::Policy;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use url::Url;

use crate::client::{validate_url, SsrfGuardResolver};

/// Retries after a failed verification, so a chain missing up to this many
/// intermediates still resolves.
pub(crate) const MAX_ROUNDS: usize = 3;
const MAX_BODY_BYTES: usize = 64 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REDIRECTS: usize = 3;
const MAX_URLS_PER_CERT: usize = 3;
const MAX_CERTS_PER_URL: usize = 8;
const MAX_CACHE_URLS: usize = 64;
const NEGATIVE_TTL: Duration = Duration::from_secs(300);

// ---- DER reading ----------------------------------------------------------

const OID_AIA: &[u8] = &[0x2B, 6, 1, 5, 5, 7, 1, 1];
const OID_CA_ISSUERS: &[u8] = &[0x2B, 6, 1, 5, 5, 7, 48, 2];
const OID_SIGNED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 1, 7, 2];

/// One TLV: (tag, content, bytes after it). A BER indefinite length (some CAs
/// emit it in PKCS#7) takes the rest of the input as content.
fn tlv(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = buf.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&l0, rest) = rest.split_first()?;
    if l0 == 0x80 {
        return Some((tag, rest, &[]));
    }
    let (len, rest) = if l0 < 0x80 {
        (l0 as usize, rest)
    } else {
        let n = (l0 & 0x7f) as usize;
        if n > 3 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |a, &b| a << 8 | b as usize), &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// Children of a constructed value as (tag, content). Stops at the first
/// malformed element or a BER end-of-contents marker.
fn children(mut buf: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    std::iter::from_fn(move || {
        let (tag, content, rest) = tlv(buf)?;
        if tag == 0 {
            return None;
        }
        buf = rest;
        Some((tag, content))
    })
}

struct CertParts<'a> {
    issuer: &'a [u8],
    subject: &'a [u8],
    extensions: &'a [u8],
}

fn cert_parts(der: &[u8]) -> Option<CertParts<'_>> {
    let (0x30, cert, _) = tlv(der)? else { return None };
    let (0x30, tbs, _) = tlv(cert)? else { return None };
    let mut it = children(tbs).peekable();
    if it.peek().is_some_and(|(t, _)| *t == 0xA0) {
        it.next(); // version
    }
    let mut next = |tag: u8| it.next().filter(|(t, _)| *t == tag).map(|(_, c)| c);
    next(0x02)?; // serial
    next(0x30)?; // signature algorithm
    let issuer = next(0x30)?;
    next(0x30)?; // validity
    let subject = next(0x30)?;
    next(0x30)?; // subject public key info
    let mut extensions = &[][..];
    for (tag, content) in it {
        if tag == 0xA3 {
            extensions = tlv(content).filter(|(t, _, _)| *t == 0x30).map_or(&[], |(_, c, _)| c);
        }
    }
    Some(CertParts { issuer, subject, extensions })
}

/// `http(s)` URLs from the certificate's AIA `caIssuers` entries.
pub(crate) fn ca_issuer_urls(der: &[u8]) -> Vec<String> {
    let Some(parts) = cert_parts(der) else { return Vec::new() };
    let mut urls = Vec::new();
    for (_, ext) in children(parts.extensions) {
        let mut f = children(ext);
        if f.next().map(|(_, c)| c) != Some(OID_AIA) {
            continue;
        }
        // Optional `critical` BOOLEAN sits before the OCTET STRING.
        let Some((_, value)) = f.find(|(t, _)| *t == 0x04) else { continue };
        let Some((0x30, descriptions, _)) = tlv(value) else { continue };
        for (_, desc) in children(descriptions) {
            let mut d = children(desc);
            if d.next().map(|(_, c)| c) != Some(OID_CA_ISSUERS) {
                continue;
            }
            // GeneralName uniformResourceIdentifier [6]
            if let Some((0x86, uri)) = d.next() {
                if let Ok(uri) = std::str::from_utf8(uri) {
                    if uri.starts_with("http://") || uri.starts_with("https://") {
                        urls.push(uri.to_string());
                    }
                }
            }
        }
    }
    urls
}

/// Certificates in a response body: a single DER certificate, a PEM file
/// (certificates or PKCS#7), or a DER PKCS#7 `SignedData` bundle (`.p7c`).
pub(crate) fn parse_certificates(body: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if body.starts_with(b"-----BEGIN") || body.trim_ascii_start().starts_with(b"-----BEGIN") {
        for block in pem_blocks(body) {
            collect_der(&block, &mut out);
        }
    } else {
        collect_der(body, &mut out);
    }
    out.truncate(MAX_CERTS_PER_URL);
    out
}

fn collect_der(der: &[u8], out: &mut Vec<Vec<u8>>) {
    let Some((0x30, content, rest)) = tlv(der) else { return };
    let mut it = children(content);
    match it.next() {
        Some((0x06, oid)) if oid == OID_SIGNED_DATA => {
            // ContentInfo { OID, [0] { SignedData { version, digests, content, [0] certs } } }
            let signed = it.find(|(t, _)| *t == 0xA0).and_then(|(_, c)| tlv(c));
            let Some((0x30, signed, _)) = signed else { return };
            let certs = children(signed).find(|(t, _)| *t == 0xA0);
            if let Some((_, set)) = certs {
                let mut buf = set;
                while let Some((0x30, _, after)) = tlv(buf) {
                    let used = buf.len() - after.len();
                    if cert_parts(&buf[..used]).is_some() {
                        out.push(buf[..used].to_vec());
                    }
                    buf = after;
                }
            }
        }
        Some((0x30, _)) => {
            let used = der.len() - rest.len();
            if cert_parts(&der[..used]).is_some() {
                out.push(der[..used].to_vec());
            }
        }
        _ => {}
    }
}

fn pem_blocks(text: &[u8]) -> Vec<Vec<u8>> {
    use base64::Engine;
    let mut blocks = Vec::new();
    let mut body: Option<Vec<u8>> = None;
    for line in text.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.starts_with(b"-----BEGIN") {
            body = Some(Vec::new());
        } else if line.starts_with(b"-----END") {
            if let Some(b64) = body.take() {
                if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(b64) {
                    blocks.push(der);
                }
            }
        } else if let Some(b64) = body.as_mut() {
            b64.extend_from_slice(line);
        }
    }
    blocks
}

fn is_self_issued(parts: &CertParts<'_>) -> bool {
    parts.subject == parts.issuer
}

/// Index of the topmost certificate reachable from `chain[0]` by following
/// issuer to subject links inside `chain`. Its issuer is what is missing.
fn tip(chain: &[Vec<u8>]) -> usize {
    let mut cur = 0;
    for _ in 0..chain.len() {
        let Some(parts) = cert_parts(&chain[cur]) else { break };
        if is_self_issued(&parts) {
            break;
        }
        let next = chain.iter().position(|c| {
            cert_parts(c).is_some_and(|p| p.subject == parts.issuer)
        });
        match next {
            Some(j) if j != cur => cur = j,
            _ => break,
        }
    }
    cur
}

// ---- cache ----------------------------------------------------------------

enum Entry {
    Certs(Vec<Arc<[u8]>>),
    Failed(Instant),
}

#[derive(Default)]
struct Cache {
    /// Oldest first; evicted from the front once `MAX_CACHE_URLS` is reached.
    entries: VecDeque<(String, Entry)>,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Cache {
    fn contains(&self, url: &str) -> bool {
        self.entries.iter().any(|(u, e)| {
            u == url
                && match e {
                    Entry::Certs(_) => true,
                    Entry::Failed(at) => at.elapsed() < NEGATIVE_TTL,
                }
        })
    }

    fn insert(&mut self, url: String, entry: Entry) {
        self.entries.retain(|(u, _)| *u != url);
        if self.entries.len() >= MAX_CACHE_URLS {
            self.entries.pop_front();
        }
        if matches!(entry, Entry::Certs(_)) {
            GENERATION.fetch_add(1, Ordering::Release);
        }
        self.entries.push_back((url, entry));
    }

    fn issuer_of(&self, issuer: &[u8]) -> Option<Arc<[u8]>> {
        self.entries.iter().find_map(|(_, e)| match e {
            Entry::Certs(certs) => certs
                .iter()
                .find(|c| cert_parts(c).is_some_and(|p| p.subject == issuer))
                .cloned(),
            Entry::Failed(_) => None,
        })
    }
}

/// Bumped whenever the cache changes. A client built at an older generation
/// may be missing intermediates.
#[cfg_attr(not(feature = "stealth"), allow(dead_code))]
pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Encoded subject name of a certificate.
#[cfg_attr(not(feature = "stealth"), allow(dead_code))]
pub(crate) fn subject_name(der: &[u8]) -> Option<Vec<u8>> {
    cert_parts(der).map(|p| p.subject.to_vec())
}

/// Encoded issuer name of the topmost certificate of `chain`.
#[cfg_attr(not(feature = "stealth"), allow(dead_code))]
pub(crate) fn tip_issuer_name(chain: &[Vec<u8>]) -> Option<Vec<u8>> {
    cert_parts(chain.get(tip(chain))?).map(|p| p.issuer.to_vec())
}

/// Append cached intermediates that extend `chain` upward. Returns how many
/// were added.
pub(crate) fn extend_from_cache(chain: &mut Vec<Vec<u8>>) -> usize {
    let cache = cache();
    let mut added = 0;
    for _ in 0..MAX_ROUNDS + 1 {
        let t = tip(chain);
        let Some(parts) = cert_parts(&chain[t]) else { break };
        if is_self_issued(&parts) {
            break;
        }
        let Some(found) = cache.issuer_of(parts.issuer) else { break };
        chain.push(found.to_vec());
        added += 1;
    }
    added
}

/// All cached intermediates, for building a client-wide certificate store.
#[cfg_attr(not(feature = "stealth"), allow(dead_code))]
pub(crate) fn cached_certificates() -> Vec<Arc<[u8]>> {
    let cache = cache();
    cache
        .entries
        .iter()
        .flat_map(|(_, e)| match e {
            Entry::Certs(certs) => certs.clone(),
            Entry::Failed(_) => Vec::new(),
        })
        .collect()
}

// ---- guarded fetch --------------------------------------------------------

/// Fetch one AIA URL under the same SSRF policy as other requests: URL and
/// every redirect hop validated, DNS results checked by `SsrfGuardResolver`,
/// small body cap, short timeout. This client never touches the AIA hook.
async fn fetch_url(url: &Url, allow_private: bool, proxy: Option<&str>) -> Option<Vec<u8>> {
    let http_only = |u: &Url| matches!(u.scheme(), "http" | "https");
    if !http_only(url) || validate_url(url, allow_private).is_err() {
        tracing::debug!(%url, "AIA URL refused by network policy");
        return None;
    }
    let policy = Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS
            || !http_only(attempt.url())
            || validate_url(attempt.url(), allow_private).is_err()
        {
            attempt.error("AIA redirect refused")
        } else {
            attempt.follow()
        }
    });
    let mut builder = reqwest::Client::builder()
        .redirect(policy)
        .timeout(FETCH_TIMEOUT)
        .dns_resolver(Arc::new(SsrfGuardResolver::new(allow_private)));
    if let Some(proxy) = proxy.and_then(|p| reqwest::Proxy::all(p).ok()) {
        builder = builder.proxy(proxy);
    }
    let mut resp = builder.build().ok()?.get(url.as_str()).send().await.ok()?;
    if !resp.status().is_success()
        || resp.content_length().is_some_and(|n| n > MAX_BODY_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if chunk.len() > MAX_BODY_BYTES - body.len() {
            tracing::debug!(%url, "AIA response over the size cap");
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

/// Fetch the issuer of `chain`'s tip from its AIA URLs into the cache. Only a
/// bundle that contains the tip's direct issuer is kept. Returns true when the
/// cache gained certificates.
pub(crate) async fn fetch_missing_issuer(
    chain: &[Vec<u8>],
    allow_private: bool,
    proxy: Option<&str>,
) -> bool {
    let Some(tip_der) = chain.get(tip(chain)) else { return false };
    let Some(parts) = cert_parts(tip_der) else { return false };
    if is_self_issued(&parts) {
        return false;
    }
    for raw in ca_issuer_urls(tip_der).into_iter().take(MAX_URLS_PER_CERT) {
        let Ok(url) = Url::parse(&raw) else { continue };
        if cache().contains(url.as_str()) {
            continue;
        }
        let certs: Vec<Arc<[u8]>> = match fetch_url(&url, allow_private, proxy).await {
            Some(body) => parse_certificates(&body).into_iter().map(Arc::from).collect(),
            None => Vec::new(),
        };
        let issues_tip = certs
            .iter()
            .any(|c| cert_parts(c).is_some_and(|p| p.subject == parts.issuer));
        let url = url.to_string();
        if issues_tip {
            cache().insert(url, Entry::Certs(certs));
            return true;
        }
        cache().insert(url, Entry::Failed(Instant::now()));
    }
    false
}

// ---- rustls hook ----------------------------------------------------------

/// Chains whose verification failed with `UnknownIssuer`, keyed by server
/// name, waiting for the request layer to resolve them. Bounded.
static FAILED: OnceLock<Mutex<HashMap<String, Vec<Vec<u8>>>>> = OnceLock::new();

fn failed() -> std::sync::MutexGuard<'static, HashMap<String, Vec<Vec<u8>>>> {
    FAILED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Wraps the stock webpki verifier. On `UnknownIssuer` it retries once with
/// cached intermediates added to the untrusted pool, and otherwise records the
/// presented chain so the request layer can fetch what is missing.
#[derive(Debug)]
pub(crate) struct AiaVerifier {
    inner: Arc<WebPkiServerVerifier>,
}

impl AiaVerifier {
    pub(crate) fn new(inner: Arc<WebPkiServerVerifier>) -> Self {
        Self { inner }
    }
}

impl ServerCertVerifier for AiaVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let unknown_issuer = TlsError::InvalidCertificate(CertificateError::UnknownIssuer);
        match self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(e) if e == unknown_issuer => {}
            other => return other,
        }
        let mut chain: Vec<Vec<u8>> = std::iter::once(end_entity)
            .chain(intermediates)
            .map(|c| c.to_vec())
            .collect();
        if extend_from_cache(&mut chain) > 0 {
            let pool: Vec<CertificateDer<'static>> =
                chain[1..].iter().cloned().map(CertificateDer::from).collect();
            match self
                .inner
                .verify_server_cert(end_entity, &pool, server_name, ocsp_response, now)
            {
                Err(e) if e == unknown_issuer => {}
                other => return other,
            }
        }
        let mut failed = failed();
        if failed.len() >= 32 {
            failed.clear();
        }
        failed.insert(server_name.to_str().into_owned(), chain);
        Err(unknown_issuer)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Send `request`; if it fails because the server's intermediate is missing,
/// fetch it via AIA and retry (at most `MAX_ROUNDS` times). The request must be
/// cloneable (no streaming body), otherwise it is sent once.
pub async fn send_with_aia(
    request: reqwest::RequestBuilder,
    allow_private: bool,
    proxy: Option<&str>,
) -> reqwest::Result<reqwest::Response> {
    let host = request
        .try_clone()
        .and_then(|r| r.build().ok())
        .and_then(|r| r.url().host_str().map(|h| h.trim_matches(['[', ']']).to_string()));
    let mut request = request;
    let mut round = 0;
    loop {
        let retry = request.try_clone();
        let err = match request.send().await {
            Ok(resp) => return Ok(resp),
            Err(e) => e,
        };
        round += 1;
        let (Some(retry), Some(host), true) = (retry, host.as_deref(), round <= MAX_ROUNDS) else {
            return Err(err);
        };
        let Some(chain) = failed().remove(host) else { return Err(err) };
        if !fetch_missing_issuer(&chain, allow_private, proxy).await {
            return Err(err);
        }
        tracing::debug!(host, "retrying after fetching AIA intermediate");
        request = retry;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, CustomExtension, IsCa, KeyPair};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// AIA extension value with one caIssuers entry per URL.
    fn aia_extension(urls: &[&str]) -> CustomExtension {
        let descriptions: Vec<u8> = urls
            .iter()
            .flat_map(|url| {
                let mut d = der(0x06, OID_CA_ISSUERS);
                d.extend(der(0x86, url.as_bytes()));
                der(0x30, &d)
            })
            .collect();
        CustomExtension::from_oid_content(&[1, 3, 6, 1, 5, 5, 7, 1, 1], der(0x30, &descriptions))
    }

    struct Pki {
        root: rcgen::Certificate,
        inter: rcgen::Certificate,
        inter_key: KeyPair,
    }

    fn ca(name: &str, aia: &[&str], signer: Option<(&rcgen::Certificate, &KeyPair)>) -> (rcgen::Certificate, KeyPair) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        if !aia.is_empty() {
            params.custom_extensions.push(aia_extension(aia));
        }
        let cert = match signer {
            Some((issuer, issuer_key)) => params.signed_by(&key, issuer, issuer_key).unwrap(),
            None => params.self_signed(&key).unwrap(),
        };
        (cert, key)
    }

    fn pki(inter_aia: &[&str]) -> Pki {
        let (root, root_key) = ca("Test Root", &[], None);
        let (inter, inter_key) = ca("Test Intermediate", inter_aia, Some((&root, &root_key)));
        Pki { root, inter, inter_key }
    }

    fn leaf(pki: &Pki, aia: &[&str]) -> Vec<u8> {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["leaf.test".to_string()]).unwrap();
        if !aia.is_empty() {
            params.custom_extensions.push(aia_extension(aia));
        }
        params.signed_by(&key, &pki.inter, &pki.inter_key).unwrap().der().to_vec()
    }

    fn pkcs7(certs: &[&[u8]]) -> Vec<u8> {
        let set = certs.concat();
        let signed = [der(0x02, &[1]), der(0x31, &[]), der(0x30, &der(0x06, &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 1, 7, 1])), der(0xA0, &set)].concat();
        der(0x30, &[der(0x06, OID_SIGNED_DATA), der(0xA0, &der(0x30, &signed))].concat())
    }

    fn pem(label: &str, der: &[u8]) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(der);
        let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap()).collect();
        format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", lines.join("\n"))
    }

    /// One-shot-per-connection HTTP server; returns its port.
    async fn serve(body: Vec<u8>, status: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = s.read(&mut buf).await;
                    let head = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&body).await;
                });
            }
        });
        port
    }

    #[test]
    fn extracts_ca_issuer_urls_and_ignores_other_methods() {
        let p = pki(&[]);
        let l = leaf(&p, &["http://ca.test/inter.cer", "http://ca.test/other.cer"]);
        assert_eq!(
            ca_issuer_urls(&l),
            ["http://ca.test/inter.cer", "http://ca.test/other.cer"]
        );
        assert!(ca_issuer_urls(&leaf(&p, &[])).is_empty());
        assert!(ca_issuer_urls(&leaf(&p, &["ldap://ca.test/x"])).is_empty());
        assert!(ca_issuer_urls(b"not a certificate").is_empty());
        assert!(ca_issuer_urls(&[]).is_empty());
    }

    #[test]
    fn parses_der_pem_and_pkcs7_bundles() {
        let p = pki(&[]);
        let inter = p.inter.der().to_vec();
        let root = p.root.der().to_vec();
        assert_eq!(parse_certificates(&inter), [inter.clone()]);
        // Trailing bytes after a DER certificate are dropped.
        let mut padded = inter.clone();
        padded.extend([0, 0, 0]);
        assert_eq!(parse_certificates(&padded), [inter.clone()]);
        assert_eq!(parse_certificates(pem("CERTIFICATE", &inter).as_bytes()), [inter.clone()]);
        let two = pem("CERTIFICATE", &inter) + &pem("CERTIFICATE", &root);
        assert_eq!(parse_certificates(two.as_bytes()), [inter.clone(), root.clone()]);
        let p7 = pkcs7(&[&inter, &root]);
        assert_eq!(parse_certificates(&p7), [inter.clone(), root.clone()]);
        assert_eq!(parse_certificates(pem("PKCS7", &p7).as_bytes()), [inter, root]);
    }

    #[test]
    fn rejects_garbage_and_truncated_input() {
        let p = pki(&[]);
        let inter = p.inter.der().to_vec();
        assert!(parse_certificates(b"").is_empty());
        assert!(parse_certificates(b"<html>404</html>").is_empty());
        assert!(parse_certificates(&inter[..inter.len() / 2]).is_empty());
        assert!(parse_certificates(b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----").is_empty());
        assert!(parse_certificates(&der(0x30, &[0x06, 0x01, 0x2A])).is_empty());
    }

    #[test]
    fn tip_follows_issuers_within_the_chain() {
        let p = pki(&[]);
        let l = leaf(&p, &[]);
        let inter = p.inter.der().to_vec();
        let root = p.root.der().to_vec();
        assert_eq!(tip(&[l.clone()]), 0);
        assert_eq!(tip(&[l.clone(), inter.clone()]), 1);
        // A self-issued root ends the walk; an unrelated certificate is skipped.
        assert_eq!(tip(&[l.clone(), root.clone(), inter.clone()]), 1);
        assert_eq!(tip(&[root.clone()]), 0);
    }

    #[test]
    fn cache_extends_a_chain_and_is_bounded() {
        let p = pki(&[]);
        let l = leaf(&p, &[]);
        let mut chain = vec![l.clone()];
        let before = generation();
        assert_eq!(extend_from_cache(&mut chain), 0);
        cache().insert(
            "http://ca.test/inter.cer".into(),
            Entry::Certs(vec![Arc::from(p.inter.der().to_vec())]),
        );
        assert!(generation() > before);
        assert_eq!(extend_from_cache(&mut chain), 1);
        assert_eq!(chain[1], p.inter.der().to_vec());
        assert!(cache().contains("http://ca.test/inter.cer"));

        for i in 0..MAX_CACHE_URLS * 2 {
            cache().insert(format!("http://ca.test/{i}"), Entry::Failed(Instant::now()));
        }
        assert_eq!(cache().entries.len(), MAX_CACHE_URLS);
        assert!(!cache().contains("http://ca.test/inter.cer"), "oldest entries are evicted");
    }

    #[test]
    fn negative_entries_expire() {
        let mut c = Cache::default();
        c.insert("http://a.test/".into(), Entry::Failed(Instant::now()));
        assert!(c.contains("http://a.test/"));
        c.insert("http://b.test/".into(), Entry::Failed(Instant::now() - NEGATIVE_TTL - Duration::from_secs(1)));
        assert!(!c.contains("http://b.test/"));
    }

    #[tokio::test]
    async fn fetches_and_caches_the_issuer_once() {
        let p = pki(&[]);
        let port = serve(p.inter.der().to_vec(), "200 OK").await;
        let url = format!("http://127.0.0.1:{port}/inter.cer");
        let chain = vec![leaf(&p, &[&url])];
        assert!(fetch_missing_issuer(&chain, true, None).await);
        let mut extended = chain.clone();
        assert_eq!(extend_from_cache(&mut extended), 1);
        // Already cached: nothing new to fetch for the same URL.
        assert!(!fetch_missing_issuer(&chain, true, None).await);
    }

    #[tokio::test]
    async fn ignores_a_bundle_without_the_direct_issuer() {
        let p = pki(&[]);
        let (other, _) = ca("Unrelated CA", &[], None);
        let port = serve(other.der().to_vec(), "200 OK").await;
        let url = format!("http://127.0.0.1:{port}/x.cer");
        let chain = vec![leaf(&p, &[&url])];
        assert!(!fetch_missing_issuer(&chain, true, None).await);
        let mut extended = chain.clone();
        assert_eq!(extend_from_cache(&mut extended), 0);
        // The failure is remembered, so the URL is not retried.
        assert!(cache().contains(&url));
    }

    #[tokio::test]
    async fn refuses_private_aia_urls_without_the_opt_in() {
        let p = pki(&[]);
        let port = serve(p.inter.der().to_vec(), "200 OK").await;
        let url = Url::parse(&format!("http://127.0.0.1:{port}/inter.cer")).unwrap();
        assert!(fetch_url(&url, false, None).await.is_none());
        assert!(fetch_url(&url, true, None).await.is_some());
        let metadata = Url::parse("http://169.254.169.254/latest/meta-data").unwrap();
        assert!(fetch_url(&metadata, false, None).await.is_none());
        let file = Url::parse("file:///etc/passwd").unwrap();
        assert!(fetch_url(&file, true, None).await.is_none());
    }

    #[tokio::test]
    async fn enforces_the_size_cap_and_status() {
        let ok = serve(vec![7; MAX_BODY_BYTES], "200 OK").await;
        let big = serve(vec![7; MAX_BODY_BYTES + 1], "200 OK").await;
        let missing = serve(b"nope".to_vec(), "404 Not Found").await;
        let get = |port: u16| Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        assert_eq!(fetch_url(&get(ok), true, None).await.map(|b| b.len()), Some(MAX_BODY_BYTES));
        assert!(fetch_url(&get(big), true, None).await.is_none());
        assert!(fetch_url(&get(missing), true, None).await.is_none());
    }

    #[tokio::test]
    async fn redirects_are_bounded_and_revalidated() {
        // A server that always redirects to itself.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 302 Found\r\nlocation: /again\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
            }
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        // An endless redirect loop ends at MAX_REDIRECTS even when allowed.
        assert!(fetch_url(&url, true, None).await.is_none());
    }
}
