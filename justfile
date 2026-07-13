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

# Run the attention board
board *ARGS:
    cargo run -p corral -- {{ARGS}}

# Run the message-routing daemon (owns the control socket + tray)
daemon *ARGS:
    cargo run -p corral-daemon -- {{ARGS}}

# Run the desktop (egui) attention board
gui *ARGS:
    cargo run -p corral-gui -- {{ARGS}}

# Watch + rebuild-and-rerun the board (TUI) on every change
watch-board *ARGS:
    cargo watch -c -x 'run -p corral -- {{ARGS}}'

# Watch + rebuild-and-rerun the desktop GUI on every change
watch-gui *ARGS:
    cargo watch -c -x 'run -p corral-gui -- {{ARGS}}'

# Watch + rebuild-and-rerun the daemon on every change
watch-daemon *ARGS:
    cargo watch -c -x 'run -p corral-daemon -- {{ARGS}}'
