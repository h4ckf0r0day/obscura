pub mod blocklist;
pub mod client;
pub mod cookies;
pub mod interceptor;
pub mod robots;
#[cfg(feature = "stealth")]
pub mod wreq_client;

pub use blocklist::is_blocked as is_tracker_blocked;
pub use client::{
    ObscuraHttpClient, ObscuraNetError, RequestInfo, ResourceType, Response, DEFAULT_SEC_CH_UA,
    DEFAULT_SEC_CH_UA_FULL_VERSION_LIST, DEFAULT_SEC_CH_UA_PLATFORM,
    DEFAULT_SEC_CH_UA_PLATFORM_VERSION, DEFAULT_USER_AGENT,
};
pub use cookies::{is_schemeful_same_site, CookieInfo, CookieJar};
pub use robots::RobotsCache;
#[cfg(feature = "stealth")]
pub use wreq_client::{StealthHttpClient, STEALTH_USER_AGENT};
