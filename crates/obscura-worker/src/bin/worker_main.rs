//! Standalone executable for the isolated runtime worker.
//! Communicates via framed JSON over stdin/stdout.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::info;

use obscura_worker::{dispatch_worker_command, SessionManager, WorkerCommand, WorkerRequest};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();

    let manager = SessionManager::new();
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    info!("Obscura Next Isolated Runtime Worker starting up");

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("Worker stdin read error: {e}");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: WorkerRequest = match serde_json::from_str(trimmed) {
            Ok(req) => req,
            Err(e) => {
                let err_resp = obscura_worker::WorkerResponse::error(
                    None,
                    format!("Invalid JSON request: {e}"),
                );
                let mut out = serde_json::to_string(&err_resp).unwrap();
                out.push('\n');
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        let is_shutdown = matches!(request.command, WorkerCommand::Shutdown);
        let resp = dispatch_worker_command(&manager, request).await;

        let mut out = serde_json::to_string(&resp).unwrap();
        out.push('\n');
        let _ = stdout.write_all(out.as_bytes()).await;
        let _ = stdout.flush().await;

        if is_shutdown {
            break;
        }
    }

    info!("Obscura Next Isolated Runtime Worker shutting down cleanly");
}
