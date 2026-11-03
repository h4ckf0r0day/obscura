//! Runtime isolation policies, resource quotas, and security boundaries.

use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;
use url::Url;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum IsolationError {
    #[error("SSRF policy blocked request to private/restricted address: {0}")]
    SsrfBlocked(String),

    #[error("Access to local file scheme ('file://') is prohibited by policy")]
    FileAccessBlocked,

    #[error("Execution budget exceeded: operation took longer than {0:?}")]
    ExecutionTimeout(Duration),

    #[error("Memory quota exceeded: {actual_mb} MB exceeds limit of {limit_mb} MB")]
    MemoryQuotaExceeded { actual_mb: usize, limit_mb: usize },

    #[error("DOM complexity exceeded: {actual} nodes exceed limit of {limit}")]
    DomComplexityExceeded { actual: usize, limit: usize },

    #[error("Internal panic trapped inside isolation boundary: {0}")]
    TrappedPanic(String),
}

/// Pre-configured security profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IsolationProfile {
    /// General web browsing with standard protections.
    Standard,
    /// Hardened multi-tenant execution with tighter bounds.
    Hardened,
    /// Untrusted sandbox with strict isolation and zero persistence.
    Untrusted,
}

/// Configurable isolation policy governing a browser session or worker.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct IsolationPolicy {
    pub profile: IsolationProfile,
    pub allow_file_access: bool,
    pub allow_private_network: bool,
    pub max_memory_mb: Option<usize>,
    pub max_execution_time: Duration,
    pub max_dom_nodes: usize,
    pub max_active_requests: usize,
    pub enforce_panic_safety: bool,
}

impl Default for IsolationPolicy {
    fn default() -> Self {
        Self::for_profile(IsolationProfile::Standard)
    }
}

impl IsolationPolicy {
    pub fn for_profile(profile: IsolationProfile) -> Self {
        match profile {
            IsolationProfile::Standard => Self {
                profile,
                allow_file_access: false,
                allow_private_network: false,
                max_memory_mb: Some(512),
                max_execution_time: Duration::from_secs(30),
                max_dom_nodes: 50_000,
                max_active_requests: 128,
                enforce_panic_safety: true,
            },
            IsolationProfile::Hardened => Self {
                profile,
                allow_file_access: false,
                allow_private_network: false,
                max_memory_mb: Some(256),
                max_execution_time: Duration::from_secs(15),
                max_dom_nodes: 25_000,
                max_active_requests: 64,
                enforce_panic_safety: true,
            },
            IsolationProfile::Untrusted => Self {
                profile,
                allow_file_access: false,
                allow_private_network: false,
                max_memory_mb: Some(128),
                max_execution_time: Duration::from_secs(5),
                max_dom_nodes: 10_000,
                max_active_requests: 32,
                enforce_panic_safety: true,
            },
        }
    }

    /// Validates an outbound URL against this policy.
    pub fn validate_url(&self, raw_url: &str) -> Result<Url, IsolationError> {
        let url = Url::parse(raw_url).map_err(|_| {
            IsolationError::SsrfBlocked(format!("Unparseable URL: {raw_url}"))
        })?;

        let scheme = url.scheme().to_ascii_lowercase();

        // 1. File access check
        if scheme == "file" && !self.allow_file_access {
            return Err(IsolationError::FileAccessBlocked);
        }

        // 2. Private network SSRF check
        if (scheme == "http" || scheme == "https") && !self.allow_private_network {
            if let Some(host) = url.host_str() {
                let host_lower = host.to_ascii_lowercase();
                if host_lower == "localhost" || host_lower.ends_with(".localhost") {
                    return Err(IsolationError::SsrfBlocked(host.to_string()));
                }

                if let Ok(ip) = host.parse::<IpAddr>() {
                    if obscura_ssrf::is_forbidden_ip(ip) {
                        return Err(IsolationError::SsrfBlocked(host.to_string()));
                    }
                }
            }
        }

        Ok(url)
    }

    /// Verifies current DOM node count against quota.
    pub fn check_dom_nodes(&self, current_nodes: usize) -> Result<(), IsolationError> {
        if current_nodes > self.max_dom_nodes {
            Err(IsolationError::DomComplexityExceeded {
                actual: current_nodes,
                limit: self.max_dom_nodes,
            })
        } else {
            Ok(())
        }
    }

    /// Verifies current memory consumption against quota.
    pub fn check_memory_mb(&self, current_mb: usize) -> Result<(), IsolationError> {
        if let Some(limit) = self.max_memory_mb {
            if current_mb > limit {
                return Err(IsolationError::MemoryQuotaExceeded {
                    actual_mb: current_mb,
                    limit_mb: limit,
                });
            }
        }
        Ok(())
    }

    /// Wraps a closure in panic-safe isolation boundary if configured.
    pub fn run_isolated<F, R>(&self, f: F) -> Result<R, IsolationError>
    where
        F: FnOnce() -> R + std::panic::UnwindSafe,
    {
        if !self.enforce_panic_safety {
            return Ok(f());
        }

        std::panic::catch_unwind(f).map_err(|payload| {
            let msg = payload
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| payload.downcast_ref::<&'static str>().copied())
                .unwrap_or("unknown panic payload");
            IsolationError::TrappedPanic(msg.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_access_restriction() {
        let policy = IsolationPolicy::default();
        let err = policy.validate_url("file:///etc/passwd").unwrap_err();
        assert_eq!(err, IsolationError::FileAccessBlocked);

        let mut allowed = IsolationPolicy::default();
        allowed.allow_file_access = true;
        assert!(allowed.validate_url("file:///tmp/test.html").is_ok());
    }

    #[test]
    fn test_ssrf_blocking_metadata_and_loopback() {
        let policy = IsolationPolicy::default();

        // Loopback
        assert!(matches!(
            policy.validate_url("http://127.0.0.1:8080/test").unwrap_err(),
            IsolationError::SsrfBlocked(_)
        ));
        assert!(matches!(
            policy.validate_url("http://localhost:3000/").unwrap_err(),
            IsolationError::SsrfBlocked(_)
        ));

        // AWS / Cloud Metadata IP
        assert!(matches!(
            policy.validate_url("http://169.254.169.254/latest/meta-data").unwrap_err(),
            IsolationError::SsrfBlocked(_)
        ));

        // Private RFC1918 networks
        assert!(matches!(
            policy.validate_url("http://10.0.0.1/admin").unwrap_err(),
            IsolationError::SsrfBlocked(_)
        ));
        assert!(matches!(
            policy.validate_url("http://192.168.1.1/router").unwrap_err(),
            IsolationError::SsrfBlocked(_)
        ));

        // Public URLs are permitted
        assert!(policy.validate_url("https://example.com/page").is_ok());
        assert!(policy.validate_url("https://github.com").is_ok());
    }

    #[test]
    fn test_dom_node_and_memory_quotas() {
        let policy = IsolationPolicy::for_profile(IsolationProfile::Untrusted);

        assert!(policy.check_dom_nodes(5_000).is_ok());
        assert!(policy.check_dom_nodes(15_000).is_err());

        assert!(policy.check_memory_mb(100).is_ok());
        assert!(policy.check_memory_mb(150).is_err());
    }

    #[test]
    fn test_panic_boundary_containment() {
        let policy = IsolationPolicy::default();

        let result = policy.run_isolated(|| {
            panic!("Exploding operation!");
        });

        assert!(result.is_err());
        if let Err(IsolationError::TrappedPanic(msg)) = result {
            assert!(msg.contains("Exploding operation!"));
        } else {
            panic!("Expected TrappedPanic error");
        }
    }
}
