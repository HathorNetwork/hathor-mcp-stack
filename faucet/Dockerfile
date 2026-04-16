# Multi-stage build for hathor-faucet
FROM rust:1-bookworm AS builder

WORKDIR /app

# Pre-cache deps
COPY Cargo.toml ./
RUN mkdir -p src && echo "fn main() {}" > src/main.rs && \
    mkdir -p static && echo "<!doctype html><title>placeholder</title>" > static/index.html && \
    cargo build --release && \
    rm -rf src target/release/deps/hathor_faucet* target/release/hathor-faucet*

# Real build
COPY src ./src
COPY static ./static
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/hathor-faucet /usr/local/bin/hathor-faucet

EXPOSE 8200

ENTRYPOINT ["hathor-faucet"]
