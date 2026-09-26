//! Token-captcha auto-solver.
//!
//! Detects a **token-based** captcha on the loaded page (reCAPTCHA v2 / hCaptcha
//! / Cloudflare Turnstile), solves it through a pluggable third-party service
//! (CapSolver or 2Captcha), and injects the returned token into the page's
//! response field — then submits the surrounding form so the page proceeds.
//!
//! This handles ONLY token captchas, where solving means *obtaining a token and
//! injecting it* — which fits obscura's DOM model (no rendering / real pointer
//! input needed). It deliberately does NOT attempt **behavioral** anti-bot
//! systems (PerimeterX/DataDome/Akamai sliders): those validate real pointer
//! telemetry + browser fingerprint + IP and can't be solved by token injection
//! (use a real-browser cookie-broker on a residential IP for those instead).
//!
//! Off unless `OBSCURA_CAPTCHA_API_KEY` is set. Provider via
//! `OBSCURA_CAPTCHA_PROVIDER` (`capsolver` default | `2captcha`).

use std::time::Duration;

use serde_json::{json, Value};

use crate::page::Page;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptchaKind {
    RecaptchaV2,
    HCaptcha,
    Turnstile,
}

impl CaptchaKind {
    fn from_tag(s: &str) -> Option<Self> {
        match s {
            "recaptcha_v2" => Some(Self::RecaptchaV2),
            "hcaptcha" => Some(Self::HCaptcha),
            "turnstile" => Some(Self::Turnstile),
            _ => None,
        }
    }

    /// The DOM field name the solved token must land in for the page's own
    /// verification flow to pick it up.
    fn response_field(&self) -> &'static str {
        match self {
            Self::RecaptchaV2 => "g-recaptcha-response",
            Self::HCaptcha => "h-captcha-response",
            Self::Turnstile => "cf-turnstile-response",
        }
    }

    fn capsolver_task_type(&self) -> &'static str {
        match self {
            Self::RecaptchaV2 => "ReCaptchaV2TaskProxyLess",
            Self::HCaptcha => "HCaptchaTaskProxyLess",
            Self::Turnstile => "AntiTurnstileTaskProxyLess",
        }
    }

    /// 2Captcha `method` (in.php) for this captcha type.
    fn twocaptcha_method(&self) -> &'static str {
        match self {
            Self::RecaptchaV2 => "userrecaptcha",
            Self::HCaptcha => "hcaptcha",
            Self::Turnstile => "turnstile",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Challenge {
    pub kind: CaptchaKind,
    pub sitekey: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    CapSolver,
    TwoCaptcha,
}

pub struct CaptchaConfig {
    pub provider: Provider,
    pub api_key: String,
}

impl CaptchaConfig {
    /// Reads `OBSCURA_CAPTCHA_API_KEY` (required — absent ⇒ feature off) and
    /// `OBSCURA_CAPTCHA_PROVIDER` (`capsolver` default | `2captcha`).
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("OBSCURA_CAPTCHA_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())?;
        let provider = match std::env::var("OBSCURA_CAPTCHA_PROVIDER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "2captcha" | "twocaptcha" => Provider::TwoCaptcha,
            _ => Provider::CapSolver,
        };
        Some(Self {
            provider,
            api_key: api_key.trim().to_string(),
        })
    }
}

/// JS that scans the DOM for a token captcha and returns `{kind, sitekey}` as a
/// JSON string, or `null`. Prefers the explicit `data-sitekey` widgets; falls
/// back to the provider iframes (sitekey in the `k`/`sitekey` query param).
const DETECT_JS: &str = r#"(function(){
  function q(sel){return document.querySelector(sel);}
  function attr(sel,a){var e=q(sel);return e?e.getAttribute(a):null;}
  function frameKey(srcPart,param){
    var f=q('iframe[src*="'+srcPart+'"]'); if(!f)return null;
    try{var u=new URL(f.getAttribute('src'),location.href);return u.searchParams.get(param);}catch(e){return null;}
  }
  // hCaptcha
  var hk=attr('.h-captcha[data-sitekey]','data-sitekey')||frameKey('hcaptcha.com','sitekey');
  if(hk) return JSON.stringify({kind:'hcaptcha',sitekey:hk});
  // Cloudflare Turnstile
  var tk=attr('.cf-turnstile[data-sitekey]','data-sitekey')||frameKey('challenges.cloudflare.com','sitekey');
  if(tk) return JSON.stringify({kind:'turnstile',sitekey:tk});
  // reCAPTCHA v2
  var rk=attr('.g-recaptcha[data-sitekey]','data-sitekey')||frameKey('recaptcha/api2','k')||frameKey('recaptcha/enterprise','k');
  if(rk) return JSON.stringify({kind:'recaptcha_v2',sitekey:rk});
  return null;
})()"#;

