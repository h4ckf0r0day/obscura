pub mod client;
pub mod cookies;
pub mod interceptor;
pub mod robots;
pub mod blocklist;
#[cfg(feature = "stealth")]
pub mod stealth_client;

pub use client::{ObscuraHttpClient, ObscuraNetError, RequestInfo, ResourceType, Response};
pub use cookies::{CookieInfo, CookieJar};
pub use robots::RobotsCache;
pub use blocklist::is_blocked as is_tracker_blocked;
#[cfg(feature = "stealth")]
pub use stealth_client::{StealthHttpClient, STEALTH_USER_AGENT, Ja3Fingerprint, Ja4Fingerprint};

/// Helper to create a reqwest::ClientBuilder with the correct TLS provider (BoringSSL if enabled)
pub fn create_client_builder() -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder();

    #[cfg(feature = "stealth")]
    {
        use std::sync::Arc;
        use bssl_rustls_adapters::CryptoProviderBuilder;
        use webpki_roots;

        let provider = CryptoProviderBuilder::full();
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let rustls_config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .expect("Failed to build rustls config")
            .with_root_certificates(root_store)
            .with_no_client_auth();

        builder = builder.use_preconfigured_tls(rustls_config);
    }

    builder
}
