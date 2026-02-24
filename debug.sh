#!/bin/sh

set -e

taplo fmt *.toml */*.toml */*/*.toml
export RUSTFLAGS="
    -C link-arg=-fuse-ld=mold
    -C link-args=-Wl,--gc-sections,--as-needed
"

cargo fmt --all
# 运行 clippy
cargo clippy --fix --allow-dirty --allow-staged --all --all-targets --all-features
cargo test
