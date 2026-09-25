#![cfg(feature = "render")]

use std::process::Command;

const PAGE: &str = "data:text/html,<p style=\"font-family:Liberation Mono\">fonts</p>";

#[test]
fn fetch_accepts_font_dir_for_screenshots() {
    let out = std::env::temp_dir().join(format!("obscura-fetch-font-dir-{}.png", std::process::id()));
    let fonts = concat!(env!("CARGO_MANIFEST_DIR"), "/../obscura-render/assets");
    let status = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["fetch", PAGE, "--font-dir", fonts, "--screenshot"])
        .arg(&out)
        .args(["--wait", "0", "--timeout", "10", "--quiet"])
        .output()
        .expect("run obscura fetch");
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    let png = std::fs::read(&out).expect("screenshot written");
    assert!(png.starts_with(b"\x89PNG"));
    let _ = std::fs::remove_file(&out);
}

#[test]
fn fetch_rejects_a_missing_font_dir() {
    let status = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["fetch", PAGE, "--font-dir", "/definitely/not/a/font/dir", "--quiet"])
        .output()
        .expect("run obscura fetch");
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("Font directory does not exist"));
}
