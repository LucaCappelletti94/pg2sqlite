# The gates Rust CI enforces, runnable locally as one command. The doc gate
# is the one that drifts out of local habit: RUSTDOCFLAGS with -D warnings
# rejects a doc link to a private item, which fmt, clippy, deny, and test all
# accept (R95). The no_std gate is the R92 job, since a std-only call on a
# primitive compiles everywhere else.

default: gates

gates: fmt clippy deny test doc no-std

fmt:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

deny:
    cargo deny check

test:
    cargo test --release --all-features

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items

no-std:
    cargo check --no-default-features --target wasm32-unknown-unknown
