#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Idle,
    Loading,
    DomContentLoaded,
    Loaded,
    NetworkIdle,
    Failed,
}

impl LifecycleState {
    pub fn is_loading(&self) -> bool {
        matches!(self, LifecycleState::Loading)
    }

    pub fn is_loaded(&self) -> bool {
        matches!(self, LifecycleState::Loaded | LifecycleState::NetworkIdle)
    }

    pub fn is_network_idle(&self) -> bool {
        matches!(self, LifecycleState::NetworkIdle)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    Load,
    DomContentLoaded,
    NetworkIdle0,
    NetworkIdle2,
    /// Load plus a content-stability poll: scrape-oriented pages and challenge
    /// interstitials keep mutating after the load event (late scripts rewrite
    /// the DOM, bot walls swap in the real page after their checks). A client
    /// asking for `settle` gets the page once it has stopped changing, instead
    /// of a snapshot of the challenge shell.
    Settle,
}

impl WaitUntil {
    pub fn from_str(s: &str) -> Self {
        match s {
            "domcontentloaded" => WaitUntil::DomContentLoaded,
            "networkidle0" | "networkIdle" | "networkidle" => WaitUntil::NetworkIdle0,
            "networkidle2" => WaitUntil::NetworkIdle2,
            "settle" => WaitUntil::Settle,
            _ => WaitUntil::Load,
        }
    }
}