/// Detect a token captcha on the current page. `None` if there's no recognized
/// token captcha (the common case — callers then proceed normally).
pub fn detect(page: &mut Page) -> Option<Challenge> {
    let raw = match page.evaluate(DETECT_JS) {
        Value::String(s) => s,
        _ => return None,
    };
    let v: Value = serde_json::from_str(&raw).ok()?;
    let kind = CaptchaKind::from_tag(v.get("kind")?.as_str()?)?;
    let sitekey = v.get("sitekey")?.as_str()?.trim().to_string();
    if sitekey.is_empty() {
        return None;
    }
    Some(Challenge { kind, sitekey })
}

/// Inject the solved token into the page and try to advance: set every matching
/// response field (creating the hidden textarea if absent), fire `input`, invoke
/// a `data-callback` if declared, and submit the surrounding form.
pub fn inject(page: &mut Page, kind: CaptchaKind, token: &str) {
    let field = kind.response_field();
    // JSON-encode so the token (opaque, may contain quotes) is safely embedded.
    let token_lit = Value::String(token.to_string()).to_string();
    let field_lit = Value::String(field.to_string()).to_string();
    let js = format!(
        r#"(function(){{
  var token={token};
  var field={field};
  var els=document.querySelectorAll('textarea[name="'+field+'"], input[name="'+field+'"], #'+field);
  if(!els.length){{
    var ta=document.createElement('textarea');
    ta.name=field; ta.id=field; ta.style.display='none';
    document.body.appendChild(ta); els=[ta];
  }}
  els.forEach(function(e){{ e.value=token; try{{e.dispatchEvent(new Event('input',{{bubbles:true}}));}}catch(_){{}} }});
  // Optional explicit callback declared on the widget.
  try{{
    var w=document.querySelector('[data-callback]');
    var cbName=w&&w.getAttribute('data-callback');
    if(cbName&&typeof window[cbName]==='function'){{ window[cbName](token); }}
  }}catch(_){{}}
  // Advance: submit the enclosing form (or the first form on the page).
  try{{
    var f=(els[0]&&els[0].form)||document.querySelector('form');
    if(f&&typeof f.submit==='function'){{ f.submit(); }}
  }}catch(_){{}}
  return true;
}})()"#,
        token = token_lit,
        field = field_lit,
    );
    let _ = page.evaluate(&js);
}

