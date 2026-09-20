default:
    @just --list

run *args:
    cargo run --locked --release -p herdr-gpui -- {{args}}

run-debug *args:
    cargo run --locked -p herdr-gpui -- {{args}}

format:
    cargo fmt --all

format-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --locked --workspace
    cargo test --locked --workspace --all-features

# Explicit opt-in: launches and cleans up its own isolated daemon only.
test-live binary:
    HERDR_TEST_BINARY="{{binary}}" cargo test --locked -p herdr-client --test live -- --ignored --nocapture

# Opens a real native window; requires an active desktop. Uses an isolated daemon.
test-gui binary:
    HERDR_TEST_BINARY="{{binary}}" cargo test --locked -p herdr-gpui --features integration-test --test live_gui -- --ignored --nocapture --test-threads=1

# Native font/glyph regression across repeated frames and sizes; no daemon needed.
test-sidebar:
    cargo test --locked -p herdr-gpui --features integration-test --test live_gui native_sidebar -- --ignored --nocapture

# Native hover/scroll CPU scene budget in milliseconds, calibrated for this machine.
test-perf budget="30":
    cargo build --locked --release -p herdr-gpui --features integration-test
    HERDR_PERF_P95_MS="{{budget}}" target/release/herdr-gpui --performance-test

build-release:
    cargo build --locked --release -p herdr-gpui

# Regenerate the checked-in artwork on macOS; no third-party image tools required.
icons:
    swift scripts/generate-icons.swift

# Local, unsigned GUI-only bundle. Never installs or packages a daemon.
bundle:
    test "$(uname -s)" = Darwin
    cargo build --locked --release --target-dir target -p herdr-gpui
    mkdir -p target/release/Herdr.app/Contents/MacOS target/release/Herdr.app/Contents/Resources
    cp target/release/herdr-gpui target/release/Herdr.app/Contents/MacOS/Herdr
    cp assets/macos/Info.plist target/release/Herdr.app/Contents/Info.plist
    cp assets/icons/Herdr.icns target/release/Herdr.app/Contents/Resources/Herdr.icns
    plutil -lint target/release/Herdr.app/Contents/Info.plist

# Keyless native updater preview; no feed, real update checks, or installation.
bundle-updater-preview: bundle
    #!/usr/bin/env bash
    set -euo pipefail
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    bash scripts/fetch-sparkle.sh "$work/sparkle"
    mkdir -p target/release/Herdr.app/Contents/Frameworks
    ditto "$work/sparkle/Sparkle.framework" target/release/Herdr.app/Contents/Frameworks/Sparkle.framework
    cp "$work/sparkle/LICENSE" target/release/Herdr.app/Contents/Resources/Sparkle-LICENSE

# Link the actual optimized application and exercise its CLI without a desktop.
test-build: build-release
    cargo test --locked --release -p herdr-gpui --test cli

ci: format-check lint test

# Audit the GitHub workflows for injection, over-broad permissions, and unpinned actions.
audit-workflows:
    zizmor .github/

# License, advisory, and source checks over the dependency graph.
audit-deps:
    cargo deny check

# Sign published release artifacts with the local GPG key (YubiKey). Not a CI step.
sign-release *args:
    ./scripts/gpg-sign-release.sh {{args}}

# Verify a published release's checksums and GPG signatures.
verify-release *args:
    ./scripts/verify-release.sh {{args}}
