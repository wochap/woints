default: build

build:
    cargo build --workspace

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets -- -D warnings

# ONNX build; tests load nixpkgs onnxruntime through ORT_DYLIB_PATH.
test-onnx:
    ORT_DYLIB_PATH=$(nix build --no-link --print-out-paths nixpkgs#onnxruntime)/lib/libonnxruntime.so cargo test --workspace --features woints-vision/onnx

lint-onnx:
    cargo clippy --workspace --all-targets --features woints-vision/onnx -- -D warnings

run-daemon *ARGS:
    RUST_LOG=${RUST_LOG:-debug} cargo run -p woints-daemon --bin wointsd -- {{ARGS}}

# Render hints for the focused window and dump one PNG per output into DIR.
dump DIR="/tmp/woints-dump":
    mkdir -p {{DIR}}
    WOINTS_DUMP={{DIR}} RUST_LOG=${RUST_LOG:-debug} cargo run -p woints-daemon --bin wointsd
