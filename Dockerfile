FROM rust:1.91-slim-bookworm AS builder

WORKDIR /build
COPY services/orders/Cargo.toml services/orders/Cargo.lock ./
COPY services/orders/src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

ENV PORT=8080
COPY --from=builder /build/target/release/orders /usr/local/bin/orders

USER 10001:10001
EXPOSE 8080

HEALTHCHECK --interval=5s --timeout=3s --start-period=3s --retries=5 \
    CMD curl --fail --silent --show-error --max-time 2 "http://127.0.0.1:${PORT}/health" || exit 1

CMD ["orders"]
