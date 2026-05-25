FROM rust:1.78-bookworm AS builder

WORKDIR /app

# Cache dependencies layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release 2>&1
RUN rm -rf src

COPY src/ ./src/
# Touch main.rs so cargo rebuilds the binary
RUN touch src/main.rs && cargo build --release 2>&1

# ── Runtime image ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/executor /usr/local/bin/executor

CMD ["executor"]
