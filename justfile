# All commands for building, testing and running corral.

# List available commands
default:
    @just --list

# Build all crates
build:
    cargo build

# Run all tests
test:
    cargo test

# Lint (fmt check + clippy)
lint:
    cargo fmt --check
    cargo clippy -- -D warnings

# Format code
fmt:
    cargo fmt

# Build the release artifacts via nix
nix-build:
    nix build

# Watch: recompile and re-run tests on change
watch:
    cargo watch -x test

# Run the manager (live session list)
manager *ARGS:
    cargo run -p corral -- {{ARGS}}

# Wrap an ACP-mode agent command, e.g.: just wrap -- claude-agent-acp
wrap *ARGS:
    cargo run -p agentwrap -- {{ARGS}}
