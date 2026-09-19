//! Which otherwise-forbidden network destinations this process may reach.
//!
//! `is_forbidden_ip` defines the deny-set: loopback, RFC1918, link-local
//! (including the cloud-metadata addresses), CGNAT, and the rest. That
//! definition is unchanged and stays the single source of truth.
//!
//! What changes is the escape hatch. `--allow-private-network` was the only
//! one, and it disables the deny-set **entirely** — including
//! `169.254.169.254` and `100.100.100.200`. It is also the flag an operator
//! must set to test an internal application, which is the common case: testing
//! an app on `10.20.0.0/16` should not also hand every page the cloud
//! metadata service.
//!
//! `NetworkPolicy` adds a scoped alternative. `--allow-network 10.20.0.0/16`
//! exempts exactly that prefix and leaves everything else denied.
//!
//! CIDR parsing is hand-rolled rather than pulling in `ipnet`: it is ~40 lines,
//! and a new dependency needs a `deny.toml` policy review that would dwarf it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::client::is_forbidden_ip;

/// A single CIDR prefix, or a bare address (an implicit /32 or /128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    addr: IpAddr,
    bits: u8,
}

impl Prefix {
    /// Parse `10.0.0.0/8`, `192.168.1.5`, `fd00::/8`, or `::1`.
    pub fn parse(entry: &str) -> Result<Self, String> {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err("empty network entry".to_string());
        }
        let (addr_part, bits_part) = match entry.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (entry, None),
        };
        let addr: IpAddr = addr_part
            .parse()
            .map_err(|_| format!("'{addr_part}' is not an IP address"))?;
        let max_bits = if addr.is_ipv4() { 32u8 } else { 128u8 };
        let bits = match bits_part {
            None => max_bits,
            Some(b) => {
                let parsed: u8 = b
                    .trim()
                    .parse()
                    .map_err(|_| format!("'{b}' is not a prefix length"))?;
                if parsed > max_bits {
                    return Err(format!(
                        "prefix /{parsed} is longer than /{max_bits} for {addr_part}"
                    ));
                }
                parsed
            }
        };
        Ok(Prefix { addr, bits })
    }

    /// Does this prefix cover `ip`?
    ///
    /// An IPv4-mapped IPv6 address is compared as its embedded v4, matching
    /// `is_forbidden_ip`: otherwise `::ffff:10.20.5.5` would slip past a
    /// `10.20.0.0/16` entry and be denied when the operator meant to allow it.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                Some(v4) if self.addr.is_ipv4() => IpAddr::V4(v4),
                _ => ip,
            },
            other => other,
        };
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(candidate)) => {
                masked_v4(net, self.bits) == masked_v4(candidate, self.bits)
            }
            (IpAddr::V6(net), IpAddr::V6(candidate)) => {
                masked_v6(net, self.bits) == masked_v6(candidate, self.bits)
            }
            _ => false,
        }
    }

    /// True for `/0`, which exempts the entire address family.
    pub fn is_everything(&self) -> bool {
        self.bits == 0
    }
}

fn masked_v4(addr: Ipv4Addr, bits: u8) -> u32 {
    let raw = u32::from(addr);
    if bits == 0 {
        0
    } else {
        raw & (!0u32 << (32 - bits))
    }
}

fn masked_v6(addr: Ipv6Addr, bits: u8) -> u128 {
    let raw = u128::from(addr);
    if bits == 0 {
        0
    } else {
        raw & (!0u128 << (128 - bits))
    }
}

/// The set of destinations a client may reach.
#[derive(Debug, Clone, Default)]
pub struct NetworkPolicy {
    /// `--allow-private-network` / `OBSCURA_ALLOW_PRIVATE_NETWORK`. Disables
    /// the whole deny-set. Unchanged behaviour, kept for compatibility.
    allow_all_private: bool,
    /// `--allow-network <CIDR>` / `OBSCURA_ALLOW_NETWORK`. Exempts only these.
    allowed: Vec<Prefix>,
}

