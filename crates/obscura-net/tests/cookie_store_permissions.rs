//! The persisted cookie store holds live session tokens in
//! plaintext, and its permissions were incidental rather than asserted.
//!
//! `NamedTempFile` creates at 0600 and `persist` renames, so a *fresh*
//! `cookies.json` happened to land at 0600. Nothing checked it, nothing set it
//! when persisting over an existing file, and the containing directory kept the
//! umask default.
//!
//! Encryption at rest is out of scope — that needs a keyring dependency and a
//! policy decision. What is in scope is not relying on an accident, and saying
//! plainly in the docs that the file is plaintext.
//!
//! Unix only: Windows has no mode bits, and the release target list includes
//! `x86_64-pc-windows-msvc`, so the guards must keep it compiling there.
//!
//! Run with `cargo test -p obscura-net --test cookie_store_permissions`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use obscura_net::CookieJar;
use url::Url;

fn mode_of(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn jar_with_session() -> CookieJar {
    let jar = CookieJar::new();
    jar.set_cookie(
        "session=VICTIM-SESSION; Path=/",
        &Url::parse("https://bank.example/").unwrap(),
    );
    jar
}

#[test]
fn a_freshly_written_cookie_store_is_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cookies.json");
    jar_with_session().save_to_file(&path).unwrap();

    assert_eq!(
        mode_of(&path),
        0o600,
        "the cookie store holds plaintext session tokens"
    );
}

/// The branch most likely to be wrong, and the reason this is asserted rather
/// than assumed: persisting over a file something else created world-readable.
#[test]
fn persisting_over_a_world_readable_file_tightens_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cookies.json");
    std::fs::write(&path, "{}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(mode_of(&path), 0o644, "precondition");

    jar_with_session().save_to_file(&path).unwrap();
    assert_eq!(
        mode_of(&path),
        0o600,
        "an existing world-readable store must be tightened, not inherited"
    );
}

#[test]
fn a_directory_created_for_the_store_is_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("created-by-obscura");
    let path = nested.join("cookies.json");
    jar_with_session().save_to_file(&path).unwrap();

    assert_eq!(
        mode_of(&nested),
        0o700,
        "a directory we create for the jar should not be group/world readable"
    );
}

/// An operator who deliberately shares a storage directory keeps their choice:
/// we tighten what we create, not what we are handed.
#[test]
fn an_existing_permissive_directory_is_left_alone_if_already_private() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("operator-owned");
    std::fs::create_dir(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o700)).unwrap();

    let path = existing.join("cookies.json");
    jar_with_session().save_to_file(&path).unwrap();
    assert_eq!(mode_of(&existing), 0o700, "an already-private dir is untouched");
    assert_eq!(mode_of(&path), 0o600);
}

/// Regression: tightening permissions must not break the round trip.
#[test]
fn the_store_still_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cookies.json");
    jar_with_session().save_to_file(&path).unwrap();

    let restored = CookieJar::new();
    let count = restored.load_from_file(&path).unwrap();
    assert!(count >= 1, "expected at least one cookie back, got {count}");
    let names: Vec<String> = restored
        .get_all_cookies()
        .into_iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect();
    assert!(
        names.iter().any(|c| c == "session=VICTIM-SESSION"),
        "expected the session cookie back, got {names:?}"
    );
}
