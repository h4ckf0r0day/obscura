use std::pin::Pin;

use deno_core::error::ModuleLoaderError;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;
use deno_core::RequestedModuleType;

pub struct ObscuraModuleLoader {
    pub base_url: String,
    /// Proxy URL threaded through to every dynamic ES-module fetch (#139).
    /// `None` keeps the pre-#139 direct-connection behaviour for callers
    /// that haven't been updated.
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
        // Capture the loader's proxy here so the async closure below owns a
        // plain Option<String> rather than borrowing &self across an `await`.
        let proxy_url = self.proxy_url.clone();

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            // Reuse the process-wide cached client (same one op_fetch_url
            // uses). Modern SPAs dynamic-import 20-50 chunks per page; the
            // old code built a fresh reqwest::Client per import, each with
            // its own empty connection pool, no reuse, fresh TLS init for
            // every chunk. The cache means the first import on a given
            // proxy pays the build cost once and every chunk after reuses
            // the same warm pool.
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
