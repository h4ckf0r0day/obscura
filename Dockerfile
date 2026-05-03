# Step 1: Use the latest stable Rust version
FROM rust:1.95-slim AS builder

# Step 2: Install system dependencies
RUN apt-get update && apt-get install -y \
    git \
    python3 \
    build-essential \
    cmake \
    clang \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Step 3: Copy local source instead of cloning from GitHub
WORKDIR /usr/src/obscura
COPY . .

# Step 4: Build from source
RUN cargo build --release --features stealth

# Step 5: Use Debian Trixie (Matches GLIBC 2.39/2.40)
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/obscura/target/release/obscura /usr/local/bin/obscura

# Step 6: Set the default command
# --host 0.0.0.0 is required so the server binds on all interfaces inside Docker,
# allowing the host to reach it through the published port (-p 9222:9222).
ENTRYPOINT ["obscura"]
CMD ["serve", "--port", "9222", "--host", "0.0.0.0"]
