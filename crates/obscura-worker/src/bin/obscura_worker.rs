//! Standalone executable alias for the isolated runtime worker.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();

    obscura_worker::run_worker_loop().await;
}