/// Full flow: detect → solve (service) → inject. Returns `Ok(true)` when a
/// challenge was found and a token injected, `Ok(false)` when there was no token
/// captcha to solve, `Err` when solving failed (network/service/timeout).
pub async fn try_solve(page: &mut Page, page_url: &str, cfg: &CaptchaConfig) -> anyhow::Result<bool> {
    let Some(challenge) = detect(page) else {
        return Ok(false);
    };
    tracing::info!(kind = ?challenge.kind, "token captcha detected — solving via service");
    let token = match cfg.provider {
        Provider::CapSolver => solve_capsolver(cfg, &challenge, page_url).await?,
        Provider::TwoCaptcha => solve_twocaptcha(cfg, &challenge, page_url).await?,
    };
    inject(page, challenge.kind, &token);
    Ok(true)
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// CapSolver: createTask → poll getTaskResult. Returns the token
/// (`gRecaptchaResponse` for recaptcha/hcaptcha, `token` for Turnstile).
async fn solve_capsolver(
    cfg: &CaptchaConfig,
    challenge: &Challenge,
    page_url: &str,
) -> anyhow::Result<String> {
    let client = http_client();
    let create = client
        .post("https://api.capsolver.com/createTask")
        .json(&json!({
            "clientKey": cfg.api_key,
            "task": {
                "type": challenge.kind.capsolver_task_type(),
                "websiteURL": page_url,
                "websiteKey": challenge.sitekey,
            }
        }))
        .send()
        .await?
        .json::<Value>()
        .await?;
    if create.get("errorId").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
        anyhow::bail!(
            "capsolver createTask: {}",
            create.get("errorDescription").and_then(|v| v.as_str()).unwrap_or("unknown")
        );
    }
    let task_id = create
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("capsolver: no taskId"))?
        .to_string();

    // Poll up to ~120s.
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let res = client
            .post("https://api.capsolver.com/getTaskResult")
            .json(&json!({ "clientKey": cfg.api_key, "taskId": task_id }))
            .send()
            .await?
            .json::<Value>()
            .await?;
        if res.get("errorId").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
            anyhow::bail!(
                "capsolver getTaskResult: {}",
                res.get("errorDescription").and_then(|v| v.as_str()).unwrap_or("unknown")
            );
        }
        match res.get("status").and_then(|v| v.as_str()) {
            Some("ready") => {
                let sol = res.get("solution").cloned().unwrap_or_default();
                let token = sol
                    .get("gRecaptchaResponse")
                    .or_else(|| sol.get("token"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("capsolver: no token in solution"))?;
                return Ok(token.to_string());
            }
            _ => continue, // "processing"
        }
    }
    anyhow::bail!("capsolver: timed out waiting for solution")
}

/// 2Captcha: in.php (submit) → res.php (poll). Classic text protocol
/// (`OK|<id>` then `OK|<token>`; `CAPCHA_NOT_READY` while pending).
async fn solve_twocaptcha(
    cfg: &CaptchaConfig,
    challenge: &Challenge,
    page_url: &str,
) -> anyhow::Result<String> {
    let client = http_client();
    let submit = client
        .get("https://2captcha.com/in.php")
        .query(&[
            ("key", cfg.api_key.as_str()),
            ("method", challenge.kind.twocaptcha_method()),
            ("sitekey", challenge.sitekey.as_str()),
            ("pageurl", page_url),
            ("json", "0"),
        ])
        .send()
        .await?
        .text()
        .await?;
    let id = submit
        .strip_prefix("OK|")
        .ok_or_else(|| anyhow::anyhow!("2captcha in.php: {submit}"))?
        .trim()
        .to_string();

    for _ in 0..40 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let res = client
            .get("https://2captcha.com/res.php")
            .query(&[
                ("key", cfg.api_key.as_str()),
                ("action", "get"),
                ("id", id.as_str()),
                ("json", "0"),
            ])
            .send()
            .await?
            .text()
            .await?;
        if res.trim() == "CAPCHA_NOT_READY" {
            continue;
        }
        if let Some(token) = res.strip_prefix("OK|") {
            return Ok(token.trim().to_string());
        }
        anyhow::bail!("2captcha res.php: {res}");
    }
    anyhow::bail!("2captcha: timed out waiting for solution")
}

