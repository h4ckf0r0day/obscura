use std::process::Command;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let mut child = Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn sleep");

    let pid = child.id();
    println!("PARENT_PID={}", std::process::id());
    println!("CHILD_PID={}", pid);

    tokio::time::sleep(Duration::from_millis(100)).await;

    let _ = child.kill();
    let _ = child.wait();
    println!("CHILD_EXITED");
    println!("REAPED");
}
