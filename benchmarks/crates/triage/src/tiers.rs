//! Conformance tier classification.
//!
//! Maps a WPT test path to a conformance tier using the published manifest in
//! `tiers.list` (compiled in via `include_str!`, so it travels with the binary
//! and stays auditable in the repo). The split is by capability, not outcome:
//! see `tiers.list` for the rules and rationale.
//!
//! Reporting aggregates these disjoint classes into the headline numbers:
//!   Core     = Core
//!   Relevant = Core + Relevant
//!   Full     = Core + Relevant + Excluded + Other

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Core,
    Relevant,
    Excluded,
    Other,
}

const MANIFEST: &str = include_str!("tiers.list");

struct Rule {
    tier: Tier,
    prefix: String,
}

/// Parsed tier rules, in manifest (first-match-wins) order.
pub struct Tiers {
    rules: Vec<Rule>,
}

impl Tiers {
    pub fn load() -> Self {
        let mut rules = Vec::new();
        for line in MANIFEST.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            let (tier, prefix) = match (it.next(), it.next()) {
                (Some(t), Some(p)) => (t, p),
                _ => continue,
            };
            let tier = match tier {
                "core" => Tier::Core,
                "relevant" => Tier::Relevant,
                "exclude" | "excluded" => Tier::Excluded,
                _ => continue,
            };
            rules.push(Rule {
                tier,
                prefix: prefix.to_string(),
            });
        }
        Tiers { rules }
    }

    /// First matching rule wins; a rule matches when the path equals its prefix
    /// or sits directly under it. Unmatched paths are `Other`.
    pub fn tier_for(&self, path: &str) -> Tier {
        let p = path.trim_start_matches('/');
        let bytes = p.as_bytes();
        for r in &self.rules {
            let pre = r.prefix.as_str();
            if p.starts_with(pre) && (p.len() == pre.len() || bytes[pre.len()] == b'/') {
                return r.tier;
            }
        }
        Tier::Other
    }
}

/// Per-tier running totals.
#[derive(Default, Clone, Copy)]
pub struct TierAgg {
    pub files: usize,
    pub files_ok: usize,
    pub pass: usize,
    pub total: usize,
}

impl TierAgg {
    pub fn add(&mut self, ok: bool, pass: usize, total: usize) {
        self.files += 1;
        if ok {
            self.files_ok += 1;
        }
        self.pass += pass;
        self.total += total;
    }
    /// Combine two aggregates (used to build Relevant = Core + Relevant, etc.).
    pub fn merged(&self, other: &TierAgg) -> TierAgg {
        TierAgg {
            files: self.files + other.files,
            files_ok: self.files_ok + other.files_ok,
            pass: self.pass + other.pass,
            total: self.total + other.total,
        }
    }
    pub fn file_pct(&self) -> f64 {
        if self.files == 0 {
            100.0
        } else {
            self.files_ok as f64 * 100.0 / self.files as f64
        }
    }
    pub fn subtest_pct(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            self.pass as f64 * 100.0 / self.total as f64
        }
    }
}

/// The four disjoint class totals collected during ingest.
#[derive(Default)]
pub struct TierTotals {
    pub core: TierAgg,
    pub relevant: TierAgg,
    pub excluded: TierAgg,
    pub other: TierAgg,
}

impl TierTotals {
    pub fn record(&mut self, tier: Tier, ok: bool, pass: usize, total: usize) {
        match tier {
            Tier::Core => self.core.add(ok, pass, total),
            Tier::Relevant => self.relevant.add(ok, pass, total),
            Tier::Excluded => self.excluded.add(ok, pass, total),
            Tier::Other => self.other.add(ok, pass, total),
        }
    }
    /// Headline aggregates.
    pub fn core(&self) -> TierAgg {
        self.core
    }
    pub fn relevant(&self) -> TierAgg {
        self.core.merged(&self.relevant)
    }
    pub fn full(&self) -> TierAgg {
        self.core
            .merged(&self.relevant)
            .merged(&self.excluded)
            .merged(&self.other)
    }
}
