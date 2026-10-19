#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
# build.rs compiles the contract into target/.../out/api.rs.
cargo build --locked --manifest-path services/orders/Cargo.toml
