use std::path::PathBuf;

use obscura_net::StealthPlatform;

/// Configuration for launching a Browser instance.
pub struct BrowserConfig {
    /// Proxy URL (e.g., "socks5://127.0.0.1:1080")
    pub proxy: Option<String>,
    /// Enable stealth mode (fingerprint spoofing)
    pub stealth: bool,
    /// Custom User-Agent string
    pub user_agent: Option<String>,
    /// Directory for persistent cookie storage
    pub storage_dir: Option<PathBuf>,
    /// The OS family the stealth identity claims. `None` means the host OS
    /// (`StealthPlatform::host()`); honored only when `stealth` is enabled.
    pub stealth_platform: Option<StealthPlatform>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            proxy: None,
            stealth: false,
            user_agent: None,
            storage_dir: None,
            stealth_platform: None,
        }
    }
}

impl BrowserConfig {
    pub fn builder() -> BrowserConfigBuilder {
        BrowserConfigBuilder::default()
    }
}

#[derive(Default)]
pub struct BrowserConfigBuilder {
    config: BrowserConfig,
}

impl BrowserConfigBuilder {
    pub fn proxy(mut self, proxy: impl Into<String>) -> Self {
        self.config.proxy = Some(proxy.into());
        self
    }

    pub fn stealth(mut self, stealth: bool) -> Self {
        self.config.stealth = stealth;
        self
    }

    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.config.user_agent = Some(ua.into());
        self
    }

    pub fn storage_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.storage_dir = Some(dir.into());
        self
    }

    /// The OS family the stealth identity claims (`StealthPlatform::Windows`,
    /// `MacOS`, or `Linux`). `None` keeps the host OS default.
    pub fn stealth_platform(mut self, platform: StealthPlatform) -> Self {
        self.config.stealth_platform = Some(platform);
        self
    }

    pub fn build(self) -> BrowserConfig {
        self.config
    }
}
