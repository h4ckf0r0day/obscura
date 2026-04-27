# Running Obscura In A Container

Running Obscura in a container is the recommended way to browse untrusted pages. Obscura only needs outbound network access and a CDP port. `file://` URLs are denied by default. If you explicitly allow them, and you do not mount host directories, they can only read files inside the container image and its writable runtime filesystem.

## Why File Access Is Restricted

Obscura fetches page subresources itself. A malicious page could otherwise reference local files with `file://` URLs, for example from a stylesheet or script tag, and then expose the loaded bytes back to page JavaScript. The default-deny policy removes that exfiltration path for normal web browsing.

Use the file access options only for workflows that intentionally browse local fixtures:

| Option | Use |
|--------|-----|
| `--allow-file-access <DIR>` | Allow `file://` reads under one canonicalized directory. Repeat it for multiple fixture roots. Relative path segments, percent-encoded traversal, and symlink escapes are resolved before the root check. |
| `--allow-all-file-urls` | Restore unrestricted legacy `file://` reads. Use only in disposable containers or other isolated environments. |

JavaScript `fetch()` remains limited to `http` and `https` even when browser-engine `file://` access is explicitly allowed.

## Install Rust On Artix Linux

You do not need host Rust to build the container image, because the `Containerfile` uses a Rust builder image. Install Rust locally only if you want to run `cargo build` or `cargo test` outside the container.

Using Artix packages:

```bash
sudo pacman -Syu base-devel rust cargo clang cmake pkgconf
rustc --version
cargo --version
```

Using rustup:

```bash
sudo pacman -Syu base-devel curl ca-certificates clang cmake pkgconf
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
rustup default stable
rustc --version
cargo --version
```

## Build From Source

```bash
cargo build --release --bin obscura --bin obscura-worker
./target/release/obscura serve --host 127.0.0.1 --port 9222
```

Use `--host 0.0.0.0` only when Obscura is already isolated, such as inside a container, VM, or locked-down network namespace.

## Build The Container Image

With Podman:

```bash
podman build -t obscura:dev -f Containerfile .
```

With Docker:

```bash
docker build -t obscura:dev -f Containerfile .
```

## Run The CDP Server With Podman

This starts Obscura inside the container, publishes the CDP port only on host loopback, drops Linux capabilities, and makes the container root filesystem read-only.

```bash
podman run --rm \
  --name obscura \
  --read-only \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
  --cap-drop=all \
  --security-opt=no-new-privileges \
  --pids-limit=256 \
  --memory=512m \
  -p 127.0.0.1:9222:9222 \
  obscura:dev
```

## Run The CDP Server With Docker

```bash
docker run --rm \
  --name obscura \
  --read-only \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --pids-limit=256 \
  --memory=512m \
  -p 127.0.0.1:9222:9222 \
  obscura:dev
```

Check the server:

```bash
curl http://127.0.0.1:9222/json/version
```

Connect Playwright from the host:

```javascript
import { chromium } from 'playwright-core';

const browser = await chromium.connectOverCDP('http://127.0.0.1:9222');
const page = await browser.newPage();
await page.goto('https://example.com');
console.log(await page.title());
await browser.close();
```

## File Access Model

By default, Obscura rejects `file://` URLs. This applies to page navigation and subresources loaded by the browser engine.

If a workflow needs local fixtures, allow only the smallest directory:

```bash
obscura fetch \
  --allow-file-access "$PWD/fixtures" \
  "file://$PWD/fixtures/page.html"
```

Do not add volume mounts unless the pages you browse need them. With no `-v` or `--mount`, even explicitly allowed `file://` reads are limited to the container filesystem. If you need to mount local test fixtures, mount the smallest possible directory read-only and allow that container path:

```bash
podman run --rm \
  --read-only \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
  --cap-drop=all \
  --security-opt=no-new-privileges \
  -p 127.0.0.1:9222:9222 \
  -v "$PWD/fixtures:/fixtures:ro,Z" \
  obscura:dev \
  serve --host 0.0.0.0 --port 9222 --allow-file-access /fixtures
```

The equivalent Docker command is:

```bash
docker run --rm \
  --read-only \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  -p 127.0.0.1:9222:9222 \
  -v "$PWD/fixtures:/fixtures:ro" \
  obscura:dev \
  serve --host 0.0.0.0 --port 9222 --allow-file-access /fixtures
```
