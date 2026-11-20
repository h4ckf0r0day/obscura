use std::process::Command;

// The obscura-net tests prove each TLS client honors the env var. This pins
// the CLI side: the global flag reaches it before or after the subcommand,
// and nothing turns verification off by default.
const URL: &str = "data:text/html,ok";
const WARNING: &str = "TLS certificate verification is disabled";

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_obscura"));
    command.env_remove("OBSCURA_IGNORE_CERTIFICATE_ERRORS");
    command.env_remove("RUST_LOG");
    command
}

fn fetch_stderr(command: &mut Command) -> String {
    let output = command.output().expect("run obscura fetch");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "obscura fetch failed: {stderr}");
    stderr
}

#[test]
fn certificate_verification_stays_on_by_default() {
    let stderr = fetch_stderr(cli().args(["fetch", URL, "--dump", "text"]));
    assert!(!stderr.contains(WARNING), "{stderr}");
}

#[test]
fn flag_disables_verification_before_or_after_the_subcommand() {
    for args in [
        &["--ignore-certificate-errors", "fetch", URL, "--dump", "text"][..],
        &["fetch", URL, "--dump", "text", "--ignore-certificate-errors"][..],
    ] {
        let stderr = fetch_stderr(cli().args(args));
        assert!(stderr.contains(WARNING), "{args:?}: {stderr}");
    }
}

#[test]
fn env_var_disables_verification() {
    let stderr = fetch_stderr(
        cli()
            .args(["fetch", URL, "--dump", "text"])
            .env("OBSCURA_IGNORE_CERTIFICATE_ERRORS", "1"),
    );
    assert!(stderr.contains(WARNING), "{stderr}");
}
