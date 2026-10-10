#![cfg(feature = "render")]

use std::process::Command;

#[test]
fn screenshot_keeps_outset_shadow_outside_transparent_border_box() {
    let path = std::env::temp_dir().join(format!(
        "obscura-outset-shadow-{}.png",
        std::process::id()
    ));
    let url = concat!(
        "data:text/html,<html style=\"margin:0\"><body style=\"margin:0;background:white\">",
        "<div style=\"position:absolute;left:20px;top:20px;width:40px;height:30px;",
        "box-shadow:4px 4px 0 black\"></div>",
        "<div style=\"position:absolute;left:100px;top:20px;width:40px;height:30px;",
        "background:lime;box-shadow:4px 4px 0 black\"></div>",
        "</body></html>"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["fetch", url, "--screenshot"])
        .arg(&path)
        .args(["--wait", "0", "--timeout", "5", "--quiet"])
        .env("OBSCURA_SHOT_W", "160")
        .env("OBSCURA_SHOT_H", "80")
        .output()
        .expect("run obscura fetch screenshot");
    assert!(
        output.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let screenshot = image::open(&path).expect("decode screenshot PNG").to_rgba8();
    assert_eq!(screenshot.get_pixel(35, 35).0, [255, 255, 255, 255]);
    assert_eq!(screenshot.get_pixel(21, 35).0, [255, 255, 255, 255]);
    assert_eq!(screenshot.get_pixel(62, 35).0, [0, 0, 0, 255]);
    assert_eq!(screenshot.get_pixel(115, 35).0, [0, 255, 0, 255]);

    std::fs::remove_file(path).expect("remove screenshot");
}
