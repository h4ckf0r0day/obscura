use std::net::{IpAddr, Ipv6Addr};

const PRIVATE_NETWORK_HINT: &str = "Local/private network access is blocked by the SSRF guard. For your own local services, start Obscura with --allow-private-network or set OBSCURA_ALLOW_PRIVATE_NETWORK=1.";

/// Hint for an operator-facing error, not a policy change. CLI and stdio MCP
/// opt into presenting it; remote transports should keep the original error.
/// Inspect the actual denied host, not the initial navigation URL, since a
/// redirect can lead elsewhere. Metadata and other special-purpose ranges
/// deliberately receive no opt-out suggestion.
pub fn private_network_error_hint(mut error: &str) -> Option<&'static str> {
    while let Some(inner) = error.strip_prefix("Network error: ") {
        error = inner;
    }
    if let Some(domain) = error
        .strip_prefix("Access to localhost domain '")
        .and_then(|value| value.strip_suffix("' is not allowed"))
    {
        let domain = domain.to_ascii_lowercase();
        return (domain == "localhost" || domain.ends_with(".localhost"))
            .then_some(PRIVATE_NETWORK_HINT);
    }
    let address = error
        .strip_prefix("Access to private/internal IP address ")
        .or_else(|| error.strip_prefix("Access to private/internal IPv6 address "))?
        .strip_suffix(" is not allowed")?
        .parse::<IpAddr>()
        .ok()?;
    let local = match address {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => {
            // AWS's IPv6 instance metadata endpoint is in the ULA range.
            let metadata = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
            ip != metadata
                && (ip.is_loopback()
                    || ip.is_unique_local()
                    || ip
                        .to_ipv4_mapped()
                        .or_else(|| ip.to_ipv4())
                        .is_some_and(|v4| v4.is_loopback() || v4.is_private()))
        }
    };
    local.then_some(PRIVATE_NETWORK_HINT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_denials_offer_the_existing_operator_opt_in() {
        for message in [
            "Access to localhost domain 'localhost' is not allowed",
            "Network error: Network error: Access to localhost domain 'app.localhost' is not allowed",
            "Access to private/internal IP address 127.0.0.1 is not allowed",
            "Access to private/internal IP address 192.168.1.1 is not allowed",
            "Access to private/internal IP address 10.0.0.1 is not allowed",
            "Access to private/internal IPv6 address ::1 is not allowed",
            "Access to private/internal IPv6 address fd12::1 is not allowed",
            "Access to private/internal IPv6 address ::ffff:127.0.0.1 is not allowed",
        ] {
            let hint = private_network_error_hint(message).expect(message);
            assert!(hint.contains("--allow-private-network"));
            assert!(hint.contains("OBSCURA_ALLOW_PRIVATE_NETWORK=1"));
        }
    }

    #[test]
    fn metadata_special_ranges_and_unrelated_errors_do_not_get_a_hint() {
        for message in [
            "Access to private/internal IP address 169.254.169.254 is not allowed",
            "Access to private/internal IP address 100.100.100.200 is not allowed",
            "Access to private/internal IPv6 address fd00:ec2::254 is not allowed",
            "Access to private/internal IPv6 address fe80::1 is not allowed",
            "Access to private/internal IPv6 address ::ffff:169.254.169.254 is not allowed",
            "Access to private/internal IP address 198.18.0.1 is not allowed",
            "Access to private/internal IP address 0.0.0.0 is not allowed",
            "Access to localhost domain 'example.com' is not allowed",
            "Network error: certificate verify failed",
            "Access to private/internal IP address invalid is not allowed",
        ] {
            assert_eq!(private_network_error_hint(message), None, "{message}");
        }
    }
}
