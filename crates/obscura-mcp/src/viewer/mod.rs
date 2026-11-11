mod http;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
#[cfg(feature = "render")]
use serde_json::json;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle;

use crate::{BrowserState, WebViewerOptions};

pub(crate) type FrameRequest = oneshot::Sender<Result<Value, String>>;

pub(crate) struct Viewer {
    pub url: String,
    requests: mpsc::Receiver<FrameRequest>,
    server: JoinHandle<()>,
    ready_file: Option<PathBuf>,
}

impl Viewer {
    pub async fn start(options: WebViewerOptions) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, options.port)).await?;
        let authority = listener.local_addr()?.to_string();
        let route = format!("/{}/", uuid::Uuid::new_v4().simple());
        let url = format!("http://{authority}{route}");
        if let Some(path) = options.ready_file.as_ref() {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            file.write_all(url.as_bytes())?;
        }
        let (sender, requests) = mpsc::channel(8);
        let server = tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(16));
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                        let sender = sender.clone();
                        let authority = authority.clone();
                        let route = route.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(15),
                                http::serve(stream, &authority, &route, sender),
                            ).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        eprintln!("Obscura viewer: {url}");
        Ok(Self { url, requests, server, ready_file: options.ready_file })
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.server.abort();
        if let Some(path) = self.ready_file.as_ref() {
            if std::fs::read_to_string(path).ok().as_deref() == Some(self.url.as_str()) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

pub(crate) async fn next_request(viewer: &mut Option<Viewer>) -> FrameRequest {
    if let Some(viewer) = viewer {
        if let Some(request) = viewer.requests.recv().await {
            return request;
        }
    }
    std::future::pending().await
}

pub(crate) fn capture(state: &mut BrowserState) -> Result<Value, String> {
    #[cfg(feature = "render")]
    {
        use base64::Engine as _;
        let Some(tab_id) = state.active_tab.clone() else {
            return Ok(json!({ "tabId": null, "url": null, "image": null }));
        };
        let page = state.page_mut();
        crate::validate_screenshot_viewport(page.viewport)?;
        let image = page.screenshot(page.viewport).ok_or("the active page has no renderable viewport")?;
        Ok(json!({
            "tabId": tab_id,
            "url": page.url_string(),
            "image": crate::BASE64.encode(image),
        }))
    }
    #[cfg(not(feature = "render"))]
    {
        let _ = state;
        Err("--web-viewer requires a render-enabled Obscura build".into())
    }
}
