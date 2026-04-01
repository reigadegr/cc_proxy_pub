#!/bin/bash

unset RUSTFLAGS

export RUSTFLAGS="
    -C relro-level=full
    -C code-model=small
    -C default-linker-libraries 
    -C link-arg=-fuse-ld=mold
    -C symbol-mangling-version=v0
    -C llvm-args=-fp-contract=off
    -C llvm-args=-enable-misched
    -C llvm-args=-enable-post-misched
    -C llvm-args=-enable-dfa-jump-thread
    -C link-arg=-Wl,--no-rosegment
    -C link-arg=-Wl,--sort-section=alignment
    -C link-args=-Wl,-O3,--gc-sections,--as-needed
    -C link-args=-Wl,-x,-z,noexecstack,--pack-dyn-relocs=relr,-s,--strip-all,--relax
"
    
echo $RUSTFLAGS

export CARGO_TERM_COLOR=always

export JEMALLOC_SYS_DISABLE_WARN_ERROR=1

cargo +stable build -r --target "$1" --bin "$2" --all-features
