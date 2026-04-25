FROM docker.io/rust:1-bookworm AS builder

WORKDIR /src
COPY . .

RUN cargo build --release --bin obscura --bin obscura-worker

FROM docker.io/debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --gid nogroup --home-dir /nonexistent --shell /usr/sbin/nologin obscura

COPY --from=builder /src/target/release/obscura /usr/local/bin/obscura
COPY --from=builder /src/target/release/obscura-worker /usr/local/bin/obscura-worker

USER 10001:65534
EXPOSE 9222

ENTRYPOINT ["obscura"]
CMD ["serve", "--host", "0.0.0.0", "--port", "9222"]
