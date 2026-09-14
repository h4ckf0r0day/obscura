//! The private-network opt-in was all-or-nothing.
//!
//! `--allow-private-network` disables the entire SSRF deny-set — loopback,
//! RFC1918, link-local, CGNAT, and with them `169.254.169.254` and
//! `100.100.100.200`. It is also the flag an operator *must* set to test an
//! internal application, so the common case handed every page a page loads an
//! unrestricted internal pivot.
//!
//! `--allow-network <CIDR>` exempts only what is named.
//!
//! These drive the real client rather than the policy type directly (the pure
//! logic has its own unit tests in `policy.rs`), so they exercise the wiring:
//! the literal-host check in `validate_url` and the client's stored policy.
//!
//! Run with `cargo test -p obscura-net --test network_allowlist`.

use std::sync::Arc;

use obscura_net::{CookieJar, NetworkPolicy, ObscuraHttpClient};
use url::Url;

/// A client built with an explicit policy, bypassing the environment so these
/// tests are order-independent (`RUST_TEST_THREADS = "1"` shares one process).
fn client_with(policy: NetworkPolicy) -> ObscuraHttpClient {
    let mut client = ObscuraHttpClient::with_options(Arc::new(CookieJar::new()), None);
    client.set_network_policy(policy);
    client
}

fn policy(entries: &[&str]) -> NetworkPolicy {
    NetworkPolicy::new(
        false,
        &entries.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
    .expect("entries must parse")
}

/// The finding. Opening an internal range must not open cloud metadata.
#[tokio::test]
async fn allowing_an_internal_range_leaves_cloud_metadata_blocked() {
    let client = client_with(policy(&["10.20.0.0/16"]));

    let metadata = Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
    let error = client
        .fetch(&metadata)
        .await
        .expect_err("cloud metadata must stay blocked");
    assert!(
        error.to_string().contains("169.254.169.254"),
        "expected an SSRF refusal naming the address, got: {error}"
    );

    let alibaba = Url::parse("http://100.100.100.200/").unwrap();
    assert!(
        client.fetch(&alibaba).await.is_err(),
        "Alibaba metadata must stay blocked"
    );
}

/// The blanket flag keeps its old meaning, including the part that makes it
/// worth avoiding.
#[tokio::test]
async fn the_blanket_flag_still_opens_metadata() {
    let client = client_with(NetworkPolicy::allow_all_private());
    // Not asserting a successful fetch -- nothing is listening -- only that the
    // request is not refused by policy before it is attempted.
    let error = client
        .fetch(&Url::parse("http://169.254.169.254/").unwrap())
        .await
        .expect_err("nothing is listening, so this fails at the transport");
    assert!(
        !error.to_string().contains("not allowed"),
        "--allow-private-network must not refuse metadata by policy: {error}"
    );
}

/// A default client refuses the allowlisted range too, so the flag is what
/// changed the outcome rather than something incidental.
#[tokio::test]
async fn the_default_policy_refuses_the_same_range() {
    let client = client_with(NetworkPolicy::deny_private());
    let error = client
        .fetch(&Url::parse("http://10.20.5.5/").unwrap())
        .await
        .expect_err("10.20.5.5 is RFC1918 and denied by default");
    assert!(
        error.to_string().contains("10.20.5.5"),
        "expected a policy refusal, got: {error}"
    );
}

/// An allowlisted literal address is reachable: it gets past the policy and
/// fails at the transport instead, which is the observable difference.
#[tokio::test]
async fn an_allowlisted_address_is_not_refused_by_policy() {
    let client = client_with(policy(&["10.20.0.0/16"]));
    let error = client
        .fetch(&Url::parse("http://10.20.5.5/").unwrap())
        .await
        .expect_err("nothing is listening there");
    assert!(
        !error.to_string().contains("not allowed"),
        "an allowlisted address must not be refused by policy: {error}"
    );
}

/// A loopback fixture is the realistic shape of "test my internal app", and it
/// must work through `--allow-network` without opening anything else.
#[tokio::test]
async fn a_loopback_allowlist_reaches_a_real_fixture() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = "INTERNAL-APP-OK";
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            );
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });

    let client = client_with(policy(&["127.0.0.1/32"]));
    let response = client
        .fetch(&Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap())
        .await
        .expect("an allowlisted loopback fixture must be reachable");
    assert!(response.text().contains("INTERNAL-APP-OK"));

    // The same client must still refuse everything it was not given.
    assert!(
        client
            .fetch(&Url::parse("http://169.254.169.254/").unwrap())
            .await
            .is_err(),
        "metadata must stay blocked for a loopback-scoped policy"
    );
}

/// An allowlist opens only what it names, and the deny-set is wider than
/// "private" in the colloquial sense.
///
/// This test exists because the first version of it was wrong: it used
/// `192.0.2.1` as a stand-in for "a public address", on the assumption that
/// TEST-NET-1 is outside the deny-set. It is not — `is_forbidden_ip` includes
/// `Ipv4Addr::is_documentation()`, which covers 192.0.2.0/24, and the client
/// correctly refused it. Pin the real behaviour rather than the assumption.
///
/// (That a policy never blocks a genuinely public address is asserted in
/// `policy.rs`'s unit tests, which need no network.)
#[tokio::test]
async fn documentation_space_stays_denied_under_an_unrelated_allowlist() {
    let client = client_with(policy(&["10.20.0.0/16"]));
    for denied in ["192.0.2.1", "198.51.100.1", "203.0.113.1"] {
        let error = client
            .fetch(&Url::parse(&format!("http://{denied}/")).unwrap())
            .await
            .expect_err("documentation space is in the deny-set");
        assert!(
            error.to_string().contains(denied),
            "{denied} should be refused by policy, got: {error}"
        );
    }
}
