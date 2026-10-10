## Linux x86_64

```bash
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-linux.tar.gz
tar xzf obscura-x86_64-linux.tar.gz
./obscura --version
```

## Linux ARM64

```bash
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-aarch64-linux.tar.gz
tar xzf obscura-aarch64-linux.tar.gz
./obscura --version
```

Linux builds target Ubuntu 22.04 and require glibc 2.35+.

## macOS Apple Silicon

```bash
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-aarch64-macos.tar.gz
tar xzf obscura-aarch64-macos.tar.gz
./obscura --version
```

## macOS Intel

```bash
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-macos.tar.gz
tar xzf obscura-x86_64-macos.tar.gz
./obscura --version
```

## Windows

Download the `.zip` from [Releases](https://github.com/h4ckf0r0day/obscura/releases), extract, run `obscura.exe --version`.

## Arch Linux (AUR)

```bash
yay -S obscura-browser
```

## Docker

```bash
docker run -d --name obscura -p 127.0.0.1:9222:9222 h4ckf0r0day/obscura
```

Image: [h4ckf0r0day/obscura](https://hub.docker.com/r/h4ckf0r0day/obscura). Built on `distroless/cc:nonroot`, with no shell or package manager in the runtime image, running as uid 65532. Note the `-p 127.0.0.1:...` above: it publishes the port to host loopback only. A mounted `--storage-dir` must be writable by uid 65532 — see [Run in production at scale](Run-in-production-at-scale.md#the-container-does-not-run-as-root).

Official archives and the Docker image include the rendering engine. Source
builders must pass `--features render`; see [Build from source](Build-from-source.md).

## From source

See [Build from source](Build-from-source.md).

## What's in the archive

- `obscura`: CLI and CDP server.
- `obscura-worker`: helper for the parallel `scrape` command. Keep both in the same directory.

Archive suffixes identify the feature set: no suffix includes rendering,
`-stealth` includes rendering and stealth, `-no-render` includes neither, and
`-no-render-stealth` includes stealth without rendering.

## Smoke test

```bash
./obscura fetch https://example.com --eval "document.title"
./obscura fetch https://example.com --screenshot smoke.png
```

Expected output: `"Example Domain"`, followed by a nonempty PNG at `smoke.png`.

## Troubleshooting

`cannot execute binary file`: wrong arch. Check `uname -m`.

`GLIBC_2.35 not found`: distro is older than Ubuntu 22.04. Use Docker or build from source.

macOS Gatekeeper warning: `xattr -d com.apple.quarantine ./obscura`.
