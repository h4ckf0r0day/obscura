//! Launches and owns the `obscura serve` engine process.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context as _, Result};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Engine {
    child: Child,
    ready_file: PathBuf,
    profile_dir: PathBuf,
    pub ws_url: String,
}

impl Engine {
    pub fn launch() -> Result<Self> {
        let binary = engine_binary()?;
        let profile_dir = profile_dir();
        std::fs::create_dir_all(&profile_dir)
            .with_context(|| format!("create profile directory {}", profile_dir.display()))?;
        let ready_file =
            std::env::temp_dir().join(format!("obscura-gui-{}.ready", std::process::id()));
        let _ = std::fs::remove_file(&ready_file);

        let mut command = Command::new(&binary);
        command
            .arg("serve")
            .args(["--port", "0", "--quiet"])
            .arg("--ready-file")
            .arg(&ready_file)
            .arg("--storage-dir")
            .arg(&profile_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command
            .spawn()
            .with_context(|| format!("start engine {}", binary.display()))?;
        let mut engine = Engine {
            child,
            ready_file,
            profile_dir,
            ws_url: String::new(),
        };
        engine.ws_url = engine.wait_until_ready()?;
        Ok(engine)
    }

    fn wait_until_ready(&mut self) -> Result<String> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!("the engine exited during startup ({status})");
            }
            if let Ok(bytes) = std::fs::read(&self.ready_file) {
                let record: serde_json::Value =
                    serde_json::from_slice(&bytes).context("parse engine ready file")?;
                let host = record["host"].as_str().unwrap_or("127.0.0.1");
                let port = record["port"].as_u64().context("ready file has no port")?;
                let path = record["websocket_path"].as_str().unwrap_or("/devtools/browser");
                return Ok(format!("ws://{host}:{port}{path}"));
            }
            if started.elapsed() > STARTUP_TIMEOUT {
                bail!("the engine did not start within {}s", STARTUP_TIMEOUT.as_secs());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The server writes cookies and localStorage when a connection closes.
    /// Give it a moment to finish after the GUI hangs up, then stop it.
    pub fn shutdown_after_disconnect(mut self) {
        let cookies = self.profile_dir.join("cookies.json");
        let before = modified(&cookies);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && modified(&cookies) == before {
            std::thread::sleep(Duration::from_millis(25));
        }
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.ready_file);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// `OBSCURA_BIN`, then the `obscura` binary next to this executable (the
/// cargo target directory or an install directory), then `PATH`.
fn engine_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("OBSCURA_BIN") {
        return Ok(PathBuf::from(path));
    }
    let name = format!("obscura{}", std::env::consts::EXE_SUFFIX);
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        let sibling = dir.join(&name);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!(
        "could not find the `{name}` engine binary. Build it with \
         `cargo build --release -p obscura-cli --features render`, or set OBSCURA_BIN."
    )
}

fn profile_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("Obscura").join("Profile")
}
