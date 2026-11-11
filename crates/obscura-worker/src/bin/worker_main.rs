//! Standalone executable for the isolated runtime worker.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // On Linux, ensure this worker dies if its parent (agent/supervisor) dies.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(1, 9); // PR_SET_PDEATHSIG = 1, SIGKILL = 9
        if libc::getppid() == 1 {
            std::process::exit(0);
        }
    }

    // Signal readiness to the supervisor before entering the request loop.
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{{\"ready\":true}}");
    let _ = std::io::stdout().flush();

    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();

    obscura_worker::run_worker_loop().await;
}
