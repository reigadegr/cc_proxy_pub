#!/bin/bash

export RUSTFLAGS="
    --cfg tokio_unstable
    -C relro-level=full
    -C code-model=small
    -C linker-plugin-lto=no
    -C relocation-model=pic
    -C symbol-mangling-version=v0
" 

export RUSTFLAGS="
    $RUSTFLAGS
    --cfg tokio_unstable
"

echo $RUSTFLAGS
# cargo update

export CARGO_TERM_COLOR=always

export JEMALLOC_SYS_DISABLE_WARN_ERROR=1

cargo +stable build -r --target "$1" --bin "$2" --all-features