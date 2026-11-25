FROM rust:1.95-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake g++ pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /app
ENV CARGO_BUILD_JOBS=4
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY schemas ./schemas
COPY sql ./sql
COPY tests ./tests
RUN cargo build --release --locked --bins
FROM build AS test
CMD ["cargo","test","--release","--locked","--","--include-ignored","--nocapture","--test-threads=1"]
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/producer /app/target/release/aggregation /usr/local/bin/
USER 10001:10001
ENV HOME=/tmp
CMD ["producer"]
