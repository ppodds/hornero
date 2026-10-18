# List all recipes
default:
    @just --list

# Build the whole workspace
build:
    cargo build --workspace --all-targets

# Run all tests
test:
    cargo test --workspace

# Run clippy, treating warnings as errors
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format the workspace
fmt:
    cargo fmt --all

# Check formatting only (for CI)
fmt-check:
    cargo fmt --all --check

# Full pre-commit check: formatting, lint, tests
check: fmt-check lint test

# Build the API docs and open them in a browser
doc:
    cargo doc --workspace --no-deps --open

# Regenerate the docs snippets and insta snapshots
update-snippets:
    HORNERO_UPDATE_SNIPPETS=1 cargo test -p hornero-docs

# Run the todo example (config and sqlite paths are relative to the working directory)
example:
    cd examples/todo && cargo run

# Coverage summary (requires cargo-llvm-cov: cargo install cargo-llvm-cov)
coverage:
    cargo llvm-cov --workspace --summary-only

# Coverage HTML report, written to target/llvm-cov/html
coverage-html:
    cargo llvm-cov --workspace --html --open

# Coverage lcov report, written to target/llvm-cov/lcov.info (for CI or editors)
coverage-lcov:
    cargo llvm-cov --workspace --lcov --output-path target/llvm-cov/lcov.info

# Remove coverage artifacts
coverage-clean:
    cargo llvm-cov clean --workspace