impl NetworkPolicy {
    /// Deny everything in the deny-set. The default.
    pub fn deny_private() -> Self {
        Self::default()
    }

    /// The blanket opt-out, preserving `--allow-private-network`.
    pub fn allow_all_private() -> Self {
        Self {
            allow_all_private: true,
            allowed: Vec::new(),
        }
    }

    /// Build from the two opt-ins. `entries` are CIDRs or bare addresses.
    pub fn new(allow_all_private: bool, entries: &[String]) -> Result<Self, String> {
        let mut allowed = Vec::with_capacity(entries.len());
        for entry in entries {
            for part in entry.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                allowed.push(Prefix::parse(part)?);
            }
        }
        Ok(Self {
            allow_all_private,
            allowed,
        })
    }

    /// Read both opt-ins from the environment, the way the CLI sets them.
    ///
    /// A malformed `OBSCURA_ALLOW_NETWORK` is a hard error rather than a silent
    /// skip: an operator who typos a prefix should be told, not quietly left
    /// with a policy that denies the thing they meant to allow.
    pub fn from_env(allow_all_private: bool) -> Result<Self, String> {
        let entries: Vec<String> = std::env::var("OBSCURA_ALLOW_NETWORK")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .into_iter()
            .collect();
        let allow_all_private =
            allow_all_private || crate::client::env_allows_private_network();
        Self::new(allow_all_private, &entries)
    }

    /// Whether the blanket private-network opt-out is in force.
    pub fn allows_all_private(&self) -> bool {
        self.allow_all_private
    }

    /// Any `/0` entry disables the guard for its address family. Callers warn
    /// about it rather than refuse, because an operator may genuinely mean it.
    pub fn has_catch_all(&self) -> bool {
        self.allowed.iter().any(Prefix::is_everything)
    }

    /// True when the operator listed at least one prefix.
    ///
    /// `validate_url` uses this for the `localhost` *domain* check: a hostname
    /// carries no IP, so an allowlist cannot be evaluated against it there.
    /// When one exists the decision is deferred to `SsrfGuardResolver`, which
    /// sees the addresses the name actually resolves to and is the
    /// authoritative check anyway.
    pub fn has_allowlist(&self) -> bool {
        !self.allowed.is_empty()
    }

    /// May `ip` be dialled?
    ///
    /// Public addresses short-circuit: the deny-set is the only thing this
    /// policy relaxes, so a policy can never *forbid* something
    /// `is_forbidden_ip` permits. That keeps `--allow-network` an escape hatch
    /// rather than a firewall an operator might mistake it for.
    pub fn permits(&self, ip: IpAddr) -> bool {
        if !is_forbidden_ip(ip) {
            return true;
        }
        if self.allow_all_private {
            return true;
        }
        self.allowed.iter().any(|prefix| prefix.contains(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn policy(entries: &[&str]) -> NetworkPolicy {
        NetworkPolicy::new(false, &entries.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .expect("entries must parse")
    }

    #[test]
    fn an_allowed_prefix_is_reachable_and_its_neighbours_are_not() {
        let p = policy(&["10.20.0.0/16"]);
        assert!(p.permits(ip("10.20.5.5")));
        assert!(p.permits(ip("10.20.255.254")));
        assert!(!p.permits(ip("10.21.0.1")), "outside the prefix");
        assert!(!p.permits(ip("127.0.0.1")), "loopback is not in the prefix");
    }

    /// The whole point of the allowlist: opening an internal range must not open the
    /// cloud metadata endpoint.
    #[test]
    fn allowing_an_internal_range_does_not_open_cloud_metadata() {
        let p = policy(&["10.20.0.0/16"]);
        assert!(!p.permits(ip("169.254.169.254")), "AWS/GCP/Azure metadata");
        assert!(!p.permits(ip("100.100.100.200")), "Alibaba metadata");
        assert!(!p.permits(ip("169.254.1.1")), "link-local generally");
    }

    /// The blanket flag keeps its old, deliberately broad meaning.
    #[test]
    fn allow_all_private_still_opens_everything() {
        let p = NetworkPolicy::allow_all_private();
        for addr in ["127.0.0.1", "10.0.0.1", "192.168.1.1", "169.254.169.254", "::1"] {
            assert!(p.permits(ip(addr)), "{addr} must be reachable");
        }
    }

    #[test]
    fn the_default_policy_denies_the_whole_deny_set() {
        let p = NetworkPolicy::deny_private();
        for addr in ["127.0.0.1", "10.0.0.1", "169.254.169.254", "::1", "fd00::1"] {
            assert!(!p.permits(ip(addr)), "{addr} must be denied by default");
        }
    }

    /// A policy relaxes the deny-set; it never tightens it. Public addresses
    /// are unaffected, so `--allow-network` cannot be mistaken for a firewall.
    #[test]
    fn public_addresses_are_unaffected_by_any_policy() {
        for p in [
            NetworkPolicy::deny_private(),
            NetworkPolicy::allow_all_private(),
            policy(&["10.0.0.0/8"]),
        ] {
            assert!(p.permits(ip("1.1.1.1")));
            assert!(p.permits(ip("2606:4700:4700::1111")));
        }
    }

    #[test]
    fn a_bare_address_is_an_implicit_host_route() {
        let p = policy(&["192.168.1.5"]);
        assert!(p.permits(ip("192.168.1.5")));
        assert!(!p.permits(ip("192.168.1.6")));
    }

    #[test]
    fn ipv6_prefixes_work() {
        let p = policy(&["fd00::/8"]);
        assert!(p.permits(ip("fd00::1")));
        assert!(p.permits(ip("fdff::9999")));
        assert!(!p.permits(ip("fc00::1")), "outside fd00::/8");
        assert!(!p.permits(ip("::1")), "loopback is not in the prefix");
    }

    /// `::ffff:10.20.5.5` must match a v4 prefix, or the guard's own
    /// IPv4-mapped unwrapping would deny what the operator allowed.
    #[test]
    fn an_ipv4_mapped_address_matches_a_v4_prefix() {
        let p = policy(&["10.20.0.0/16"]);
        assert!(p.permits(ip("::ffff:10.20.5.5")));
        assert!(!p.permits(ip("::ffff:10.21.5.5")));
    }

    #[test]
    fn several_entries_and_comma_separated_lists_both_work() {
        let p = policy(&["10.0.0.0/8", "192.168.0.0/16,172.16.0.0/12"]);
        assert!(p.permits(ip("10.1.2.3")));
        assert!(p.permits(ip("192.168.9.9")));
        assert!(p.permits(ip("172.16.0.1")));
        assert!(!p.permits(ip("127.0.0.1")));
    }

    #[test]
    fn malformed_entries_are_rejected_with_a_reason() {
        for bad in ["10.0.0.0/33", "nonsense", "10.0.0.0/", "999.1.1.1", "fd00::/129"] {
            let err = NetworkPolicy::new(false, &[bad.to_string()])
                .expect_err(&format!("{bad} must not parse"));
            assert!(!err.is_empty(), "the error should say what was wrong");
        }
    }

    #[test]
    fn a_catch_all_prefix_is_detectable_so_callers_can_warn() {
        assert!(policy(&["0.0.0.0/0"]).has_catch_all());
        assert!(policy(&["::/0"]).has_catch_all());
        assert!(!policy(&["10.0.0.0/8"]).has_catch_all());
        // It does what it says, which is why it is worth warning about.
        assert!(policy(&["0.0.0.0/0"]).permits(ip("127.0.0.1")));
    }

    #[test]
    fn prefix_boundaries_are_exact() {
        let p = policy(&["10.20.0.0/16"]);
        assert!(p.permits(ip("10.20.0.0")));
        assert!(p.permits(ip("10.20.255.255")));
        assert!(!p.permits(ip("10.19.255.255")));
        assert!(!p.permits(ip("10.21.0.0")));
    }
}
