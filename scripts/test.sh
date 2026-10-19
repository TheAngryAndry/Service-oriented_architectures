#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo test --locked --manifest-path services/orders/Cargo.toml
docker compose up --build -d --wait --wait-timeout 180
docker compose --profile test run --build --rm e2e
