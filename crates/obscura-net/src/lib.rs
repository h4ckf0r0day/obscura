pub mod client;
pub mod cookies;
pub mod interceptor;
pub mod robots;
pub mod blocklist;
#[cfg(feature = "stealth")]
pub mod wreq_client;

pub use client::{
    ObscuraHttpClient, ObscuraNetError, RequestInfo, ResourceType, Response,
    DEFAULT_SEC_CH_UA, DEFAULT_SEC_CH_UA_FULL_VERSION_LIST, DEFAULT_SEC_CH_UA_PLATFORM,
    DEFAULT_SEC_CH_UA_PLATFORM_VERSION, DEFAULT_USER_AGENT,
};
pub use cookies::{CookieInfo, CookieJar};
pub use robots::RobotsCache;
pub use blocklist::is_blocked as is_tracker_blocked;
#[cfg(feature = "stealth")]
pub use wreq_client::{StealthHttpClient, STEALTH_USER_AGENT};
