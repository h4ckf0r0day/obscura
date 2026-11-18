use std::env;

fn env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_version(version: String) -> String {
    version.strip_prefix('v').unwrap_or(&version).to_string()
}

fn github_tag_version() -> Option<String> {
    if env_value("GITHUB_REF_TYPE").as_deref() == Some("tag") {
        return env_value("GITHUB_REF_NAME").map(normalize_version);
    }

    env_value("GITHUB_REF")
        .and_then(|value| value.strip_prefix("refs/tags/").map(str::to_owned))
        .map(normalize_version)
}

/// Source tarballs (GitHub's `/archive/refs/tags/vX.Y.Z.tar.gz`, which Homebrew
/// and nixpkgs build from) have neither git metadata nor GITHUB_* env, so the
/// tag is otherwise unknown. `.git_archival.txt` is marked `export-subst`, so
/// `git archive` (and GitHub's tarball endpoint) expands its placeholder to the
/// tag; in a git checkout the placeholder stays unexpanded and is ignored.
fn archive_version() -> Option<String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").ok()?;
    let path = std::path::Path::new(&manifest_dir).join("../../.git_archival.txt");
    if !path.exists() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    std::fs::read_to_string(&path)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("describe-name:"))
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.contains("$Format"))
        .map(|value| normalize_version(value.to_string()))
}

fn git(args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|out| out.trim().to_string())
        .filter(|out| !out.is_empty())
}

/// Cargo reruns a build script that prints any `rerun-if` line only when one
/// of those inputs changes, so the commit in the version went stale after a
/// commit or a checkout. Watch the files git rewrites when HEAD moves.
/// `--git-path` also resolves them in a linked worktree.
fn rerun_when_head_moves() {
    let mut names = vec!["HEAD".to_string(), "packed-refs".to_string(), "logs/HEAD".to_string()];
    names.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for name in names {
        // A path that does not exist would rerun the script on every build.
        // The reflog covers a branch whose ref is packed until its next commit.
        if let Some(path) = git(&["rev-parse", "--git-path", &name]).filter(|path| std::path::Path::new(path).exists()) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

/// Local builds are not cut from a tag, so the bare workspace version cannot
/// tell two checkouts apart. Append the commit when git is there to answer;
/// tarball and minimal-image builds without git keep the bare version.
fn local_version() -> String {
    let package = env::var("CARGO_PKG_VERSION").expect("Cargo sets CARGO_PKG_VERSION");
    match git(&["rev-parse", "--short", "HEAD"]) {
        Some(sha) => {
            rerun_when_head_moves();
            format!("{package}-dev+{sha}")
        }
        None => package,
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=OBSCURA_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    println!("cargo:rerun-if-env-changed=GITHUB_REF");

    let version = env_value("OBSCURA_VERSION")
        .map(normalize_version)
        .or_else(github_tag_version)
        .or_else(archive_version)
        .unwrap_or_else(local_version);

    println!("cargo:rustc-env=OBSCURA_BUILD_VERSION={version}");
}
