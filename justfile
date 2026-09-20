default:
    @just --list

run *args:
    cargo run --locked -p herdr-gpui -- {{args}}

format:
    cargo fmt --all

format-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --locked --workspace --all-features

# Explicit opt-in: launches and cleans up its own isolated daemon only.
test-live binary:
    HERDR_TEST_BINARY="{{binary}}" cargo test --locked -p herdr-client --test live -- --ignored --nocapture

# Opens a real native window; requires an active desktop. Uses an isolated daemon.
test-gui binary:
    HERDR_TEST_BINARY="{{binary}}" cargo test --locked -p herdr-gpui --features integration-test --test live_gui -- --ignored --nocapture

build-release:
    cargo build --locked --release -p herdr-gpui

# Link the actual optimized application and exercise its CLI without a desktop.
test-build: build-release
    cargo test --locked --release -p herdr-gpui --test cli

ci: format-check lint test