// ============================================================
// Type-B: anti-bot CHALLENGE systems (cookie-returning)
// ============================================================
// Unlike token-widget captchas, these (AWS WAF, Cloudflare managed challenge,
// DataDome) are full-page interstitials. The solver returns a CLEARANCE COOKIE
// (and sometimes a User-Agent) rather than a form token; we set the cookie and
// RE-FETCH the page. The clearance cookie is IP-bound, so CapSolver must solve
// through the SAME egress proxy obscura uses (passed via the task `proxy` field)
// — otherwise the cookie is invalid from our IP. Behavioral systems re-validate
// via sensor JS, so this is best-effort (mostly reliable for AWS WAF / Cloudflare).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeKind {
    AwsWaf,
    Cloudflare,
    DataDome,
}

impl ChallengeKind {
    fn from_tag(s: &str) -> Option<Self> {
        match s {
            "awswaf" => Some(Self::AwsWaf),
            "cloudflare" => Some(Self::Cloudflare),
            "datadome" => Some(Self::DataDome),
            _ => None,
        }
    }
    fn capsolver_task_type(&self) -> &'static str {
        match self {
            Self::AwsWaf => "AntiAwsWafTask",
            Self::Cloudflare => "AntiCloudflareTask",
            Self::DataDome => "DatadomeSliderTask",
        }
    }
    /// AWS WAF has a proxyless variant; Cloudflare/DataDome cookies are strictly
    /// IP-bound and REQUIRE a proxy that matches obscura's egress.
    fn proxyless_ok(&self) -> bool {
        matches!(self, Self::AwsWaf)
    }
    /// Default clearance-cookie name when the solver returns a bare value.
    fn cookie_name(&self) -> &'static str {
        match self {
            Self::AwsWaf => "aws-waf-token",
            Self::Cloudflare => "cf_clearance",
            Self::DataDome => "datadome",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SolvedChallenge {
    pub cookies: Vec<(String, String)>,
    pub user_agent: Option<String>,
}

const DETECT_CHALLENGE_JS: &str = r#"(function(){
  var h=(document.documentElement&&document.documentElement.outerHTML)||'';
  var s=[];try{s=Array.from(document.querySelectorAll('script[src]')).map(function(x){return x.src;});}catch(e){}
  var blob=h+' '+s.join(' ');
  if(/token\.awswaf\.com|AwsWafIntegration|awswaf/i.test(blob)) return 'awswaf';
  if(/cdn-cgi\/challenge-platform|_cf_chl_opt/i.test(blob)) return 'cloudflare';
  if(/captcha-delivery\.com|datadome/i.test(blob)) return 'datadome';
  return null;
})()"#;

/// Detect a type-B anti-bot challenge interstitial on the current page.
pub fn detect_challenge(page: &mut Page) -> Option<ChallengeKind> {
    match page.evaluate(DETECT_CHALLENGE_JS) {
        Value::String(s) => ChallengeKind::from_tag(&s),
        _ => None,
    }
}

/// Convert obscura's `scheme://user:pass@host:port` proxy URL to CapSolver's
/// `scheme:host:port[:user:pass]` proxy string.
fn capsolver_proxy(proxy_url: &str) -> Option<String> {
    let u = url::Url::parse(proxy_url).ok()?;
    let scheme = match u.scheme() {
        "socks5" | "socks5h" => "socks5",
        _ => "http",
    };
    let host = u.host_str()?;
    let port = u.port()?;
    let user = u.username();
    if user.is_empty() {
        Some(format!("{scheme}:{host}:{port}"))
    } else {
        let pass = u.password().unwrap_or("");
        Some(format!("{scheme}:{host}:{port}:{user}:{pass}"))
    }
}

