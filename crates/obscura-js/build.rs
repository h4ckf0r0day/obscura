use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=js/bootstrap.js");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let snapshot_path = out_dir.join("OBSCURA_SNAPSHOT.bin");

    // Detect cross-compilation: build script runs on host, target differs.
    let target = std::env::var("TARGET").unwrap_or_default();
    let host = std::env::var("HOST").unwrap_or_default();
    let is_cross = target != host;

    let bootstrap_js = include_str!("js/bootstrap.js");

    if is_cross {
        // Cross-compilation: the host V8 (linked into this build script) produces
        // snapshots for the host architecture, which crash on the target when
        // deserializing due to architecture-specific heap layout differences.
        //
        // Solution: use the target-specific `mksnapshot` tool (built by V8's GN
        // build system) which runs on the host but generates snapshots for the
        // target architecture. Then wrap the raw blob in deno_core's format.
        eprintln!("[obscura-js build.rs] Cross-compilation detected (host={host}, target={target})");
        eprintln!("[obscura-js build.rs] Using cross-mksnapshot for target-compatible snapshot");

        // Locate the cross-mksnapshot binary.
        // OUT_DIR is under target/<triple>/<profile>/build/obscura-js-*/out,
        // so the GN output directory is 4 levels up.
        let target_dir = out_dir
            .parent() // build/obscura-js-*/
            .and_then(|p| p.parent()) // build/
            .and_then(|p| p.parent()) // <profile>/
            .and_then(|p| p.parent()) // <triple>/
            .expect("Cannot determine target directory from OUT_DIR");

        let profile = if std::env::var("PROFILE").unwrap_or_default() == "release" {
            "release"
        } else {
            "debug"
        };

        // The cross-mksnapshot lives in gn_out/clang_x64_v8_arm64/ (or similar).
        // Try known paths.
        let cross_mksnapshot = find_cross_mksnapshot(target_dir, &target, profile);
        let cross_mksnapshot = match cross_mksnapshot {
            Some(p) => {
                eprintln!("[obscura-js build.rs] Found cross-mksnapshot: {}", p.display());
                p
            }
            None => {
                // Fallback: use host V8 (will produce wrong-arch snapshot).
                eprintln!("[obscura-js build.rs] WARNING: cross-mksnapshot not found, falling back to host V8 snapshot");
                eprintln!("[obscura-js build.rs] The snapshot may crash on the target device!");
                generate_host_snapshot(bootstrap_js, &snapshot_path);
                return;
            }
        };

        // Step 1: Generate a host snapshot to extract the deno_core sidecar data.
        // The sidecar contains only fixed-size types (i32, u32, String, Vec) so it
        // is architecture-independent and can be reused with the target blob.
        eprintln!("[obscura-js build.rs] Extracting deno_core sidecar from host snapshot...");
        let host_output = deno_core::snapshot::create_snapshot(
            deno_core::snapshot::CreateSnapshotOptions {
                cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
                startup_snapshot: None,
                skip_op_registration: true,
                extensions: vec![],
                extension_transpiler: None,
                with_runtime_cb: Some(Box::new(move |runtime| {
                    runtime
                        .execute_script("<obscura:bootstrap>", bootstrap_js.to_string())
                        .expect("bootstrap.js should not fail during snapshot creation");
                })),
            },
            None,
        )
        .expect("Failed to create host snapshot for sidecar extraction");

        // Decompose the host snapshot: [v8_blob][sidecar][8-byte v8_blob_len]
        let host_data = &*host_output.output;
        let ulen = std::mem::size_of::<usize>();
        let v8_blob_len = usize::from_le_bytes(
            host_data[host_data.len() - ulen..]
                .try_into()
                .unwrap(),
        );
        let sidecar = &host_data[v8_blob_len..host_data.len() - ulen];

        // Step 2: Run cross-mksnapshot to generate the target-architecture raw blob.
        eprintln!("[obscura-js build.rs] Running cross-mksnapshot for target architecture...");
        let raw_blob_path = out_dir.join("cross_raw_blob.bin");
        let bootstrap_path = out_dir.join("bootstrap.js");
        std::fs::write(&bootstrap_path, bootstrap_js).expect("Failed to write bootstrap.js");

        let status = Command::new(&cross_mksnapshot)
            .arg(format!("--startup-blob={}", raw_blob_path.display()))
            .arg("--target_arch=arm64")
            .arg("--target_os=android")
            .arg(&bootstrap_path)
            .status()
            .expect("Failed to run cross-mksnapshot");
        assert!(status.success(), "cross-mksnapshot failed with status: {status}");

        let mut raw_blob = std::fs::read(&raw_blob_path).expect("Failed to read cross-mksnapshot output");

        // Step 3: Patch the snapshot version in the raw blob header.
        // mksnapshot produces version 1 (no external refs), but the target V8
        // (built with SnapshotCreator) expects version 2 (with external refs).
        // The actual serialized heap data is compatible; only the version tag differs.
        let expected_version: u32 = u32::from_le_bytes(
            host_data[0..4].try_into().unwrap(),
        );
        let raw_version: u32 = u32::from_le_bytes(
            raw_blob[0..4].try_into().unwrap(),
        );
        if raw_version != expected_version {
            eprintln!(
                "[obscura-js build.rs] Patching snapshot version: {} -> {}",
                raw_version, expected_version
            );
            raw_blob[0..4].copy_from_slice(&expected_version.to_le_bytes());
        }

        // Step 4: Combine the patched raw blob with the extracted sidecar.
        let mut combined = Vec::with_capacity(raw_blob.len() + sidecar.len() + ulen);
        combined.extend_from_slice(&raw_blob);
        combined.extend_from_slice(sidecar);
        combined.extend_from_slice(&(raw_blob.len() as u64).to_le_bytes());

        std::fs::write(&snapshot_path, &combined).expect("Failed to write cross-compiled snapshot");
        eprintln!(
            "[obscura-js build.rs] Cross-compiled snapshot written: {} bytes (v8_blob={}, sidecar={})",
            combined.len(),
            raw_blob.len(),
            sidecar.len()
        );

        println!(
            "cargo:rustc-env=OBSCURA_SNAPSHOT_PATH={}",
            snapshot_path.display()
        );

        // Mark bootstrap.js as a dependency.
        println!("cargo:rerun-if-changed=js/bootstrap.js");
    } else {
        // Native build: use the host V8 directly (snapshot matches the target).
        generate_host_snapshot(bootstrap_js, &snapshot_path);
    }
}

