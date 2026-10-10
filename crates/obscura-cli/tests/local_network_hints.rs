use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_obscura"));
    for name in [
        "OBSCURA_ALLOW_PRIVATE_NETWORK",
        "OBSCURA_PROXY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(name);
    }
    command.env("OBSCURA_STEALTH", "0");
    command
}

#[test]
fn local_cli_denials_explain_the_existing_opt_in_without_granting_access() {
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        for dump in ["text", "original"] {
            let output = cli()
                .args(["fetch", &format!("http://{host}:9/"), "--dump", dump])
                .output()
                .unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains("is not allowed"), "{error}");
            assert!(error.contains("--allow-private-network"), "{error}");
            assert!(error.contains("OBSCURA_ALLOW_PRIVATE_NETWORK=1"), "{error}");
        }
    }
}

#[test]
fn cli_metadata_denials_do_not_suggest_relaxing_the_guard() {
    for host in ["169.254.169.254", "100.100.100.200", "[fd00:ec2::254]"] {
        let output = cli()
            .args(["fetch", &format!("http://{host}/"), "--dump", "text"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("is not allowed"), "{error}");
        assert!(!error.contains("--allow-private-network"), "{error}");
    }
}

#[test]
fn stdio_mcp_denials_are_actionable_and_still_errors() {
    let mut child = cli()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"regression","version":"1"}}})).unwrap();
    for (id, url) in [(2, "http://localhost:9/"), (3, "http://169.254.169.254/")] {
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"browser_navigate","arguments":{"url":url}}})).unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3);
    for (index, hint) in [(1, true), (2, false)] {
        assert_eq!(responses[index]["result"]["isError"], true);
        let error = responses[index]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(error.contains("is not allowed"), "{error}");
        assert_eq!(error.contains("--allow-private-network"), hint, "{error}");
    }
}
