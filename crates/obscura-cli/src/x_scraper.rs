use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use obscura_net::{CookieJar, ObscuraHttpClient};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use url::Url;

#[derive(Debug)]
pub struct XScrapeOptions {
    pub handles: Vec<String>,
    pub base_url: String,
    pub nitter_base_url: String,
    pub source: String,
    pub limit: usize,
    pub concurrency: usize,
    pub timeout_secs: u64,
    pub proxy: Option<String>,
    pub quiet: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct XStatusItem {
    pub handle: String,
    pub status_id: String,
    pub url: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repost_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub like_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_followers_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_following_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_statuses_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_listed_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_verified: Option<bool>,
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct XUserProfile {
    name: Option<String>,
    description: Option<String>,
    followers_count: Option<u64>,
    following_count: Option<u64>,
    statuses_count: Option<u64>,
    listed_count: Option<u64>,
    verified: Option<bool>,
}

#[derive(Clone, Debug)]
struct XWebSession {
    base_url: Url,
    bearer_token: String,
    guest_token: String,
    user_by_screen_name: OperationMetadata,
    user_tweets: OperationMetadata,
}

#[derive(Clone, Debug)]
struct OperationMetadata {
    query_id: String,
    operation_name: String,
    feature_switches: Vec<String>,
    field_toggles: Vec<String>,
}

pub async fn scrape_x_profiles(options: XScrapeOptions) -> anyhow::Result<Vec<XStatusItem>> {
    if options.handles.is_empty() {
        anyhow::bail!("No X handles provided.");
    }
    if options.limit == 0 {
        anyhow::bail!("--limit must be greater than 0.");
    }
    if options.concurrency == 0 {
        anyhow::bail!("--concurrency must be greater than 0.");
    }

    let source = options.source.trim().to_ascii_lowercase();
    if !matches!(source.as_str(), "nitter" | "x-web" | "auto") {
        anyhow::bail!("--source must be one of: nitter, x-web, auto.");
    }

    let client = Arc::new(ObscuraHttpClient::with_options(
        Arc::new(CookieJar::new()),
        options.proxy.as_deref(),
    ));
    client
        .user_agent
        .write()
        .await
        .clone_from(&"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36".to_string());

    if source == "nitter" {
        return scrape_nitter_profiles(&client, &options).await;
    }
    if source == "auto" {
        let items = scrape_nitter_profiles(&client, &options).await?;
        if !items.is_empty() {
            return Ok(items);
        }
        if !options.quiet {
            eprintln!("Nitter-compatible RSS returned no items, falling back to X web data.");
        }
    }

    let base_url = normalize_base_url(&options.base_url)?;
    let reference_handle = normalize_handle(&options.handles[0]);
    let session = match prepare_x_web_session(&client, base_url, &reference_handle).await {
        Ok(session) => Arc::new(session),
        Err(error) => return Err(error),
    };
    let semaphore = Arc::new(Semaphore::new(options.concurrency));
    let mut tasks = Vec::new();

    for raw_handle in &options.handles {
        let handle = normalize_handle(raw_handle);
        if handle.is_empty() {
            continue;
        }
        let client = client.clone();
        let session = session.clone();
        let semaphore = semaphore.clone();
        let quiet = options.quiet;
        let limit = options.limit;
        let timeout_secs = options.timeout_secs;

        tasks.push(tokio::spawn(async move {
            let _permit = semaphore.acquire().await.expect("semaphore open");
            let result = timeout(
                Duration::from_secs(timeout_secs),
                fetch_handle_tweets(&client, &session, &handle, limit),
            )
            .await;

            match result {
                Ok(Ok(items)) => items,
                Ok(Err(error)) => {
                    if !quiet {
                        eprintln!("x scrape failed for @{}: {}", handle, error);
                    }
                    Vec::new()
                }
                Err(_) => {
                    if !quiet {
                        eprintln!("x scrape timed out for @{} after {}s", handle, timeout_secs);
                    }
                    Vec::new()
                }
            }
        }));
    }

    let mut items = Vec::new();
    for task in tasks {
        items.extend(task.await?);
    }

    Ok(items)
}

async fn scrape_nitter_profiles(
    client: &Arc<ObscuraHttpClient>,
    options: &XScrapeOptions,
) -> anyhow::Result<Vec<XStatusItem>> {
    let base_url = normalize_base_url(&options.nitter_base_url)?;
    let semaphore = Arc::new(Semaphore::new(options.concurrency));
    let mut tasks = Vec::new();

    for raw_handle in &options.handles {
        let handle = normalize_handle(raw_handle);
        if handle.is_empty() {
            continue;
        }
        let client = client.clone();
        let semaphore = semaphore.clone();
        let base_url = base_url.clone();
        let quiet = options.quiet;
        let limit = options.limit;
        let timeout_secs = options.timeout_secs;

        tasks.push(tokio::spawn(async move {
            let _permit = semaphore.acquire().await.expect("semaphore open");
            let result = timeout(
                Duration::from_secs(timeout_secs),
                fetch_nitter_rss(&client, &base_url, &handle, limit),
            )
            .await;

            match result {
                Ok(Ok(items)) => items,
                Ok(Err(error)) => {
                    if !quiet {
                        eprintln!("x scrape failed for @{}: {}", handle, error);
                    }
                    Vec::new()
                }
                Err(_) => {
                    if !quiet {
                        eprintln!("x scrape timed out for @{} after {}s", handle, timeout_secs);
                    }
                    Vec::new()
                }
            }
        }));
    }

    let mut items = Vec::new();
    for task in tasks {
        items.extend(task.await?);
    }

    Ok(items)
}

async fn fetch_nitter_rss(
    client: &ObscuraHttpClient,
    base_url: &Url,
    handle: &str,
    limit: usize,
) -> anyhow::Result<Vec<XStatusItem>> {
    let url = base_url.join(&format!("{handle}/rss"))?;
    let xml = fetch_text(client, url).await?;
    let items = parse_nitter_rss(handle, &xml, limit);
    if items.is_empty() {
        anyhow::bail!(
            "Nitter-compatible RSS returned no status items for @{}",
            handle
        );
    }
    Ok(items)
}

async fn prepare_x_web_session(
    client: &ObscuraHttpClient,
    base_url: Url,
    reference_handle: &str,
) -> anyhow::Result<XWebSession> {
    let profile_url = base_url.join(reference_handle)?;
    let html = fetch_text(client, profile_url).await?;
    let guest_token = extract_guest_token(&html)
        .ok_or_else(|| anyhow::anyhow!("Could not find X guest token in profile HTML"))?;
    let main_js_url = extract_main_js_url(&html)
        .ok_or_else(|| anyhow::anyhow!("Could not find X main JavaScript URL"))?;
    let main_js = fetch_text(client, Url::parse(&main_js_url)?).await?;
    let bearer_token = extract_bearer_token(&main_js)
        .ok_or_else(|| anyhow::anyhow!("Could not find X bearer token in main JavaScript"))?;

    Ok(XWebSession {
        base_url,
        bearer_token,
        guest_token,
        user_by_screen_name: extract_operation_metadata(&main_js, "UserByScreenName")?,
        user_tweets: extract_operation_metadata(&main_js, "UserTweets")?,
    })
}

async fn fetch_handle_tweets(
    client: &ObscuraHttpClient,
    session: &XWebSession,
    handle: &str,
    limit: usize,
) -> anyhow::Result<Vec<XStatusItem>> {
    let user_id = fetch_user_id(client, session, handle).await?;
    let timeline = fetch_user_tweets(client, session, &user_id, limit).await?;
    Ok(parse_user_tweets(handle, &user_id, &timeline, limit))
}

async fn fetch_user_id(
    client: &ObscuraHttpClient,
    session: &XWebSession,
    handle: &str,
) -> anyhow::Result<String> {
    let variables = serde_json::json!({ "screen_name": handle });
    let body = fetch_graphql(client, session, &session.user_by_screen_name, variables).await?;
    body.pointer("/data/user/result/rest_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("UserByScreenName returned no rest_id for @{}", handle))
}

async fn fetch_user_tweets(
    client: &ObscuraHttpClient,
    session: &XWebSession,
    user_id: &str,
    limit: usize,
) -> anyhow::Result<Value> {
    let variables = serde_json::json!({
        "userId": user_id,
        "count": limit.max(5).min(40),
        "includePromotedContent": false,
        "withQuickPromoteEligibilityTweetFields": false,
        "withVoice": true
    });
    fetch_graphql(client, session, &session.user_tweets, variables).await
}

async fn fetch_graphql(
    client: &ObscuraHttpClient,
    session: &XWebSession,
    operation: &OperationMetadata,
    variables: Value,
) -> anyhow::Result<Value> {
    let features = json_bool_map(&operation.feature_switches);
    let field_toggles = json_bool_map(&operation.field_toggles);
    let query = serde_json::json!({
        "variables": variables,
        "features": features,
        "fieldToggles": field_toggles,
    });

    let mut url = session.base_url.join(&format!(
        "i/api/graphql/{}/{}",
        operation.query_id, operation.operation_name
    ))?;
    {
        let mut pairs = url.query_pairs_mut();
        for key in ["variables", "features", "fieldToggles"] {
            pairs.append_pair(key, &serde_json::to_string(&query[key])?);
        }
    }

    let mut headers = client.extra_headers.write().await;
    headers.insert(
        "authorization".to_string(),
        format!("Bearer {}", session.bearer_token),
    );
    headers.insert("x-guest-token".to_string(), session.guest_token.clone());
    headers.insert("x-twitter-active-user".to_string(), "yes".to_string());
    headers.insert("x-twitter-client-language".to_string(), "en".to_string());
    drop(headers);

    let response = client.fetch(&url).await?;
    if response.status != 200 {
        anyhow::bail!("{} returned HTTP {}", url, response.status);
    }
    Ok(serde_json::from_str(&response.text()?)?)
}

async fn fetch_text(client: &ObscuraHttpClient, url: Url) -> anyhow::Result<String> {
    let response = client.fetch(&url).await?;
    if response.status != 200 {
        anyhow::bail!("{} returned HTTP {}", url, response.status);
    }
    Ok(response.text()?)
}

fn parse_user_tweets(handle: &str, user_id: &str, value: &Value, limit: usize) -> Vec<XStatusItem> {
    let handle = normalize_handle(handle);
    let mut tweets = Vec::new();
    let mut seen = HashSet::new();
    let profile = extract_user_profile(value, user_id);
    collect_tweets(
        value,
        &handle,
        user_id,
        profile.as_ref(),
        &mut seen,
        &mut tweets,
        limit,
    );
    tweets
}

fn collect_tweets(
    value: &Value,
    handle: &str,
    user_id: &str,
    profile: Option<&XUserProfile>,
    seen: &mut HashSet<String>,
    tweets: &mut Vec<XStatusItem>,
    limit: usize,
) {
    if tweets.len() >= limit {
        return;
    }

    match value {
        Value::Object(map) => {
            if map
                .get("__typename")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "Tweet")
            {
                if let Some(tweet) = tweet_from_object(map, handle, user_id, profile) {
                    if seen.insert(tweet.status_id.clone()) {
                        tweets.push(tweet);
                    }
                }
            }

            for child in map.values() {
                collect_tweets(child, handle, user_id, profile, seen, tweets, limit);
                if tweets.len() >= limit {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_tweets(child, handle, user_id, profile, seen, tweets, limit);
                if tweets.len() >= limit {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn tweet_from_object(
    map: &serde_json::Map<String, Value>,
    handle: &str,
    user_id: &str,
    profile: Option<&XUserProfile>,
) -> Option<XStatusItem> {
    let status_id = map.get("rest_id")?.as_str()?.to_string();
    let legacy = map.get("legacy")?;
    if legacy.get("retweeted_status_result").is_some() {
        return None;
    }

    let author_id =
        map_get(map, &["core", "user_results", "result", "rest_id"]).and_then(Value::as_str)?;
    if author_id != user_id {
        return None;
    }

    let screen_name = map_get(
        map,
        &["core", "user_results", "result", "core", "screen_name"],
    )
    .and_then(Value::as_str)
    .map(normalize_handle)
    .unwrap_or_else(|| handle.to_string());
    if screen_name != handle {
        return None;
    }

    let text = legacy
        .get("full_text")
        .and_then(Value::as_str)
        .map(normalize_space)
        .filter(|text| !text.is_empty())?;

    Some(XStatusItem {
        handle: handle.to_string(),
        url: format!("https://x.com/{}/status/{}", handle, status_id),
        status_id,
        text,
        published_at: legacy
            .get("created_at")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        reply_count: legacy.get("reply_count").and_then(Value::as_u64),
        repost_count: legacy.get("retweet_count").and_then(Value::as_u64),
        quote_count: legacy.get("quote_count").and_then(Value::as_u64),
        like_count: legacy.get("favorite_count").and_then(Value::as_u64),
        view_count: map_get(map, &["views", "count"])
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok()),
        profile_name: profile.and_then(|value| value.name.clone()),
        profile_description: profile.and_then(|value| value.description.clone()),
        profile_followers_count: profile.and_then(|value| value.followers_count),
        profile_following_count: profile.and_then(|value| value.following_count),
        profile_statuses_count: profile.and_then(|value| value.statuses_count),
        profile_listed_count: profile.and_then(|value| value.listed_count),
        profile_verified: profile.and_then(|value| value.verified),
        source: "x_web".to_string(),
    })
}

fn extract_user_profile(value: &Value, user_id: &str) -> Option<XUserProfile> {
    match value {
        Value::Object(map) => {
            let matching_user = map
                .get("rest_id")
                .and_then(Value::as_str)
                .is_some_and(|rest_id| rest_id == user_id)
                && map.get("legacy").is_some();
            if matching_user {
                if let Some(profile) = user_profile_from_object(map) {
                    return Some(profile);
                }
            }
            for child in map.values() {
                if let Some(profile) = extract_user_profile(child, user_id) {
                    return Some(profile);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|child| extract_user_profile(child, user_id)),
        _ => None,
    }
}

fn user_profile_from_object(map: &serde_json::Map<String, Value>) -> Option<XUserProfile> {
    let legacy = map.get("legacy")?;
    let name = legacy
        .get("name")
        .and_then(Value::as_str)
        .map(normalize_space)
        .filter(|value| !value.is_empty());
    let description = legacy
        .get("description")
        .and_then(Value::as_str)
        .map(normalize_space)
        .filter(|value| !value.is_empty());
    Some(XUserProfile {
        name,
        description,
        followers_count: legacy.get("followers_count").and_then(Value::as_u64),
        following_count: legacy.get("friends_count").and_then(Value::as_u64),
        statuses_count: legacy.get("statuses_count").and_then(Value::as_u64),
        listed_count: legacy.get("listed_count").and_then(Value::as_u64),
        verified: legacy
            .get("verified")
            .and_then(Value::as_bool)
            .or_else(|| map.get("is_blue_verified").and_then(Value::as_bool)),
    })
}

fn map_get<'a>(map: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let mut current = map.get(*path.first()?)?;
    for segment in &path[1..] {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn normalize_base_url(base_url: &str) -> anyhow::Result<Url> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        anyhow::bail!("--base-url cannot be empty.");
    }
    Ok(Url::parse(&format!("{trimmed}/"))?)
}

fn normalize_handle(handle: &str) -> String {
    handle
        .trim()
        .trim_start_matches('@')
        .trim_matches('/')
        .to_ascii_lowercase()
}

fn extract_guest_token(html: &str) -> Option<String> {
    extract_digits_after(html, "gt=")
}

fn extract_main_js_url(html: &str) -> Option<String> {
    let marker = "https://abs.twimg.com/responsive-web/client-web/main.";
    let start = html.find(marker)?;
    let end = html[start..].find(".js")? + start + 3;
    Some(html[start..end].to_string())
}

fn extract_bearer_token(js: &str) -> Option<String> {
    let marker = "AAAAAAAAAAAAAAAAAAAA";
    let start = js.find(marker)?;
    let token: String = js[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '%' | '_' | '-'))
        .collect();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn extract_operation_metadata(js: &str, operation_name: &str) -> anyhow::Result<OperationMetadata> {
    let operation_marker = format!("operationName:\"{operation_name}\"");
    let op_index = js
        .find(&operation_marker)
        .ok_or_else(|| anyhow::anyhow!("Could not find X operation {}", operation_name))?;
    let query_marker = "queryId:\"";
    let query_start = js[..op_index]
        .rfind(query_marker)
        .ok_or_else(|| anyhow::anyhow!("Could not find queryId for {}", operation_name))?
        + query_marker.len();
    let query_end = js[query_start..]
        .find('"')
        .ok_or_else(|| anyhow::anyhow!("Malformed queryId for {}", operation_name))?
        + query_start;

    let metadata_start = js[op_index..]
        .find("metadata:{")
        .map(|offset| op_index + offset)
        .unwrap_or(query_start);
    let metadata_end = js[op_index..]
        .find("}}}")
        .map(|offset| op_index + offset)
        .unwrap_or(op_index + operation_marker.len());
    let metadata = &js[metadata_start..metadata_end];

    Ok(OperationMetadata {
        query_id: js[query_start..query_end].to_string(),
        operation_name: operation_name.to_string(),
        feature_switches: extract_string_array(metadata, "featureSwitches:["),
        field_toggles: extract_string_array(metadata, "fieldToggles:["),
    })
}

fn extract_string_array(input: &str, marker: &str) -> Vec<String> {
    let Some(start) = input.find(marker).map(|value| value + marker.len()) else {
        return Vec::new();
    };
    let Some(end) = input[start..].find(']').map(|value| value + start) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    let mut cursor = start;
    while let Some(open) = input[cursor..end].find('"') {
        let value_start = cursor + open + 1;
        let Some(close) = input[value_start..end].find('"') else {
            break;
        };
        let value_end = value_start + close;
        values.push(input[value_start..value_end].to_string());
        cursor = value_end + 1;
    }
    values
}

fn extract_digits_after(input: &str, marker: &str) -> Option<String> {
    let start = input.find(marker)? + marker.len();
    let digits: String = input[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

fn json_bool_map(keys: &[String]) -> Value {
    Value::Object(
        keys.iter()
            .map(|key| (key.clone(), Value::Bool(true)))
            .collect(),
    )
}

fn normalize_space(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_nitter_rss(handle: &str, xml: &str, limit: usize) -> Vec<XStatusItem> {
    let handle = normalize_handle(handle);
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = 0;

    while items.len() < limit {
        let Some(item_start) = xml[cursor..].find("<item>").map(|offset| cursor + offset) else {
            break;
        };
        let content_start = item_start + "<item>".len();
        let Some(item_end) = xml[content_start..]
            .find("</item>")
            .map(|offset| content_start + offset)
        else {
            break;
        };
        cursor = item_end + "</item>".len();
        let item = &xml[content_start..item_end];

        let Some(raw_link) = extract_xml_tag(item, "link") else {
            continue;
        };
        let Some((link_handle, status_id)) = parse_status_link(&raw_link) else {
            continue;
        };
        if !seen.insert(status_id.clone()) {
            continue;
        }

        let raw_title = extract_xml_tag(item, "title")
            .or_else(|| extract_xml_tag(item, "description"))
            .unwrap_or_default();
        let text = normalize_space(&decode_xml_entities(&strip_cdata(&raw_title)));
        if text.is_empty() {
            continue;
        }

        items.push(XStatusItem {
            handle: handle.clone(),
            url: format!("https://x.com/{}/status/{}", link_handle, status_id),
            status_id,
            text,
            published_at: extract_xml_tag(item, "pubDate")
                .map(|value| normalize_space(&decode_xml_entities(&strip_cdata(&value)))),
            reply_count: None,
            repost_count: None,
            quote_count: None,
            like_count: None,
            view_count: None,
            profile_name: None,
            profile_description: None,
            profile_followers_count: None,
            profile_following_count: None,
            profile_statuses_count: None,
            profile_listed_count: None,
            profile_verified: None,
            source: "nitter_rss".to_string(),
        });
    }

    items
}

fn extract_xml_tag(input: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = input.find(&open)? + open.len();
    let end = input[start..].find(&close)? + start;
    Some(input[start..end].trim().to_string())
}

fn strip_cdata(input: &str) -> String {
    input
        .strip_prefix("<![CDATA[")
        .and_then(|value| value.strip_suffix("]]>"))
        .unwrap_or(input)
        .to_string()
}

fn parse_status_link(link: &str) -> Option<(String, String)> {
    let url = Url::parse(link).ok()?;
    let mut parts = url.path_segments()?;
    let handle = normalize_handle(parts.next()?);
    if parts.next()? != "status" {
        return None;
    }
    let status_id: String = parts
        .next()?
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if handle.is_empty() || status_id.is_empty() {
        None
    } else {
        Some((handle, status_id))
    }
}

fn decode_xml_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        extract_bearer_token, extract_guest_token, extract_main_js_url, extract_operation_metadata,
        parse_nitter_rss, parse_status_link, parse_user_tweets, XStatusItem,
    };

    #[test]
    fn extracts_x_web_bootstrap_values() {
        let html = r#"<script>document.cookie="gt=1234567890; Max-Age=9000";</script>
            <script src="https://abs.twimg.com/responsive-web/client-web/main.abc123.js"></script>"#;
        let js = r#"
            e.exports={queryId:"userQuery",operationName:"UserByScreenName",operationType:"query",metadata:{featureSwitches:["one"],fieldToggles:["withPayments"]}}},
            e.exports={queryId:"tweetsQuery",operationName:"UserTweets",operationType:"query",metadata:{featureSwitches:["two","three"],fieldToggles:["withArticlePlainText"]}}},
            const bearer="AAAAAAAAAAAAAAAAAAAATOKEN%3Dabc";
        "#;

        assert_eq!(extract_guest_token(html).as_deref(), Some("1234567890"));
        assert_eq!(
            extract_main_js_url(html).as_deref(),
            Some("https://abs.twimg.com/responsive-web/client-web/main.abc123.js")
        );
        assert_eq!(
            extract_bearer_token(js).as_deref(),
            Some("AAAAAAAAAAAAAAAAAAAATOKEN%3Dabc")
        );

        let tweets = extract_operation_metadata(js, "UserTweets").expect("metadata");
        assert_eq!(tweets.query_id, "tweetsQuery");
        assert_eq!(tweets.feature_switches, vec!["two", "three"]);
        assert_eq!(tweets.field_toggles, vec!["withArticlePlainText"]);
    }

    #[test]
    fn parses_direct_x_user_tweets_json() {
        let data = json!({
            "data": {
                "user": {
                    "result": {
                        "rest_id": "44196397",
                        "legacy": {
                            "name": "Elon Musk",
                            "description": "Technoking",
                            "followers_count": 250000000,
                            "friends_count": 1200,
                            "statuses_count": 75000,
                            "listed_count": 160000,
                            "verified": true
                        },
                        "timeline": {
                            "timeline": {
                                "instructions": [{
                                    "entries": [{
                                        "content": {
                                            "itemContent": {
                                                "tweet_results": {
                                                    "result": {
                                                        "__typename": "Tweet",
                                                        "rest_id": "2053484280052076966",
                                                        "core": {
                                                            "user_results": {
                                                                "result": {
                                                                    "rest_id": "44196397",
                                                                    "core": {
                                                                        "screen_name": "elonmusk"
                                                                    }
                                                                }
                                                            }
                                                        },
                                                        "legacy": {
                                                            "created_at": "Sun May 10 14:36:00 +0000 2026",
                                                            "full_text": "Perhaps a restoration of dignity is in order",
                                                            "reply_count": 11619,
                                                            "retweet_count": 11376,
                                                            "quote_count": 1000,
                                                            "favorite_count": 149725
                                                        },
                                                        "views": {
                                                            "count": "23209089"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }]
                                }]
                            }
                        }
                    }
                }
            }
        });

        let items = parse_user_tweets("ElonMusk", "44196397", &data, 5);

        assert_eq!(
            items,
            vec![XStatusItem {
                handle: "elonmusk".to_string(),
                status_id: "2053484280052076966".to_string(),
                url: "https://x.com/elonmusk/status/2053484280052076966".to_string(),
                text: "Perhaps a restoration of dignity is in order".to_string(),
                published_at: Some("Sun May 10 14:36:00 +0000 2026".to_string()),
                reply_count: Some(11619),
                repost_count: Some(11376),
                quote_count: Some(1000),
                like_count: Some(149725),
                view_count: Some(23209089),
                profile_name: Some("Elon Musk".to_string()),
                profile_description: Some("Technoking".to_string()),
                profile_followers_count: Some(250000000),
                profile_following_count: Some(1200),
                profile_statuses_count: Some(75000),
                profile_listed_count: Some(160000),
                profile_verified: Some(true),
                source: "x_web".to_string(),
            }]
        );
    }

    #[test]
    fn parses_nitter_rss_status_items() {
        let xml = r#"
            <rss><channel>
              <item>
                <title>interesting &amp; useful</title>
                <link>https://nitter.net/sama/status/2053566155571560868#m</link>
                <pubDate>Sun, 10 May 2026 20:01:39 GMT</pubDate>
              </item>
              <item>
                <title>RT by @sama: should still be linkable</title>
                <link>https://nitter.net/another/status/2053000000000000000#m</link>
                <pubDate>Sun, 10 May 2026 18:00:00 GMT</pubDate>
              </item>
            </channel></rss>
        "#;

        let items = parse_nitter_rss("Sama", xml, 5);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].handle, "sama");
        assert_eq!(items[0].status_id, "2053566155571560868");
        assert_eq!(
            items[0].url,
            "https://x.com/sama/status/2053566155571560868"
        );
        assert_eq!(items[0].text, "interesting & useful");
        assert_eq!(
            items[0].published_at.as_deref(),
            Some("Sun, 10 May 2026 20:01:39 GMT")
        );
        assert_eq!(items[0].source, "nitter_rss");
        assert_eq!(
            items[1].url,
            "https://x.com/another/status/2053000000000000000"
        );
    }

    #[test]
    fn parses_status_links() {
        assert_eq!(
            parse_status_link("https://nitter.net/elonmusk/status/2053484280052076966#m"),
            Some(("elonmusk".to_string(), "2053484280052076966".to_string()))
        );
        assert_eq!(parse_status_link("https://nitter.net/elonmusk"), None);
    }
}