/// Generate a snapshot using the host V8 (for native builds or as fallback).
fn generate_host_snapshot(bootstrap_js: &str, snapshot_path: &PathBuf) {
    let bootstrap_js = bootstrap_js.to_string();
    let output = deno_core::snapshot::create_snapshot(
        deno_core::snapshot::CreateSnapshotOptions {
            cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
            startup_snapshot: None,
            skip_op_registration: true,
            extensions: vec![],
            extension_transpiler: None,
            with_runtime_cb: Some(Box::new(move |runtime| {
                runtime
                    .execute_script("<obscura:bootstrap>", bootstrap_js.to_string())
                    .expect("bootstrap.js should not fail during snapshot creation");
            })),
        },
        None,
    )
    .expect("Failed to create V8 snapshot");

    std::fs::write(&snapshot_path, &*output.output).expect("Failed to write snapshot");
    println!(
        "cargo:rustc-env=OBSCURA_SNAPSHOT_PATH={}",
        snapshot_path.display()
    );

    for file in &output.files_loaded_during_snapshot {
        println!("cargo:rerun-if-changed={}", file.display());
    }
}

/// Find the cross-mksnapshot binary for the target architecture.
fn find_cross_mksnapshot(target_dir: &std::path::Path, target: &str, profile: &str) -> Option<PathBuf> {
    let gn_out = target_dir.join(profile).join("gn_out");

    // Determine the cross directory name based on host and target architectures.
    // Common patterns: clang_x64_v8_arm64, clang_x64_v8_arm, etc.
    let host_arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };

    let target_v8_arch = if target.contains("aarch64") || target.contains("arm64") {
        "arm64"
    } else if target.contains("x86_64") || target.contains("x64") {
        "x64"
    } else if target.contains("arm") {
        "arm"
    } else if target.contains("x86") {
        "x86"
    } else {
        "unknown"
    };

    // Try the cross directory first (e.g., clang_x64_v8_arm64)
    let cross_dir_name = format!("clang_{host_arch}_v8_{target_v8_arch}");
    let cross_mksnapshot = gn_out.join(&cross_dir_name).join("mksnapshot");
    if cross_mksnapshot.exists() {
        return Some(cross_mksnapshot);
    }

    // Try without the host arch prefix (e.g., just clang_x64_v8_arm64)
    for entry in std::fs::read_dir(&gn_out).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("clang_") && name_str.contains(&format!("v8_{target_v8_arch}")) {
            let candidate = entry.path().join("mksnapshot");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}
