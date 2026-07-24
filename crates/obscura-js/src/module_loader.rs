use std::pin::Pin;

use deno_core::error::ModuleLoaderError;
use deno_core::ModuleLoadOptions;
use deno_core::ModuleLoadReferrer;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;
use deno_error::JsErrorBox;

pub struct ObscuraModuleLoader {
    pub base_url: String,
    pub proxy_url: Option<String>,
}

impl ObscuraModuleLoader {
    pub fn new(base_url: &str) -> Self {
        Self::with_proxy(base_url, None)
    }

    pub fn with_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
        }
    }
}

fn io_err(msg: String) -> ModuleLoaderError {
    JsErrorBox::from_err(std::io::Error::new(std::io::ErrorKind::Other, msg))
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

        deno_core::resolve_import(specifier, base).map_err(JsErrorBox::from_err)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.to_string();
        let proxy_url = self.proxy_url.clone();

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            let client = crate::ops::cached_request_client(proxy_url.as_deref())
                .map_err(io_err)?;

            tracing::debug!(
                "Loading ES module: {} (proxy: {})",
                url,
                proxy_url.as_deref().unwrap_or("direct")
            );

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

            let code = resp.text().await.map_err(|e| {
                io_err(format!("Failed to read module body {}: {}", url, e))
            })?;

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
