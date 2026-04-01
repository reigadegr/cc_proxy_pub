#!/bin/bash

unset RUSTFLAGS

export RUSTFLAGS="
    -Z mir-opt-level=2
    -Z dylib-lto=yes
    -Z inline-mir=yes
    -Z share-generics=yes
    -Z remap-cwd-prefix=.
    -Z function-sections=yes
    -Z dep-info-omit-d-target
    -Z flatten-format-args=yes
    -Z saturating-float-casts=yes
    -Z mir-enable-passes=+Inline
    -Z precise-enum-drop-elaboration=yes
    -C relro-level=full
    -C default-linker-libraries
    -C code-model=small
    -C relocation-model=pie
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

cargo +nightly build -r --target "$1" -Z build-std=core,alloc,std,panic_abort --bin "$2" --all-features
