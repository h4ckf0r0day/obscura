## Requirements

- Current stable Rust ([rustup.rs](https://rustup.rs))
- C compiler (gcc or clang)
- ~5 GB free disk space (V8 compiles from source on first build)

Build time depends on the machine and cache state. Cargo reuses unchanged
dependencies, but a release source edit can still require substantial compilation
and linking. For the scoped incremental runtime workflow, see
[Fast edit/test loop](Testing-and-debugging.md#fast-edittest-loop).

## Build

```bash
git clone https://github.com/h4ckf0r0day/obscura.git
cd obscura
cargo build --release -p obscura-cli --bins --features render
```

Binary is at `./target/release/obscura`.

This produces the release binary with geometry, screenshots, screencasting,
and PDF export.

## Rendering and stealth

```bash
cargo build --release -p obscura-cli --bins --features render,stealth
```

This is the complete rendering build with the stealth wreq/BoringSSL transport,
a consistent browser TLS fingerprint, browser-identity protections, and tracker
blocklist. See [Configure stealth and proxies](Configure-stealth-and-proxies.md).

## Without rendering

```bash
cargo build --release -p obscura-cli --bins --no-default-features
cargo build --release -p obscura-cli --bins --no-default-features --features stealth
```

The second command keeps stealth while excluding layout, screenshots,
screencasting, and PDF export.

The stealth feature builds BoringSSL and generates Rust bindings. In addition
to the default requirements, install CMake, Clang, and the libclang/LLVM
development libraries. On Ubuntu/Debian:

```bash
sudo apt-get install build-essential cmake clang libclang-dev llvm-dev
```

On macOS, install the Xcode Command Line Tools and CMake. On Windows, install
the Visual Studio C++ Build Tools, CMake, and LLVM/Clang. Ensure the directory
containing `libclang` is available through `LIBCLANG_PATH` if bindgen cannot
locate it automatically.

On macOS 26 with the standalone Command Line Tools, Apple Clang may not find
libc++ while compiling BoringSSL. Use the active SDK for that build:

```bash
SDK_PATH="$(xcrun --show-sdk-path)"
SDKROOT="$SDK_PATH" CXXFLAGS="-isystem $SDK_PATH/usr/include/c++/v1" \
  cargo build --release -p obscura-cli --bins --features render,stealth
```

## OpenSSL on older systems

If the build fails on the vendored OpenSSL with an AVX-512 assembler error (common on older VPS hosts):

```bash
OPENSSL_NO_VENDOR=1 cargo build --release -p obscura-cli --bins --features render
```

Uses the system OpenSSL instead.

## Android ARM64

The supported Android target is 64-bit ARM (`aarch64-linux-android`) with a
minimum Android API level of 29. Android builds use Bionic and the NDK r26c
toolchain, and run the JavaScript bootstrap in the target runtime instead of
loading an architecture-specific startup snapshot.

The reproducible CI build is
[`.github/workflows/android-cross-build.yml`](../.github/workflows/android-cross-build.yml).
It builds V8 from the pinned `rusty_v8` source checkout and validates that the
result is an ELF64 AArch64 binary using `/system/bin/linker64`. The resulting
`obscura` executable can be run on an Android device with:

```bash
./obscura fetch https://example.com --eval "document.title"
```

If the executable imports `libc++_shared.so`, place the matching NDK library
in the same directory and run the command with `LD_LIBRARY_PATH=.`.

Android runtime-bootstrap builds do not currently create isolated child iframe
realms. An affected iframe is not presented as an empty successful document:
the page receives a visible error document, a `console.error`, and a native error
log stating that the child realm was not executed. The main document remains
usable.

## Run from the build

```bash
./target/release/obscura --version
./target/release/obscura fetch https://example.com --eval "document.title"
```

Install system-wide:

```bash
cargo install --path crates/obscura-cli --features render
```

## Tests

```bash
cargo nextest run --release --features render --no-fail-fast
```

CDP integration tests are registered in `crates/obscura-cdp/tests/integration.rs`.
Run them with:

```bash
cargo nextest run --release --features render -p obscura-cdp --test integration
```

Use `cargo nextest`, not `cargo test`: runtime tests require process isolation
to keep V8 runtime state isolated.