/// Pull clearance cookie(s) + optional UA out of a CapSolver `solution` object,
/// tolerating the several shapes the API uses (`cookie` as `"n=v"` / bare value /
/// `"n=v; n=v"`, a `cookies` object/string, or a bare `token`).
fn extract_clearance(solution: &Value, kind: ChallengeKind) -> SolvedChallenge {
    let user_agent = solution
        .get("userAgent")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut cookies = Vec::new();
    let default = kind.cookie_name();
    let parse_str = |s: &str, out: &mut Vec<(String, String)>| {
        for part in s.split(';') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            match p.split_once('=') {
                Some((n, v)) => out.push((n.trim().to_string(), v.trim().to_string())),
                None => out.push((default.to_string(), p.to_string())),
            }
        }
    };
    if let Some(c) = solution.get("cookie").and_then(|v| v.as_str()) {
        parse_str(c, &mut cookies);
    } else if let Some(obj) = solution.get("cookies").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                cookies.push((k.clone(), s.to_string()));
            }
        }
    } else if let Some(s) = solution.get("cookies").and_then(|v| v.as_str()) {
        parse_str(s, &mut cookies);
    } else if let Some(t) = solution.get("token").and_then(|v| v.as_str()) {
        cookies.push((default.to_string(), t.to_string()));
    }
    SolvedChallenge { cookies, user_agent }
}

/// Solve a type-B challenge via CapSolver. `proxy` (obscura's egress) is passed
/// through so the clearance cookie is valid from our IP.
async fn solve_challenge(
    cfg: &CaptchaConfig,
    kind: ChallengeKind,
    page_url: &str,
    proxy: Option<&str>,
) -> anyhow::Result<SolvedChallenge> {
    let mut task = json!({ "websiteURL": page_url });
    match proxy {
        Some(p) => {
            task["type"] = json!(kind.capsolver_task_type());
            let cs = capsolver_proxy(p)
                .ok_or_else(|| anyhow::anyhow!("invalid proxy URL for CapSolver: {p}"))?;
            task["proxy"] = json!(cs);
        }
        None if kind.proxyless_ok() => {
            task["type"] = json!(format!("{}ProxyLess", kind.capsolver_task_type()));
        }
        None => anyhow::bail!(
            "{kind:?} needs a proxy (cookie is IP-bound) — set OBSCURA_PROXIES so obscura \
             and CapSolver share an egress IP"
        ),
    }

    let client = http_client();
    let create = client
        .post("https://api.capsolver.com/createTask")
        .json(&json!({ "clientKey": cfg.api_key, "task": task }))
        .send()
        .await?
        .json::<Value>()
        .await?;
    if create.get("errorId").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
        anyhow::bail!(
            "capsolver createTask: {}",
            create.get("errorDescription").and_then(|v| v.as_str()).unwrap_or("unknown")
        );
    }
    let task_id = create
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("capsolver: no taskId"))?
        .to_string();

    for _ in 0..40 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let res = client
            .post("https://api.capsolver.com/getTaskResult")
            .json(&json!({ "clientKey": cfg.api_key, "taskId": task_id }))
            .send()
            .await?
            .json::<Value>()
            .await?;
        if res.get("errorId").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
            anyhow::bail!(
                "capsolver getTaskResult: {}",
                res.get("errorDescription").and_then(|v| v.as_str()).unwrap_or("unknown")
            );
        }
        if res.get("status").and_then(|v| v.as_str()) == Some("ready") {
            let sol = res.get("solution").cloned().unwrap_or_default();
            let solved = extract_clearance(&sol, kind);
            if solved.cookies.is_empty() {
                anyhow::bail!("capsolver: solution had no clearance cookie");
            }
            return Ok(solved);
        }
    }
    anyhow::bail!("capsolver: timed out waiting for challenge solution")
}

