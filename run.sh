#!/bin/sh
export RUSTFLAGS="
    -C link-arg=-fuse-ld=mold
    -C link-args=-Wl,--gc-sections,--as-needed
"
killall -9 cc_proxy pga_demo cargo timeout claude_proxy_rs >/dev/null 2>&1

cargo watch -x "r" | tee target/app_log.txt 2>&1
