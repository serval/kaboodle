# List available recipes
help:
    just -l

# Run a single copy of the app
@run:
    cargo run

# Run four copies of the app in a 2x2 grid
@run2x2:
    cargo build && zellij --layout 2x2-layout.kdl

# Run two copies of the app in a 2x1 grid
@run2x1:
    cargo build && zellij --layout 2x1-layout.kdl

# Build all targets in debug mode
@build:
    cargo build --release --all-targets

# Build all targets in release mode
@release:
    cargo build --release --all-targets

# Build documentation for all crates
@doc *FLAGS:
    cargo doc --release --no-deps --workspace {{FLAGS}}

# Run the same checks we run in CI
@ci: test
    cargo clippy
    cargo fmt --check
    cargo deny check licenses

# Get security advisories from cargo-deny
@security:
    cargo deny check advisories

# Run tests with nextest
@test:
    cargo nextest run --all-targets

# Lint and automatically fix what we can fix
@fix:
    cargo clippy --fix --allow-dirty --allow-staged
    cargo fmt

# Install required linting/testing tools via cargo.
@install-tools:
    cargo install cargo-nextest
    cargo install cargo-deny

# Check for unused dependencies.
check-unused:
    cargo +nightly udeps --all

set positional-arguments
tailscale *args:
    #!/usr/bin/env bash
    set -e
    if [ $(uname) == "Darwin" ]; then
        echo "Kaboodle doesn't currently work properly on a Mac when run against Tailscale using IPv6; aborting."
        exit 1
    fi

    if ! hash tailscale 2>/dev/null; then
        echo This command requires the Tailscale CLI tool to be installed:
        echo https://tailscale.com/kb/1080/cli/
        exit 1
    fi

    ADDR=$(tailscale ip --6 2>/dev/null)
    if [ "${ADDR}" == "" ]; then
        echo Tailscale does not have an IPv6 address; aborting.
        exit 1
    fi

    cargo run -- --interface "${ADDR}" "$@"

