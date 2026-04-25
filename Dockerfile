FROM rust:1.88-bookworm AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        libclang-dev \
        pkg-config \
        python3 \
    && rm -rf /var/lib/apt/lists/*

COPY . .

RUN cargo build --release -p obscura-cli

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/obscura /usr/local/bin/obscura
COPY --from=builder /app/target/release/obscura-worker /usr/local/bin/obscura-worker

EXPOSE 9222

ENTRYPOINT ["/usr/local/bin/obscura"]
CMD ["serve", "--host", "0.0.0.0", "--port", "9222"]
