/// The OS family the stealth identity claims. The default is the host OS:
/// detectors see the kernel the connection exits from, so claiming a
/// different OS family is itself a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StealthPlatform {
    Windows,
    MacOS,
    Linux,
}

impl StealthPlatform {
    /// The platform of the OS this binary was built for.
    pub const fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOS
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// CLI/env name ("windows" | "macos" | "linux").
    pub const fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOS => "macos",
            Self::Linux => "linux",
        }
    }

    /// The User-Agent header of the identity. Pinned to Chrome 145 to match
    /// the wreq emulation profile (`Profile::Chrome145`); keep in lockstep
    /// when that profile bumps.
    pub const fn user_agent(self) -> &'static str {
        match self {
            Self::Windows => {
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/145.0.0.0 Safari/537.36"
            }
            Self::MacOS => {
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/145.0.0.0 Safari/537.36"
            }
            Self::Linux => {
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/145.0.0.0 Safari/537.36"
            }
        }
    }

    /// `navigator.platform` value of the identity.
    pub const fn navigator_platform(self) -> &'static str {
        match self {
            Self::Windows => "Win32",
            Self::MacOS => "MacIntel",
            Self::Linux => "Linux x86_64",
        }
    }

    /// `sec-ch-ua-platform` value of the identity (unquoted; the client adds
    /// the quotes on the wire).
    pub const fn ua_platform(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::MacOS => "macOS",
            Self::Linux => "Linux",
        }
    }

    /// `sec-ch-ua-platform-version` value of the identity. Real Chrome does
    /// not send the header on Linux, so that row is empty.
    pub const fn ua_platform_version(self) -> &'static str {
        match self {
            Self::Windows => "15.0.0",
            Self::MacOS => "14.5.0",
            Self::Linux => "",
        }
    }

    /// Map onto the wreq emulation builder. Stealth builds only.
    #[cfg(feature = "stealth")]
    pub fn wreq(self) -> wreq_util::Platform {
        match self {
            Self::Windows => wreq_util::Platform::Windows,
            Self::MacOS => wreq_util::Platform::MacOS,
            Self::Linux => wreq_util::Platform::Linux,
        }
    }
}

impl std::str::FromStr for StealthPlatform {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::MacOS),
            "linux" => Ok(Self::Linux),
            other => Err(format!(
                "unknown platform {other:?}; expected windows, macos, or linux"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_a_valid_platform() {
        let host = StealthPlatform::host();
        assert!(matches!(
            host,
            StealthPlatform::Windows | StealthPlatform::MacOS | StealthPlatform::Linux
        ));
    }

    #[test]
    fn from_str_round_trips_through_name() {
        for platform in [
            StealthPlatform::Windows,
            StealthPlatform::MacOS,
            StealthPlatform::Linux,
        ] {
            let parsed = platform.name().parse::<StealthPlatform>().unwrap();
            assert_eq!(parsed, platform);
        }
    }

    #[test]
    fn from_str_is_case_and_whitespace_insensitive() {
        assert_eq!("WINDOWS ".parse::<StealthPlatform>(), Ok(StealthPlatform::Windows));
        assert_eq!(" macOS".parse::<StealthPlatform>(), Ok(StealthPlatform::MacOS));
        assert_eq!("LINUX".parse::<StealthPlatform>(), Ok(StealthPlatform::Linux));
    }

    #[test]
    fn from_str_rejects_unknown_names() {
        let err = "freebsd".parse::<StealthPlatform>().unwrap_err();
        assert!(err.contains("freebsd"), "error should name the input: {err}");
    }
}
