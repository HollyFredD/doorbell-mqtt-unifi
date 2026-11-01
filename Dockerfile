# Build stage
FROM rust:1.88-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY web/ ./web/

ARG APP_VERSION=0.0.0-dev
RUN APP_VERSION="${APP_VERSION}" cargo build --release --locked

# Final stage
FROM debian:bookworm

RUN apt-get update && apt-get install -y \
    libssl3 \
    ca-certificates \
    gosu \
    jq \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/doorbell-mqtt-unifi /app/doorbell-mqtt-unifi

COPY config.example.json /defaults/config.json

COPY test-profile/ /app/test-profile/

COPY docker-entrypoint.sh /entrypoint.sh
COPY docker-config.sh /usr/local/bin/config
RUN chmod +x /entrypoint.sh /usr/local/bin/config

VOLUME ["/config", "/profiles", "/sounds"]

ENTRYPOINT ["/entrypoint.sh"]