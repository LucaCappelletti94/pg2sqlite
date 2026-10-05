#!/bin/bash
set -eu

cd "$SRC/pg2sqlite"
# the base image exports its own nightly as RUSTUP_TOOLCHAIN, so no toolchain is named here
cargo fuzz build -O --debug-assertions --fuzz-dir fuzz

targets=$(cargo fuzz list --fuzz-dir fuzz)
if [[ -z "$targets" ]]; then
    echo "cargo fuzz list named no target" >&2
    exit 1
fi

target_dir=fuzz/target/x86_64-unknown-linux-gnu/release
for name in $targets; do
    cp "$target_dir/$name" "$OUT/"
    # a fresh engine per input fragments the ASAN heap past the default ceiling
    printf '[libfuzzer]\ntimeout = 15\nmax_len = 65536\nrss_limit_mb = 6144\n' >"$OUT/$name.options"
    # the runner unpacks <target>_seed_corpus.zip as the starting corpus
    seeds="fuzz/seeds/$name"
    if [[ ! -d "$seeds" ]] || [[ -z "$(ls -A "$seeds")" ]]; then
        echo "fuzz target $name has no seeds in $seeds" >&2
        exit 1
    fi
    zip -qj "$OUT/${name}_seed_corpus.zip" "$seeds"/*
done
