//! Enumerate testharness tests from WPT's `MANIFEST.json`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct TestCase {
    pub path: String,
    pub url: String,
    pub long_timeout: bool,
    pub unsupported_reason: Option<String>,
}

impl TestCase {
    pub fn explicit(url: &str) -> Self {
        Self {
            path: url.to_string(),
            url: url.to_string(),
            long_timeout: false,
            unsupported_reason: None,
        }
    }
}

pub struct UrlBuilder {
    pub host: String,
    pub http_port: u16,
    pub https_port: u16,
    pub h2_port: u16,
}

impl UrlBuilder {
    fn build(&self, url_path: &str, file_path: &str) -> String {
        if url_path.contains("://")
            || url_path.starts_with("about:")
            || url_path.starts_with("data:")
        {
            return url_path.to_string();
        }
        let path = if url_path.starts_with('/') {
            url_path.to_string()
        } else {
            format!("/{url_path}")
        };
        if file_path.contains(".h2.") {
            format!("https://{}:{}{}", self.host, self.h2_port, path)
        } else if file_path.contains(".https.")
            || file_path.contains(".serviceworker.")
            || file_path.contains(".h3.")
        {
            format!("https://{}:{}{}", self.host, self.https_port, path)
        } else {
            format!("http://{}:{}{}", self.host, self.http_port, path)
        }
    }
}

pub fn load_tests(
    manifest_path: &Path,
    base: &UrlBuilder,
    filter: Option<&str>,
) -> Result<Vec<TestCase>> {
    let data = fs::read_to_string(manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let value: Value = serde_json::from_str(&data).context("parse MANIFEST.json")?;
    Ok(load_value(&value, base, filter))
}

fn load_value(value: &Value, base: &UrlBuilder, filter: Option<&str>) -> Vec<TestCase> {
    let mut out = Vec::new();
    if let Some(tree) = value.pointer("/items/testharness") {
        walk(tree, &mut String::new(), base, &mut out);
    }
    if let Some(filter) = filter {
        out.retain(|test| test.path.contains(filter) || test.url.contains(filter));
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.url.cmp(&b.url)));
    out.dedup_by(|a, b| a.url == b.url);
    out
}

fn walk(node: &Value, prefix: &mut String, base: &UrlBuilder, out: &mut Vec<TestCase>) {
    match node {
        Value::Object(map) => {
            for (key, child) in map {
                let saved = prefix.len();
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(key);
                walk(child, prefix, base, out);
                prefix.truncate(saved);
            }
        }
        Value::Array(variants) => {
            let file_path = prefix.clone();
            for variant in variants.iter().skip(1).filter_map(Value::as_array) {
                let url_path = variant
                    .first()
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("/{file_path}"));
                let metadata = variant.get(1).and_then(Value::as_object);
                out.push(TestCase {
                    path: file_path.clone(),
                    url: base.build(&url_path, &file_path),
                    long_timeout: metadata
                        .and_then(|m| m.get("timeout"))
                        .and_then(Value::as_str)
                        == Some("long"),
                    unsupported_reason: unsupported_reason(&file_path, metadata),
                });
            }
        }
        _ => {}
    }
}

fn unsupported_reason(
    path: &str,
    metadata: Option<&serde_json::Map<String, Value>>,
) -> Option<String> {
    if path.contains(".h3.") {
        return Some("HTTP/3 WPT serving is not configured".into());
    }
    let metadata = metadata?;
    if metadata.get("testdriver").and_then(Value::as_bool) == Some(true)
        || metadata
            .get("testdriver_features")
            .and_then(Value::as_array)
            .is_some()
    {
        return Some("testdriver automation is not implemented".into());
    }
    if metadata.get("pac").is_some() {
        return Some("PAC automation is not implemented".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn base() -> UrlBuilder {
        UrlBuilder {
            host: "web-platform.test".into(),
            http_port: 8000,
            https_port: 8443,
            h2_port: 9000,
        }
    }

    #[test]
    fn loads_harness_variants_protocols_and_metadata() {
        let manifest = json!({"items":{"testharness":{"a":{
            "basic.html":["hash",[null,{}]],
            "secure.h2.html":["hash",[null,{"timeout":"long","testdriver":true}]]
        }}}});
        let tests = load_value(&manifest, &base(), None);
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].url, "http://web-platform.test:8000/a/basic.html");
        assert_eq!(
            tests[1].url,
            "https://web-platform.test:9000/a/secure.h2.html"
        );
        assert!(tests[1].long_timeout);
        assert!(tests[1]
            .unsupported_reason
            .as_deref()
            .unwrap()
            .contains("testdriver"));
        assert_eq!(base().build("about:blank", "a/basic.html"), "about:blank");
    }
}
