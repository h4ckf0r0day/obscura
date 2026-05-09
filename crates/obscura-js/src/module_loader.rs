use std::pin::Pin;

use deno_core::error::ModuleLoaderError;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;
use deno_core::RequestedModuleType;

use crate::ops::SharedState;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

pub struct ObscuraModuleLoader {
    pub base_url: String,
    state: SharedState,
}

impl ObscuraModuleLoader {
    pub fn new(base_url: &str, state: SharedState) -> Self {
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            state,
        }
    }
}

fn io_err(msg: String) -> ModuleLoaderError {
    std::io::Error::new(std::io::ErrorKind::Other, msg).into()
}

impl ModuleLoader for ObscuraModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: deno_core::ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let base = if referrer.is_empty()
            || referrer.starts_with('<')
            || referrer == "."
            || referrer == "about:blank"
        {
            &self.base_url
        } else {
            referrer
        };

        deno_core::resolve_import(specifier, base).map_err(|e| e.into())
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleSpecifier>,
        _is_dyn_import: bool,
        _requested_module_type: RequestedModuleType,
    ) -> ModuleLoadResponse {
        let url = module_specifier.to_string();
        let state = self.state.clone();

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            tracing::debug!("Loading ES module: {}", url);

            if let Some(code) = decode_data_module(&url) {
                let specifier = ModuleSpecifier::parse(&url)
                    .map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
                return Ok(ModuleSource::new(
                    deno_core::ModuleType::JavaScript,
                    ModuleSourceCode::String(code.into()),
                    &specifier,
                    None,
                ));
            }

            let http_client = {
                let gs = state.borrow();
                gs.http_client.clone()
            };

            let code = if let Some(client) = http_client {
                let parsed_url = url::Url::parse(&url)
                    .map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
                let resp = client
                    .fetch(&parsed_url)
                    .await
                    .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?;

                if !(200..300).contains(&resp.status) {
                    return Err(io_err(format!(
                        "Module {} returned HTTP {}",
                        url, resp.status
                    )));
                }

                String::from_utf8_lossy(&resp.body).to_string()
            } else {
                let client = reqwest::Client::builder()
                    .build()
                    .map_err(|e| io_err(format!("HTTP client error: {}", e)))?;

                let resp = client
                    .get(&url)
                    .header("Accept", "application/javascript, text/javascript, */*")
                    .send()
                    .await
                    .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?;

                if !resp.status().is_success() {
                    return Err(io_err(format!(
                        "Module {} returned HTTP {}",
                        url,
                        resp.status()
                    )));
                }

                resp.text()
                    .await
                    .map_err(|e| io_err(format!("Failed to read module body {}: {}", url, e)))?
            };

            let specifier = ModuleSpecifier::parse(&url)
                .map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;

            Ok(ModuleSource::new(
                deno_core::ModuleType::JavaScript,
                ModuleSourceCode::String(code.into()),
                &specifier,
                None,
            ))
        })))
    }
}

fn decode_data_module(url: &str) -> Option<String> {
    if !url.starts_with("data:") {
        return None;
    }

    let comma = url.find(',')?;
    let (metadata, data) = url.split_at(comma);
    let data = &data[1..];
    let bytes = if metadata.to_ascii_lowercase().contains(";base64") {
        BASE64.decode(data).ok()?
    } else {
        percent_decode(data)
    };

    String::from_utf8(bytes).ok()
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                decoded.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }

        decoded.push(bytes[i]);
        i += 1;
    }

    decoded
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
