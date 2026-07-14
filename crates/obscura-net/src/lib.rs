pub mod client;
pub mod cookies;
pub mod encoding;
pub mod interceptor;
pub mod robots;
pub mod blocklist;
#[cfg(feature = "stealth")]
pub mod wreq_client;

pub use client::{
    env_allows_private_network, is_forbidden_ip, CallbackRegistry, ObscuraHttpClient,
    ObscuraNetError, RequestCallback, RequestInfo, ResourceType, Response, ResponseCallback,
    SsrfGuardResolver,
};
pub use cookies::{default_cookie_path, CookieInfo, CookieJar};
pub use encoding::{
    decode_non_html, decode_response, decode_response_with_name, decode_with_label, label_name,
    url_encode_query,
};
pub use robots::RobotsCache;
pub use blocklist::is_blocked as is_tracker_blocked;
#[cfg(feature = "stealth")]
pub use wreq_client::{
    StealthHttpClient, STEALTH_NAVIGATOR_PLATFORM, STEALTH_UA_PLATFORM,
    STEALTH_UA_PLATFORM_VERSION, STEALTH_USER_AGENT,
};
