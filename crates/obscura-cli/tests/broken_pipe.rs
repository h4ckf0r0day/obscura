use std::process::{Command, Stdio};

#[test]
fn fetch_accepts_a_closed_output_pipe() {
    let path = std::env::temp_dir().join(format!("obscura-closed-pipe-{}.html", std::process::id()));
    std::fs::write(&path, "<p>hello</p>").expect("fixture");
    let url = url::Url::from_file_path(&path).expect("file URL");
    for mode in ["text", "html", "markdown", "original", "eval"] {
        let (reader, writer) = std::io::pipe().expect("stdout pipe");
        // Close before spawning, so even a small first write must see EPIPE.
        drop(reader);
        let mut command = Command::new(env!("CARGO_BIN_EXE_obscura"));
        command.args(["fetch", url.as_str(), "--quiet"]);
        if mode == "eval" {
            command.args(["--eval", "'hello'"]);
        } else {
            command.args(["--dump", mode]);
        }
        let output = command.stdout(writer).stderr(Stdio::piped()).output().expect("fetch");
        assert!(output.status.success(), "{mode}: {}", String::from_utf8_lossy(&output.stderr));
        assert!(output.stderr.is_empty(), "{mode}: {}", String::from_utf8_lossy(&output.stderr));
    }
    std::fs::remove_file(path).expect("remove fixture");
}

#[cfg(target_os = "linux")]
#[test]
fn fetch_preserves_other_output_errors() {
    let path = std::env::temp_dir().join(format!("obscura-full-output-{}.html", std::process::id()));
    std::fs::write(&path, "<p>hello</p>").expect("fixture");
    let url = url::Url::from_file_path(&path).expect("file URL");
    for mode in ["text", "original"] {
        let output = Command::new(env!("CARGO_BIN_EXE_obscura"))
            .args(["fetch", url.as_str(), "--quiet", "--dump", mode])
            .stdout(std::fs::OpenOptions::new().write(true).open("/dev/full").expect("full device"))
            .output().expect("fetch");
        assert!(!output.status.success(), "{mode}: a full output device must fail");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!error.contains("panicked"), "{mode}: {error}");
        assert!(error.contains("stdout"), "{mode}: {error}");
    }
    std::fs::remove_file(path).expect("remove fixture");
}