/// Full type-B flow: detect challenge → solve (CapSolver, via obscura's proxy) →
/// set the clearance cookie(s) + UA on the context. Returns `Ok(true)` when a
/// clearance cookie was set (caller must RE-NAVIGATE to load real content),
/// `Ok(false)` when no challenge is present.
pub async fn try_solve_challenge(page: &mut Page, cfg: &CaptchaConfig) -> anyhow::Result<bool> {
    let Some(kind) = detect_challenge(page) else {
        return Ok(false);
    };
    if cfg.provider != Provider::CapSolver {
        tracing::warn!(?kind, "type-B challenge solving requires the CapSolver provider; skipping");
        return Ok(false);
    }
    let page_url = page.url_string();
    let proxy = page.context.proxy_url.clone();
    tracing::info!(?kind, "anti-bot challenge detected — solving via CapSolver");
    let solved = solve_challenge(cfg, kind, &page_url, proxy.as_deref()).await?;
    if let Ok(nav_url) = url::Url::parse(&page_url) {
        for (name, value) in &solved.cookies {
            page.context
                .cookie_jar
                .set_cookie(&format!("{name}={value}; Path=/"), &nav_url);
        }
    }
    if let Some(ua) = &solved.user_agent {
        page.http_client.set_user_agent(ua).await;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_task_types_and_proxy_rules() {
        assert_eq!(ChallengeKind::AwsWaf.capsolver_task_type(), "AntiAwsWafTask");
        assert_eq!(ChallengeKind::Cloudflare.capsolver_task_type(), "AntiCloudflareTask");
        assert!(ChallengeKind::AwsWaf.proxyless_ok());
        assert!(!ChallengeKind::Cloudflare.proxyless_ok());
        assert_eq!(ChallengeKind::from_tag("awswaf"), Some(ChallengeKind::AwsWaf));
        assert_eq!(ChallengeKind::from_tag("nope"), None);
    }

    #[test]
    fn capsolver_proxy_formats() {
        assert_eq!(
            capsolver_proxy("http://user:pass@1.2.3.4:8080").as_deref(),
            Some("http:1.2.3.4:8080:user:pass")
        );
        assert_eq!(
            capsolver_proxy("http://1.2.3.4:8080").as_deref(),
            Some("http:1.2.3.4:8080")
        );
        assert_eq!(
            capsolver_proxy("socks5://u:p@host.net:1080").as_deref(),
            Some("socks5:host.net:1080:u:p")
        );
    }

    #[test]
    fn extract_clearance_handles_shapes() {
        // bare value
        let s = extract_clearance(&json!({"cookie":"abc123"}), ChallengeKind::AwsWaf);
        assert_eq!(s.cookies, vec![("aws-waf-token".to_string(), "abc123".to_string())]);
        // name=value with UA
        let s = extract_clearance(
            &json!({"cookie":"cf_clearance=XYZ","userAgent":"UA/1"}),
            ChallengeKind::Cloudflare,
        );
        assert_eq!(s.cookies, vec![("cf_clearance".to_string(), "XYZ".to_string())]);
        assert_eq!(s.user_agent.as_deref(), Some("UA/1"));
        // cookies object
        let s = extract_clearance(&json!({"cookies":{"datadome":"DD1"}}), ChallengeKind::DataDome);
        assert_eq!(s.cookies, vec![("datadome".to_string(), "DD1".to_string())]);
    }

    #[test]
    fn kind_maps_response_field_and_task_types() {
        assert_eq!(CaptchaKind::RecaptchaV2.response_field(), "g-recaptcha-response");
        assert_eq!(CaptchaKind::HCaptcha.response_field(), "h-captcha-response");
        assert_eq!(CaptchaKind::Turnstile.response_field(), "cf-turnstile-response");
        assert_eq!(CaptchaKind::Turnstile.capsolver_task_type(), "AntiTurnstileTaskProxyLess");
        assert_eq!(CaptchaKind::HCaptcha.twocaptcha_method(), "hcaptcha");
        assert_eq!(CaptchaKind::from_tag("recaptcha_v2"), Some(CaptchaKind::RecaptchaV2));
        assert_eq!(CaptchaKind::from_tag("nope"), None);
    }

    #[test]
    fn config_off_without_key() {
        std::env::remove_var("OBSCURA_CAPTCHA_API_KEY");
        assert!(CaptchaConfig::from_env().is_none());
    }
}
