FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
        perl \
        make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation by copying manifests first
COPY Cargo.toml Cargo.lock ./
COPY vendor/ vendor/
COPY crates/obscura-dom/Cargo.toml       crates/obscura-dom/Cargo.toml
COPY crates/obscura-net/Cargo.toml       crates/obscura-net/Cargo.toml
COPY crates/obscura-browser/Cargo.toml   crates/obscura-browser/Cargo.toml
COPY crates/obscura-cdp/Cargo.toml       crates/obscura-cdp/Cargo.toml
COPY crates/obscura-js/Cargo.toml        crates/obscura-js/Cargo.toml
COPY crates/obscura-mcp/Cargo.toml       crates/obscura-mcp/Cargo.toml
COPY crates/obscura-render/Cargo.toml    crates/obscura-render/Cargo.toml
COPY crates/obscura-cli/Cargo.toml       crates/obscura-cli/Cargo.toml
COPY crates/obscura/Cargo.toml           crates/obscura/Cargo.toml

# Create stub src files so cargo can resolve the dependency graph
RUN for crate in obscura-dom obscura-net obscura-browser obscura-cdp obscura-js obscura-mcp obscura-render obscura; do \
        mkdir -p crates/$crate/src && echo "// stub" > crates/$crate/src/lib.rs; \
    done && \
    mkdir -p crates/obscura-cli/src && \
    echo "fn main() {}" > crates/obscura-cli/src/main.rs && \
    echo "fn main() {}" > crates/obscura-cli/src/worker.rs

RUN cargo build --release --features render --bin obscura --bin obscura-worker 2>/dev/null || true

ARG OBSCURA_VERSION

# Copy real sources and build
COPY crates/ crates/
RUN echo "Building Obscura version ${OBSCURA_VERSION:-from Cargo.toml}" && \
    touch crates/*/src/*.rs && cargo build --release --features render --bin obscura --bin obscura-worker

# ---

# distroless/cc: glibc + libgcc + CA certs only — no shell, no package manager.
#
# `:nonroot` runs as uid/gid 65532 instead of root. Obscura executes untrusted
# page JavaScript in-process through V8, so a V8 exploit lands with the
# process's privileges; there is no reason for those to be root's. The image
# needs no privileged operation: it binds an unprivileged port, reads the CA
# bundle, and writes only to the storage dir and a temp dir.
#
# The tag is deliberately not pinned to a digest. distroless is rebuilt often
# with base-layer security patches, and tracking the tag picks those up; a
# digest pin would freeze them until someone remembers to bump it, which for a
# *base* image trades a real ongoing risk for a theoretical one.
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /build/target/release/obscura /obscura
COPY --from=builder /build/target/release/obscura-worker /obscura-worker

EXPOSE 9222

# Bind to 0.0.0.0 *inside the container*: a container-loopback bind is
# unreachable through `-p`, so this is required for the port to work at all.
# It is not a licence to expose the port — publish to host loopback
# (`-p 127.0.0.1:9222:9222`) unless something in front of it enforces auth.
# The native binary still defaults to 127.0.0.1.
#
# The CDP control plane has no authentication: anything that can reach this
# port can drive the browser. Network isolation is the control.
ENTRYPOINT ["/obscura"]
CMD ["serve", "--port", "9222", "--host", "0.0.0.0"]
