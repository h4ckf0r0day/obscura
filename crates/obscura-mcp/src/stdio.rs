use std::path::PathBuf;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{dispatch, viewer, BrowserState, RpcMessage};

#[derive(Default)]
pub struct HeadedOptions {
    /// Zero chooses an available loopback port, allowing independent agents.
    pub port: u16,
    /// Created exclusively and removed on a graceful exit.
    pub ready_file: Option<PathBuf>,
}

pub async fn run_with_options(
    proxy: Option<String>,
    user_agent: Option<String>,
    stealth: bool,
    headed: Option<HeadedOptions>,
) -> Result<()> {
    if headed.is_some() && !cfg!(feature = "render") {
        anyhow::bail!("--headed requires a render-enabled Obscura build");
    }
    let mut viewer = match headed {
        Some(options) => Some(viewer::Viewer::start(options).await?),
        None => None,
    };
    let mut state = BrowserState::new(proxy, user_agent, stealth);
    state.operator_network_hints = true;
    state.viewer_url = viewer.as_ref().map(|viewer| viewer.url.clone());
    let mut reader = BufReader::new(tokio::io::stdin()).lines();
    let mut writer = tokio::io::stdout();
    let mut runtime_pump_armed = false;

    loop {
        enum Wake {
            Line(Option<String>),
            Frame(viewer::FrameRequest),
            PageTurn(Result<bool, String>),
        }
        let wake = tokio::select! {
            biased;
            // next_line retains partial input when a frame or page task wins.
            line = reader.next_line() => Wake::Line(line?),
            request = viewer::next_request(&mut viewer) => Wake::Frame(request),
            result = async { state.page_mut().run_autonomous_event_loop_turn().await }, if runtime_pump_armed => {
                Wake::PageTurn(result)
            }
        };
        let line = match wake {
            Wake::Line(Some(line)) => line,
            Wake::Line(None) => {
                state.context.save_storage();
                return Ok(());
            }
            Wake::Frame(request) => {
                if !request.is_closed() {
                    let _ = request.send(viewer::capture(&mut state));
                }
                continue;
            }
            Wake::PageTurn(result) => {
                let result = match result {
                    Ok(reached_idle) => state.finish_active_page_turn(reached_idle).await,
                    Err(error) => Err(error),
                };
                match result {
                    Ok(reached_idle) => runtime_pump_armed = !reached_idle,
                    Err(error) => {
                        runtime_pump_armed = false;
                        eprintln!("MCP page task failed: {error}");
                    }
                }
                continue;
            }
        };
        let message: RpcMessage = match serde_json::from_str(line.trim()) {
            Ok(message) => message,
            Err(_) => continue,
        };
        let Some(id) = message.id else { continue };
        let response = dispatch(&message.method, id, &message.params, &mut state).await;
        runtime_pump_armed = state.has_active_page_runtime();
        let mut body = serde_json::to_string(&response)?;
        body.push('\n');
        writer.write_all(body.as_bytes()).await?;
        writer.flush().await?;
    }
}

#[cfg(all(test, not(feature = "render")))]
mod tests {
    #[tokio::test]
    async fn headed_rejects_no_render_before_starting_a_session() {
        let result = super::run_with_options(None, None, false, Some(super::HeadedOptions::default())).await;
        assert!(result.unwrap_err().to_string().contains("render-enabled"));
    }
}
