//! The filesystem-read primitive had no gate of its own.
//!
//! `fetch_file_url` read whatever path it was handed. The `--allow-file-access`
//! check lived entirely in the callers — CDP's `Page.navigate` and
//! `Target.createTarget` via `util::url_is_file_scheme`, and
//! `subresource_allowed` in obscura-browser — so the invariant held only as
//! long as every future call site remembered it.
//!
//! That is the shape of GHSA-q55h-vfv9-qcr5 and of its incomplete-fix variant.
//! It is also the shape of two other gaps in this same batch: a security
//! decision enforced at N call sites gets enforced at N-1 sooner or later.
//! Nothing here claims the old arrangement was exploitable through a path that
//! shipped — the point is that it was one forgetful call site away.
//!
//! These tests drive the public client rather than the private primitive,
//! because what matters is the decision a caller actually gets by default.
//!
//! Run with `cargo test -p obscura-net --test file_scheme_gate`.

use std::sync::Arc;

use obscura_net::{CookieJar, ObscuraHttpClient};
use url::Url;

fn temp_file_with(contents: &str) -> (tempfile::NamedTempFile, Url) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), contents).unwrap();
    let url = Url::from_file_path(file.path()).expect("temp path must be a valid file URL");
    (file, url)
}

/// The default. A freshly built client must refuse `file://`, so a caller that
/// forgets the gate gets the safe answer rather than the filesystem.
#[tokio::test]
async fn a_default_client_refuses_to_read_a_file_url() {
    let (_keep, url) = temp_file_with("SECRET-LOCAL-FILE");
    let client = ObscuraHttpClient::with_options(Arc::new(CookieJar::new()), None);

    let error = client
        .fetch(&url)
        .await
        .expect_err("a default client must not read local files");
    let rendered = error.to_string();
    assert!(
        !rendered.contains("SECRET-LOCAL-FILE"),
        "the refusal must not leak file contents: {rendered}"
    );
    assert!(
        rendered.contains("file://") || rendered.to_lowercase().contains("disabled"),
        "the refusal should say why: {rendered}"
    );
}

/// The opt-in still works, or `--allow-file-access` and `obscura fetch
/// file://...` would both be broken.
#[tokio::test]
async fn a_client_with_file_access_enabled_reads_the_file() {
    let (_keep, url) = temp_file_with("LOCAL-FILE-BODY");
    let client = ObscuraHttpClient::with_options(Arc::new(CookieJar::new()), None);
    client.set_allow_file_access(true);

    let response = client
        .fetch(&url)
        .await
        .expect("an opted-in client must read local files");
    assert_eq!(response.status, 200);
    assert!(
        response.text().contains("LOCAL-FILE-BODY"),
        "expected the file body, got {:?}",
        response.text()
    );
}

/// The flag is readable, and defaults off. This is the assertion that fails if
/// someone later flips the default "for convenience".
#[tokio::test]
async fn file_access_is_off_by_default() {
    let client = ObscuraHttpClient::with_options(Arc::new(CookieJar::new()), None);
    assert!(
        !client.allow_file_access(),
        "file:// access must be opt-in, never the default"
    );
    client.set_allow_file_access(true);
    assert!(client.allow_file_access());
    client.set_allow_file_access(false);
    assert!(!client.allow_file_access());
}

/// Turning the gate back off must actually stop reads — a one-way latch would
/// mean a context could never revoke the permission it granted.
#[tokio::test]
async fn revoking_file_access_stops_further_reads() {
    let (_keep, url) = temp_file_with("LOCAL-FILE-BODY");
    let client = ObscuraHttpClient::with_options(Arc::new(CookieJar::new()), None);

    client.set_allow_file_access(true);
    assert!(client.fetch(&url).await.is_ok(), "opted in, so the read must succeed");

    client.set_allow_file_access(false);
    assert!(
        client.fetch(&url).await.is_err(),
        "after revoking, the read must be refused again"
    );
}

/// A missing file with the gate closed must report the gate, not the missing
/// file: leaking "no such file" for an arbitrary path is a filesystem oracle.
#[tokio::test]
async fn the_refusal_does_not_double_as_a_filesystem_oracle() {
    let client = ObscuraHttpClient::with_options(Arc::new(CookieJar::new()), None);
    let absent = Url::parse("file:///definitely/not/here/obscura-m7-probe").unwrap();

    let rendered = client
        .fetch(&absent)
        .await
        .expect_err("must be refused")
        .to_string()
        .to_lowercase();
    assert!(
        !rendered.contains("no such file") && !rendered.contains("not found"),
        "the refusal must not reveal whether the path exists: {rendered}"
    );
}
