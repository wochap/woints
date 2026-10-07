default: build

build:
    cargo build --workspace

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings

run-daemon *ARGS:
    RUST_LOG=${RUST_LOG:-debug} cargo run -p woints-daemon --bin wointsd -- {{ARGS}}

# Render hints for the focused window and dump one PNG per output into DIR.
dump DIR="/tmp/woints-dump":
    mkdir -p {{DIR}}
    WOINTS_DUMP={{DIR}} RUST_LOG=${RUST_LOG:-debug} cargo run -p woints-daemon --bin wointsd
